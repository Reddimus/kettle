use super::mark_fallback::{LineShifts, MarkShift, shift_for, shifts_of};
use super::theme::pane_inactive_amount;
use super::{
    Config, ContextMenu, FontSystem, GColor, GlyphInstance, GlyphPipeline, OutlineInstance,
    Overlay, PaneView, PatchRect, QuadInstance, RasterGlyph, Rect4, RenderUploads, Renderer,
    Result, Rgb, SwashCache, TextArea, TextBounds, TextBuffer, TextRendererMode, UploadCounts,
    cell_locked_pen_x, imgpipe, quad,
};

/// One open menu level's instances: its rounded surfaces, flat chrome quads
/// and swatches. The scene pass draws them, then the level's labels, before
/// the next level.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MenuLevelDraw {
    pub surfaces: std::ops::Range<u32>,
    pub quads: std::ops::Range<u32>,
    pub swatches: std::ops::Range<u32>,
}

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
            row.description.hash(&mut hash);
            row.heading.hash(&mut hash);
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
            self.menu_surfaces.upload_counts(),
            self.menu_swatch_surfaces.upload_counts(),
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
            .is_some_and(|cursor| cursor.builtin.is_some())
            || self.cursor_emoji_bounds.is_some();
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
        // Menu chrome sits above terminal content. The chrome quads queued
        // before the first menu level, the receipt thumbnail and the toasts
        // draw first, so every open menu covers them. Their labels are in the
        // shared chrome-text pass below, cut out of each menu panel, which
        // keeps them readable above the thumbnail. Each open menu level is a
        // layer of its own: its rounded surfaces, its quads, its swatches and
        // then its labels, before the next level, so a submenu covers its
        // parent's selection and text. The quads queued after the last level
        // (a picker's caret and selection) draw above them.
        self.rounded_surfaces.draw_range(
            &mut pass,
            self.transport_surface_count
                ..self.transport_surface_count + self.settings_surface_count,
        );
        let levels = &self.menu_level_draws;
        let first = levels.first().map_or(u32::MAX, |level| level.quads.start);
        let last = levels.last().map_or(u32::MAX, |level| level.quads.end);
        self.menu_quads.draw_range(&mut pass, 0..first);
        self.media_receipt_img.draw(&mut pass);
        self.toast_surfaces.draw_range(&mut pass, 0..u32::MAX);
        for (index, level) in levels.iter().enumerate() {
            self.menu_surfaces
                .draw_range(&mut pass, level.surfaces.clone());
            self.menu_quads.draw_range(&mut pass, level.quads.clone());
            self.menu_swatch_surfaces
                .draw_range(&mut pass, level.swatches.clone());
            if let Some(text) = self.menu_level_text.get(index) {
                text.render(&self.atlas, &self.viewport, &mut pass)?;
            }
        }
        self.menu_quads.draw_range(&mut pass, last..u32::MAX);
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
        dash: 0.0,
        gap: 0.0,
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
/// `emoji_cells` receives each emoji cluster drawn: its buffer line, grid
/// column and range of `out`, so the block cursor can draw those same images.
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
    emoji_cells: &mut Vec<EmojiCellImages>,
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
        AtlasGlyphs {
            glyph_pipeline,
            swash,
            font_system,
            device,
            queue,
            emoji_cells,
        },
    );
}

/// One emoji cluster the grid emit drew: buffer `line`, grid `column`, and
/// the range of the emit's output holding its images.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmojiCellImages {
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) instances: std::ops::Range<usize>,
}

/// Where the grid emit gets its glyph images. A closure over cache keys is a
/// source of shaped-size images only; its emoji are scaled on the GPU.
pub(crate) trait GlyphSource {
    /// The image of a glyph at its shaped size.
    fn slot(&mut self, key: glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot>;

    /// The raster placement of an emoji glyph at its shaped size, which sizes
    /// its cluster's fitted images. Only the kind, size and bearings are read.
    fn placement(
        &mut self,
        key: glyphon::cosmic_text::CacheKey,
    ) -> Option<super::glyphpipe::GlyphSlot> {
        self.slot(key)
    }

    /// The emoji glyph `key`, whose shaped-size image was `natural`,
    /// rasterized at exactly `size` pixels. `None` draws the cluster from
    /// its shaped-size images instead, scaled to the same place.
    fn fitted(
        &mut self,
        _key: glyphon::cosmic_text::CacheKey,
        _natural: super::glyphpipe::GlyphSlot,
        _size: [u32; 2],
    ) -> Option<super::glyphpipe::GlyphSlot> {
        None
    }

    /// The emit drew an emoji cluster.
    fn emoji_cell(&mut self, _cell: EmojiCellImages) {}

    /// The joining stroke (tatweel, U+0640) of the face and size `base` was
    /// shaped with, as a mask image, or `None` when the face has none.
    fn connector(
        &mut self,
        _base: glyphon::cosmic_text::CacheKey,
    ) -> Option<super::glyphpipe::GlyphSlot> {
        None
    }

    /// The emit placed one run of a grid cell's glyphs as `instances` of its
    /// output. Arabic join shifts move them again before the emit returns.
    fn drew(&mut self, _column: usize, _width: usize, _instances: std::ops::Range<usize>) {}
}

impl<F> GlyphSource for F
where
    F: FnMut(glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot>,
{
    fn slot(&mut self, key: glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot> {
        self(key)
    }
}

/// The live source: images rasterized into the glyph pipeline's atlases.
struct AtlasGlyphs<'a> {
    glyph_pipeline: &'a mut GlyphPipeline,
    swash: &'a mut SwashCache,
    font_system: &'a mut FontSystem,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    emoji_cells: &'a mut Vec<EmojiCellImages>,
}

impl GlyphSource for AtlasGlyphs<'_> {
    fn slot(&mut self, key: glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot> {
        let Self {
            glyph_pipeline,
            swash,
            font_system,
            device,
            queue,
            ..
        } = self;
        let mut image = None;
        glyph_pipeline.ensure_glyph(device, queue, key, || {
            image = swash.get_image_uncached(font_system, key);
            RasterGlyph::from_swash(image.as_ref()?)
        })
    }

    fn placement(
        &mut self,
        key: glyphon::cosmic_text::CacheKey,
    ) -> Option<super::glyphpipe::GlyphSlot> {
        let Self {
            glyph_pipeline,
            swash,
            font_system,
            ..
        } = self;
        glyph_pipeline.placement(key, || raster_placement(swash, font_system, key))
    }

    fn fitted(
        &mut self,
        key: glyphon::cosmic_text::CacheKey,
        natural: super::glyphpipe::GlyphSlot,
        size: [u32; 2],
    ) -> Option<super::glyphpipe::GlyphSlot> {
        let Self {
            glyph_pipeline,
            swash,
            font_system,
            device,
            queue,
            ..
        } = self;
        let slot_size = [u16::try_from(size[0]).ok()?, u16::try_from(size[1]).ok()?];
        // The fitted image is resampled from an unshifted source and drawn on
        // whole pixels, so every subpixel position shares one.
        let mut unshifted = key;
        unshifted.x_bin = glyphon::cosmic_text::SubpixelBin::Zero;
        unshifted.y_bin = glyphon::cosmic_text::SubpixelBin::Zero;
        let mut raster = None;
        glyph_pipeline.ensure_glyph(
            device,
            queue,
            super::glyphpipe::SlotKey::Emoji {
                key: unshifted,
                size: slot_size,
            },
            || {
                raster = super::emoji_raster::fitted_emoji_raster(
                    font_system,
                    swash,
                    key,
                    natural,
                    size,
                );
                let raster = raster.as_ref()?;
                Some(RasterGlyph {
                    color: raster.color,
                    width: raster.width,
                    height: raster.height,
                    left: 0,
                    top: 0,
                    data: &raster.data,
                })
            },
        )
    }

    fn emoji_cell(&mut self, cell: EmojiCellImages) {
        self.emoji_cells.push(cell);
    }

    fn connector(
        &mut self,
        base: glyphon::cosmic_text::CacheKey,
    ) -> Option<super::glyphpipe::GlyphSlot> {
        let key = connector_key(self.font_system, base)?;
        self.slot(key).filter(|slot| slot.kind == 1)
    }
}

/// The kind, size and bearings of `key`'s raster image, with no atlas
/// position.
fn raster_placement(
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    key: glyphon::cosmic_text::CacheKey,
) -> Option<super::glyphpipe::GlyphSlot> {
    let image = swash.get_image_uncached(font_system, key)?;
    let kind = match image.content {
        glyphon::SwashContent::Color => 0,
        glyphon::SwashContent::Mask => 1,
        glyphon::SwashContent::SubpixelMask => return None,
    };
    let placement = image.placement;
    (placement.width > 0 && placement.height > 0).then_some(super::glyphpipe::GlyphSlot {
        kind,
        atlas_x: 0.0,
        atlas_y: 0.0,
        w: placement.width as f32,
        h: placement.height as f32,
        left: placement.left,
        top: placement.top,
    })
}

/// Glyph images by their raster placement alone. Nothing is rasterized into,
/// or evicted from, the atlas, so instances already uploaded keep their
/// pixels; the emit through it only measures where each cell is drawn.
struct PlacedGlyphs<'a> {
    glyph_pipeline: &'a mut GlyphPipeline,
    swash: &'a mut SwashCache,
    font_system: &'a mut FontSystem,
    drawn: Vec<(usize, usize, std::ops::Range<usize>)>,
}

impl GlyphSource for &mut PlacedGlyphs<'_> {
    fn slot(&mut self, key: glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot> {
        if let Some(slot) = self.glyph_pipeline.resident(key) {
            return Some(slot);
        }
        let PlacedGlyphs {
            glyph_pipeline,
            swash,
            font_system,
            ..
        } = &mut **self;
        glyph_pipeline.placement(key, || raster_placement(swash, font_system, key))
    }

    fn connector(
        &mut self,
        base: glyphon::cosmic_text::CacheKey,
    ) -> Option<super::glyphpipe::GlyphSlot> {
        let key = connector_key(self.font_system, base)?;
        self.slot(key).filter(|slot| slot.kind == 1)
    }

    fn drew(&mut self, column: usize, width: usize, instances: std::ops::Range<usize>) {
        self.drawn.push((column, width, instances));
    }
}

/// The pixel x span the grid emit draws each cell of buffer `line` over,
/// as `(column, width, left, right)`, one entry per run of a cell's glyphs.
/// A glyph wider than its cell reaches past the cell's edges: an Arabic
/// final or isolated letter, after its join shift.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drawn_cell_spans(
    buf: &TextBuffer,
    marks: &[LineShifts],
    line: usize,
    origin: (f32, f32),
    cell_width: f32,
    positioning: GlyphPositioning,
    glyph_pipeline: &mut GlyphPipeline,
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    char_starts: &mut Vec<u32>,
) -> Vec<(usize, usize, f32, f32)> {
    let mut source = PlacedGlyphs {
        glyph_pipeline,
        swash,
        font_system,
        drawn: Vec::new(),
    };
    let mut out = Vec::new();
    emit_positioned_glyphs(
        &mut out,
        buf,
        marks,
        origin,
        cell_width,
        positioning,
        Some((line, None)),
        GColor::rgb(0, 0, 0),
        char_starts,
        &mut source,
    );
    source
        .drawn
        .into_iter()
        .filter_map(|(column, width, instances)| {
            out.get(instances)?
                .iter()
                .map(|glyph| (glyph.pos[0], glyph.pos[0] + glyph.size[0]))
                .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
                .map(|(left, right)| (column, width, left, right))
        })
        .collect()
}

fn connector_key(
    fonts: &mut FontSystem,
    base: glyphon::cosmic_text::CacheKey,
) -> Option<glyphon::cosmic_text::CacheKey> {
    let font = fonts.get_font(base.font_id, base.font_weight)?;
    let glyph_id = font.as_swash().charmap().map('\u{0640}');
    if glyph_id == 0 {
        return None;
    }
    let mut key = base;
    key.glyph_id = glyph_id;
    key.x_bin = glyphon::cosmic_text::SubpixelBin::Zero;
    Some(key)
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
/// optionally a grid column, it emits that line's or that cell's glyphs
/// alone, placed exactly as in the full emit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_positioned_glyphs(
    out: &mut Vec<GlyphInstance>,
    buf: &TextBuffer,
    marks: &[LineShifts],
    origin: (f32, f32),
    cell_width: f32,
    positioning: GlyphPositioning,
    only: Option<(usize, Option<usize>)>,
    default_color: GColor,
    char_starts: &mut Vec<u32>,
    mut glyph_source: impl GlyphSource,
) {
    let mut emoji_keys = Vec::new();
    let only_column = only.and_then(|(_, column)| column);
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
        let mut joined: Option<super::arabic_join::JoinedRow> = None;
        for (range, attrs) in buf.lines[run.line_i].attrs_list().spans_iter() {
            let Some(cell) = super::geometry::GridGlyphCell::from_metadata(attrs.metadata) else {
                continue;
            };
            if let Some(cw) = cell_w
                && (cell.joins.left || cell.joins.right)
                && let Some(text) = run.text.get(range.clone())
            {
                joined.get_or_insert_with(Default::default).add(
                    cell,
                    range.start + alacritty_terminal::term::grapheme_base_offset(text),
                    origin.0,
                    cw,
                );
            }
            if !cell.clustered {
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
            // A cluster with a missing glyph draws its tofu as text does.
            let fitted_cluster =
                cluster.is_some() && glyphs.iter().all(|glyph| glyph.glyph_id != 0);
            emoji_keys.clear();
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
                // Keep the shaped offsets until every image in the cluster
                // has been rasterized, so fitting uses their combined ink.
                let off_x = cell_w.map_or(origin.0, |cw| {
                    let cell_left = origin.0 + col as f32 * cw;
                    cell_locked_glyph_offset(cell_left, glyph, cluster_x)
                });
                let phys = glyph.physical((off_x, origin.1), 1.0);
                let selected = only_column.is_none_or(|column| column == col);
                let joining = glyph.metadata & 96 != 0 && joined.is_some();
                if !selected
                    && (!joining
                        || only_column
                            .is_some_and(|target| !joined.as_ref().unwrap().needs(col, target)))
                {
                    continue;
                }
                let slot = if !selected && glyph.w <= 0.0 {
                    None
                } else if fitted_cluster {
                    glyph_source.placement(phys.cache_key)
                } else {
                    glyph_source.slot(phys.cache_key)
                };
                if joining {
                    let pen = origin.0
                        + col as f32 * cell_w.unwrap()
                        + (glyph.x - cluster_x)
                        + glyph.font_size * glyph.x_offset;
                    joined.as_mut().unwrap().observe(
                        glyph,
                        pen,
                        line_y.round() + phys.y as f32,
                        slot,
                        phys.cache_key,
                        glyph.color_opt.unwrap_or(default_color),
                        (selected && slot.is_some()).then_some(out.len()),
                        &mut glyph_source,
                    );
                }
                if !selected {
                    continue;
                }
                let Some(slot) = slot else {
                    continue;
                };
                if fitted_cluster {
                    emoji_keys.push((phys.cache_key, slot));
                }
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
            let mut emoji_column = None;
            if let Some(index) = cluster
                && fitted_cluster
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
                let cell_box = (
                    x,
                    origin.1 + run.line_top,
                    columns as f32 * cell_width,
                    run.line_height,
                );
                if !fit_emoji_images(&mut out[start..], &emoji_keys, cell_box, &mut glyph_source) {
                    // Draw the shaped-size images, scaled into the same place.
                    let mut kept = start;
                    for (index, &(key, _)) in emoji_keys.iter().enumerate() {
                        let Some(slot) = glyph_source.slot(key) else {
                            continue;
                        };
                        let mut glyph = out[start + index];
                        glyph.uv = [slot.atlas_x, slot.atlas_y];
                        glyph.kind = slot.kind;
                        out[kept] = glyph;
                        kept += 1;
                    }
                    out.truncate(kept);
                    fit_emoji_glyphs(&mut out[start..], super::fonts::emoji_box(cell_box));
                }
                emoji_column = Some(col);
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
            if let Some(column) = emoji_column
                && out.len() > start
            {
                glyph_source.emoji_cell(EmojiCellImages {
                    line: run.line_i,
                    column,
                    instances: start..out.len(),
                });
            }
            if let Some(cell) = cell
                && out.len() > start
            {
                glyph_source.drew(cell.column, cell.width, start..out.len());
            }
        }
        if let Some(joined) = joined {
            joined.finish(out, only_column);
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
    /// The source rectangle distinguishes a stretched connector from a
    /// natural glyph using the same raster key in the cursor atlas.
    pub(crate) sample: [u16; 4],
}

impl CellImage {
    pub(crate) fn raster_key(&self) -> (glyphon::cosmic_text::CacheKey, [u16; 4]) {
        (self.key, self.sample)
    }
}

/// The images the grid emit draws for `cell` (a buffer line and grid column)
/// of `buf` when fallback placement moved a mark or the cell carries Arabic
/// join sides. Includes its connector fragments; empty for ordinary unmoved
/// cells or when a source image is not a plain coverage mask.
/// The caller supplies `joined` from the row's bidi map.
///
/// The block cursor's inverted glyph draws these, so it lands on the pane's
/// copy pixel for pixel: glyphon can only place a whole shaped buffer, and
/// over a moved mark the pane's copy would show beside the inverted one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn moved_cell_images(
    buf: &TextBuffer,
    marks: &[LineShifts],
    cell: (usize, usize),
    joined: bool,
    origin: (f32, f32),
    cell_width: f32,
    positioning: GlyphPositioning,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    char_starts: &mut Vec<u32>,
) -> Vec<CellImage> {
    let (line, column) = cell;
    let shifts = match positioning {
        GlyphPositioning::Grid(_) => shifts_of(marks, line),
        _ => &[],
    };
    if !joined && shifts.is_empty() {
        return Vec::new();
    }
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
    if !moved && !joined {
        return Vec::new();
    }
    let mut source = CursorGlyphs {
        fonts,
        swash,
        masks: Vec::new(),
        plain: true,
    };
    let mut out = Vec::new();
    emit_positioned_glyphs(
        &mut out,
        buf,
        marks,
        origin,
        cell_width,
        positioning,
        Some((line, Some(column))),
        GColor::rgb(0, 0, 0),
        char_starts,
        &mut source,
    );
    if !source.plain {
        return Vec::new();
    }
    out.iter()
        .map(|glyph| source.image(glyph))
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

struct CursorGlyphs<'a> {
    fonts: &'a mut FontSystem,
    swash: &'a mut SwashCache,
    masks: Vec<(glyphon::cosmic_text::CacheKey, glyphon::SwashImage)>,
    plain: bool,
}

impl GlyphSource for &mut CursorGlyphs<'_> {
    fn slot(&mut self, key: glyphon::cosmic_text::CacheKey) -> Option<super::glyphpipe::GlyphSlot> {
        let image = self.swash.get_image_uncached(self.fonts, key)?;
        let placement = image.placement;
        if placement.width == 0 || placement.height == 0 {
            return None;
        }
        self.plain &= image.content == glyphon::SwashContent::Mask;
        let index = self.masks.len();
        self.masks.push((key, image));
        Some(super::glyphpipe::GlyphSlot {
            kind: 1,
            atlas_x: 0.0,
            atlas_y: index as f32,
            w: placement.width as f32,
            h: placement.height as f32,
            left: placement.left,
            top: placement.top,
        })
    }

    fn connector(
        &mut self,
        base: glyphon::cosmic_text::CacheKey,
    ) -> Option<super::glyphpipe::GlyphSlot> {
        let key = connector_key(self.fonts, base)?;
        self.slot(key)
    }
}

impl CursorGlyphs<'_> {
    fn image(&self, glyph: &GlyphInstance) -> Option<CellImage> {
        let (key, image) = self.masks.get(glyph.uv[1] as usize)?;
        let width = u16::try_from(glyph.size[0] as u32).ok()?;
        let height = u16::try_from(glyph.size[1] as u32).ok()?;
        let count = usize::from(width).checked_mul(usize::from(height))?;
        if count == 0 || count > 1 << 20 {
            return None;
        }
        let sample = [
            glyph.uv[0] as u16,
            0,
            glyph.uv_size[0] as u16,
            glyph.uv_size[1] as u16,
        ];
        let mut mask = Vec::with_capacity(count);
        for y in 0..height {
            for x in 0..width {
                let sx =
                    u32::from(sample[0]) + u32::from(x) * u32::from(sample[2]) / u32::from(width);
                let sy = u32::from(y) * u32::from(sample[3]) / u32::from(height);
                mask.push(*image.data.get((sy * image.placement.width + sx) as usize)?);
            }
        }
        Some(CellImage {
            key: *key,
            rect: [
                glyph.pos[0] as i32,
                glyph.pos[1] as i32,
                i32::from(width),
                i32::from(height),
            ],
            mask,
            sample,
        })
    }
}

/// Place an emoji cluster's images, emitted at their shaped size and
/// position, in whole pixels inside `cell`'s emoji box
/// (`fonts::fit_emoji_pixels`) and replace each with an image rasterized at
/// exactly the size it is drawn. The cluster's union keeps its aspect ratio;
/// each image's edges are the union's scaled edges rounded to whole pixels,
/// so images that touch still touch. `keys` holds each image's cache key and
/// shaped-size placement. Returns `false`, changing nothing, when any fitted
/// image is unavailable.
fn fit_emoji_images(
    glyphs: &mut [GlyphInstance],
    keys: &[(glyphon::cosmic_text::CacheKey, super::glyphpipe::GlyphSlot)],
    cell: Rect4,
    source: &mut impl GlyphSource,
) -> bool {
    if glyphs.is_empty() || glyphs.len() != keys.len() {
        return false;
    }
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
    let Some([x, y, w, h]) =
        super::fonts::fit_emoji_pixels(super::fonts::emoji_box(cell), (width, height))
    else {
        return false;
    };
    let (sx, sy) = (w as f32 / width, h as f32 / height);
    let mut fitted = Vec::with_capacity(glyphs.len());
    for (glyph, &(key, natural)) in glyphs.iter().zip(keys) {
        let (left, right) = fitted_span(
            glyph.pos[0],
            glyph.pos[0] + glyph.size[0],
            bounds.0,
            sx,
            x,
            w,
        );
        let (top, bottom) = fitted_span(
            glyph.pos[1],
            glyph.pos[1] + glyph.size[1],
            bounds.1,
            sy,
            y,
            h,
        );
        let size = [(right - left) as u32, (bottom - top) as u32];
        let Some(slot) = source.fitted(key, natural, size) else {
            return false;
        };
        if slot.w != size[0] as f32 || slot.h != size[1] as f32 {
            return false;
        }
        fitted.push((left, top, size, slot));
    }
    for (glyph, (left, top, size, slot)) in glyphs.iter_mut().zip(fitted) {
        glyph.pos = [left as f32, top as f32];
        glyph.size = [size[0] as f32, size[1] as f32];
        glyph.uv = [slot.atlas_x, slot.atlas_y];
        glyph.uv_size = glyph.size;
        glyph.kind = slot.kind;
    }
    true
}

/// The whole-pixel span `[lo, hi)` an image edge pair `start..end` (shaped
/// pixels, measured from the cluster union's edge `origin`) takes when the
/// union is scaled by `scale` onto the fitted span `at..at + len`: the scaled
/// edges rounded, at least one pixel wide and never outside the fitted span.
/// A sliver of a mark at the union's far edge, which rounds to nothing there,
/// keeps its last pixel inside rather than spilling past it.
pub(crate) fn fitted_span(
    start: f32,
    end: f32,
    origin: f32,
    scale: f32,
    at: i32,
    len: i32,
) -> (i32, i32) {
    let limit = at + len.max(1);
    let lo = (at + ((start - origin) * scale).round() as i32).clamp(at, limit - 1);
    let hi = (at + ((end - origin) * scale).round() as i32).clamp(lo + 1, limit);
    (lo, hi)
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
