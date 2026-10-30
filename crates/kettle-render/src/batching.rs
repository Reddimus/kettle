use super::theme::pane_inactive_amount;
use super::{
    Config, ContextMenu, FontSystem, GColor, GlyphInstance, GlyphPipeline, OutlineInstance,
    Overlay, PaneView, PatchRect, QuadInstance, RasterGlyph, Rect4, RenderUploads, Renderer,
    Result, Rgb, SwashCache, TextArea, TextBounds, TextBuffer, TextRendererMode, UploadCounts,
    cell_locked_pen_x, glyph_grid_col, imgpipe, live_underlay_clear_color, quad,
    use_live_pane_bases,
};

/// Text/layout damage key for the context-menu renderer.
///
/// The highlighted row is deliberately excluded: moving the pointer changes
/// only a cheap quad. Text must be prepared again when its content, color,
/// position, or visible scroll window changes.
pub(crate) fn context_menu_text_damage_key(
    menu: Option<&ContextMenu>,
    foreground: Rgb,
    background: Rgb,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::hash::DefaultHasher::new();
    menu.is_some().hash(&mut hash);
    if let Some(menu) = menu {
        // glyphon bakes TextArea::default_color into retained vertices. Include
        // both colors: enabled rows use foreground directly, while disabled
        // rows blend it toward the panel background.
        (foreground.r, foreground.g, foreground.b).hash(&mut hash);
        (background.r, background.g, background.b).hash(&mut hash);
        menu.anchor.0.to_bits().hash(&mut hash);
        menu.anchor.1.to_bits().hash(&mut hash);
        menu.scroll_offset.hash(&mut hash);
        menu.panel_w_clamped.to_bits().hash(&mut hash);
        menu.panel_h_clamped.to_bits().hash(&mut hash);
        menu.rows.len().hash(&mut hash);
        for row in &menu.rows {
            row.label.hash(&mut hash);
            row.separator.hash(&mut hash);
            row.enabled.hash(&mut hash);
            row.hint.hash(&mut hash);
        }
    }
    hash.finish()
}

pub(crate) fn prepared_text_areas_damage_key(areas: &[TextArea<'_>]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::hash::DefaultHasher::new();
    areas.len().hash(&mut hash);
    for area in areas {
        (area.buffer as *const TextBuffer as usize).hash(&mut hash);
        area.left.to_bits().hash(&mut hash);
        area.top.to_bits().hash(&mut hash);
        area.scale.to_bits().hash(&mut hash);
        area.bounds.left.hash(&mut hash);
        area.bounds.top.hash(&mut hash);
        area.bounds.right.hash(&mut hash);
        area.bounds.bottom.hash(&mut hash);
        area.default_color.hash(&mut hash);
        area.custom_glyphs.len().hash(&mut hash);
    }
    hash.finish()
}

/// Retained-text damage key for the completion card. Glyphon keeps each buffer
/// at the same address when a same-size candidate list changes, so text-area
/// identity alone cannot detect new labels, a new header count, a moved
/// selection, or a re-tinted emphasis span.
pub(crate) fn completion_text_damage_key(
    header: &str,
    count: &str,
    labels: &[String],
    descriptions: &[String],
    spans: &[Option<(usize, usize)>],
    selected: &[bool],
    emphasis_colors: &[Rgb],
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::hash::DefaultHasher::new();
    header.hash(&mut hash);
    count.hash(&mut hash);
    labels.hash(&mut hash);
    descriptions.hash(&mut hash);
    spans.hash(&mut hash);
    selected.hash(&mut hash);
    for color in emphasis_colors {
        (color.r, color.g, color.b).hash(&mut hash);
    }
    hash.finish()
}

/// Text overlays whose content can change without a structural damage key.
/// Completion rows are deliberately absent: their geometry and source-string
/// keys catch each meaningful edge without reshaping on a cursor blink.
pub(crate) fn text_overlay_requires_continuous_prepare(overlay: &Overlay) -> bool {
    overlay.search.is_some()
        || !overlay.hint_labels.is_empty()
        || overlay.ime_preedit.is_some()
        || overlay.ssh_query.is_some()
        || overlay.palette_query.is_some()
        || overlay.layout_picker_query.is_some()
        || overlay.theme_picker_query.is_some()
        || overlay.edit_title.is_some()
        || overlay.settings.is_some()
        || overlay.resize_overlay.is_some()
}

pub(crate) fn push_ime_preedit_quads(
    quads: &mut Vec<QuadInstance>,
    bounds: Rect4,
    pane: Rect4,
    background: Rgb,
    underline: Rgb,
) -> TextBounds {
    let (x, y, width, height) = bounds;
    for (top, height, color) in [(y, height, background), (y + height - 2.0, 2.0, underline)] {
        let left = x.max(pane.0);
        let top_clipped = top.max(pane.1);
        let right = (x + width).min(pane.0 + pane.2);
        let bottom = (top + height).min(pane.1 + pane.3);
        if right > left && bottom > top_clipped {
            quads.push(rect(
                left,
                top_clipped,
                right - left,
                bottom - top_clipped,
                color,
                1.0,
            ));
        }
    }
    // Keep glyph overhang inside fully opaque pixels, including fractional
    // pane origins and the last column of a clipped composition.
    TextBounds {
        left: x.max(pane.0).ceil() as i32,
        top: y.max(pane.1).ceil() as i32,
        right: (x + width).min(pane.0 + pane.2).floor() as i32,
        bottom: (y + height).min(pane.1 + pane.3).floor() as i32,
    }
}

/// The quad and image pipelines a renderer compiles once and hands to every
/// layer that draws with them. Its five quad layers use two blend modes and
/// its three image layers one, so a window compiles three render pipelines
/// here instead of eight. The offscreen self-test and `--screenshot`, which
/// draw one frame, keep the standalone constructors.
pub(crate) struct SharedPipelines {
    pub(crate) quads: quad::QuadShared,
    pub(crate) images: imgpipe::ImageShared,
}

impl SharedPipelines {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            quads: quad::QuadShared::new(device, format),
            images: imgpipe::ImageShared::new(device, format),
        }
    }
}

impl Renderer {
    /// Everything this renderer has sent to the GPU so far (see
    /// `RenderUploads`), summed over its pipelines.
    pub fn render_uploads(&self) -> RenderUploads {
        let counts: UploadCounts = [
            self.glyph_pipeline.upload_counts(),
            self.pane_bases.upload_counts(),
            self.live_pane_bases.upload_counts(),
            self.quads.upload_counts(),
            self.pane_dimming.upload_counts(),
            self.text_decorations.upload_counts(),
            self.ime_quads.upload_counts(),
            self.pane_outlines.upload_counts(),
            self.overlay_quads.upload_counts(),
            self.menu_quads.upload_counts(),
            self.rounded_surfaces.upload_counts(),
            self.imgs.upload_counts(),
            self.card_base.upload_counts(),
            self.card_cursors.upload_counts(),
            self.card_decoration.upload_counts(),
            self.card_posters
                .as_ref()
                .map(imgpipe::ImagePipeline::upload_counts)
                .unwrap_or_default(),
            self.media_receipt_img.upload_counts(),
            self.lane_quads.upload_counts(),
            self.lane_img
                .as_ref()
                .map(imgpipe::ImagePipeline::upload_counts)
                .unwrap_or_default(),
            self.bg_imgs.upload_counts(),
            self.starfield.upload_counts(),
        ]
        .into_iter()
        .sum();
        RenderUploads {
            frames_presented: self.frames_presented,
            buffer_writes: counts.buffer_writes,
            buffer_bytes: counts.buffer_bytes,
            texture_writes: counts.texture_writes,
            text_prepares: self.text_prepares,
            chrome_prepares: self.chrome_prepares,
            skipped_writes: counts.skipped_writes,
            mapped_uploads: self.gpu.mapped_uploads(),
            mapped_writes: counts.mapped_writes,
            mapped_bytes: counts.mapped_bytes,
        }
    }

    /// Encode the whole scene into `target`. With a `window`, `target` holds
    /// only that rect of the `target_size` scene (a cursor patch pass): the
    /// viewport maps the scene 1:1 with the window's top-left at the origin,
    /// so every pipeline keeps its full-size screen uniform.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_scene_pass(
        &self,
        target: &wgpu::TextureView,
        target_size: [u32; 2],
        cfg: &Config,
        live_window: bool,
        cursor_on: bool,
        window: Option<PatchRect>,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        let clear = live_underlay_clear_color(
            cfg.theme.background,
            self.live_background_opacity_floor,
            self.config.alpha_mode,
            live_window,
        );
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(window) = window {
            pass.set_viewport(
                -(window.x as f32),
                -(window.y as f32),
                target_size[0] as f32,
                target_size[1] as f32,
                0.0,
                1.0,
            );
        }
        // Layering, back to front: wallpaper, then cell + chrome + border
        // quads opaquely on top, then inline kitty/sixel images over the cell
        // backgrounds, then text. Drawing the wallpaper after `quads` would
        // hide every cell background and bleed the animation through the
        // chrome.
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            // The procedural starfield is the opaque back-most wallpaper
            // (mutually exclusive with an image background).
            self.starfield.draw(&mut pass);
        } else {
            self.bg_imgs.draw(&mut pass);
        }
        if use_live_pane_bases(live_window, self.live_background_opacity_floor) {
            self.live_pane_bases.draw(&mut pass);
        } else {
            self.pane_bases.draw(&mut pass);
        }
        // The blink's off phase skips the cursor's quads; the same instances
        // stay uploaded, and the blend order of everything else is unchanged.
        let hidden_cursor = if cursor_on && !self.ime_prepared {
            None
        } else {
            self.cursor_quad_range.clone()
        };
        self.quads.draw_hiding(&mut pass, hidden_cursor);
        self.pane_outlines.draw(&mut pass);
        self.imgs.draw(&mut pass);
        self.card_base.draw(&mut pass);
        if let Some(posters) = &self.card_posters {
            posters.draw(&mut pass);
        }
        self.card_decoration.draw(&mut pass);
        self.card_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        self.pane_dimming.draw(&mut pass);
        self.text_decorations.draw(&mut pass);
        // Pane text sits above backgrounds + images and below chrome and the
        // cursor glyph. Grid pins cells; inactive Legacy retains continuous
        // positions. Other Legacy pane text rides glyphon below.
        match window {
            None => {
                self.glyph_pipeline
                    .draw(&mut pass, &self.glyph_clips, target_size);
            }
            Some(window) => self.glyph_pipeline.draw_in_window(
                &mut pass,
                &self.glyph_clips,
                target_size,
                [window.x, window.y, window.width, window.height],
            ),
        }
        self.text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        self.card_cursors.draw_hiding(
            &mut pass,
            if cursor_on && !self.ime_prepared {
                None
            } else {
                self.card_cursor_quad_range.clone()
            },
        );
        // The preedit is its own layer above the terminal and card text,
        // and the composition caret draws again above it.
        self.ime_quads.draw(&mut pass);
        self.ime_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        if cursor_on && self.ime_prepared {
            if let Some(range) = self.cursor_quad_range.clone() {
                self.quads.draw_range(&mut pass, range);
            }
            if let Some(range) = self.card_cursor_quad_range.clone() {
                self.card_cursors.draw_range(&mut pass, range);
            }
        }
        // The focused solid-block cursor's inverted glyph sits on its block
        // quad and the normal glyph, but below every overlay and chrome layer:
        // an opaque menu or receipt must cover all of the terminal cursor. The
        // off phase shows the glyph beneath in its normal colour instead.
        if cursor_on && self.pending_cursor_glyph.is_some() && !self.ime_prepared {
            self.cursor_glyph_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
        }
        // Preview lanes sit above the terminal and below its dimming,
        // scrollbar and every overlay: menus and dialogs cover them whole.
        self.lane_quads.draw(&mut pass);
        if let Some(lanes) = &self.lane_img {
            lanes.draw(&mut pass);
        }
        self.rounded_surfaces
            .draw_range(&mut pass, 0..self.transport_surface_count);
        self.lane_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        // Lane dimming and scrollbars sit above the complete lane.
        self.overlay_quads.draw(&mut pass);
        // Menu chrome sits above terminal content. The receipt thumbnail is
        // then composited below the shared chrome-text pass, so its own labels
        // and the context-menu labels both remain readable above images.
        self.menu_quads.draw(&mut pass);
        self.media_receipt_img.draw(&mut pass);
        self.menu_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        self.rounded_surfaces
            .draw_range(&mut pass, self.transport_surface_count..u32::MAX);
        self.confirmation_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        Ok(())
    }
}

/// Shrink each of `quads` to its part inside `clip`, to nothing when it lies
/// wholly outside. In place, so ranges into the list stay valid.
pub(crate) fn clip_quads(quads: &mut [QuadInstance], clip: (f32, f32, f32, f32)) {
    let (cx, cy, cw, ch) = clip;
    for quad in quads {
        let x0 = quad.pos[0].max(cx);
        let y0 = quad.pos[1].max(cy);
        let x1 = (quad.pos[0] + quad.size[0]).min(cx + cw);
        let y1 = (quad.pos[1] + quad.size[1]).min(cy + ch);
        quad.pos = [x0, y0];
        quad.size = [(x1 - x0).max(0.0), (y1 - y0).max(0.0)];
    }
}

pub(crate) fn rect(x: f32, y: f32, w: f32, h: f32, c: Rgb, a: f32) -> QuadInstance {
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

/// Clip chrome text to the interactive rect that positioned it. Vertical tab
/// rows do not share the strip's first-row y range, so reusing a bar-wide clip
/// can make a correctly positioned glyph disappear outside its bounds.
pub(crate) fn text_bounds_for_rect((x, y, width, height): Rect4) -> TextBounds {
    TextBounds {
        left: x as i32,
        top: y as i32,
        right: (x + width) as i32,
        bottom: (y + height) as i32,
    }
}

pub(crate) fn pane_outline(
    rect: (f32, f32, f32, f32),
    color: Rgb,
    border_width: f32,
    corner_radius: f32,
    corner_mask: u32,
) -> OutlineInstance {
    OutlineInstance {
        pos: [rect.0, rect.1],
        size: [rect.2.max(0.0), rect.3.max(0.0)],
        color: [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            1.0,
        ],
        border_width: border_width.max(0.0),
        corner_radius: corner_radius.max(0.0),
        corner_mask,
        _pad: 0,
    }
}

pub(crate) fn apply_quad_alpha_floor(quads: &mut [QuadInstance], floor: f32) {
    let floor = floor.clamp(0.0, 1.0);
    for quad in quads {
        quad.color[3] = quad.color[3].max(floor);
    }
}

pub(crate) fn cell_locked_glyph_offset(
    cell_left: f32,
    glyph: &glyphon::cosmic_text::LayoutGlyph,
    cluster_x: f32,
) -> f32 {
    let x_off_px = glyph.font_size * glyph.x_offset;
    cell_locked_pen_x(cell_left + (glyph.x - cluster_x), x_off_px) - glyph.x - x_off_px
}

/// Emit cell-locked glyph instances for ONE shaped `TextBuffer`, appending to
/// `out`. This is the single source of truth for the Grid (cell-locked)
/// renderer's per-glyph emit: `emit_pane_glyphs` (live panes), the offscreen
/// `capture_png_with_annotation` screenshot path, and the `grid_prompt_blink`
/// test fixture all call it so they can never drift apart.
///
/// A `None` cell width retains glyphon's continuous layout for inactive Legacy
/// panes, whose color bitmaps need this pipeline's dimming shader.
/// `origin` is the buffer's top-left in physical pixels (the same coordinate a
/// glyphon `TextArea`'s `left`/`top` would use). With a cell width, each glyph is
/// pinned to its grid cell and snapped to an integer pixel, preserving any
/// intra-cluster `x_offset` (combining marks). `default_color` is
/// the fallback for a glyph with no explicit color span (mirrors a `TextArea`'s
/// `default_color`). `char_starts` is a scratch buffer reused across runs to
/// avoid per-line allocation. Glyphs are appended in buffer order, so the
/// caller can wrap the appended range in one `GlyphClip` for scissor clipping.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_cell_locked_glyphs(
    out: &mut Vec<GlyphInstance>,
    buf: &TextBuffer,
    origin: (f32, f32),
    cell_w: Option<f32>,
    default_color: GColor,
    glyph_pipeline: &mut GlyphPipeline,
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    char_starts: &mut Vec<u32>,
) {
    for run in buf.layout_runs() {
        // Use the engine's width classifier: zero-width scalars attach to
        // the preceding cell; wide bases still have an explicit spacer cell.
        char_starts.clear();
        for (b, ch) in run.text.char_indices() {
            if unicode_width::UnicodeWidthChar::width(ch) != Some(0) {
                char_starts.push(b as u32);
            }
        }
        let line_y = run.line_y;
        let mut active_cell = None;
        let mut cluster_x = 0.0;
        for glyph in run.glyphs.iter() {
            let col = glyph_grid_col(char_starts, glyph.start);
            if active_cell != Some(col) {
                active_cell = Some(col);
                cluster_x = glyph.x;
            }
            // Pin the pen to the cell, snapped to an integer physical pixel
            // (crisp + cache-friendly: x_bin = 0), while keeping any intra-
            // cluster x_offset. Feeding cosmic-text's `physical()` an offset that
            // lands the logical X exactly there reuses its exact cache-key +
            // vertical + subpixel math, so the rasterized bitmap is byte-
            // identical to glyphon's.
            let off_x = cell_w.map_or(origin.0, |cw| {
                let cell_left = origin.0 + col as f32 * cw;
                cell_locked_glyph_offset(cell_left, glyph, cluster_x)
            });
            let phys = glyph.physical((off_x, origin.1), 1.0);
            let key = phys.cache_key;
            let mut image = None;
            let slot = match glyph_pipeline.ensure_glyph(device, queue, key, || {
                image = swash.get_image_uncached(font_system, key);
                RasterGlyph::from_swash(image.as_ref()?)
            }) {
                Some(s) => s,
                None => continue, // empty / whitespace glyph — nothing to draw
            };
            let color = glyph.color_opt.unwrap_or(default_color);
            let qx = phys.x + slot.left;
            let qy = line_y.round() as i32 + phys.y - slot.top;
            out.push(GlyphInstance {
                pos: [qx as f32, qy as f32],
                size: [slot.w, slot.h],
                uv: [slot.atlas_x, slot.atlas_y],
                color: [
                    color.r() as f32 / 255.0,
                    color.g() as f32 / 255.0,
                    color.b() as f32 / 255.0,
                    color.a() as f32 / 255.0,
                ],
                kind: slot.kind,
                _pad: [0; 3],
            });
        }
    }
}

pub(crate) fn text_layout_damage_key(
    panes: &[PaneView<'_>],
    cfg: &Config,
    surface: (f32, f32),
    cell: (f32, f32),
    pane_titlebar_h: f32,
) -> u64 {
    use std::hash::{Hash, Hasher};

    fn hf<H: Hasher>(h: &mut H, v: f32) {
        // Normalize -0.0 so arithmetic-equivalent layout inputs hash together.
        let v = if v == 0.0 { 0.0 } else { v };
        v.to_bits().hash(h);
    }

    let mut h = std::hash::DefaultHasher::new();
    match cfg.text_renderer {
        TextRendererMode::Grid => 0u8.hash(&mut h),
        TextRendererMode::Legacy => 1u8.hash(&mut h),
    }
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        cfg.family_for(bold, italic).hash(&mut h);
    }
    cfg.font_ligatures.hash(&mut h);
    for f in &cfg.font_features {
        f.tag.hash(&mut h);
        f.value.hash(&mut h);
    }
    for v in [
        surface.0,
        surface.1,
        cell.0,
        cell.1,
        cfg.font_size,
        cfg.cell_height,
        cfg.cell_width,
        cfg.padding_x,
        cfg.padding_y,
        pane_titlebar_h,
    ] {
        hf(&mut h, v);
    }
    cfg.show_titlebar.hash(&mut h);
    cfg.title_at_bottom.hash(&mut h);
    for pv in panes {
        pv.id.hash(&mut h);
        hf(&mut h, pane_inactive_amount(pv, panes.len(), cfg));
        // The terminal can move inside an unmoved pane (a preview lane
        // opening or closing), and its glyphs move with it.
        for v in [
            pv.rect.0,
            pv.rect.1,
            pv.rect.2,
            pv.rect.3,
            pv.terminal.0,
            pv.terminal.1,
            pv.terminal.2,
            pv.terminal.3,
        ] {
            hf(&mut h, v);
        }
        pv.snap.columns.hash(&mut h);
        pv.snap.screen_lines.hash(&mut h);
        pv.snap.display_offset.hash(&mut h);
    }
    h.finish()
}
