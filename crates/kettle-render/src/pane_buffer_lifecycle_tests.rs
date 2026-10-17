use kettle_config::Rgb;

#[test]
fn prepared_chrome_colors_and_geometry_invalidate_retained_vertices() {
    let mut font_system = super::FontSystem::new();
    let buffer = super::TextBuffer::new(&mut font_system, super::Metrics::new(14.0, 18.0));
    let area = |color, left| super::TextArea {
        buffer: &buffer,
        left,
        top: 4.0,
        scale: 1.0,
        bounds: super::TextBounds {
            left: 0,
            top: 0,
            right: 100,
            bottom: 30,
        },
        default_color: color,
        custom_glyphs: &[],
    };
    let idle = super::prepared_text_areas_damage_key(&[area(super::GColor::rgb(80, 80, 80), 10.0)]);

    for (name, changed) in [
        (
            "close hover",
            super::prepared_text_areas_damage_key(&[area(super::GColor::rgb(20, 20, 20), 10.0)]),
        ),
        (
            "pane focus",
            super::prepared_text_areas_damage_key(&[area(super::GColor::rgb(230, 230, 230), 10.0)]),
        ),
        (
            "broadcast",
            super::prepared_text_areas_damage_key(&[area(super::GColor::rgb(240, 210, 80), 10.0)]),
        ),
        (
            "area position",
            super::prepared_text_areas_damage_key(&[area(super::GColor::rgb(80, 80, 80), 11.0)]),
        ),
    ] {
        assert_ne!(idle, changed, "{name} must force main-text preparation");
    }
}

#[test]
fn same_shape_completion_text_changes_invalidate_retained_vertices() {
    let header = "Completions · fish";
    let count = "1/2";
    let labels = vec!["checkout".to_string(), "cherry-pick".to_string()];
    let descriptions = vec!["switch branch".to_string(), "apply commit".to_string()];
    let spans = vec![Some((0, 2)), Some((0, 2))];
    let selected = vec![true, false];
    let colors = vec![Rgb::new(10, 20, 30), Rgb::new(40, 50, 60)];
    let key = |header: &str,
               count: &str,
               labels: &[String],
               descriptions: &[String],
               spans: &[Option<(usize, usize)>],
               selected: &[bool],
               colors: &[Rgb]| {
        super::completion_text_damage_key(
            header,
            count,
            labels,
            descriptions,
            spans,
            selected,
            colors,
        )
    };
    let baseline = key(
        header,
        count,
        &labels,
        &descriptions,
        &spans,
        &selected,
        &colors,
    );

    let changed_labels = vec!["clean-up".to_string(), "cherry-pick".to_string()];
    assert_ne!(
        baseline,
        key(
            header,
            count,
            &changed_labels,
            &descriptions,
            &spans,
            &selected,
            &colors,
        ),
        "same-sized label edits must prepare new completion glyphs"
    );

    let changed_descriptions = vec!["switch branch".to_string(), "pick commit".to_string()];
    assert_ne!(
        baseline,
        key(
            header,
            count,
            &labels,
            &changed_descriptions,
            &spans,
            &selected,
            &colors,
        ),
        "same-sized description edits must prepare new completion glyphs"
    );
    assert_ne!(
        baseline,
        key(
            header,
            "2/2",
            &labels,
            &descriptions,
            &spans,
            &selected,
            &colors,
        ),
        "header counters must prepare new completion glyphs"
    );
    let changed_colors = vec![Rgb::new(11, 20, 30), Rgb::new(40, 50, 60)];
    assert_ne!(
        baseline,
        key(
            header,
            count,
            &labels,
            &descriptions,
            &spans,
            &selected,
            &changed_colors,
        ),
        "live theme changes must reshape explicit emphasis colors"
    );
    let changed_spans = vec![Some((1, 3)), Some((0, 2))];
    assert_ne!(
        baseline,
        key(
            header,
            count,
            &labels,
            &descriptions,
            &changed_spans,
            &selected,
            &colors,
        ),
        "a moved emphasis span must prepare new completion glyphs"
    );
    let changed_selection = vec![false, true];
    assert_ne!(
        baseline,
        key(
            header,
            count,
            &labels,
            &descriptions,
            &spans,
            &changed_selection,
            &colors,
        ),
        "selection changes must prepare the selected text colors"
    );

    let src = super::production_source();
    let refresh = src
        .find("if self.completion_texts[line_index] != line")
        .expect("completion label buffers are refreshed");
    let damage = src
        .find("completion_text_damage_key(\n                &self.completion_header_text")
        .expect("completion source strings enter the retained-text damage key");
    assert!(
        refresh < damage,
        "completion strings must be refreshed before their retained-text damage key is computed"
    );
}

#[test]
fn context_menu_hover_preserves_text_damage_key() {
    let mut menu = super::ContextMenu {
        anchor: (20.0, 30.0),
        rows: vec![
            super::ContextMenuRow {
                label: "Copy".to_string(),
                separator: false,
                enabled: true,
                hint: "Ctrl+Shift+C".to_string(),
            },
            super::ContextMenuRow {
                label: "Paste".to_string(),
                separator: false,
                enabled: true,
                hint: "Ctrl+Shift+V".to_string(),
            },
        ],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 240.0,
        panel_h_clamped: 120.0,
        a11y_label: None,
    };
    let foreground = kettle_config::Rgb::new(220, 220, 220);
    let background = kettle_config::Rgb::new(20, 20, 20);
    let initial = super::context_menu_text_damage_key(Some(&menu), foreground, background);

    menu.highlight = 1;
    assert_eq!(
        super::context_menu_text_damage_key(Some(&menu), foreground, background),
        initial,
        "highlight motion changes quads, not shaped menu text"
    );

    menu.scroll_offset = 1;
    assert_ne!(
        super::context_menu_text_damage_key(Some(&menu), foreground, background),
        initial
    );
    menu.scroll_offset = 0;
    menu.rows[0].enabled = false;
    assert_ne!(
        super::context_menu_text_damage_key(Some(&menu), foreground, background),
        initial
    );
    assert_ne!(
        super::context_menu_text_damage_key(
            Some(&menu),
            kettle_config::Rgb::new(221, 220, 220),
            background,
        ),
        initial,
        "glyphon retains the menu foreground in prepared vertices"
    );
    assert_ne!(
        super::context_menu_text_damage_key(
            Some(&menu),
            foreground,
            kettle_config::Rgb::new(21, 20, 20),
        ),
        initial,
        "disabled-row colors depend on the menu background"
    );
}

#[test]
fn cursor_glyph_damage_key_reuses_only_identical_vertices() {
    let mut cursor = super::PendingCursorGlyph {
        x: 10.0,
        y: 20.0,
        ch: 'x',
        text: "x".into(),
        emoji_qualified: false,
        attrs: super::AttrsOwned::new(
            &super::Attrs::new().family(glyphon::Family::Name("Kettle Mono")),
        ),
        color: kettle_config::Rgb::new(1, 2, 3),
        clip: (0.0, 0.0, 100.0, 80.0),
    };
    let initial = super::cursor_glyph_damage_key(
        Some(&cursor),
        glyphon::Metrics::new(14.0, 18.0),
        "Kettle Mono",
    );
    assert_eq!(
        super::cursor_glyph_damage_key(
            Some(&cursor),
            glyphon::Metrics::new(14.0, 18.0),
            "Kettle Mono",
        ),
        initial
    );

    cursor.attrs.weight = glyphon::Weight::BOLD;
    assert_ne!(
        super::cursor_glyph_damage_key(
            Some(&cursor),
            glyphon::Metrics::new(14.0, 18.0),
            "Kettle Mono",
        ),
        initial
    );
    cursor.attrs.weight = glyphon::Weight::NORMAL;
    cursor.text.push('\u{301}');
    assert_ne!(
        super::cursor_glyph_damage_key(
            Some(&cursor),
            glyphon::Metrics::new(14.0, 18.0),
            "Kettle Mono",
        ),
        initial
    );
    cursor.text = "x".into();
    cursor.x = 11.0;
    assert_ne!(
        super::cursor_glyph_damage_key(
            Some(&cursor),
            glyphon::Metrics::new(14.0, 18.0),
            "Kettle Mono",
        ),
        initial
    );
    cursor.x = 10.0;
    assert_ne!(
        super::cursor_glyph_damage_key(
            Some(&cursor),
            glyphon::Metrics::new(15.0, 19.0),
            "Kettle Mono",
        ),
        initial
    );
    assert_eq!(
        super::cursor_glyph_damage_key(None, glyphon::Metrics::new(14.0, 18.0), "Kettle Mono",),
        None
    );
}

#[test]
fn failed_text_prepare_keeps_the_retry_latch_armed() {
    let src = super::production_source();
    let start = src
        .find("let need_prepare = self.text_prepare_dirty")
        .expect("prepare retry latch participates in damage");
    let body = &src[start..];
    let arm = body
        .find("self.text_prepare_dirty = true;")
        .expect("retry latch armed before fallible prepares");
    let main_prepare = body
        .find("self.text_renderer.prepare(")
        .expect("main text prepare present");
    let menu_prepare = body
        .find("self.menu_text_renderer.prepare(")
        .expect("menu text prepare present");
    let cursor_prepare = body
        .find("self.cursor_glyph_renderer.prepare(")
        .expect("cursor text prepare present");
    let clear = body
        .find("self.text_prepare_dirty = false;")
        .expect("retry latch clears after successful prepares");
    assert!(
        arm < main_prepare
            && main_prepare < menu_prepare
            && menu_prepare < cursor_prepare
            && cursor_prepare < clear,
        "the retry latch must remain armed across every fallible shared-atlas prepare"
    );
}

/// Drift guard. The per-pane text-buffer vecs are grown with
/// `while len < panes.len()` and must be truncated back down when panes
/// close, or they sit at the session's high-water pane count holding idle
/// glyph buffers. A behavioral test would need a full GPU `Renderer`, so
/// pin the invariant at the source level (same shape as term.rs's
/// detach-never-joins guard): all three truncate calls must stay present.
#[test]
fn render_frame_truncates_pane_buffers_on_shrink() {
    let src = super::production_source();
    assert!(
        src.contains("self.pane_buffers.truncate(panes.len())"),
        "pane_buffers must be truncated to panes.len() so closed panes \
             don't leak their text buffers"
    );
    assert!(
        src.contains("self.pane_buffer_ids.truncate(panes.len())"),
        "pane_buffer_ids must be truncated with pane_buffers so slot ids \
             cannot outlive their buffers"
    );
    assert!(
        src.contains("self.pane_titlebar_buffers.truncate(panes.len())"),
        "pane_titlebar_buffers must be truncated to panes.len() too"
    );
}

/// Per-pane renderer caches must stay attached to stable pane ids rather
/// than transient visible-pane indices. Otherwise a split reorder or tab
/// move cold-starts line shaping and title caches for panes that did not
/// change. Source-level guard: the behavioral path needs a live `Renderer`.
#[test]
fn pane_buffers_are_keyed_by_stable_pane_id() {
    let src = super::production_source();
    assert!(
        src.contains("pub id: u64,"),
        "PaneView must carry the process-global pane id into the renderer"
    );
    assert!(
        src.contains("pane_buffer_ids: Vec<Option<u64>>"),
        "Renderer must track which pane id occupies each buffer slot"
    );
    assert!(
        src.contains("self.pane_buffer_ids.swap(i, j)")
            && src.contains("self.pane_buffers.swap(i, j)")
            && src.contains("self.pane_line_keys.swap(i, j)"),
        "render_frame must swap all per-pane caches when a pane reappears \
             at a different visible index"
    );
}

/// Startup should parse only the regular bundled face. The bold/italic
/// faces load once styled terminal text appears, then invalidate text caches
/// that may have shaped before the complete family was available.
#[test]
fn bundled_style_faces_load_lazily_and_invalidate_text_caches() {
    let src = super::production_source();
    assert!(
        src.contains("bundled_style_faces_loaded: bool"),
        "Renderer must track whether optional bundled style faces loaded"
    );
    assert!(
        src.contains("load_bundled_font(&mut font_system, kettle_config::font::REGULAR)"),
        "Renderer::new should eagerly load only the regular bundled face"
    );
    let old_loader = concat!("load_font", "_data(");
    assert!(
        src.contains("fontdb::Source::Binary(Arc::new(face))") && !src.contains(old_loader),
        "bundled fonts must be registered from embedded static bytes, not copied into Vecs"
    );
    assert!(
        src.contains("kettle_config::font::BOLD")
            && src.contains("kettle_config::font::ITALIC")
            && src.contains("kettle_config::font::BOLD_ITALIC"),
        "ensure_bundled_style_faces must load every bundled styled face"
    );
    assert!(
        src.contains("self.pane_style_keys.fill(0)")
            && src.contains("self.pane_line_keys.iter_mut().for_each(Vec::clear)")
            && src.contains("self.chrome_style_key = 0"),
        "loading styled faces must invalidate text caches shaped without them"
    );
}

/// An idle repaint (cursor blink, bell decay, focus dim) must NOT re-run
/// the whole-viewport glyphon `prepare`, which re-encodes every visible
/// glyph's vertices. `build_pane` reports whether it reshaped a row;
/// `render_frame_with_status` gates `prepare` (and the paired `atlas.trim`)
/// on that + a chrome-text hash + any open overlay.
#[test]
fn idle_repaint_skips_glyphon_prepare_when_nothing_changed() {
    let src = super::production_source();
    assert!(
        src.contains("let need_prepare = self.text_prepare_dirty")
            && src.contains("if need_prepare {"),
        "render_frame must gate the text prepare on a need_prepare flag"
    );
    assert!(
        src.contains("any_pane_text_changed |= self.build_pane("),
        "render_frame must accumulate whether any pane reshaped a row"
    );
    // atlas.trim must be gated with the prepare: trimming without a
    // following prepare clears the in-use set and lets a later prepare
    // evict glyphs the cached vertices still reference. Every trim sits
    // inside its own `if need_prepare`.
    let trim_idx = src.find("self.atlas.trim();").expect("atlas.trim present");
    let before_trim = &src[trim_idx.saturating_sub(120)..trim_idx];
    assert!(
        before_trim.contains("if need_prepare {"),
        "atlas.trim must be guarded by `if need_prepare`"
    );
}

/// Closing an overlay (settings/palette/search/menu) must force ONE
/// clearing prepare, or the closed panel's cached text vertices linger
/// until the next keystroke. The gate tracks the previous overlay-open state
/// and ORs the open↔closed transition into `need_prepare`.
#[test]
fn overlay_close_forces_a_clearing_prepare() {
    let src = super::production_source();
    assert!(
        src.contains("let overlay_changed = overlay_open != self.last_overlay_open;")
            && src.contains("self.last_overlay_open = overlay_open;"),
        "the gate must compare overlay_open against the previous frame"
    );
    assert!(
        src.contains("|| overlay_changed;"),
        "overlay_changed must feed need_prepare so a close repaints once"
    );
}

/// The inverted glyph under a focused SOLID block cursor is drawn in a
/// dedicated 1-glyph renderer ON TOP of the block, NOT recolored into the
/// pane text buffer. Recoloring in-buffer would dirty the cursor row every
/// blink and force the whole-viewport prepare; the dedicated pass keeps the
/// pane buffer byte-identical across a blink.
#[test]
fn block_cursor_glyph_is_decoupled_from_the_pane_buffer() {
    let src = super::production_source();
    assert!(
        src.contains("cursor_glyph_renderer: TextRenderer")
            && src.contains("pending_cursor_glyph: Option<PendingCursorGlyph>"),
        "Renderer must own a dedicated cursor-glyph renderer + pending slot"
    );
    assert!(
        src.contains("self.cursor_glyph_renderer.prepare(")
            && src.contains("self.cursor_glyph_renderer")
            && src
                .matches(".render(&self.atlas, &self.viewport, &mut pass)")
                .count()
                >= 3,
        "the cursor glyph must be prepared + rendered in its own pass \
             (after the pane + menu text renders)"
    );
}

/// An unfocused pane carrying its own OSC 11
/// background must paint a backdrop over its interior, because the
/// per-cell loop skips default-bg cells (they'd otherwise leak the
/// focused pane's clear color). The backdrop rect must stay INSIDE the
/// border and clear of the titlebar strip so it never overpaints the
/// focus border or per-pane titlebar. The terminal it is given is the
/// pane less its titlebar, as the layout carves it.
#[test]
fn pane_backdrop_rect_stays_inside_border_and_titlebar() {
    use super::{pane_backdrop_rect, pane_grid_origin};
    // 200x150 pane at (10, 20), 2px border, 18px titlebar at the top.
    let pane = (10.0, 20.0, 200.0, 150.0);
    let below_title = (10.0, 38.0, 200.0, 132.0);
    let above_title = (10.0, 20.0, 200.0, 132.0);
    let (x, y, w, h) = pane_backdrop_rect(below_title, 2.0).unwrap();
    // Inside the left/top border, below the top titlebar.
    assert_eq!(x, 12.0);
    assert_eq!(y, 40.0); // 20 + 2 (border) + 18 (titlebar)
    assert_eq!(w, 196.0); // 200 - 2*2
    assert_eq!(h, 128.0); // 150 - 2*2 - 18
    // Backdrop must end at/above the bottom border.
    assert!(y + h <= pane.1 + pane.3 - 2.0 + f32::EPSILON);

    // Titlebar at the bottom: interior shifts to leave the bottom strip.
    let (_, yb, _, hb) = pane_backdrop_rect(above_title, 2.0).unwrap();
    assert_eq!(yb, 22.0); // 20 + 2 (border), no top titlebar
    assert_eq!(hb, 128.0); // 150 - 2*2 - 18 (bottom titlebar)

    // No titlebar (h = 0): interior is the full pane minus border.
    let (_, y0, _, h0) = pane_backdrop_rect(pane, 1.0).unwrap();
    assert_eq!(y0, 21.0);
    assert_eq!(h0, 148.0);

    // Degenerate pane (border ≥ half the size) → None, no quad pushed.
    assert!(pane_backdrop_rect((0.0, 0.0, 3.0, 3.0), 2.0).is_none());

    // The terminal grid follows the title position: top titles move row
    // zero down, bottom titles reserve the same height after the grid.
    assert_eq!(pane_grid_origin(below_title, (6.0, 8.0)), (16.0, 46.0));
    assert_eq!(pane_grid_origin(above_title, (6.0, 8.0)), (16.0, 28.0));
}

/// The background-image cache must (a) key on blur
/// radius so toggling `background-blur` reloads, and (b) be freed when
/// the config moves away from `background-type = image`. Pinned at the
/// source level — exercising it needs a full GPU `Renderer`.
#[test]
fn bg_image_cache_keys_on_blur_and_frees_on_disable() {
    let src = super::production_source();
    assert!(
        src.contains("bg_image_cache: Option<BgImageAnim>") && src.contains("struct BgImageAnim"),
        "bg_image_cache holds a BgImageAnim (path, blur, frames, gaps, started)"
    );
    assert!(
        src.contains("c.path != want") && src.contains("c.blur != blur_radius"),
        "need_reload must compare blur radius, not just the path"
    );
    // A FAILED decode self-heals on a THROTTLE — the
    // reload condition includes `c.frames.is_empty()` gated on
    // `bg_image_retry_at`, so a transient error / in-place fix recovers
    // without re-decoding a broken path every frame.
    assert!(
        src.contains("c.frames.is_empty()") && src.contains("self.bg_image_retry_at"),
        "a failed bg-image decode must retry (empty frames) but throttled \
             via bg_image_retry_at — self-heal without per-frame thrash"
    );
    // On a needed reload the key is stored UNCONDITIONALLY (empty
    // frames on decode failure), and only a successfully-decoded entry
    // renders. Together these stop a stale wallpaper rendering for a broken
    // new path and stop re-decoding the failing file every frame.
    assert!(
        src.contains("self.bg_image_cache = Some(BgImageAnim {"),
        "a failed decode must still cache the (path, blur) key to avoid a \
             per-frame re-decode of the broken path"
    );
    assert!(
        src.contains("filter(|c| !c.frames.is_empty())"),
        "only a successfully-decoded cache entry may render (no stale image)"
    );
    // Animated backgrounds advance on the media clock, gated for proactive
    // waking on focus (battery), and never index out of bounds.
    assert!(
        src.contains("bg_image::bg_current_frame(&c.gaps, c.started.elapsed().as_millis())")
            && src.contains("idx.min(c.frames.len() - 1)"),
        "animated bg must pick the clock frame, bounded to the frame count"
    );
}

/// The wallpaper draws in its OWN pipeline (`bg_imgs`) BEFORE the
/// cell/chrome `quads` pass, so chrome (tab bar/status/titlebar), cell
/// backgrounds (selection/syntax/TUI), and borders composite opaquely on
/// top of it instead of being hidden under an opaque wallpaper, and an
/// animated wallpaper cannot bleed through the tab bar. Pinned at the
/// source level since exercising the pass needs a full GPU `Renderer`.
#[test]
fn wallpaper_draws_behind_quads_in_its_own_pass() {
    let src = super::production_source();
    // A dedicated pipeline exists and is constructed. It draws with the
    // image pipeline the renderer shares, but keeps its own layer.
    assert!(
        src.contains("bg_imgs: imgpipe::ImagePipeline,") && src.contains("let bg_imgs = shared"),
        "the wallpaper must have its own ImagePipeline field + construction"
    );
    // The wallpaper items go to bg_img_items, inline images stay in img_items.
    assert!(
        src.contains("bg_img_items.push(")
            && src.contains("img_items.push(imgpipe::ImageItem::placement("),
        "wallpaper pushes to bg_img_items; inline images to img_items"
    );
    // Draw order: wallpaper (back) -> replacement pane bases -> ordinary
    // quads -> inline images -> text.
    let bg = src
        .find("self.bg_imgs.draw(&mut pass);")
        .expect("bg_imgs draw");
    let pane_bases = src
        .find("self.pane_bases.draw(&mut pass);")
        .expect("pane bases draw");
    let quads = src
        .find("self.quads.draw_hiding(&mut pass, hidden_cursor);")
        .expect("quads draw");
    let inline = src.find("self.imgs.draw(&mut pass);").expect("imgs draw");
    assert!(
        bg < pane_bases && pane_bases < quads && quads < inline,
        "draw order must be wallpaper -> pane bases -> quads -> inline images"
    );
}

/// A renderer compiles each distinct quad and image pipeline once, through
/// `SharedPipelines`; a layer built with a standalone constructor would
/// compile its own shader and pipeline again on the way to the first frame.
#[test]
fn a_renderer_builds_its_quad_and_image_layers_from_shared_pipelines() {
    let src = super::production_source();
    let with_gpu = src
        .split_once("    fn with_gpu(\n")
        .and_then(|(_, rest)| rest.split_once("        Ok(Renderer {"))
        .map(|(body, _)| body)
        .expect("with_gpu builds the renderer");
    assert!(
        with_gpu.contains("let shared = SharedPipelines::new(&device, format);"),
        "with_gpu must compile the shared pipelines once"
    );
    for standalone in ["QuadPipeline::new", "ImagePipeline::new"] {
        assert!(
            !with_gpu.contains(standalone),
            "with_gpu must build its layers from SharedPipelines, not {standalone}"
        );
    }
}

/// `chrome-background` only recolors the chrome with a wallpaper; theme
/// mode + the no-wallpaper case keep `palette[8]`; auto keeps the tab text
/// readable; black/white are fixed.
#[test]
#[allow(
    clippy::field_reassign_with_default,
    reason = "stepwise cfg tweaks read clearer than a full struct literal here"
)]
fn resolve_chrome_bg_modes() {
    use super::{color, resolve_chrome_bg};
    use kettle_config::{BackgroundType, ChromeBackground, Rgb};
    let theme = kettle_config::Theme::default();
    let avg = Rgb::new(90, 60, 120); // a nebula-ish purple
    let mut cfg = kettle_config::Config::default();

    // No wallpaper → always the theme chrome color, whatever the mode.
    cfg.background_type = BackgroundType::Solid;
    cfg.chrome_background = ChromeBackground::Black;
    assert_eq!(resolve_chrome_bg(&cfg, &theme, Some(avg)), theme.palette[8]);

    // Wallpaper + theme (default) → theme chrome color.
    cfg.background_type = BackgroundType::Image;
    cfg.chrome_background = ChromeBackground::Theme;
    assert_eq!(resolve_chrome_bg(&cfg, &theme, Some(avg)), theme.palette[8]);

    // Black / white are fixed.
    cfg.chrome_background = ChromeBackground::Black;
    assert_eq!(
        resolve_chrome_bg(&cfg, &theme, Some(avg)),
        Rgb::new(0, 0, 0)
    );
    cfg.chrome_background = ChromeBackground::White;
    assert_eq!(
        resolve_chrome_bg(&cfg, &theme, Some(avg)),
        Rgb::new(255, 255, 255)
    );

    // Auto with a sampled frame → contrasts with the (theme) tab text ≥3:1.
    cfg.chrome_background = ChromeBackground::Auto;
    let out = resolve_chrome_bg(&cfg, &theme, Some(avg));
    assert!(
        color::contrast_ratio(out, theme.foreground) + 1e-6 >= 3.0,
        "auto chrome must stay readable under the tab text"
    );
    // Auto with no frame sampled yet → falls back to the theme chrome color.
    assert_eq!(resolve_chrome_bg(&cfg, &theme, None), theme.palette[8]);

    // The starfield is a wallpaper too, so chrome modes apply.
    cfg.background_type = BackgroundType::Starfield;
    cfg.chrome_background = ChromeBackground::Black;
    assert_eq!(resolve_chrome_bg(&cfg, &theme, None), Rgb::new(0, 0, 0));
    // Auto over the black starfield (no sampled frame) → a black/near-black
    // bar that still clears the contrast bar, NOT the theme fallback.
    cfg.chrome_background = ChromeBackground::Auto;
    let out = resolve_chrome_bg(&cfg, &theme, None);
    assert!(
        color::contrast_ratio(out, theme.foreground) + 1e-6 >= 3.0,
        "auto chrome over the starfield must stay readable"
    );
    // With a light (default Mocha) foreground, black already passes → black.
    assert_eq!(out, Rgb::new(0, 0, 0));
}

#[test]
fn pinned_adapter_ids_then_name_fallback_are_deterministic() {
    use super::{can_probe_native_backend_directly, has_gpu_pin, pin_match_rank_fields};

    let cfg = kettle_config::Config {
        gpu_vendor_id: 0x10de,
        gpu_device_id: 0x2191,
        gpu_name: "NVIDIA GeForce GTX 1660 Ti".to_string(),
        ..kettle_config::Config::default()
    };
    assert_eq!(
        pin_match_rank_fields(0x10de, 0x2191, "renamed adapter", &cfg),
        0
    );
    assert_eq!(
        pin_match_rank_fields(0x8086, 0x9a49, "NVIDIA GeForce GTX 1660 Ti", &cfg),
        1
    );
    assert_eq!(
        pin_match_rank_fields(0x8086, 0x9a49, "Mobile NVIDIA GeForce GTX 1660 Ti", &cfg),
        2
    );
    assert_eq!(
        pin_match_rank_fields(0x8086, 0x9a49, "Intel Iris Plus", &cfg),
        u8::MAX
    );

    let software = kettle_config::Config {
        gpu_name: "llvmpipe".to_string(),
        ..kettle_config::Config::default()
    };
    assert!(has_gpu_pin(&software));
    assert_eq!(
        pin_match_rank_fields(0, 0, "llvmpipe (LLVM 19.1.7)", &software),
        2,
        "software adapters exposed by the settings picker remain valid pins"
    );
    assert!(
        !can_probe_native_backend_directly(&software),
        "pins require cross-backend resolution before hardware/software partitioning"
    );
}

#[test]
fn backend_policy_is_native_first_and_explicit_when_available() {
    use super::{
        AdapterEscalation, BackendPlatform, adapter_priority_for, backend_attempt_order_for,
        backend_rank_for, effective_backend, should_query_preferred_adapter,
    };
    use kettle_config::GpuPowerPreference;
    use wgpu::{Backend, DeviceType};

    assert!(
        backend_rank_for(BackendPlatform::Windows, Backend::Dx12)
            < backend_rank_for(BackendPlatform::Windows, Backend::Vulkan)
    );
    assert!(
        backend_rank_for(BackendPlatform::MacOs, Backend::Metal)
            < backend_rank_for(BackendPlatform::MacOs, Backend::Vulkan)
    );
    assert!(
        backend_rank_for(BackendPlatform::Other, Backend::Vulkan)
            < backend_rank_for(BackendPlatform::Other, Backend::Gl)
    );

    let available = [Backend::Dx12, Backend::Vulkan];
    assert_eq!(
        effective_backend(Some(Backend::Vulkan), &available),
        Some(Backend::Vulkan),
        "an explicit backend must apply even without a physical GPU pin"
    );
    assert_eq!(
        effective_backend(Some(Backend::Metal), &available),
        None,
        "an unavailable explicit backend must enter the observable fallback path"
    );

    assert_eq!(
        backend_attempt_order_for(BackendPlatform::Windows, None),
        [Backend::Dx12, Backend::Vulkan, Backend::Gl]
    );
    assert_eq!(
        backend_attempt_order_for(BackendPlatform::Windows, Some(Backend::Vulkan)),
        [Backend::Vulkan, Backend::Dx12, Backend::Gl],
        "an explicit backend is tried once, then native fallback order"
    );
    assert!(
        should_query_preferred_adapter(AdapterEscalation::Preferred, false),
        "cross-backend initial selection needs wgpu's physical-adapter preference"
    );
    assert!(!should_query_preferred_adapter(
        AdapterEscalation::Preferred,
        true
    ));
    assert!(!should_query_preferred_adapter(
        AdapterEscalation::AlternateBackend,
        false
    ));

    assert!(
        adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Vulkan,
            DeviceType::DiscreteGpu,
            GpuPowerPreference::High,
            false,
        ) < adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Dx12,
            DeviceType::IntegratedGpu,
            GpuPowerPreference::High,
            false,
        ),
        "high preference must choose a discrete GPU across backend ranks"
    );
    assert!(
        adapter_priority_for(
            BackendPlatform::Other,
            Backend::Gl,
            DeviceType::IntegratedGpu,
            GpuPowerPreference::Low,
            false,
        ) < adapter_priority_for(
            BackendPlatform::Other,
            Backend::Vulkan,
            DeviceType::DiscreteGpu,
            GpuPowerPreference::Low,
            false,
        ),
        "low preference must choose an integrated GPU across backend ranks"
    );
    assert!(
        adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Dx12,
            DeviceType::IntegratedGpu,
            GpuPowerPreference::Auto,
            false,
        ) < adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Vulkan,
            DeviceType::DiscreteGpu,
            GpuPowerPreference::Auto,
            false,
        ),
        "Auto remains native-backend first"
    );
    assert!(
        adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Dx12,
            DeviceType::IntegratedGpu,
            GpuPowerPreference::Auto,
            true,
        ) < adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Dx12,
            DeviceType::DiscreteGpu,
            GpuPowerPreference::Auto,
            false,
        ),
        "a stale pin falling back to Auto must preserve wgpu's preferred physical GPU"
    );
    assert!(
        adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Vulkan,
            DeviceType::DiscreteGpu,
            GpuPowerPreference::High,
            true,
        ) < adapter_priority_for(
            BackendPlatform::Windows,
            Backend::Dx12,
            DeviceType::DiscreteGpu,
            GpuPowerPreference::High,
            false,
        ),
        "equal-class low/high ties must preserve wgpu's physical GPU before backend rank"
    );
}

/// Every pane owns a default-background layer. Wallpaper configurations
/// source-over it after the wallpaper; solid/transparent configurations
/// route it through the replacement pipeline so OSC 11 panes cannot
/// compound each other's alpha.
#[test]
fn pane_default_backdrop_is_wired_into_both_compositing_paths() {
    let src = super::production_source();
    let wallpaper_branch = ["if background_has_", "wallpaper(cfg) {"].concat();
    let replacement_branch = ["pane_", "bases.push(backdrop)"].concat();
    assert!(
        src.contains(&wallpaper_branch) && src.contains(&replacement_branch),
        "build_pane must route every pane backdrop through wallpaper-over or replacement semantics"
    );
    assert!(
        src.contains("pane_backdrop_rect(pv.terminal, bw)"),
        "the backdrop must use the border/titlebar-aware geometry helper"
    );
}

/// Drift guard. The overlay text-buffer pools grow with `while len < N`
/// exactly like the pane pools and must be truncated back down too, or each
/// ratchets to its session high-water mark (peak menu rows / hint labels /
/// tab count) holding idle shaped-glyph buffers. Pin the truncate calls at
/// the source level (a behavioral test would need a full GPU `Renderer`).
#[test]
fn render_frame_truncates_overlay_buffer_pools_on_shrink() {
    let src = super::production_source();
    for (call, what) in [
        (
            "self.tab_buffers.truncate(tabbar.segments.len())",
            "tab_buffers",
        ),
        (
            "self.context_menu_buffers.truncate(menu.rows.len())",
            "context_menu_buffers",
        ),
        (
            "self.hint_buffers.truncate(overlay.hint_labels.len())",
            "hint_buffers",
        ),
        (
            "self.settings_buffers.truncate(lines.len())",
            "settings_buffers",
        ),
    ] {
        assert!(
            src.contains(call),
            "{what} must be truncated each frame so the pool can't grow \
                 unbounded across overlay open/close cycles (missing `{call}`)"
        );
    }
}

/// Drift guard. `build_pane`'s per-cell style-run scratch must be POOLED on
/// `self` (taken + returned) and reuse each run's `String` buffer by index
/// (clear + refill), not `Vec::new()` + `to_string()` per frame. Otherwise
/// a busy colored pane mints dozens to hundreds of `String` allocations on
/// the 60 fps hot path. A behavioral test needs a full GPU `Renderer`; pin
/// the pattern at the source level.
#[test]
fn build_pane_pools_the_span_scratch() {
    let src = super::production_source();
    assert!(
        src.contains("std::mem::take(&mut self.span_scratch)"),
        "span scratch must be taken from the self-pool, not allocated fresh"
    );
    assert!(
        src.contains("self.span_scratch = spans;"),
        "span scratch must be returned to the pool for the next frame"
    );
    // The per-frame quad list is pooled the same way.
    assert!(
        src.contains("std::mem::take(&mut self.quad_scratch)"),
        "the frame quad Vec must be taken from the pool, not allocated fresh"
    );
    assert!(
        src.contains("self.quad_scratch = quads;"),
        "the frame quad Vec must be returned to the pool after upload"
    );
    assert!(
        src.contains("slot.0.clear();"),
        "per-run String slots must be cleared + reused, not freshly allocated"
    );
}

/// Drift guard. Tab text must use the UI-computed title lane as its shaping
/// budget and drawing bounds. The lane derives from the full rendered
/// segment and excludes only fixed controls such as the close button; it
/// must not regress to compact visual/pressed affordance rects.
#[test]
fn tab_text_uses_full_title_lane_budget() {
    let src = super::production_source();
    assert!(
        src.contains("let (_, _, title_w, title_h) = s.title_rect;"),
        "tab label shaping must budget from the full title lane"
    );
    assert!(
        src.contains("let (tx, ty_px, tw, th) = s.title_rect;"),
        "tab label drawing bounds must use the full title lane"
    );
    let visual_token = ["visual", "_rect"].concat();
    let pressed_token = ["pressed", "_rect"].concat();
    assert!(
        !src.contains(&visual_token) && !src.contains(&pressed_token),
        "tab text must not depend on compact visual/pressed rects"
    );
}

/// Drift guard. Image-placement draw must keep the `quota > 1` fast-path
/// so a pane admitted zero or one visible image doesn't pay a per-frame
/// `Vec` alloc + sort, AND must still z-sort the 2+ case so higher-z images
/// land on top. A behavioral test needs a full GPU `Renderer`; pin both at
/// the source level (same shape as the buffer-truncate guards above).
#[test]
fn image_placement_draw_keeps_len_fastpath_and_z_sort() {
    let src = super::production_source();
    assert!(
        src.contains("if quota > 1"),
        "image placement draw must fast-path the 0–1 case to skip the \
             per-frame Vec alloc + sort"
    );
    assert!(
        src.contains("ordered.sort_by_key(|placement| placement.z)"),
        "2+ image placements must still be z-sorted so higher z lands on top"
    );
}

/// Drift guard. `PaneView` must *borrow* its per-frame
/// images/title/group_name from the frame's `metas` collection (exactly as
/// `snap` borrows the pooled `PaneSnapshot`), not own clones. Otherwise
/// `redraw()` double-clones every visible pane's image `Vec` + title
/// `String` every frame. A behavioral test needs the full app frame loop;
/// pin the borrowed field types at the source.
///
/// The next test, `font_family_is_arc_str_not_string`, is also a drift
/// guard. `render_frame_with_status` clones `self.font_family` every frame
/// to hold an owned handle while `&mut self.font_system` is borrowed across
/// ~20 `Family::Name(&family)` reads. The field must stay `Arc<str>` so
/// that clone is a refcount bump, not a per-frame heap alloc + memcpy at
/// 60fps. A behavioral test needs a GPU `Renderer`; pin the field type at
/// the source level.
#[test]
fn paneview_borrows_per_frame_data() {
    let src = super::production_source();
    assert!(
        src.contains("pub images: &'a [kettle_core::Placement],"),
        "PaneView.images must borrow the frame's image Vec, not clone it"
    );
    assert!(
        src.contains("pub title: &'a str,"),
        "PaneView.title must borrow, not own a cloned String"
    );
    assert!(
        src.contains("pub group_name: Option<&'a str>,"),
        "PaneView.group_name must borrow, not own a cloned String"
    );
}

#[test]
fn font_family_is_arc_str_not_string() {
    let src = super::production_source();
    assert!(
        src.contains("font_family: Arc<str>,"),
        "Renderer.font_family must be Arc<str> so the per-frame clone is a \
             refcount bump, not a heap alloc"
    );
    // Build the needle at runtime so this very assertion isn't a false
    // positive (the literal would otherwise appear in `src`).
    let reverted = format!("font_family: {}", "String,");
    assert!(
        !src.contains(&reverted),
        "Renderer.font_family must not revert to String (per-frame alloc on \
             the 60fps render path)"
    );
}
