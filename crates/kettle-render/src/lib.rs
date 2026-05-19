//! GPU renderer: wgpu surface + glyphon (cosmic-text) glyph atlas for text,
//! plus an instanced quad pipeline for cell backgrounds, cursor, selection and
//! search highlights.

mod color;
mod quad;

use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use anyhow::{Result, anyhow};
use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution,
    Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use kettle_config::{Config, CursorStyle, Rgb};
use kettle_core::EventProxy;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub use color::resolve;
use quad::{QuadInstance, QuadPipeline};

/// A search match expressed in viewport rows (already scrolled).
#[derive(Clone, Copy)]
pub struct HighlightRect {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub active: bool,
}

/// Optional UI overlays drawn on top of the grid.
#[derive(Default)]
pub struct Overlay {
    /// `Some(query)` when the search bar is open.
    pub search_query: Option<String>,
    pub search_count: usize,
    pub search_index: usize,
    pub highlights: Vec<HighlightRect>,
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
    text_buffer: TextBuffer,
    overlay_buffer: TextBuffer,

    quads: QuadPipeline,

    font_family: String,
    font_size: f32,
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

        // Font system seeded with the bundled Nerd Font faces (so AstroNvim
        // icons render with zero setup) plus the user's system fonts.
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

        let font_size = cfg.font_size;
        let metrics = Metrics::new(font_size, font_size * 1.25);
        let mut text_buffer = TextBuffer::new(&mut font_system, metrics);
        let overlay_buffer = TextBuffer::new(&mut font_system, metrics);

        let (cell_w, cell_h) = measure_cell(
            &mut font_system,
            &mut text_buffer,
            &cfg.font_family,
            metrics,
        );

        let quads = QuadPipeline::new(&device, format);

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
            text_buffer,
            overlay_buffer,
            quads,
            font_family: cfg.font_family.clone(),
            font_size,
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

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = size.clamp(5.0, 72.0);
        let metrics = Metrics::new(self.font_size, self.font_size * 1.25);
        let family = self.font_family.clone();
        let (cw, ch) = measure_cell(
            &mut self.font_system,
            &mut self.text_buffer,
            &family,
            metrics,
        );
        self.cell_w = cw;
        self.cell_h = ch;
    }

    /// Grid size for the current surface, accounting for padding.
    pub fn grid_size(&self, pad_x: f32, pad_y: f32) -> (usize, usize) {
        let w = (self.config.width as f32 - pad_x * 2.0).max(0.0);
        let h = (self.config.height as f32 - pad_y * 2.0).max(0.0);
        let cols = (w / self.cell_w).floor() as usize;
        let rows = (h / self.cell_h).floor() as usize;
        (cols.max(1), rows.max(1))
    }

    pub fn render(
        &mut self,
        term: &Term<EventProxy>,
        cfg: &Config,
        overlay: &Overlay,
    ) -> Result<()> {
        let theme = &cfg.theme;
        let content = term.renderable_content();
        let term_colors = content.colors;
        let cols = term.grid().columns();
        let pad_x = cfg.padding_x;
        let pad_y = cfg.padding_y;
        let cw = self.cell_w;
        let ch = self.cell_h;
        let default_bg = theme.background;

        // 1. Build text spans + background quads from the visible grid.
        let mut quads: Vec<QuadInstance> = Vec::new();
        let mut line_text: Vec<String> = Vec::new();
        // Per span: (text, fg, bold, italic)
        let mut spans: Vec<(String, Rgb, bool, bool)> = Vec::new();
        let mut span_line_breaks: Vec<usize> = Vec::new(); // span index where a newline precedes

        let mut cur_row = 0i32;
        let mut cur: Option<(String, Rgb, bool, bool)> = None;
        let mut last_col = 0usize;

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
                last_col = 0;
            }
            let _ = last_col;
            last_col = col;

            let flags = cell.flags;
            let mut fg = color::resolve(cell.fg, theme, term_colors);
            let mut bg = color::resolve(cell.bg, theme, term_colors);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let bold = flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            let hidden = flags.contains(Flags::HIDDEN);

            // Background quad (skip the default terminal background).
            if bg != default_bg {
                quads.push(rect(
                    pad_x + col as f32 * cw,
                    pad_y + row as f32 * ch,
                    cw,
                    ch,
                    bg,
                    1.0,
                ));
            }

            let ch_draw = if hidden { ' ' } else { cell.c };
            match &mut cur {
                Some((text, cfg_, cb, ci)) if *cfg_ == fg && *cb == bold && *ci == italic => {
                    text.push(ch_draw);
                }
                _ => {
                    if let Some(s) = cur.take() {
                        spans.push(s);
                    }
                    let mut s = String::new();
                    s.push(ch_draw);
                    cur = Some((s, fg, bold, italic));
                }
            }
        }
        if let Some(s) = cur.take() {
            spans.push(s);
        }
        let _ = &mut line_text;

        // Selection highlight.
        if let Some(sel) = content.selection {
            let s = sel.start;
            let e = sel.end;
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
                    pad_x + c0 as f32 * cw,
                    pad_y + r as f32 * ch,
                    w as f32 * cw,
                    ch,
                    theme.selection_background,
                    1.0,
                ));
            }
        }

        // Search highlights.
        for h in &overlay.highlights {
            quads.push(rect(
                pad_x + h.col as f32 * cw,
                pad_y + h.row as f32 * ch,
                h.width as f32 * cw,
                ch,
                if h.active {
                    cfg.search_background
                } else {
                    theme.selection_background
                },
                1.0,
            ));
        }

        // Cursor.
        let cur_pt = content.cursor.point;
        if cur_pt.line.0 >= 0 {
            let (cwidth, alpha) = match cfg.cursor_style {
                CursorStyle::Bar => (cw * 0.15, 1.0),
                CursorStyle::Underline => (cw, 1.0),
                CursorStyle::Block => (cw, 0.55),
            };
            let y = if matches!(cfg.cursor_style, CursorStyle::Underline) {
                pad_y + cur_pt.line.0 as f32 * ch + ch - 2.0
            } else {
                pad_y + cur_pt.line.0 as f32 * ch
            };
            let cheight = if matches!(cfg.cursor_style, CursorStyle::Underline) {
                2.0
            } else {
                ch
            };
            quads.push(rect(
                pad_x + cur_pt.column.0 as f32 * cw,
                y,
                cwidth,
                cheight,
                theme.cursor,
                alpha,
            ));
        }

        // 2. Lay out the text buffer.
        let metrics = Metrics::new(self.font_size, self.font_size * 1.25);
        self.text_buffer.set_metrics(&mut self.font_system, metrics);
        self.text_buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.config.height as f32),
        );
        let family = self.font_family.clone();
        let default_attrs = Attrs::new().family(Family::Name(&family));

        // Reconstruct the rich text: spans separated by line breaks.
        let mut rich: Vec<(String, Attrs)> = Vec::new();
        let mut next_break = 0usize;
        for (i, (text, fg, bold, italic)) in spans.iter().enumerate() {
            while next_break < span_line_breaks.len() && span_line_breaks[next_break] == i {
                rich.push(("\n".to_string(), default_attrs.clone()));
                next_break += 1;
            }
            let mut a = Attrs::new().family(Family::Name(&family));
            a = a.color(GColor::rgb(fg.r, fg.g, fg.b));
            if *bold {
                a = a.weight(Weight::BOLD);
            }
            if *italic {
                a = a.style(Style::Italic);
            }
            rich.push((text.clone(), a));
        }
        self.text_buffer.set_rich_text(
            &mut self.font_system,
            rich.iter().map(|(s, a)| (s.as_str(), a.clone())),
            &default_attrs,
            Shaping::Advanced,
            None,
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        // Overlay (search bar) text.
        let mut overlay_areas: Vec<TextArea> = Vec::new();
        if let Some(q) = &overlay.search_query {
            let bar_h = self.cell_h + 10.0;
            quads.push(rect(
                0.0,
                self.config.height as f32 - bar_h,
                self.config.width as f32,
                bar_h,
                theme.palette[8],
                0.95,
            ));
            let label = format!(
                "  search: {}    [{}/{}]   (Enter next · Shift+Enter prev · Esc close)",
                q,
                if overlay.search_count == 0 {
                    0
                } else {
                    overlay.search_index + 1
                },
                overlay.search_count
            );
            self.overlay_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.overlay_buffer.set_size(
                &mut self.font_system,
                Some(self.config.width as f32),
                Some(bar_h),
            );
            self.overlay_buffer.set_text(
                &mut self.font_system,
                &label,
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.overlay_buffer
                .shape_until_scroll(&mut self.font_system, false);
            overlay_areas.push(TextArea {
                buffer: &self.overlay_buffer,
                left: 0.0,
                top: self.config.height as f32 - bar_h + 5.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.config.width as i32,
                    bottom: self.config.height as i32,
                },
                default_color: GColor::rgb(
                    theme.foreground.r,
                    theme.foreground.g,
                    theme.foreground.b,
                ),
                custom_glyphs: &[],
            });
        }

        // 3. Prepare GPU resources.
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        let fg = theme.foreground;
        let mut areas = vec![TextArea {
            buffer: &self.text_buffer,
            left: pad_x,
            top: pad_y,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: self.config.width as i32,
                bottom: self.config.height as i32,
            },
            default_color: GColor::rgb(fg.r, fg.g, fg.b),
            custom_glyphs: &[],
        }];
        areas.extend(overlay_areas);

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )?;

        self.quads.upload(
            &self.device,
            &self.queue,
            [self.config.width as f32, self.config.height as f32],
            &quads,
        );

        // 4. Encode the frame.
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
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        self.atlas.trim();
        Ok(())
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, c: Rgb, a: f32) -> QuadInstance {
    QuadInstance {
        pos: [x, y],
        size: [w, h],
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

/// Measure a single monospace cell by shaping `M`.
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
