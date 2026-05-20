//! GPU renderer: wgpu surface + glyphon (cosmic-text) glyph atlas for text,
//! plus an instanced quad pipeline for cell backgrounds, cursor, selection,
//! search highlights, split dividers, focus borders and the tab bar.
//!
//! Multiple panes are tiled in a single frame: each pane gets its own
//! cosmic-text buffer clipped to its rectangle; all backgrounds/UI go through
//! one instanced quad pass and all text through one glyphon prepare/render.

mod color;
mod imgpipe;
mod quad;

use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use anyhow::{Result, anyhow};
use glyphon::cosmic_text::{FeatureTag, FontFeatures};
use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution,
    Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use kettle_config::{Config, Rgb, ScrollbarMode};
use kettle_core::EventProxy;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use color::{
    dim as dim_color, reply_for_query, reply_for_text_area_size, resolve, resolve_query,
};
use quad::{QuadInstance, QuadPipeline};

/// A search match in a pane's viewport (grid coords, pre-scrolled).
#[derive(Clone, Copy)]
pub struct HighlightRect {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub active: bool,
}

/// A hyperlink underline in a pane's viewport (grid coords).
#[derive(Clone, Copy)]
pub struct LinkRect {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub hover: bool,
}

/// One quick-select hint label drawn over the focused pane at a grid cell.
#[derive(Clone)]
pub struct HintLabel {
    pub row: usize,
    pub col: usize,
    pub label: String,
    /// Dimmed because the typed prefix no longer matches it.
    pub dim: bool,
}

/// Search-bar + hyperlink overlay state.
#[derive(Default)]
pub struct Overlay {
    pub search_query: Option<String>,
    pub search_count: usize,
    pub search_index: usize,
    pub highlights: Vec<HighlightRect>,
    pub links: Vec<LinkRect>,
    /// Quick-select hint labels (drawn over the focused pane).
    pub hint_labels: Vec<HintLabel>,
    /// `Some(typed)` while the SSH launcher is open.
    pub ssh_query: Option<String>,
    pub ssh_hint: String,
    /// `Some(typed)` while the command palette is open.
    pub palette_query: Option<String>,
    /// The ranked command labels (selected one marked) for the palette.
    pub palette_hint: String,
    /// Window has keyboard focus (solid vs hollow cursor, pane dimming).
    pub window_focused: bool,
    /// Cursor is in its "on" blink phase.
    pub cursor_visible: bool,
    /// Visual-bell intensity, 0.0 (none) .. 1.0 (just rang).
    pub bell: f32,
}

/// Pixel rectangle `(x, y, w, h)`.
pub type Rect4 = (f32, f32, f32, f32);

/// One tab segment in the tab bar.
pub struct TabSeg {
    pub idx: usize,
    /// Full segment rect.
    pub rect: Rect4,
    /// Close-button (✕) hit rect within the segment.
    pub close: Rect4,
    pub title: String,
    pub active: bool,
}

/// The tab bar geometry — computed once in the UI, used for both drawing
/// (here) and click hit-testing (app), so there is a single source of truth.
pub struct TabBar {
    /// Bar height in px (0 = hidden).
    pub height: f32,
    /// Top-left Y of the bar (0 for top position, `surface_h - h` for bottom).
    pub y: f32,
    pub segments: Vec<TabSeg>,
    /// The trailing "new tab" (+) button rect.
    pub new_tab: Rect4,
}

impl TabBar {
    pub fn hidden() -> Self {
        TabBar {
            height: 0.0,
            y: 0.0,
            segments: Vec::new(),
            new_tab: (0.0, 0.0, 0.0, 0.0),
        }
    }
}

/// One tiled pane to draw this frame.
pub struct PaneView<'a> {
    /// Pixel rect `(x, y, w, h)` within the surface.
    pub rect: (f32, f32, f32, f32),
    pub term: &'a Term<EventProxy>,
    pub focused: bool,
    /// Decoded images placed in this pane (Sixel / kitty / iTerm2).
    pub images: Vec<kettle_core::Placement>,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
    pane_buffers: Vec<TextBuffer>,
    tab_buffers: Vec<TextBuffer>,
    hint_buffers: Vec<TextBuffer>,
    tabbar_buffer: TextBuffer,
    search_buffer: TextBuffer,

    quads: QuadPipeline,
    /// Second quad pass drawn *after* text (pane dimming, scrollbar).
    overlay_quads: QuadPipeline,
    imgs: imgpipe::ImagePipeline,

    font_family: String,
    font_size: f32,
    metrics: Metrics,
    pub cell_w: f32,
    pub cell_h: f32,
    pub scale: f32,
}

impl Renderer {
    pub async fn new<W>(
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("no suitable GPU adapter: {e:?}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-device"),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("failed to create device: {e:?}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        for face in kettle_config::font::all() {
            font_system.db_mut().load_font_data(face.to_vec());
        }

        let swash = SwashCache::new();
        let cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let viewport = Viewport::new(&device, &cache);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        // Clamp `cfg.font_size` here (same range as `set_font_size`'s
        // runtime path: [5.0, 72.0]). Without this, a user config of
        // `font-size = 200` boots the renderer with 200pt cells and
        // hits the wgpu 8192px-per-side texture limit (or floods the
        // window with one giant glyph). 5.0 is below "tiny but
        // legible"; 72.0 is "billboard". The runtime setter already
        // had this clamp (cycle 73); `Renderer::new` silently didn't,
        // so the bound was only enforced after a Ctrl+0 ResetFontSize
        // round-trip — same "downstream cache stale at startup" shape
        // as cycle 98's font-family fix.
        let font_size = clamp_font_size(cfg.font_size);
        let metrics = Metrics::new(font_size, font_size * 1.25);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let tabbar_buffer = TextBuffer::new(&mut font_system, metrics);
        let search_buffer = TextBuffer::new(&mut font_system, metrics);
        let (cell_w, cell_h) =
            measure_cell(&mut font_system, &mut measure, &cfg.font_family, metrics);

        let quads = QuadPipeline::new(&device, format);
        let overlay_quads = QuadPipeline::new(&device, format);
        let imgs = imgpipe::ImagePipeline::new(&device, format);

        Ok(Renderer {
            surface,
            device,
            queue,
            config,
            font_system,
            swash,
            atlas,
            viewport,
            text_renderer,
            pane_buffers: Vec::new(),
            tab_buffers: Vec::new(),
            hint_buffers: Vec::new(),
            tabbar_buffer,
            search_buffer,
            quads,
            overlay_quads,
            imgs,
            font_family: cfg.font_family.clone(),
            font_size,
            metrics,
            cell_w,
            cell_h,
            scale,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = clamp_font_size(size);
        self.metrics = Metrics::new(self.font_size, self.font_size * 1.25);
        self.remeasure_cell();
    }

    /// Update the primary font family and re-measure the cell. Called by
    /// `reload_config` so a `font-family = …` change in the user's config
    /// actually takes effect at runtime — without this, the renderer kept
    /// the family it was constructed with forever and only the `font-size`
    /// part of a reload was visible (silent partial-apply, same family as
    /// the cycle-44+ "reload doesn't re-flow downstream caches" gap).
    pub fn set_font_family(&mut self, family: String) {
        if self.font_family == family {
            return;
        }
        self.font_family = family;
        self.remeasure_cell();
    }

    /// Re-derive `cell_w`/`cell_h` from the current `font_family` + `metrics`
    /// using the same measurer the constructor used. Extracted so font-size
    /// and font-family updates share one implementation; otherwise the two
    /// setters would drift on which fields they touch.
    fn remeasure_cell(&mut self) {
        let family = self.font_family.clone();
        let m = self.metrics;
        let mut measure = TextBuffer::new(&mut self.font_system, m);
        let (cw, ch) = measure_cell(&mut self.font_system, &mut measure, &family, m);
        self.cell_w = cw;
        self.cell_h = ch;
    }

    /// Render a full frame of tiled panes plus the tab bar and search overlay.
    pub fn render_frame(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
    ) -> Result<()> {
        let theme = &cfg.theme;
        // OSC 11 (set default background) override from the focused pane.
        // The engine stores it in `Colors[257]`; the renderer needs it for
        // the surface clear-color (chrome regions: window padding, gaps
        // between panes, tab-bar background) so a program-driven bg flip
        // reaches the *whole* window rather than just cells with explicit
        // `Named(Background)`. Same precedence the OSC 11 query path
        // returns (cycle 44) — override wins, theme is fallback.
        let default_bg = panes
            .iter()
            .find(|p| p.focused)
            .and_then(|p| p.term.colors()[257])
            .map(|c| Rgb::new(c.r, c.g, c.b))
            .unwrap_or(theme.background);
        let pad_x = cfg.padding_x;
        let pad_y = cfg.padding_y;
        let cw = self.cell_w;
        let ch = self.cell_h;
        let metrics = self.metrics;
        let family = self.font_family.clone();
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;

        // Ensure one text buffer per pane.
        while self.pane_buffers.len() < panes.len() {
            let b = TextBuffer::new(&mut self.font_system, metrics);
            self.pane_buffers.push(b);
        }

        let mut quads: Vec<QuadInstance> = Vec::new();
        // Drawn *after* text: unfocused-pane dimming + scrollbar thumbs.
        let mut over: Vec<QuadInstance> = Vec::new();
        let mut img_items: Vec<(f32, f32, f32, f32, kettle_core::ImageData)> = Vec::new();
        let mut live: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // Tab bar background + per-segment chrome (text added later).
        if tabbar.height > 0.0 {
            let by = tabbar.y;
            quads.push(rect(0.0, by, sw, tabbar.height, theme.palette[8], 1.0));
            for s in &tabbar.segments {
                let (x, _, w, _) = s.rect;
                if s.active {
                    quads.push(rect(x, by, w, tabbar.height, default_bg, 1.0));
                    // Active accent bar on the left edge.
                    quads.push(rect(x, by, 2.0, tabbar.height, theme.palette[4], 1.0));
                }
                // Thin separator on the right of each segment.
                quads.push(rect(
                    x + w - 1.0,
                    by,
                    1.0,
                    tabbar.height,
                    theme.background,
                    0.5,
                ));
            }
            // New-tab (+) button background.
            let (nx, _, nw, _) = tabbar.new_tab;
            quads.push(rect(nx, by, nw, tabbar.height, theme.palette[8], 1.0));
        }

        // Per-pane grid + dividers/border.
        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.rect;
            // Pane separators / focus border. Both colors are config-
            // overridable: `split-divider-color` for inactive panes
            // (defaults to theme `palette[8]`, the dim color) and
            // `focused-split-color` for the focused pane (defaults to
            // theme `palette[4]`, the accent blue).
            let border = if pv.focused {
                cfg.focused_split_color.unwrap_or(theme.palette[4])
            } else {
                cfg.split_divider_color.unwrap_or(theme.palette[8])
            };
            quads.push(rect(rx, ry, rw, 1.0, border, 1.0));
            quads.push(rect(rx, ry + rh - 1.0, rw, 1.0, border, 1.0));
            quads.push(rect(rx, ry, 1.0, rh, border, 1.0));
            quads.push(rect(rx + rw - 1.0, ry, 1.0, rh, border, 1.0));

            self.build_pane(
                i,
                pv,
                cfg,
                &family,
                overlay.window_focused,
                overlay.cursor_visible,
                &mut quads,
            );

            // Image placements, anchored history-aware so they scroll.
            {
                let g = pv.term.grid();
                let top = g.history_size() as i64 - g.display_offset() as i64;
                let nrows = g.screen_lines() as i64;
                // Draw in ascending z so higher z-index images land on top.
                let mut ordered: Vec<&kettle_core::Placement> = pv.images.iter().collect();
                ordered.sort_by_key(|p| p.z);
                for p in ordered {
                    let row = p.abs_line - top;
                    if row + p.cell_rows as i64 <= 0 || row >= nrows {
                        continue;
                    }
                    live.insert(std::sync::Arc::as_ptr(&p.img.rgba) as usize);
                    img_items.push((
                        rx + pad_x + p.col as f32 * cw,
                        ry + pad_y + row as f32 * ch,
                        p.cell_cols as f32 * cw,
                        p.cell_rows as f32 * ch,
                        p.img.clone(),
                    ));
                }
            }

            // Hyperlink underlines (all panes show them; brighter on hover).
            for ln in &overlay.links {
                if !pv.focused {
                    break;
                }
                let col = if ln.hover {
                    theme.palette[6]
                } else {
                    theme.palette[4]
                };
                quads.push(rect(
                    rx + pad_x + ln.col as f32 * cw,
                    ry + pad_y + ln.row as f32 * ch + ch - 1.5,
                    ln.width as f32 * cw,
                    1.5,
                    col,
                    1.0,
                ));
            }

            // Search highlights are drawn over the focused pane.
            if pv.focused {
                for hl in &overlay.highlights {
                    quads.push(rect(
                        rx + pad_x + hl.col as f32 * cw,
                        ry + pad_y + hl.row as f32 * ch,
                        hl.width as f32 * cw,
                        ch,
                        if hl.active {
                            cfg.search_background
                        } else {
                            theme.selection_background
                        },
                        0.85,
                    ));
                }
                // Quick-select hint label chips.
                for hint in &overlay.hint_labels {
                    let n = hint.label.chars().count().max(1) as f32;
                    quads.push(rect(
                        rx + pad_x + hint.col as f32 * cw,
                        ry + pad_y + hint.row as f32 * ch,
                        n * cw,
                        ch,
                        if hint.dim {
                            theme.palette[8]
                        } else {
                            cfg.search_background
                        },
                        if hint.dim { 0.6 } else { 0.96 },
                    ));
                }
            }

            // Post-text overlay: dim unfocused panes; per-pane scrollbar.
            if !pv.focused && panes.len() > 1 && cfg.unfocused_split_opacity < 1.0 {
                over.push(rect(
                    rx,
                    ry,
                    rw,
                    rh,
                    theme.background,
                    1.0 - cfg.unfocused_split_opacity,
                ));
            }
            if cfg.scrollbar != ScrollbarMode::Never {
                let g = pv.term.grid();
                let (rows, hist, off) = (g.screen_lines(), g.history_size(), g.display_offset());
                let show = cfg.scrollbar == ScrollbarMode::Always
                    || (cfg.scrollbar == ScrollbarMode::Auto && off > 0);
                if show && let Some((ty, th)) = kettle_core::scrollbar::thumb(rows, hist, off, rh) {
                    over.push(rect(rx + rw - 4.0, ry + ty, 3.0, th, theme.palette[8], 0.8));
                }
            }
        }

        // Visual bell: a brief full-surface flash (replaces an audible beep).
        if overlay.bell > 0.0 {
            quads.push(rect(
                0.0,
                0.0,
                sw,
                sh,
                theme.foreground,
                overlay.bell * 0.18,
            ));
        }

        // Search bar overlay.
        let mut have_search = false;
        if let Some(q) = &overlay.search_query {
            have_search = true;
            let bar_h = ch + 10.0;
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[8], 0.96));
            let label = format!(
                "  search: {}_    [{}/{}]   (Enter next · Shift+Enter prev · Esc close)",
                q,
                if overlay.search_count == 0 {
                    0
                } else {
                    overlay.search_index + 1
                },
                overlay.search_count
            );
            self.search_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.search_buffer
                .set_size(&mut self.font_system, Some(sw), Some(bar_h));
            self.search_buffer.set_text(
                &mut self.font_system,
                &label,
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else if let Some(q) = &overlay.palette_query {
            have_search = true;
            let bar_h = ch + 10.0;
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[5], 0.96));
            let label = format!(
                "  ⌘ {q}_   ▸ {}   (Enter run · Tab/↑↓ select · Esc cancel)",
                overlay.palette_hint
            );
            self.search_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.search_buffer
                .set_size(&mut self.font_system, Some(sw), Some(bar_h));
            self.search_buffer.set_text(
                &mut self.font_system,
                &label,
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else if let Some(q) = &overlay.ssh_query {
            have_search = true;
            let bar_h = ch + 10.0;
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[4], 0.96));
            let label = format!(
                "  ssh ❯ {q}_    {}   (Enter connect · Tab complete · Esc cancel)",
                overlay.ssh_hint
            );
            self.search_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.search_buffer
                .set_size(&mut self.font_system, Some(sw), Some(bar_h));
            self.search_buffer.set_text(
                &mut self.font_system,
                &label,
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        }

        // Tab-bar text: one buffer per segment + the `+` button.
        let have_tabs = tabbar.height > 0.0 && !tabbar.segments.is_empty();
        if have_tabs {
            while self.tab_buffers.len() < tabbar.segments.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.tab_buffers.push(b);
            }
            for (bi, s) in tabbar.segments.iter().enumerate() {
                let (_, _, w, _) = s.rect;
                // chars that fit: segment minus the ✕ zone, ~cell_w each.
                let maxc = (((w - tabbar.height) / cw) as usize).clamp(3, 24);
                let n = (s.idx + 1).to_string();
                let title = truncate(&s.title, maxc);
                let body =
                    kettle_config::template::fill(&cfg.tab_format, &[("n", &n), ("title", &title)]);
                let label = format!(" {body}  ✕");
                let buf = &mut self.tab_buffers[bi];
                buf.set_metrics(&mut self.font_system, metrics);
                buf.set_size(&mut self.font_system, Some(w), Some(tabbar.height));
                buf.set_text(
                    &mut self.font_system,
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }
            // `+` button glyph.
            self.tabbar_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.tabbar_buffer.set_size(
                &mut self.font_system,
                Some(tabbar.new_tab.2),
                Some(tabbar.height),
            );
            self.tabbar_buffer.set_text(
                &mut self.font_system,
                " +",
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.tabbar_buffer
                .shape_until_scroll(&mut self.font_system, false);
        }

        // Quick-select hint label glyphs (one buffer per label).
        if !overlay.hint_labels.is_empty() {
            while self.hint_buffers.len() < overlay.hint_labels.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.hint_buffers.push(b);
            }
            for (i, hint) in overlay.hint_labels.iter().enumerate() {
                let n = hint.label.chars().count().max(1) as f32;
                let buf = &mut self.hint_buffers[i];
                buf.set_metrics(&mut self.font_system, metrics);
                buf.set_size(&mut self.font_system, Some(n * cw + 2.0), Some(ch));
                buf.set_text(
                    &mut self.font_system,
                    &hint.label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Basic,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }
        let focus_origin = panes.iter().find(|p| p.focused).map(|p| p.rect);

        // Assemble text areas (panes + tab bar + search).
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        // Theme foreground — used by tab bar text and other chrome below
        // (where there's no specific pane to take an OSC 10 override from).
        let fg = theme.foreground;
        let mut areas: Vec<TextArea> = Vec::with_capacity(panes.len() + 2);
        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.rect;
            // Per-pane OSC 10 default-fg: glyphon's `default_color` is the
            // fallback when a span lacks an explicit color. Almost every
            // cell does carry an explicit color via `Attrs::color`, but
            // whitespace / IME composition / chrome strings ride the
            // default. Matches the OSC 11 chrome path landed in cycle 65 —
            // engine override (Colors[256]) wins, theme is fallback.
            let pane_fg = pv.term.colors()[256]
                .map(|c| Rgb::new(c.r, c.g, c.b))
                .unwrap_or(theme.foreground);
            areas.push(TextArea {
                buffer: &self.pane_buffers[i],
                left: rx + pad_x,
                top: ry + pad_y,
                scale: 1.0,
                bounds: TextBounds {
                    left: rx as i32,
                    top: ry as i32,
                    right: (rx + rw) as i32,
                    bottom: (ry + rh) as i32,
                },
                default_color: GColor::rgb(pane_fg.r, pane_fg.g, pane_fg.b),
                custom_glyphs: &[],
            });
        }
        if have_tabs {
            let ty = tabbar.y as i32;
            let tb = (tabbar.y + tabbar.height) as i32;
            for (bi, s) in tabbar.segments.iter().enumerate() {
                let (x, _, w, _) = s.rect;
                areas.push(TextArea {
                    buffer: &self.tab_buffers[bi],
                    left: x + 6.0,
                    top: tabbar.y + 4.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: x as i32,
                        top: ty,
                        right: (x + w) as i32,
                        bottom: tb,
                    },
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
            }
            let (nx, _, nw, _) = tabbar.new_tab;
            areas.push(TextArea {
                buffer: &self.tabbar_buffer,
                left: nx + 4.0,
                top: tabbar.y + 4.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: nx as i32,
                    top: ty,
                    right: (nx + nw) as i32,
                    bottom: tb,
                },
                default_color: GColor::rgb(fg.r, fg.g, fg.b),
                custom_glyphs: &[],
            });
        }
        if have_search {
            let bar_h = ch + 10.0;
            areas.push(TextArea {
                buffer: &self.search_buffer,
                left: 0.0,
                top: sh - bar_h + 5.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.config.width as i32,
                    bottom: self.config.height as i32,
                },
                default_color: GColor::rgb(fg.r, fg.g, fg.b),
                custom_glyphs: &[],
            });
        }
        // Hint labels over the focused pane (chips drawn above as quads).
        if let Some((frx, fry, frw, frh)) = focus_origin {
            let lab = cfg.search_foreground;
            for (i, hint) in overlay.hint_labels.iter().enumerate() {
                areas.push(TextArea {
                    buffer: &self.hint_buffers[i],
                    left: frx + pad_x + hint.col as f32 * cw,
                    top: fry + pad_y + hint.row as f32 * ch,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: frx as i32,
                        top: fry as i32,
                        right: (frx + frw) as i32,
                        bottom: (fry + frh) as i32,
                    },
                    default_color: GColor::rgb(lab.r, lab.g, lab.b),
                    custom_glyphs: &[],
                });
            }
        }

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )?;
        self.quads
            .upload(&self.device, &self.queue, [sw, sh], &quads);
        self.imgs
            .upload(&self.device, &self.queue, [sw, sh], &img_items);
        self.imgs.gc(&live);
        self.overlay_quads
            .upload(&self.device, &self.queue, [sw, sh], &over);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            _ => return Ok(()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kettle-encoder"),
            });
        {
            let bg = default_bg;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: srgb(bg.r),
                            g: srgb(bg.g),
                            b: srgb(bg.b),
                            a: cfg.background_opacity as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.quads.draw(&mut pass);
            self.imgs.draw(&mut pass);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
            // Dimming + scrollbar sit on top of glyphs.
            self.overlay_quads.draw(&mut pass);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        self.atlas.trim();
        Ok(())
    }

    /// Build one pane's text buffer + background/cursor/selection/search quads.
    #[allow(clippy::too_many_arguments)]
    fn build_pane(
        &mut self,
        idx: usize,
        pv: &PaneView<'_>,
        cfg: &Config,
        family: &str,
        window_focused: bool,
        cursor_visible: bool,
        quads: &mut Vec<QuadInstance>,
    ) {
        let theme = &cfg.theme;
        let (rx, ry, rw, rh) = pv.rect;
        let ox = rx + cfg.padding_x;
        let oy = ry + cfg.padding_y;
        let cw = self.cell_w;
        let ch = self.cell_h;
        let term = pv.term;
        let content = term.renderable_content();
        let term_colors = content.colors;
        let cols = term.grid().columns();
        // Capture the selection range *before* the display iterator
        // consumes its sibling field; cells inside this range get their
        // fg swapped to `theme.selection_foreground` so dark-on-dark
        // themes stay readable under the highlight. Without this, the
        // configured `selection-foreground` color was parsed and stored
        // but the renderer ignored it.
        let selection_range = content.selection;
        // Match the surface clear-color so a cell whose bg resolves to the
        // active default (OSC 11 override or theme bg) doesn't paint a
        // redundant quad over the already-correct backdrop.
        let default_bg = term_colors[257]
            .map(|c| Rgb::new(c.r, c.g, c.b))
            .unwrap_or(theme.background);

        let mut spans: Vec<(String, Rgb, bool, bool)> = Vec::new();
        let mut span_line_breaks: Vec<usize> = Vec::new();
        let mut cur_row = 0i32;
        let mut cur: Option<(String, Rgb, bool, bool)> = None;

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let row = point.line.0;
            let col = point.column.0;
            if row != cur_row {
                if let Some(s) = cur.take() {
                    spans.push(s);
                }
                for _ in cur_row..row {
                    span_line_breaks.push(spans.len());
                }
                cur_row = row;
            }

            let flags = cell.flags;
            let mut fg = color::resolve(cell.fg, theme, term_colors);
            let mut bg = color::resolve(cell.bg, theme, term_colors);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            // Selection foreground override — applied *after* INVERSE so the
            // selection always wins for readability (alacritty / iTerm2
            // behavior). Without this, a cell with INVERSE under a selection
            // would render as inverse-fg on selection-bg, often invisible.
            if selection_range.is_some_and(|r| r.contains(point)) {
                fg = theme.selection_foreground;
            }
            // SGR 2 dim/faint — blend the foreground halfway toward the
            // background. Renderer was ignoring `Flags::DIM` so `\e[2m`
            // looked the same as normal weight; fish prompt themers,
            // `less` status lines, mc all use it. Applied *before* the
            // minimum-contrast lift so the lift can claw back legibility
            // when a theme's dim level becomes unreadable.
            if flags.contains(Flags::DIM) {
                fg = color::dim(fg, bg);
            }
            // Lift fg toward the higher-contrast extreme if the theme/SGR
            // combo falls below the configured WCAG ratio (off by default).
            if cfg.minimum_contrast > 1.0 {
                fg = color::with_min_contrast(fg, bg, cfg.minimum_contrast as f64);
            }
            let bold = flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            let hidden = flags.contains(Flags::HIDDEN);

            if bg != default_bg {
                quads.push(rect(
                    ox + col as f32 * cw,
                    oy + row as f32 * ch,
                    cw,
                    ch,
                    bg,
                    1.0,
                ));
            }
            // SGR 4 underline family / SGR 9 strikeout — both engine-
            // tracked since cycle ~14 (`sgr_underline_dim_strike` test),
            // rendered from cycle 79 onward.
            //
            // Underline color: SGR 58 (`\e[58;2;r;g;bm` / `[58;5;Nm`) sets
            // a per-cell `underline_color`, used by neovim spell-check to
            // draw red squiggles on otherwise-normal text. Resolve it via
            // the same path as fg/bg; fall back to `fg` when unset so
            // every existing usage keeps working.
            //
            // Underline style: alacritty exposes five style bits —
            // UNDERLINE, DOUBLE_UNDERLINE, UNDERCURL, DOTTED_UNDERLINE,
            // DASHED_UNDERLINE — all reached via `Flags::ALL_UNDERLINES`.
            // Today: plain / curl / dotted / dashed all draw as a single
            // 1-px line at the cell bottom; DOUBLE_UNDERLINE draws two
            // stacked lines (the visually-distinct case). Wave / dotted
            // / dashed visual styles want a shader path and are deferred
            // — the presence/absence cue is what matters most.
            if flags.intersects(Flags::ALL_UNDERLINES) {
                let line_color = cell
                    .underline_color()
                    .map(|c| color::resolve(c, theme, term_colors))
                    .unwrap_or(fg);
                let x = ox + col as f32 * cw;
                let y = oy + row as f32 * ch;
                quads.push(rect(x, y + ch - 2.0, cw, 1.0, line_color, 1.0));
                if flags.contains(Flags::DOUBLE_UNDERLINE) {
                    quads.push(rect(x, y + ch - 4.0, cw, 1.0, line_color, 1.0));
                }
            }
            if flags.contains(Flags::STRIKEOUT) {
                quads.push(rect(
                    ox + col as f32 * cw,
                    oy + row as f32 * ch + ch * 0.5,
                    cw,
                    1.0,
                    fg,
                    1.0,
                ));
            }
            let dc = if hidden { ' ' } else { cell.c };
            match &mut cur {
                Some((t, f, cb, ci)) if *f == fg && *cb == bold && *ci == italic => t.push(dc),
                _ => {
                    if let Some(s) = cur.take() {
                        spans.push(s);
                    }
                    cur = Some((dc.to_string(), fg, bold, italic));
                }
            }
        }
        if let Some(s) = cur.take() {
            spans.push(s);
        }

        // Selection.
        if let Some(sel) = content.selection {
            let (s, e) = (sel.start, sel.end);
            for r in s.line.0..=e.line.0 {
                if r < 0 {
                    continue;
                }
                let (c0, c1) = if s.line.0 == e.line.0 {
                    (s.column.0, e.column.0)
                } else if r == s.line.0 {
                    (s.column.0, cols.saturating_sub(1))
                } else if r == e.line.0 {
                    (0, e.column.0)
                } else {
                    (0, cols.saturating_sub(1))
                };
                let w = (c1 + 1).saturating_sub(c0).max(1);
                quads.push(rect(
                    ox + c0 as f32 * cw,
                    oy + r as f32 * ch,
                    w as f32 * cw,
                    ch,
                    theme.selection_background,
                    1.0,
                ));
            }
        }

        // Cursor: hollow when the window is unfocused, blink-aware otherwise.
        // Shape comes from the engine's live `RenderableContent.cursor.shape`
        // which DECSCUSR (`CSI Ps SP q`) updates per-pane — vim/neovim/fish
        // use this to flip between block/underline/beam for normal/insert/
        // replace modes. The engine is seeded from `cfg.cursor_style` at pane
        // creation so the default still matches the user's config.
        use alacritty_terminal::vte::ansi::CursorShape as EShape;
        let cp = content.cursor.point;
        let shape = content.cursor.shape;
        let draw_cursor = shape != EShape::Hidden && cp.line.0 >= 0 && pv.focused;
        if draw_cursor {
            let bx = ox + cp.column.0 as f32 * cw;
            let by = oy + cp.line.0 as f32 * ch;
            // OSC 12 cursor color override (stored in `term_colors[258]`)
            // takes precedence over the theme — same precedence rule the
            // OSC 4/10/11/12 *query* path returns. Without this, programs
            // could set the cursor color but the renderer kept drawing the
            // theme cursor (a silent drop, mirror of the OSC color-query
            // bug that was fixed two weeks ago for the *read* direction).
            let cursor_color =
                color::resolve_query(258, theme, term_colors).unwrap_or(theme.cursor);
            // Hollow outline — used by the unfocused-window state *and* when
            // the running program asks for `HollowBlock` (the DECSCUSR
            // semantics most apps treat as "I'm not in this pane right now").
            if !window_focused || shape == EShape::HollowBlock {
                quads.push(rect(bx, by, cw, 1.0, cursor_color, 1.0));
                quads.push(rect(bx, by + ch - 1.0, cw, 1.0, cursor_color, 1.0));
                quads.push(rect(bx, by, 1.0, ch, cursor_color, 1.0));
                quads.push(rect(bx + cw - 1.0, by, 1.0, ch, cursor_color, 1.0));
            } else if cursor_visible {
                let (cwidth, alpha, cheight, yoff) = match shape {
                    EShape::Beam => (cw * 0.15, 1.0, ch, 0.0),
                    EShape::Underline => (cw, 1.0, 2.0, ch - 2.0),
                    // Engine variants we draw as a solid block.
                    EShape::Block | EShape::HollowBlock | EShape::Hidden => (cw, 0.55, ch, 0.0),
                };
                quads.push(rect(bx, by + yoff, cwidth, cheight, cursor_color, alpha));
            }
        }

        // Lay out the text buffer.
        let buf = &mut self.pane_buffers[idx];
        buf.set_metrics(&mut self.font_system, self.metrics);
        buf.set_size(
            &mut self.font_system,
            Some((rw - cfg.padding_x * 2.0).max(1.0)),
            Some((rh - cfg.padding_y * 2.0).max(1.0)),
        );
        let ff = font_features(cfg);
        let default_attrs = Attrs::new()
            .family(Family::Name(family))
            .font_features(ff.clone());
        let mut rich: Vec<(String, Attrs)> = Vec::new();
        let mut nb = 0usize;
        for (i, (text, fg, bold, italic)) in spans.iter().enumerate() {
            while nb < span_line_breaks.len() && span_line_breaks[nb] == i {
                rich.push(("\n".to_string(), default_attrs.clone()));
                nb += 1;
            }
            let mut a = Attrs::new()
                .family(Family::Name(cfg.family_for(*bold, *italic)))
                .font_features(ff.clone())
                .color(GColor::rgb(fg.r, fg.g, fg.b));
            if *bold {
                a = a.weight(Weight::BOLD);
            }
            if *italic {
                a = a.style(Style::Italic);
            }
            rich.push((text.clone(), a));
        }
        // Advanced shaping applies OpenType features (ligatures, ss##,
        // cv##, …). Drop to Basic only when ligatures are off *and* there
        // are no explicit features to honor — the fast path with no shaping.
        let shaping = if cfg.font_ligatures || !cfg.font_features.is_empty() {
            Shaping::Advanced
        } else {
            Shaping::Basic
        };
        buf.set_rich_text(
            &mut self.font_system,
            rich.iter().map(|(s, a)| (s.as_str(), a.clone())),
            &default_attrs,
            shaping,
            None,
        );
        buf.shape_until_scroll(&mut self.font_system, false);
    }
}

/// OpenType features to shape pane text with: the coarse ligature toggle
/// expressed as `liga/clig/calt/dlig = 0` when off, then the user's explicit
/// `font-feature` overrides applied on top (so they can re-enable or tune
/// individual features). Cited: Ghostty `font-feature`, kitty `font_features`.
fn font_features(cfg: &Config) -> FontFeatures {
    let mut ff = FontFeatures::new();
    if !cfg.font_ligatures {
        for tag in [b"liga", b"clig", b"calt", b"dlig"] {
            ff.disable(FeatureTag::new(tag));
        }
    }
    for f in &cfg.font_features {
        ff.set(FeatureTag::new(&f.tag), f.value);
    }
    ff
}

/// Truncate `s` to at most `n` **display columns** (not chars), adding `…`
/// when something was cut. CJK characters and emoji are wide (2 cells
/// each), so a char-count truncation overflows the tab segment / title
/// when these are present; this honors the cell width that the renderer
/// Sanitize a font size against the renderer's safe range. 5.0 is the
/// floor below which cosmic-text's metrics become numerically unstable
/// (sub-pixel cell dims, antialiasing falls apart); 72.0 is the ceiling
/// above which a typical 1080p window's worth of cells exceeds the wgpu
/// 8192-px-per-side texture limit. Shared by `Renderer::new` and
/// `set_font_size` so the startup path and the runtime path can't
/// drift on which sizes they accept. Pure so the bounds are unit-tested
/// without standing up wgpu.
pub fn clamp_font_size(size: f32) -> f32 {
    // `clamp` on f32 panics on NaN; treat that as "use default" by
    // routing it to the floor rather than letting it propagate to
    // cosmic-text where it would silently produce zero-sized cells.
    if size.is_nan() {
        return 5.0;
    }
    size.clamp(5.0, 72.0)
}

/// actually paints into. The ellipsis itself is 1 cell so we reserve a
/// column for it.
fn truncate(s: &str, n: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= n {
        return s.to_string();
    }
    let limit = n.saturating_sub(1); // reserve 1 col for the `…`
    let mut acc = 0usize;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if acc + w > limit {
            break;
        }
        out.push(c);
        acc += w;
    }
    out.push('…');
    out
}

fn rect(x: f32, y: f32, w: f32, h: f32, c: Rgb, a: f32) -> QuadInstance {
    QuadInstance {
        pos: [x, y],
        size: [w.max(0.0), h.max(0.0)],
        color: [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            a,
        ],
    }
}

fn srgb(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn measure_cell(
    fs: &mut FontSystem,
    buf: &mut TextBuffer,
    family: &str,
    metrics: Metrics,
) -> (f32, f32) {
    buf.set_metrics(fs, metrics);
    buf.set_size(fs, Some(1000.0), Some(100.0));
    buf.set_text(
        fs,
        "MMMMMMMMMM",
        &Attrs::new().family(Family::Name(family)),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(fs, false);
    let mut w = metrics.font_size * 0.6;
    if let Some(run) = buf.layout_runs().next()
        && run.line_w > 0.0
    {
        w = run.line_w / 10.0;
    }
    (w, metrics.line_height)
}

fn gc(c: Rgb) -> GColor {
    GColor::rgb(c.r, c.g, c.b)
}

/// Render a representative kettle frame **offscreen** (no window/surface) and
/// write it to a PNG. Used by `kettle --screenshot <out.png>` to produce the
/// showcase images embedded in `docs/UX-COMPARISON.md`.
///
/// This drives kettle's *real* GPU text + quad path (bundled Nerd Font,
/// `glyphon` shaping, the `QuadPipeline`, the active theme) over a scripted
/// demo: a two-pane vertical split under the redesigned tab bar (active tab,
/// per-tab `✕`, trailing `+`), with a themed shell session on the left and a
/// monitor-style readout on the right. Content is synthetic; the rendering
/// pipeline is identical to the live one.
pub fn capture_png(cfg: &Config, cols: u32, rows: u32, out: &std::path::Path) -> Result<()> {
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("no GPU adapter: {e:?}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-screenshot"),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("device: {e:?}"))?;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;

        let mut font_system = FontSystem::new();
        for face in kettle_config::font::all() {
            font_system.db_mut().load_font_data(face.to_vec());
        }
        let swash_cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &swash_cache, format);
        let viewport = Viewport::new(&device, &swash_cache);
        let mut text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let mut swash = SwashCache::new();
        let mut quads = QuadPipeline::new(&device, format);

        let theme = &cfg.theme;
        let fam = cfg.font_family.clone();
        let metrics = Metrics::new(cfg.font_size, cfg.font_size * 1.25);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let (cw, ch) = measure_cell(&mut font_system, &mut measure, &fam, metrics);

        let pad = cfg.padding_x.max(8.0);
        let tab_h = ch + 12.0;
        let body_w = cols as f32 * cw;
        let body_h = rows as f32 * ch;
        let w = (pad * 2.0 + body_w).ceil() as u32;
        let h = (tab_h + body_h + pad * 2.0).ceil() as u32;
        let (wf, hf) = (w as f32, h as f32);
        let split_x = (wf / 2.0).round();

        let base = Attrs::new().family(Family::Name(&fam));
        let mut q: Vec<QuadInstance> = Vec::new();

        // --- Tab bar (redesigned: active accent + per-tab ✕ + trailing +).
        q.push(rect(0.0, 0.0, wf, tab_h, theme.palette[8], 1.0));
        let segw = 240.0_f32.min((wf - 44.0) / 2.0);
        // Active tab 0: themed background + left accent bar.
        q.push(rect(0.0, 0.0, segw, tab_h, theme.background, 1.0));
        q.push(rect(0.0, 0.0, 2.0, tab_h, theme.palette[4], 1.0));
        // Per-segment separators.
        q.push(rect(segw - 1.0, 0.0, 1.0, tab_h, theme.background, 0.5));
        q.push(rect(
            2.0 * segw - 1.0,
            0.0,
            1.0,
            tab_h,
            theme.background,
            0.5,
        ));
        // Trailing new-tab (+) button.
        q.push(rect(2.0 * segw, 0.0, 40.0, tab_h, theme.palette[8], 1.0));

        // --- Two-pane vertical split with focus border on the left pane.
        q.push(rect(
            split_x - 1.0,
            tab_h,
            2.0,
            hf - tab_h,
            theme.palette[8],
            1.0,
        ));
        let foc = theme.palette[4];
        let ly = tab_h;
        let lh = hf - tab_h;
        q.push(rect(0.0, ly, split_x, 1.0, foc, 1.0));
        q.push(rect(0.0, ly + lh - 1.0, split_x, 1.0, foc, 1.0));
        q.push(rect(0.0, ly, 1.0, lh, foc, 1.0));
        q.push(rect(split_x - 1.0, ly, 1.0, lh, foc, 1.0));

        // Block cursor on the left pane's active prompt line.
        let cur_row = 6.0;
        q.push(rect(
            pad + 22.0 * cw,
            ly + pad + cur_row * ch,
            cw,
            ch,
            theme.cursor,
            1.0,
        ));

        // --- Text buffers (rich, themed spans) -------------------------------
        let p = theme.palette;
        let dim = Attrs::new().family(Family::Name(&fam)).color(gc(p[8]));
        let grn = Attrs::new().family(Family::Name(&fam)).color(gc(p[2]));
        let blu = Attrs::new().family(Family::Name(&fam)).color(gc(p[4]));
        let yel = Attrs::new().family(Family::Name(&fam)).color(gc(p[3]));
        let mag = Attrs::new().family(Family::Name(&fam)).color(gc(p[5]));
        let fg = Attrs::new()
            .family(Family::Name(&fam))
            .color(gc(theme.foreground));

        let mut tab_buf = TextBuffer::new(&mut font_system, metrics);
        tab_buf.set_size(&mut font_system, Some(wf), Some(tab_h));
        tab_buf.set_rich_text(
            &mut font_system,
            [
                (" 1: zsh  ✕   ", fg.clone()),
                ("2: ssh prod  ✕", dim.clone()),
                ("     +", grn.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        tab_buf.shape_until_scroll(&mut font_system, false);

        let mut left = TextBuffer::new(&mut font_system, metrics);
        left.set_size(&mut font_system, Some(split_x - pad), Some(lh));
        left.set_rich_text(
            &mut font_system,
            [
                ("kevim@kettle", grn.clone()),
                (":", fg.clone()),
                ("~/Repos/kettle", blu.clone()),
                ("$ cargo test --workspace\n", fg.clone()),
                ("   Compiling ", dim.clone()),
                ("kettle v0.1.0\n", dim.clone()),
                ("    Finished ", grn.clone()),
                ("`test` profile [optimized]\n", fg.clone()),
                ("     Running ", grn.clone()),
                ("unittests\n", fg.clone()),
                ("test result: ", fg.clone()),
                ("ok", grn.clone()),
                (". 74 passed; 0 failed\n\n", fg.clone()),
                ("kevim@kettle", grn.clone()),
                (":", fg.clone()),
                ("~/Repos/kettle", blu.clone()),
                ("$ ", fg.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        left.shape_until_scroll(&mut font_system, false);

        let mut right = TextBuffer::new(&mut font_system, metrics);
        right.set_size(&mut font_system, Some(wf - split_x - pad), Some(lh));
        right.set_rich_text(
            &mut font_system,
            [
                ("  kettle — cross-platform terminal\n\n", mag.clone()),
                ("CPU ", fg.clone()),
                ("|||||||||||", grn.clone()),
                ("|||||", yel.clone()),
                ("        37%\n", fg.clone()),
                ("MEM ", fg.clone()),
                ("||||||||", blu.clone()),
                ("            5.1G/32G\n", fg.clone()),
                ("NET ", fg.clone()),
                ("↓ 1.2 MB/s  ↑ 88 KB/s\n\n", dim.clone()),
                ("  GPU: ", fg.clone()),
                ("wgpu", grn.clone()),
                (" · font: ", fg.clone()),
                ("JetBrainsMono NF", blu.clone()),
                ("\n  theme: ", fg.clone()),
                (cfg.theme_name.as_str(), yel.clone()),
                ("\n  splits · tabs · ligatures ✓\n", dim.clone()),
                ("  sixel · kitty · OSC 8 ✓", dim.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        right.shape_until_scroll(&mut font_system, false);

        let areas = vec![
            TextArea {
                buffer: &tab_buf,
                left: 8.0,
                top: 6.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: w as i32,
                    bottom: tab_h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            },
            TextArea {
                buffer: &left,
                left: pad,
                top: ly + pad,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: ly as i32,
                    right: split_x as i32,
                    bottom: h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            },
            TextArea {
                buffer: &right,
                left: split_x + pad,
                top: ly + pad,
                scale: 1.0,
                bounds: TextBounds {
                    left: split_x as i32,
                    top: ly as i32,
                    right: w as i32,
                    bottom: h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            },
        ];

        let mut vp = viewport;
        vp.update(
            &queue,
            Resolution {
                width: w,
                height: h,
            },
        );
        quads.upload(&device, &queue, [wf, hf], &q);
        text_renderer.prepare(
            &device,
            &queue,
            &mut font_system,
            &mut atlas,
            &vp,
            areas,
            &mut swash,
        )?;

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-screenshot-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        let bpp = 4u32;
        let unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-screenshot-readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let bg = theme.background;
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-screenshot-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: srgb(bg.r),
                            g: srgb(bg.g),
                            b: srgb(bg.b),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            quads.draw(&mut pass);
            text_renderer.render(&atlas, &vp, &mut pass)?;
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .map_err(|_| anyhow!("map channel closed"))?
            .map_err(|e| anyhow!("buffer map failed: {e:?}"))?;

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();

        let img = image::RgbaImage::from_raw(w, h, pixels)
            .ok_or_else(|| anyhow!("image buffer size mismatch"))?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        img.save(out)
            .map_err(|e| anyhow!("write {}: {e}", out.display()))?;
        Ok(())
    })
}

/// Headless GPU validation. Builds the real wgpu pipelines (compiling the
/// WGSL on whatever backend the platform uses — Vulkan/Metal/DX12/GL) and
/// runs one offscreen render pass with no window. CI runs this on Linux,
/// macOS and Windows so the GPU stack is verified on every platform.
///
/// Returns `Ok(false)` when the host has no usable adapter at all (so CI on a
/// GPU-less box is informative, not flaky); `Ok(true)` on success.
pub fn offscreen_selftest() -> anyhow::Result<bool> {
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
        {
            Ok(a) => a,
            Err(_) => return Ok(false),
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-selftest"),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("device: {e:?}"))?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        // Pipeline construction compiles our WGSL on the active backend —
        // this is the part that historically breaks per-platform.
        let mut quads = QuadPipeline::new(&device, format);
        let mut imgs = imgpipe::ImagePipeline::new(&device, format);
        quads.upload(
            &device,
            &queue,
            [8.0, 8.0],
            &[QuadInstance {
                pos: [0.0, 0.0],
                size: [4.0, 4.0],
                color: [1.0, 0.0, 0.0, 1.0],
            }],
        );
        imgs.upload(&device, &queue, [8.0, 8.0], &[]);

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-selftest-target"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-selftest-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            quads.draw(&mut pass);
            imgs.draw(&mut pass);
        }
        queue.submit(std::iter::once(enc.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        Ok(true)
    })
}

#[cfg(test)]
mod gpu_tests {
    #[test]
    fn gpu_pipelines_compile_and_render_offscreen() {
        match super::offscreen_selftest() {
            Ok(true) => {}
            Ok(false) => eprintln!("no GPU adapter on this host; skipped"),
            Err(e) => panic!("offscreen GPU self-test failed: {e}"),
        }
    }
}

#[cfg(test)]
mod clamp_font_size_tests {
    use super::clamp_font_size;

    #[test]
    fn clamp_font_size_bounds_match_set_font_size() {
        // Floor + ceiling pinned: 5.0 and 72.0. Below cycle 73 only
        // set_font_size enforced these; Renderer::new took
        // cfg.font_size raw, so a `font-size = 200` config booted with
        // 200pt cells (texture-limit risk) until a Ctrl+0 reload
        // happened to flow it through set_font_size.
        assert_eq!(clamp_font_size(13.0), 13.0, "in-range passes through");
        assert_eq!(clamp_font_size(72.0), 72.0, "at-ceiling stays");
        assert_eq!(clamp_font_size(5.0), 5.0, "at-floor stays");
        assert_eq!(clamp_font_size(200.0), 72.0, "above ceiling clamps");
        assert_eq!(clamp_font_size(3.0), 5.0, "below floor clamps");
        // Negative is a parse-corrupted value; clamp to floor not panic.
        assert_eq!(clamp_font_size(-1.0), 5.0);
        // NaN routes to floor (f32::clamp panics on NaN; sanitize first).
        assert_eq!(clamp_font_size(f32::NAN), 5.0);
        // Infinities round to the bounds.
        assert_eq!(clamp_font_size(f32::INFINITY), 72.0);
        assert_eq!(clamp_font_size(f32::NEG_INFINITY), 5.0);
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate;

    #[test]
    fn truncate_respects_display_columns_not_chars() {
        // ASCII: 1 col per char. Trivially fits.
        assert_eq!(truncate("hello", 10), "hello");
        // ASCII overflow: keep n-1 chars, append `…` (1 col).
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        // CJK: each char is 2 cells. "中文中文" = 8 cells. Limit 8 = fits.
        assert_eq!(truncate("中文中文", 8), "中文中文");
        // CJK overflow: 8 cells doesn't fit in 6, reserve 1 col for `…`
        // → can fit at most 2 chars (4 cells) + `…` (1 col) = 5 cols ≤ 6.
        // Greedy: 2 chars + ellipsis.
        assert_eq!(truncate("中文中文", 6), "中文…");
        // Mixed ASCII + CJK: "abc中文" = 3+4 = 7 cells. Limit 5 →
        // overflow; reserve 1 col, take 4 cols worth = "abc" + ellipsis.
        // (next char is `中` (2 cols), 3+2=5 > 4-limit-after-ellipsis-reserve,
        // so stops at 3 ASCII chars.)
        assert_eq!(truncate("abc中文", 5), "abc…");
        // Limit 0 / 1: edge cases. limit=0 always returns just `…` if
        // anything was cut, but a truly-empty string fits in 0.
        assert_eq!(truncate("", 0), "");
        assert_eq!(truncate("a", 0), "…");
        // Total-equals-limit: no ellipsis (everything fits exactly).
        assert_eq!(truncate("中", 2), "中");
    }
}
