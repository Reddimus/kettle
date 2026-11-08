use super::mark_fallback::{LineShifts, MarkShift, shift_for, shifts_of};
use super::theme::pane_inactive_amount;
use super::{
    Config, ContextMenu, FontSystem, GColor, GlyphInstance, GlyphPipeline, OutlineInstance,
    Overlay, PaneView, PatchRect, QuadInstance, RasterGlyph, Rect4, RenderUploads, Renderer,
    Result, Rgb, SwashCache, TextArea, TextBounds, TextBuffer, TextRendererMode, UploadCounts,
    cell_locked_pen_x, imgpipe, quad,
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
        menu.picker_header_height.to_bits().hash(&mut hash);
        menu.status.is_some().hash(&mut hash);
        if let Some(status) = &menu.status {
            status.lines.hash(&mut hash);
            status.line_height.to_bits().hash(&mut hash);
        }
        menu.rows.len().hash(&mut hash);
        for row in &menu.rows {
            row.label.hash(&mut hash);
            row.separator.hash(&mut hash);
            row.enabled.hash(&mut hash);
            row.hint.hash(&mut hash);
            row.matched_chars.hash(&mut hash);
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
        || !overlay.pane_statuses.is_empty()
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
            self.quads.upload_counts(),
            self.pane_dimming.upload_counts(),
            self.text_decorations.upload_counts(),
            self.ime_quads.upload_counts(),
            self.pane_outlines.upload_counts(),
            self.overlay_quads.upload_counts(),
            self.menu_quads.upload_counts(),
            self.rounded_surfaces.upload_counts(),
            self.picker_surfaces.upload_counts(),
            self.toast_surfaces.upload_counts(),
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
    /// so every pipeline keeps its full-size screen uniform. The live window
    /// and an offscreen capture draw the same pass, configured alpha included.
    pub(crate) fn encode_scene_pass(
        &self,
        target: &wgpu::TextureView,
        target_size: [u32; 2],
        cfg: &Config,
        cursor_on: bool,
        window: Option<PatchRect>,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
        self.pane_bases.draw(&mut pass);
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
        let whole = [0, 0, target_size[0].max(1), target_size[1].max(1)];
        let glyph_window = window.map_or(whole, |w| [w.x, w.y, w.width, w.height]);
        self.glyph_pipeline.draw_in_window(
            &mut pass,
            &self.glyph_clips,
            target_size,
            glyph_window,
            cursor_on && !self.ime_prepared,
        );
        // Legacy pane text uses glyphon. Clip its normal ink as well as
        // Grid instances: neighboring raster glyphs can overhang this cell.
        let excluded = (cursor_on && !self.ime_prepared)
            .then(|| {
                self.glyph_pipeline
                    .cursor_exclusion(target_size, glyph_window)
            })
            .flatten();
        for [x, y, w, h] in
            super::glyphpipe::scissors_without([0, 0, glyph_window[2], glyph_window[3]], excluded)
                .into_iter()
                .flatten()
        {
            pass.set_scissor_rect(x, y, w, h);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
        }
        pass.set_scissor_rect(0, 0, glyph_window[2], glyph_window[3]);
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
        // A glyph Kettle draws itself (box drawing, blocks, Powerline,
        // braille) has no glyphon vertices: the glyph pipeline draws it, in
        // the same place in the order.
        let builtin_cursor = self
            .pending_cursor_glyph
            .as_ref()
            .is_some_and(|cursor| cursor.builtin.is_some());
        if cursor_on && self.pending_cursor_glyph.is_some() && !self.ime_prepared && !builtin_cursor
        {
            self.cursor_glyph_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
            if let Some(marks) = &self.cursor_marks {
                marks.render(&self.viewport, &mut pass)?;
            }
        }
        if cursor_on && builtin_cursor && !self.ime_prepared {
            // Normal pane ink was excluded, so partial coverage shows only
            // cursor colours without repainting over decorations or images.
            self.glyph_pipeline
                .draw_cursor(&mut pass, target_size, glyph_window);
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
        self.picker_surfaces.draw_range(&mut pass, 0..u32::MAX);
        self.rounded_surfaces.draw_range(
            &mut pass,
            self.transport_surface_count
                ..self.transport_surface_count + self.settings_surface_count,
        );
        self.menu_quads.draw(&mut pass);
        self.media_receipt_img.draw(&mut pass);
        self.toast_surfaces.draw_range(&mut pass, 0..u32::MAX);
        self.menu_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        self.rounded_surfaces.draw_range(
            &mut pass,
            self.transport_surface_count + self.settings_surface_count..u32::MAX,
        );
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

pub(crate) fn cell_locked_glyph_offset(
    cell_left: f32,
    glyph: &glyphon::cosmic_text::LayoutGlyph,
    cluster_x: f32,
) -> f32 {
    let x_off_px = glyph.font_size * glyph.x_offset;
    cell_locked_pen_x(cell_left + (glyph.x - cluster_x), x_off_px) - glyph.x - x_off_px
}

#[derive(Clone, Copy)]
pub(crate) enum GlyphPositioning {
    Continuous,
    Grid(f32),
    GraphemeRows(f32),
}

/// Emit cell-locked glyph instances for ONE shaped `TextBuffer`, appending to
/// `out`. This is the single source of truth for the Grid (cell-locked)
/// renderer's per-glyph emit: `emit_pane_glyphs` (live panes), the offscreen
/// `capture_png_with_annotation` screenshot path, and the `grid_prompt_blink`
/// test fixture all call it so they can never drift apart.
///
/// `Continuous` retains glyphon's layout for inactive Legacy panes, whose color
/// bitmaps need this pipeline's dimming shader. `GraphemeRows` locks only rows
/// carrying explicit cell metadata, preserving other Legacy rows' positions.
/// `origin` is the buffer's top-left in physical pixels (the same coordinate a
/// glyphon `TextArea`'s `left`/`top` would use). With a cell width, each glyph is
/// pinned to its grid cell and snapped to an integer pixel, preserving any
/// intra-cluster `x_offset` (combining marks). `default_color` is
/// the fallback for a glyph with no explicit color span (mirrors a `TextArea`'s
/// `default_color`). `char_starts` is a scratch buffer reused across runs to
/// avoid per-line allocation. Glyphs are appended in buffer order, so the
/// caller can wrap the appended range in one `GlyphClip` for scissor clipping.
/// `marks` holds each buffer line's fallback mark placements
/// (`mark_fallback::line_shifts`); only `Grid` applies them, since Legacy
/// text that glyphon draws cannot follow them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_cell_locked_glyphs(
    out: &mut Vec<GlyphInstance>,
    buf: &TextBuffer,
    marks: &[LineShifts],
    origin: (f32, f32),
    cell_width: f32,
    positioning: GlyphPositioning,
    default_color: GColor,
    glyph_pipeline: &mut GlyphPipeline,
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    char_starts: &mut Vec<u32>,
) {
    emit_positioned_glyphs(
        out,
        buf,
        marks,
        origin,
        cell_width,
        positioning,
        None,
        default_color,
        char_starts,
        |key| {
            let mut image = None;
            glyph_pipeline.ensure_glyph(device, queue, key, || {
                image = swash.get_image_uncached(font_system, key);
                RasterGlyph::from_swash(image.as_ref()?)
            })
        },
    );
}

/// The glyph instance for one cell Kettle draws itself, its mask rasterized
/// into the mask atlas on a miss. Masks are cached like font glyphs, keyed by
/// the glyph, the cell's pixel size and the stroke widths, so a steady frame
/// rasterizes nothing; `mask` is the one buffer every miss reuses.
pub(crate) fn builtin_glyph_instance(
    cell: &super::builtin_glyphs::BuiltinCell,
    strokes: super::builtin_glyphs::Strokes,
    glyph_pipeline: &mut GlyphPipeline,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mask: &mut Vec<u8>,
) -> Option<GlyphInstance> {
    let key = cell.key(strokes)?;
    let slot = glyph_pipeline.ensure_glyph(device, queue, key, || {
        if !super::builtin_glyphs::rasterize(key, mask) {
            return None;
        }
        Some(RasterGlyph {
            color: false,
            width: u32::from(key.width),
            height: u32::from(key.height),
            left: 0,
            top: 0,
            data: mask,
        })
    })?;
    Some(builtin_glyph_at(cell, slot))
}

/// The instance drawing `cell` from the atlas `slot` holding its mask.
pub(crate) fn builtin_glyph_at(
    cell: &super::builtin_glyphs::BuiltinCell,
    slot: super::glyphpipe::GlyphSlot,
) -> GlyphInstance {
    let [r, g, b] = cell.color;
    GlyphInstance {
        pos: [cell.rect[0] as f32, cell.rect[1] as f32],
        size: [slot.w, slot.h],
        uv: [slot.atlas_x, slot.atlas_y],
        uv_size: [slot.w, slot.h],
        color: [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ],
        kind: slot.kind,
        _pad: [0; 1],
    }
}

/// The emit behind [`emit_cell_locked_glyphs`]. With `only`, a buffer line and
/// grid column, it emits that cell's glyphs alone, placed exactly as in the
/// full emit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_positioned_glyphs(
    out: &mut Vec<GlyphInstance>,
    buf: &TextBuffer,
    marks: &[LineShifts],
    origin: (f32, f32),
    cell_width: f32,
    positioning: GlyphPositioning,
    only: Option<(usize, usize)>,
    default_color: GColor,
    char_starts: &mut Vec<u32>,
    mut glyph_slot: impl FnMut(glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot>,
) {
    for run in buf.layout_runs() {
        if only.is_some_and(|(line, _)| line != run.line_i) {
            continue;
        }
        let shifts: &[MarkShift] = match positioning {
            GlyphPositioning::Grid(_) => shifts_of(marks, run.line_i),
            GlyphPositioning::Continuous | GlyphPositioning::GraphemeRows(_) => &[],
        };
        let mut next_shift = 0;
        let mut index = 0;
        let cell_w = match positioning {
            GlyphPositioning::Continuous => None,
            GlyphPositioning::Grid(width) => Some(width),
            GlyphPositioning::GraphemeRows(width) => run
                .glyphs
                .iter()
                .any(|glyph| {
                    super::geometry::GridGlyphCell::from_metadata(glyph.metadata).is_some()
                })
                .then_some(width),
        };
        // Use the engine's width classifier: zero-width scalars attach to
        // the preceding cell; wide bases still have an explicit spacer cell.
        char_starts.clear();
        for (b, ch) in run.text.char_indices() {
            if unicode_width::UnicodeWidthChar::width(ch) != Some(0) {
                char_starts.push(b as u32);
            }
        }
        let line_y = run.line_y;
        let mut prefix_anchors = std::collections::HashMap::new();
        for (range, attrs) in buf.lines[run.line_i].attrs_list().spans_iter() {
            if super::geometry::GridGlyphCell::from_metadata(attrs.metadata)
                .is_none_or(|cell| !cell.clustered)
            {
                continue;
            }
            let Some(text) = run.text.get(range.clone()) else {
                continue;
            };
            let offset = alacritty_terminal::term::grapheme_base_offset(text);
            if offset == 0 {
                continue;
            }
            let byte = range.start + offset;
            if let Some(base) = run.glyphs.iter().find(|glyph| {
                glyph.metadata == attrs.metadata && glyph.start <= byte && byte < glyph.end
            }) {
                prefix_anchors.insert(attrs.metadata, base.x);
            }
        }
        let mut active_cell = None;
        let mut cluster_x = 0.0;
        use unicode_segmentation::UnicodeSegmentation;
        let emoji_clusters: Vec<_> = run
            .text
            .grapheme_indices(true)
            .filter(|(_, text)| super::fonts::emoji_text(text))
            .map(|(start, text)| start..start + text.len())
            .collect();
        let emoji_cluster = |glyph: &glyphon::cosmic_text::LayoutGlyph| {
            let index = emoji_clusters
                .partition_point(|range| range.start <= glyph.start)
                .checked_sub(1)?;
            (glyph.start < emoji_clusters[index].end).then_some(index)
        };
        // Only emoji images share fitting and cell clipping. Text ligatures
        // can extend left into preceding cells and must keep their full ink.
        for glyphs in run.glyphs.chunk_by(|a, b| {
            a.metadata == b.metadata
                && emoji_cluster(a).is_some_and(|index| emoji_cluster(b) == Some(index))
        }) {
            let first = &glyphs[0];
            let cluster = emoji_cluster(first);
            let start = out.len();
            for glyph in glyphs {
                let shift = shift_for(shifts, &mut next_shift, index, glyph.glyph_id);
                index += 1;
                let moved;
                let (glyph, raise) = match shift {
                    Some(shift) => {
                        moved = shift.moved(glyph);
                        (
                            &moved,
                            shift.raise_px(
                                glyph.font_size,
                                run.line_top,
                                run.line_height,
                                run.line_y,
                            ),
                        )
                    }
                    None => (glyph, 0),
                };
                let col = super::geometry::glyph_grid_col_with_metadata(
                    char_starts,
                    glyph.start,
                    glyph.metadata,
                );
                if active_cell != Some(col) {
                    active_cell = Some(col);
                    cluster_x = prefix_anchors
                        .get(&glyph.metadata)
                        .copied()
                        .unwrap_or(glyph.x);
                }
                if only.is_some_and(|(_, column)| column != col) {
                    continue;
                }
                // Keep the shaped offsets until every image in the cluster
                // has been rasterized, so fitting uses their combined ink.
                let off_x = cell_w.map_or(origin.0, |cw| {
                    let cell_left = origin.0 + col as f32 * cw;
                    cell_locked_glyph_offset(cell_left, glyph, cluster_x)
                });
                let phys = glyph.physical((off_x, origin.1), 1.0);
                let Some(slot) = glyph_slot(phys.cache_key) else {
                    continue;
                };
                let color = glyph.color_opt.unwrap_or(default_color);
                out.push(GlyphInstance {
                    pos: [
                        (phys.x + slot.left) as f32,
                        (line_y.round() as i32 + phys.y - slot.top - raise) as f32,
                    ],
                    size: [slot.w, slot.h],
                    uv: [slot.atlas_x, slot.atlas_y],
                    uv_size: [slot.w, slot.h],
                    color: [
                        color.r() as f32 / 255.0,
                        color.g() as f32 / 255.0,
                        color.b() as f32 / 255.0,
                        color.a() as f32 / 255.0,
                    ],
                    kind: slot.kind,
                    _pad: [0; 1],
                });
            }
            let cell = super::geometry::GridGlyphCell::from_metadata(first.metadata);
            if let Some(index) = cluster
                && glyphs.iter().all(|glyph| glyph.glyph_id != 0)
            {
                let text = &run.text[emoji_clusters[index].clone()];
                let columns = cell.map_or_else(
                    || {
                        text[alacritty_terminal::term::grapheme_base_offset(text)..]
                            .chars()
                            .next()
                            .and_then(unicode_width::UnicodeWidthChar::width)
                            .unwrap_or(1)
                            .max(1)
                    },
                    |cell| cell.width,
                );
                let col = super::geometry::glyph_grid_col_with_metadata(
                    char_starts,
                    first.start,
                    first.metadata,
                );
                let x = cell_w.map_or(origin.0 + first.x, |cw| origin.0 + col as f32 * cw);
                fit_emoji_glyphs(
                    &mut out[start..],
                    (
                        x,
                        origin.1 + run.line_top,
                        columns as f32 * cell_width,
                        run.line_height,
                    ),
                );
            }
            if cluster.is_some()
                && let Some(cw) = cell_w
                && let Some(cell) = cell
                && cell.clustered
            {
                let left = (origin.0 + cell.column as f32 * cw).round();
                let right = (origin.0 + (cell.column + cell.width) as f32 * cw).round();
                let mut kept = start;
                for index in start..out.len() {
                    let mut glyph = out[index];
                    let clipped = left.max(glyph.pos[0]);
                    let scale = glyph.size[0] / glyph.uv_size[0];
                    let width = (glyph.pos[0] + glyph.size[0]).min(right) - clipped;
                    if width <= 0.0 {
                        continue;
                    }
                    glyph.uv[0] += (clipped - glyph.pos[0]) / scale;
                    glyph.uv_size[0] = width / scale;
                    glyph.pos[0] = clipped;
                    glyph.size[0] = width;
                    out[kept] = glyph;
                    kept += 1;
                }
                out.truncate(kept);
            }
        }
    }
}

/// One glyph image the grid emit draws: its surface rectangle, raster key and
/// coverage mask.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CellImage {
    pub(crate) key: glyphon::cosmic_text::CacheKey,
    /// `x, y, width, height`, in surface pixels.
    pub(crate) rect: [i32; 4],
    pub(crate) mask: Vec<u8>,
}

/// The images the grid emit draws for `cell` (a buffer line and grid column)
/// of `buf` when fallback placement in `marks` moved one of its glyphs;
/// empty when it moved none, or when one is not a plain coverage mask.
///
/// The block cursor's inverted glyph draws these, so it lands on the pane's
/// copy pixel for pixel: glyphon can only place a whole shaped buffer, and
/// over a moved mark the pane's copy would show beside the inverted one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn moved_cell_images(
    buf: &TextBuffer,
    marks: &[LineShifts],
    cell: (usize, usize),
    origin: (f32, f32),
    cell_width: f32,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    char_starts: &mut Vec<u32>,
) -> Vec<CellImage> {
    let (line, column) = cell;
    let shifts = shifts_of(marks, line);
    if shifts.is_empty() {
        return Vec::new();
    };
    let Some(run) = buf.layout_runs().find(|run| run.line_i == line) else {
        return Vec::new();
    };
    char_starts.clear();
    for (b, ch) in run.text.char_indices() {
        if unicode_width::UnicodeWidthChar::width(ch) != Some(0) {
            char_starts.push(b as u32);
        }
    }
    let moved = shifts.iter().any(|shift| {
        run.glyphs.get(shift.index as usize).is_some_and(|glyph| {
            glyph.glyph_id == shift.glyph_id
                && super::geometry::glyph_grid_col_with_metadata(
                    char_starts,
                    glyph.start,
                    glyph.metadata,
                ) == column
        })
    });
    if !moved {
        return Vec::new();
    }
    let mut masks = Vec::new();
    let mut plain = true;
    let mut out = Vec::new();
    emit_positioned_glyphs(
        &mut out,
        buf,
        marks,
        origin,
        cell_width,
        GlyphPositioning::Grid(cell_width),
        Some(cell),
        GColor::rgb(0, 0, 0),
        char_starts,
        |key| {
            let image = swash.get_image_uncached(fonts, key)?;
            let placement = image.placement;
            if placement.width == 0 || placement.height == 0 {
                return None;
            }
            plain &= image.content == glyphon::SwashContent::Mask;
            masks.push((key, image.data));
            Some(super::glyphpipe::GlyphSlot {
                kind: 0,
                atlas_x: 0.0,
                atlas_y: 0.0,
                w: placement.width as f32,
                h: placement.height as f32,
                left: placement.left,
                top: placement.top,
            })
        },
    );
    if !plain || out.len() != masks.len() {
        return Vec::new();
    }
    out.iter()
        .zip(masks)
        .map(|(glyph, (key, mask))| CellImage {
            key,
            rect: [
                glyph.pos[0] as i32,
                glyph.pos[1] as i32,
                glyph.size[0] as i32,
                glyph.size[1] as i32,
            ],
            mask,
        })
        .collect()
}

fn fit_emoji_glyphs(glyphs: &mut [GlyphInstance], cell: Rect4) {
    let mut bounds = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for glyph in glyphs.iter() {
        bounds.0 = bounds.0.min(glyph.pos[0]);
        bounds.1 = bounds.1.min(glyph.pos[1]);
        bounds.2 = bounds.2.max(glyph.pos[0] + glyph.size[0]);
        bounds.3 = bounds.3.max(glyph.pos[1] + glyph.size[1]);
    }
    let (width, height) = (bounds.2 - bounds.0, bounds.3 - bounds.1);
    if !(width > 0.0 && height > 0.0) {
        return;
    }
    let fitted = super::fonts::fit_emoji(cell, (width, height));
    let scale = fitted.2 / width;
    for glyph in glyphs {
        glyph.pos[0] = fitted.0 + (glyph.pos[0] - bounds.0) * scale;
        glyph.pos[1] = fitted.1 + (glyph.pos[1] - bounds.1) * scale;
        glyph.size[0] *= scale;
        glyph.size[1] *= scale;
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
