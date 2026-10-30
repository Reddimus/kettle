use super::{
    Attrs, Cache, Config, ContextMenu, ContextMenuRow, Family, FontSystem, GColor, GlyphClip,
    GlyphInstance, GlyphPipeline, Metrics, OUTLINE_BOTTOM_LEFT, OUTLINE_BOTTOM_RIGHT,
    OutlinePipeline, QuadInstance, QuadPipeline, Resolution, Result, Rgb, Shaping, SwashCache,
    Text, TextArea, TextAtlas, TextBounds, TextBuffer, TextRenderer, TextRendererMode, Translator,
    Viewport, Wrap, anyhow, bell_flash_alpha, cap_axis_cells, chrome, clamp_font_size, color,
    composed_bg_alpha, context_menu_panel_width, emit_cell_locked_glyphs, gc, imgpipe,
    live_device_limits, load_bundled_font, measure_cell, menu_chrome_quads, pane_outline, rect,
    resolve_headless_adapter, surface_clear_color, unpremultiply_rgba8,
};

pub(crate) fn screenshot_text_buffer(
    fonts: &mut FontSystem,
    metrics: Metrics,
    size: (f32, f32),
) -> TextBuffer {
    let mut buffer = TextBuffer::new(fonts, metrics);
    buffer.set_size(Some(size.0), Some(size.1));
    buffer.set_wrap(Wrap::None);
    buffer
}

/// The current crate version in the offscreen demo's compile line.
pub(crate) const SCREENSHOT_DEMO_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Filled and empty cells for a bounded, 32-cell offscreen demo meter.
pub(crate) fn screenshot_meter(value: u32, total: u32) -> (String, String) {
    const CELLS: u64 = 32;
    let filled = if total == 0 {
        0
    } else {
        ((u64::from(value.min(total)) * CELLS + u64::from(total) / 2) / u64::from(total)) as usize
    };
    ("|".repeat(filled), "-".repeat(CELLS as usize - filled))
}

/// Which synthetic scene to render in [`capture_png_with`].
///
/// Every scene is rendered **offscreen** (no window/surface) through kettle's
/// *real* GPU text + quad path (bundled Nerd Font, `glyphon` shaping, the
/// `QuadPipeline`, the active theme). The base frame is a scripted two-pane
/// vertical split under the tab bar (active tab, per-tab `✕`, trailing `+`),
/// with a themed shell session on the left and a monitor-style readout on the
/// right. This synthetic fixture exercises the offscreen text and quad pipelines;
/// it does not capture native window chrome.
///
/// Render the synthetic showcase scene used by offscreen screenshot checks.
/// `kettle --screenshot <out.png>` renders `Default`, the base frame with no overlay.
/// `ContextMenu` adds a synthetic right-click context menu over the pane so the
/// menu's render path can be visually verified without opening the windowed
/// app; it is visible only via the `kettle --screenshot-menu PATH` CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugScene {
    /// Existing `--screenshot` behavior.
    #[default]
    Default,
    /// Render with a synthetic right-click context menu open over the
    /// pane. The menu carries a fixed eight-row subset of the live menu
    /// (Copy, Paste, sep, Split Right, Split Down, Close Pane, sep, New
    /// Tab) with the first enabled row highlighted, anchored at a
    /// fixed position so the resulting PNG is byte-deterministic
    /// across runs.
    ContextMenu,
    /// Render an active, partially-scrolled compact overlay scrollbar. Used by
    /// visual regression coverage; it is not exposed as a public CLI mode.
    Scrollbar,
    /// Render the pane at the peak of a visual-bell flash through the same
    /// `bell_flash_alpha` math and quad pipeline the live frame uses, so a
    /// pixel test can measure the shipped lightness step. Not a CLI mode.
    BellFlash,
}

/// Back-compat wrapper for external `capture_png` callers. Always renders
/// [`DebugScene::Default`].
pub fn capture_png(
    cfg: &Config,
    cols: u32,
    rows: u32,
    out: &std::path::Path,
) -> Result<(u32, u32)> {
    capture_png_with(cfg, cols, rows, out, DebugScene::Default)
}

/// Render a screenshot PNG; returns the **actual** (cols, rows) used after
/// the texture-limit cap so the CLI can report what was rendered
/// rather than what was requested (which can differ when the user asks for
/// more cells than the wgpu 8192-px-per-side limit allows at the active
/// font size).
pub fn capture_png_with(
    cfg: &Config,
    cols: u32,
    rows: u32,
    out: &std::path::Path,
    scene: DebugScene,
) -> Result<(u32, u32)> {
    capture_png_with_annotation(cfg, cols, rows, out, scene, None)
}

/// Offscreen capture with an optional caption on a translucent bottom strip.
/// `None` matches [`capture_png_with`]; `--annotate` supplies text from the CLI.
/// This synthetic fixture does not include native window chrome.
pub fn capture_png_with_annotation(
    cfg: &Config,
    cols: u32,
    rows: u32,
    out: &std::path::Path,
    scene: DebugScene,
    annotation: Option<&str>,
) -> Result<(u32, u32)> {
    pollster::block_on(async {
        let (_instance, adapter) = resolve_headless_adapter(cfg, "capture_png").await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-screenshot"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("device: {e:?}"))?;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;

        let mut font_system = FontSystem::new();
        for face in kettle_config::font::all() {
            load_bundled_font(&mut font_system, face);
        }
        let swash_cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &swash_cache, format);
        let viewport = Viewport::new(&device, &swash_cache);
        let mut text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let mut swash = SwashCache::new();
        let mut quads = QuadPipeline::new(&device, format);
        // Second pipelines for the debug-scene overlays (context menu, bell
        // flash, scrollbar). Allocated unconditionally (small, cheap) so the
        // render pass can always call `draw` / `render` on them; empty uploads
        // are a no-op. Mirrors the live `Renderer`.
        let mut menu_quads_pipe = QuadPipeline::new(&device, format);
        let mut menu_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let theme = &cfg.theme;
        let fam = cfg.font_family.clone();
        // Same clamp Renderer::new and set_font_size apply.
        // capture_png builds its OWN device + texture chain rather than
        // going through Renderer::new, so the bound has to be repeated
        // here — without it, a `font-size = 500` config + `--screenshot
        // --cols 200` config still walks past the wgpu 8192-px-per-side
        // texture limit and the PNG generator errors out.
        let font_size = clamp_font_size(cfg.font_size);
        let metrics = Metrics::new(font_size, font_size * 1.25);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let (cw, ch) = measure_cell(&mut font_system, &mut measure, &fam, metrics);

        let pad = cfg.padding_x.max(8.0);
        let tab_h = ch + 12.0;
        // wgpu's max-texture-per-side is 8192 on every backend / GPU
        // class we care about. The CLI already clamps `--cols ≤ 400` /
        // `--rows ≤ 200`, but at a 72pt clamped font size the
        // cell can be ~35×90px, so 200 rows × 90px = 18000px tall
        // exceeds the limit even without an enormous font config. Cap
        // each side dynamically against the actual cell size so the
        // user never sees a panic about texture dims for any cli /
        // config combination. `cap_axis_cells` is pure (max-px ÷ cell-
        // px minus chrome) so the same arithmetic is unit-tested. Floor
        // at 1 so a degenerate clamp doesn't yield zero-cell PNGs.
        let cols = cap_axis_cells(cols, cw, pad * 2.0);
        let rows = cap_axis_cells(rows, ch, pad * 2.0 + tab_h);
        let body_w = cols as f32 * cw;
        let body_h = rows as f32 * ch;
        let w = (pad * 2.0 + body_w).ceil() as u32;
        let h = (tab_h + body_h + pad * 2.0).ceil() as u32;
        let texture_limit = device.limits().max_texture_dimension_2d.min(8192);
        if w > texture_limit || h > texture_limit {
            let tr = Translator::new(cfg.language.resolve(kettle_i18n::os_locale));
            return Err(anyhow!(tr.text(Text::ScreenshotDimensionsTooLarge)));
        }
        let (wf, hf) = (w as f32, h as f32);
        let split_x = (wf / 2.0).round();

        let base = Attrs::new().family(Family::Name(&fam));
        let mut q: Vec<QuadInstance> = Vec::new();

        // --- Tab bar (active accent + per-tab ✕ + trailing +).
        //
        // Tab labels are defined once and reused for BOTH the chrome geometry
        // and the text buffer below, so the highlighted segment + separators
        // always line up with the glyphs.
        let tab_text_left = 0.0_f32;
        // This deterministic fixture has synthetic chrome. The tab1/tab2
        // boundary lands on the split centre (`split_x`), so the vertical split
        // divider visually continues it. Only the last tab yields room for the
        // trailing `+` button. Monospace, so a label's pixel width is its char
        // count × `cw`.
        let tabplus_label = "  +  ";
        let plus_w = tabplus_label.chars().count() as f32 * cw;
        // Tab 1 spans [0, split_x]; tab 2 spans [split_x, wf - plus_w].
        let w0 = (split_x - tab_text_left).max(cw).floor();
        let w1 = (wf - plus_w - split_x).max(cw).floor();
        let fill_tab = |title: &str, cols: usize| -> String {
            let head = format!(" {title}");
            let tail = "✕ ";
            let used = head.chars().count() + tail.chars().count();
            let gap = cols.saturating_sub(used);
            format!("{head}{}{tail}", " ".repeat(gap))
        };
        let tab0_label = fill_tab("1: zsh", (w0 / cw).max(1.0) as usize);
        let tab1_label = fill_tab("2: ssh prod", (w1 / cw).max(1.0) as usize);
        q.push(rect(0.0, 0.0, wf, tab_h, theme.palette[8], 1.0));
        // Active tab 0: themed background and a 2px left accent bar.
        // Cascade through the resolved accent so peacock + the theme's
        // signature accent show in --screenshot too.
        let screenshot_accent = cfg.resolved_accent(theme);
        q.push(rect(tab_text_left, 0.0, w0, tab_h, theme.background, 1.0));
        q.push(rect(tab_text_left, 0.0, 2.0, tab_h, screenshot_accent, 1.0));
        // Inactive tab 1: a muted box (slightly lower opacity than the active tab).
        q.push(rect(
            tab_text_left + w0,
            0.0,
            w1,
            tab_h,
            theme.background,
            0.9,
        ));
        // 1px separators at each segment's right edge.
        q.push(rect(
            tab_text_left + w0 - 1.0,
            0.0,
            1.0,
            tab_h,
            theme.background,
            0.5,
        ));
        q.push(rect(
            tab_text_left + w0 + w1 - 1.0,
            0.0,
            1.0,
            tab_h,
            theme.background,
            0.5,
        ));

        // --- Two-pane vertical split with focus border on the left pane.
        q.push(rect(
            split_x - 1.0,
            tab_h,
            2.0,
            hf - tab_h,
            theme.palette[8],
            1.0,
        ));
        // focused_split_color → resolved accent (explicit → Peacock →
        // theme signature), same order as the live renderer.
        let foc = cfg
            .focused_split_color
            .unwrap_or_else(|| cfg.resolved_accent(theme));
        let ly = tab_h;
        let lh = hf - tab_h;
        q.push(rect(0.0, ly, split_x, 1.0, foc, 1.0));
        q.push(rect(0.0, ly + lh - 1.0, split_x, 1.0, foc, 1.0));
        q.push(rect(0.0, ly, 1.0, lh, foc, 1.0));
        q.push(rect(split_x - 1.0, ly, 1.0, lh, foc, 1.0));

        let demo_user = "demo@kettle";
        let demo_directory = "~/projects/demo";
        let cur_row = 6.0;
        let cur_col = (demo_user.len() + demo_directory.len() + 3) as f32;
        q.push(rect(
            pad + cur_col * cw,
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

        let mut tab_buf = screenshot_text_buffer(&mut font_system, metrics, (wf, tab_h));
        tab_buf.set_rich_text(
            [
                (tab0_label.as_str(), fg.clone()),
                (tab1_label.as_str(), dim.clone()),
                (tabplus_label, grn.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        tab_buf.shape_until_scroll(&mut font_system, false);

        // The demo `cargo test` compile line carries the live crate
        // version — never a hardcoded literal — so the hero /
        // showcase screenshots track the real product version forever.
        let compile_line = format!("kettle v{SCREENSHOT_DEMO_VERSION}\n");
        let mut left = screenshot_text_buffer(&mut font_system, metrics, (split_x - pad, lh));
        left.set_rich_text(
            [
                (demo_user, grn.clone()),
                (":", fg.clone()),
                (demo_directory, blu.clone()),
                // Keep this command short enough that it never wraps even in the
                // narrow showcase split (~50-col left pane) — a wrap would push
                // every line down one and strand the hardcoded `cur_row` cursor
                // on a blank line.
                ("$ cargo test\n", fg.clone()),
                ("   Compiling ", dim.clone()),
                (compile_line.as_str(), dim.clone()),
                ("    Finished ", grn.clone()),
                ("`test` profile [optimized]\n", fg.clone()),
                ("     Running ", grn.clone()),
                ("unittests\n", fg.clone()),
                ("test result: ", fg.clone()),
                ("ok", grn.clone()),
                (". workspace checks passed\n\n", fg.clone()),
                (demo_user, grn.clone()),
                (":", fg.clone()),
                (demo_directory, blu.clone()),
                ("$ ", fg.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        left.shape_until_scroll(&mut font_system, false);

        let (cpu_used, cpu_free) = screenshot_meter(37, 100);
        let (memory_used, memory_free) = screenshot_meter(51, 320);
        let mut right = screenshot_text_buffer(&mut font_system, metrics, (wf - split_x - pad, lh));
        right.set_rich_text(
            [
                ("  kettle — cross-platform terminal\n\n", mag.clone()),
                ("CPU ", fg.clone()),
                (cpu_used.as_str(), grn.clone()),
                (cpu_free.as_str(), dim.clone()),
                (" 37%\n", fg.clone()),
                ("MEM ", fg.clone()),
                (memory_used.as_str(), blu.clone()),
                (memory_free.as_str(), dim.clone()),
                (" 5.1/32GiB\n", fg.clone()),
                ("NET ", fg.clone()),
                ("↓ 1.2 MB/s  ↑ 88 KB/s\n\n", dim.clone()),
                ("  GPU: ", fg.clone()),
                ("wgpu", grn.clone()),
                (" · font: ", fg.clone()),
                ("JetBrainsMono NF", blu.clone()),
                ("\n  theme: ", fg.clone()),
                (cfg.theme_name.as_str(), yel.clone()),
                ("\n  splits · tabs · search · settings ✓\n", dim.clone()),
                ("  keybinds · sixel · kitty · OSC 8 ✓", dim.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        right.shape_until_scroll(&mut font_system, false);

        // When `annotation` is Some, paint a translucent caption strip (at
        // least 24px tall) across the bottom of the image and render the text
        // in theme.foreground.
        let mut annotate_buf = TextBuffer::new(&mut font_system, metrics);
        let annotate_h = (ch + 8.0).max(24.0);
        if let Some(text) = annotation {
            annotate_buf.set_size(Some(wf - 16.0), Some(annotate_h));
            annotate_buf.set_text(text, &base, Shaping::Advanced, None);
            annotate_buf.shape_until_scroll(&mut font_system, false);
            // Translucent panel + one-px top border.
            q.push(rect(
                0.0,
                hf - annotate_h,
                wf,
                annotate_h,
                theme.background,
                0.92,
            ));
            q.push(rect(0.0, hf - annotate_h, wf, 1.0, theme.palette[8], 1.0));
        }

        // Offscreen pane text follows the configured live text renderer.
        // Shared origins keep Grid and Legacy columns aligned; annotation
        // and menu chrome use glyphon.
        let grid = cfg.text_renderer == TextRendererMode::Grid;
        // (left, top, clip-rect) for each pane body buffer, shared between the
        // glyphon `TextArea`s (Legacy) and the GlyphPipeline emit (Grid) so the
        // two paths can never disagree on placement.
        let tab_origin = (8.0_f32, 6.0_f32);
        let tab_clip = [0.0, 0.0, wf, tab_h];
        let left_origin = (pad, ly + pad);
        let left_clip = [0.0, ly, split_x, hf - ly];
        let right_origin = (split_x + pad, ly + pad);
        let right_clip = [split_x, ly, wf - split_x, hf - ly];

        let mut areas: Vec<TextArea> = Vec::new();
        if !grid {
            areas.push(TextArea {
                buffer: &tab_buf,
                left: tab_origin.0,
                top: tab_origin.1,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: w as i32,
                    bottom: tab_h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            });
            areas.push(TextArea {
                buffer: &left,
                left: left_origin.0,
                top: left_origin.1,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: ly as i32,
                    right: split_x as i32,
                    bottom: h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            });
            areas.push(TextArea {
                buffer: &right,
                left: right_origin.0,
                top: right_origin.1,
                scale: 1.0,
                bounds: TextBounds {
                    left: split_x as i32,
                    top: ly as i32,
                    right: w as i32,
                    bottom: h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            });
        }
        // Append the annotation TextArea if set. Bottom-
        // anchored — left margin 8 px, text baseline ~4 px above
        // the bottom edge so the descenders don't clip.
        if annotation.is_some() {
            areas.push(TextArea {
                buffer: &annotate_buf,
                left: 8.0,
                top: hf - annotate_h + 4.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: (hf - annotate_h) as i32,
                    right: w as i32,
                    bottom: h as i32,
                },
                default_color: gc(theme.foreground),
                custom_glyphs: &[],
            });
        }

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

        // Grid (default `text-renderer`): emit the pane body buffers through the
        // cell-locked GlyphPipeline — the exact renderer the shipped product
        // uses — so the README imagery isn't a legacy-glyphon misrepresentation.
        // Each buffer's appended instance range gets its own `GlyphClip` so the
        // scissor clips text to its pane, mirroring the live `emit_pane_glyphs`.
        let mut grid_glyphs =
            GlyphPipeline::new_with_budget(&device, format, kettle_core::GraphicsBudget::default())
                .ok_or_else(|| {
                    anyhow!("GPU graphics budget exhausted while capturing screenshot")
                })?;
        let mut grid_instances: Vec<GlyphInstance> = Vec::new();
        let mut grid_clips: Vec<GlyphClip> = Vec::new();
        if grid {
            let mut char_starts: Vec<u32> = Vec::new();
            let default_color = gc(theme.foreground);
            for (buf_ref, origin, clip) in [
                (&tab_buf, tab_origin, tab_clip),
                (&left, left_origin, left_clip),
                (&right, right_origin, right_clip),
            ] {
                let start = grid_instances.len() as u32;
                emit_cell_locked_glyphs(
                    &mut grid_instances,
                    buf_ref,
                    origin,
                    Some(cw),
                    default_color,
                    &mut grid_glyphs,
                    &mut swash,
                    &mut font_system,
                    &device,
                    &queue,
                    &mut char_starts,
                );
                grid_clips.push(GlyphClip {
                    rect: clip,
                    start,
                    count: grid_instances.len() as u32 - start,
                });
            }
            grid_glyphs.upload(&device, &queue, [wf, hf], &grid_instances);
        }

        // `DebugScene::ContextMenu`: build a synthetic context menu at
        // a fixed anchor (so the resulting PNG is byte-deterministic)
        // with a fixed eight-row subset of the live `App::context_menu_items`.
        // Quads go through the shared `menu_chrome_quads`
        // helper; text areas are built inline here because the
        // capture-path text-buffer pool is local to this function.
        let mut menu_text_buffers: Vec<TextBuffer> = Vec::new();
        let mut menu_q: Vec<QuadInstance> = Vec::new();
        let mut menu_areas: Vec<TextArea> = Vec::new();
        if scene == DebugScene::ContextMenu {
            // Copy is *disabled* in the synthetic scene because there is no
            // selection (matches the more-common state a user opens the menu
            // in). Highlight starts on Paste (idx 1), the first enabled
            // non-separator row.
            // English catalogue text: the capture must stay byte-identical.
            let tr = Translator::default();
            let rows = vec![
                ContextMenuRow {
                    label: tr.text(Text::MenuCopy).into(),
                    separator: false,
                    enabled: false,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: tr.text(Text::MenuPaste).into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: String::new(),
                    separator: true,
                    enabled: false,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: tr.text(Text::MenuSplitRight).into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: tr.text(Text::MenuSplitDown).into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: tr.text(Text::MenuClosePane).into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: String::new(),
                    separator: true,
                    enabled: false,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: tr.text(Text::MenuNewTab).into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
            ];
            let menu = ContextMenu {
                // Anchor at a fixed offset from the top-left chrome.
                // Keeps the resulting PNG deterministic regardless of
                // window dimensions (--cols / --rows from CLI).
                anchor: (pad + cw * 2.0, tab_h + pad + ch * 2.0),
                rows,
                highlight: 1,
                // Deterministic screenshot fixture stays unscrolled +
                // unclamped (the harness paints all 8 rows in their
                // natural height).
                scroll_offset: 0,
                panel_w_clamped: 0.0,
                panel_h_clamped: 0.0,
                a11y_label: None,
            };
            menu_q.extend(menu_chrome_quads(
                &menu,
                theme,
                cfg.resolved_accent(theme),
                cw,
                ch,
                &mut chrome::Colors::default(),
                format.is_srgb(),
            ));

            // Text areas — one TextBuffer per non-separator row.
            // Positioning mirrors the live renderer's menu block.
            let panel_w = context_menu_panel_width(&menu, cw);
            let row_h = ch + 12.0;
            let sep_h = 8.0_f32;
            let (ax, ay) = menu.anchor;
            // Allocate buffers first (one per row, separators get an
            // empty placeholder so indices align with `menu.rows`).
            for row in &menu.rows {
                let mut buf = TextBuffer::new(&mut font_system, metrics);
                if !row.separator {
                    buf.set_metrics(metrics);
                    buf.set_size(Some(panel_w), Some(row_h));
                    buf.set_text(
                        &row.label,
                        &Attrs::new().family(Family::Name(&fam)),
                        Shaping::Advanced,
                        None,
                    );
                    buf.shape_until_scroll(&mut font_system, false);
                }
                menu_text_buffers.push(buf);
            }
            // Second pass: build TextAreas over the freshly-shaped buffers.
            // Every buffer was pushed above, so no push mutates
            // `menu_text_buffers` while `menu_areas` borrows from it.
            let mut row_y = ay;
            for (i, row) in menu.rows.iter().enumerate() {
                if row.separator {
                    row_y += sep_h;
                    continue;
                }
                let background = if i == menu.highlight && row.enabled {
                    color::over(
                        theme.background,
                        cfg.resolved_accent(theme),
                        0.18,
                        format.is_srgb(),
                    )
                } else {
                    theme.background
                };
                let mut colors = chrome::Colors::default();
                let fg = if row.enabled {
                    colors.text(theme.foreground, background)
                } else {
                    colors.muted(theme.foreground, background)
                };
                menu_areas.push(TextArea {
                    buffer: &menu_text_buffers[i],
                    left: ax + 16.0,
                    top: row_y + 6.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: ax as i32,
                        top: row_y as i32,
                        right: (ax + panel_w) as i32,
                        bottom: (row_y + row_h) as i32,
                    },
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
                row_y += row_h;
            }
        }
        if scene == DebugScene::BellFlash {
            // The live frame washes the ringing pane's rect under the text;
            // the fixture's pane is the whole body below the tab bar.
            let alpha = bell_flash_alpha(theme, cfg, 1.0);
            if alpha > 0.0 {
                menu_q.push(rect(0.0, tab_h, wf, hf - tab_h, theme.foreground, alpha));
            }
        }
        if scene == DebugScene::Scrollbar {
            let track_y = tab_h + pad;
            let track_h = body_h;
            let bar_w = cfg.scrollbar_width.clamp(2.0, 40.0);
            let bar_x = wf - pad - bar_w - 2.0;
            let track_w = 1.5_f32.clamp(1.0, bar_w);
            menu_q.push(rect(
                bar_x + (bar_w - track_w) / 2.0,
                track_y,
                track_w,
                track_h,
                theme.foreground,
                0.14,
            ));
            if let Some((thumb_y, thumb_h)) =
                kettle_core::scrollbar::thumb_with_min(rows as usize, 400, 200, track_h, 24.0)
            {
                menu_q.push(rect(
                    bar_x,
                    track_y + thumb_y,
                    bar_w,
                    thumb_h,
                    theme.foreground,
                    0.82,
                ));
            }
        }
        menu_quads_pipe.upload(&device, &queue, [wf, hf], &menu_q);
        menu_text_renderer.prepare(
            &device,
            &queue,
            &mut font_system,
            &mut atlas,
            &vp,
            menu_areas,
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
                        // Route through composed_bg_alpha so the screenshot
                        // honors background-type, background-darkness, and
                        // cfg.background_opacity the way the live window does.
                        // A hardcoded `a: 1.0` would turn `kettle --screenshot
                        // --config /transparent.conf` into an opaque PNG.
                        //
                        // This target is ours end to end, so it clears
                        // premultiplied to match what the pipelines drawing
                        // over it expect, and `unpremultiply_rgba8` converts
                        // back to the straight alpha PNG stores before the
                        // pixels are saved.
                        load: wgpu::LoadOp::Clear(surface_clear_color(
                            bg,
                            composed_bg_alpha(cfg),
                            wgpu::CompositeAlphaMode::PreMultiplied,
                        )),
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
            // Grid mode draws pane body text through the cell-locked pipeline
            // (the `grid_clips` scissor each pane); the `text_renderer` then only
            // carries the annotation chrome. Legacy mode left all pane text in
            // `text_renderer` and `grid_clips` is empty (no-op draw). Same pass
            // order as the live `Renderer::render_frame`: quads, then glyphs.
            grid_glyphs.draw(&mut pass, &grid_clips, [w, h]);
            text_renderer.render(&atlas, &vp, &mut pass)?;
            // Menu chrome + menu text, same pass order as
            // the live `Renderer::render_frame`. Cheap no-ops for the
            // `DebugScene::Default` path because both uploads are
            // empty.
            menu_quads_pipe.draw(&mut pass);
            menu_text_renderer.render(&atlas, &vp, &mut pass)?;
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

        let data = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("capture mapped range failed: {e:?}"))?;
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();

        // The pass composited premultiplied; PNG stores straight alpha. Ask
        // the format which space the multiply happened in rather than assuming
        // the one this function currently picks — the two have to stay in
        // agreement, and a hardcoded answer would go quietly wrong if the
        // capture target ever stopped being sRGB.
        unpremultiply_rgba8(&mut pixels, format.is_srgb());

        let img = image::RgbaImage::from_raw(w, h, pixels)
            .ok_or_else(|| anyhow!("image buffer size mismatch"))?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        img.save(out)
            .map_err(|e| anyhow!("write {}: {e}", out.display()))?;
        Ok((cols, rows))
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
    offscreen_selftest_with_config(&Config::default())
}

/// Config-aware self-test entry point for embedders and focused diagnostics.
/// [`offscreen_selftest`] supplies `Config::default()`; the repository's GPU
/// unit test passes a config derived from it, so the gate never depends on
/// developer configuration.
pub fn offscreen_selftest_with_config(cfg: &Config) -> anyhow::Result<bool> {
    pollster::block_on(async {
        let (_instance, adapter) = match resolve_headless_adapter(cfg, "offscreen_selftest").await {
            Ok(a) => a,
            Err(_) => return Ok(false),
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-selftest"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("device: {e:?}"))?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        // Pipeline construction compiles our WGSL on the active backend —
        // this is the part that historically breaks per-platform.
        let mut quads = QuadPipeline::new(&device, format);
        let mut pane_outlines = OutlinePipeline::new(&device, format);
        let Some(mut imgs) = imgpipe::ImagePipeline::new(&device, format) else {
            return Err(anyhow!(
                "GPU graphics budget exhausted while creating offscreen image pipeline"
            ));
        };
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
        pane_outlines.upload(
            &device,
            &queue,
            [8.0, 8.0],
            &[pane_outline(
                (0.0, 0.0, 8.0, 8.0),
                Rgb::new(0, 255, 0),
                1.0,
                3.0,
                OUTLINE_BOTTOM_LEFT | OUTLINE_BOTTOM_RIGHT,
            )],
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
            pane_outlines.draw(&mut pass);
            imgs.draw(&mut pass);
        }
        queue.submit(std::iter::once(enc.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        Ok(true)
    })
}
