use super::*;

/// Terminator parity (`key_zoom_in` / `key_zoom_out` via
/// Ctrl+wheel): pure decision for whether the wheel notch should resize
/// the font.
///
///   * `ctrl` — Ctrl modifier held (the canonical zoom modifier across
///     gnome-terminal, Terminator, xterm-via-ctrl-shift-plus, etc.).
///   * `notches` — whole detents drained from `WheelAccum`; sign drives
///     the zoom direction, zero short-circuits. Deliberately detents and
///     not scaled lines: one physical notch must be one font step
///     regardless of `scroll-multiplier`, and a sub-detent touchpad event
///     must not zoom at all.
///   * `disabled` — `cfg.disable_mousewheel_zoom`. Opt-out for users
///     who scroll-zoom by accident (laptop touchpads + a Ctrl-meta
///     remap is a common collision).
///
/// Returns `Some(+1)` to grow, `Some(-1)` to shrink, `None` for no-op.
/// Extracted as a pure helper so the policy is unit-testable without
/// constructing a full App + winit event loop.
pub(super) fn should_zoom_font(ctrl: bool, notches: i32, disabled: bool) -> Option<i32> {
    if !ctrl || disabled || notches == 0 {
        None
    } else if notches > 0 {
        Some(1)
    } else {
        Some(-1)
    }
}

/// Fallback vertical-strip width used by layout helpers; production uses
/// cfg.tab_bar_width.
#[allow(dead_code)] // doc-only reference + layout-helper test fixture; production uses cfg.tab_bar_width
pub const VERTICAL_TAB_STRIP_W: f32 = 180.0;

/// The tab bar's band: where it draws and where a dragged tab docks. A bottom
/// bar sits above the search bar and a bottom status bar, and a side strip
/// stops above the search bar, so the docking target matches what is painted.
pub(super) fn tab_band_rect(
    surface: (u32, u32),
    pos: TabBarPos,
    height: f32,
    strip_width: f32,
    search_height: f32,
    bottom_status_height: f32,
) -> Rect {
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    match pos {
        TabBarPos::Top => (0.0, 0.0, sw, height),
        TabBarPos::Bottom => (
            0.0,
            sh - search_height - bottom_status_height - height,
            sw,
            height,
        ),
        TabBarPos::Left => (0.0, 0.0, strip_width, sh - search_height),
        TabBarPos::Right => (sw - strip_width, 0.0, strip_width, sh - search_height),
    }
}

/// Pure helper that computes the pane-content rect from the surface size, bar
/// metrics, and the edge each bar occupies.
///
/// Returns `(x, y, width, height)` in pixel coordinates.
///
/// A `TabBarPos::Left` or `Right` strip claims a per-side width slice
/// (`VERTICAL_TAB_STRIP_W`, 180 px) rather than a per-edge height.
///
/// Pure (no `&self`, renderer, or winit) so the layout math can be unit-tested
/// without constructing a full App. `App::area` calls
/// [`content_rect_for_with_strip`] directly with the configured strip width.
#[allow(dead_code)]
// production callers use content_rect_for_with_strip; this wrapper drives the layout-math drift guards (app.rs:9411+)
pub(super) fn content_rect_for(
    surface: (u32, u32),
    tab_bar_h: f32,
    status_bar_h: f32,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
) -> Rect {
    content_rect_for_with_strip(
        surface,
        ContentBands {
            tab_bar_h,
            status_bar_h,
            ..ContentBands::default()
        },
        tab_bar_pos,
        status_bar_mode,
        VERTICAL_TAB_STRIP_W,
    )
}

/// Horizontal chrome bands that reduce the pane-content area.
#[derive(Clone, Copy, Default)]
pub(super) struct ContentBands {
    pub(super) tab_bar_h: f32,
    pub(super) status_bar_h: f32,
    pub(super) vertical_title_edit_h: f32,
}

impl ContentBands {
    pub(super) fn for_restore(mut self, mode: TabBarMode, tabs: usize, cell_height: f32) -> Self {
        if matches!(mode, TabBarMode::Auto) {
            self.tab_bar_h = if tabs > 1 { cell_height + 8.0 } else { 0.0 };
        }
        self
    }
}

/// Explicit strip-width variant, so callers with `cfg.tab_bar_width` in scope
/// can pass it through. The non-`_with_strip` wrapper above keeps the same
/// signature for code paths without a Config.
pub(super) fn content_rect_for_with_strip(
    surface: (u32, u32),
    bands: ContentBands,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
    strip_w: f32,
) -> Rect {
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    let tb_on_top = matches!(tab_bar_pos, kettle_config::TabBarPos::Top);
    let tb_on_bottom = matches!(tab_bar_pos, kettle_config::TabBarPos::Bottom);
    let tb_on_left = matches!(tab_bar_pos, kettle_config::TabBarPos::Left);
    let tb_on_right = matches!(tab_bar_pos, kettle_config::TabBarPos::Right);
    let sb_on_top = matches!(status_bar_mode, kettle_config::StatusBarMode::Top);
    let sb_on_bottom = matches!(status_bar_mode, kettle_config::StatusBarMode::Bottom);

    // Vertical: status bar and title editing claim y-bands (status is always
    // horizontal in v1); the persistent tab strip claims only an x-band.
    let top_offset = (if tb_on_top { bands.tab_bar_h } else { 0.0 })
        + (if sb_on_top { bands.status_bar_h } else { 0.0 })
        + (if tb_on_left || tb_on_right {
            bands.vertical_title_edit_h.max(0.0)
        } else {
            0.0
        });
    let bot_offset = (if tb_on_bottom { bands.tab_bar_h } else { 0.0 })
        + (if sb_on_bottom {
            bands.status_bar_h
        } else {
            0.0
        });
    let left_offset = if tb_on_left { strip_w } else { 0.0 };
    let right_offset = if tb_on_right { strip_w } else { 0.0 };
    let content_h = (sh - top_offset - bot_offset).max(1.0);
    let content_w = (sw - left_offset - right_offset).max(1.0);
    (left_offset, top_offset, content_w, content_h)
}

/// Pure cols/rows-that-fit math for a terminal rect, shared by `grid_of`. The
/// terminal is already clear of the pane's titlebar and lane, so only the
/// padding comes off. `max(1)` keeps a degenerate tiny pane at ≥ 1×1.
pub(super) fn grid_dims_px(size: (f32, f32), cell: (f32, f32), pad: (f32, f32)) -> (usize, usize) {
    let (w, h) = size;
    let (cw, ch) = cell;
    let (pad_x, pad_y) = pad;
    let cols = ((w - pad_x * 2.0) / cw).floor().max(1.0) as usize;
    let rows = ((h - pad_y * 2.0) / ch).floor().max(1.0) as usize;
    (cols, rows)
}

/// Round a fractional renderer cell metric only after multiplying by the
/// complete grid dimension. ConPTY and Unix `winsize` pixel fields are `u16`,
/// but truncating each cell first compounds the error across every row/column
/// (9.6 px × 100 columns must report 960 px, not 900 px).
pub(super) fn pty_pixel_extent(cell: f32, count: usize) -> u16 {
    let cell = if cell.is_finite() {
        f64::from(cell.max(1.0))
    } else {
        1.0
    };
    let count = count.min(u16::MAX as usize) as f64;
    (cell * count).round().clamp(1.0, f64::from(u16::MAX)) as u16
}

pub(super) fn pty_pixel_size(cell: (f32, f32), grid: (usize, usize)) -> (u16, u16) {
    (
        pty_pixel_extent(cell.0, grid.0),
        pty_pixel_extent(cell.1, grid.1),
    )
}

/// The surface size, in physical pixels, whose content area holds exactly
/// `grid` cells of `cell` inside `pad` and the chrome `bands` describe.
///
/// `content_rect_for_with_strip` subtracts fixed bands, so their total is read
/// off a probe surface and added back. Half a pixel of slack before rounding
/// up keeps float error from costing a column; the slack stays under a cell.
pub(super) fn surface_for_grid(
    grid: (usize, usize),
    cell: (f32, f32),
    pad: (f32, f32),
    bands: ContentBands,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar: kettle_config::StatusBarMode,
    strip_w: f32,
) -> (u32, u32) {
    const PROBE: u32 = 1 << 16;
    let (_, _, content_w, content_h) =
        content_rect_for_with_strip((PROBE, PROBE), bands, tab_bar_pos, status_bar, strip_w);
    let chrome_w = PROBE as f32 - content_w;
    let chrome_h = PROBE as f32 - content_h;
    let width = grid.0 as f32 * cell.0 + pad.0 * 2.0 + chrome_w;
    let height = grid.1 as f32 * cell.1 + pad.1 * 2.0 + chrome_h;
    (
        (width + 0.5).ceil().max(1.0) as u32,
        (height + 0.5).ceil().max(1.0) as u32,
    )
}

/// How long the transient `cols×rows` chip stays up after the last resize
/// event. Ghostty `resize-overlay` parity, using Ghostty's
/// `resize-overlay-duration` default.
pub(crate) const RESIZE_OVERLAY_DURATION: std::time::Duration =
    std::time::Duration::from_millis(750);

impl App {
    fn cell_px(&self, ws: &WindowState) -> (u16, u16) {
        ws.renderer
            .as_ref()
            .map(|r| {
                (
                    r.cell_w.round().clamp(1.0, u16::MAX as f32) as u16,
                    r.cell_h.round().clamp(1.0, u16::MAX as f32) as u16,
                )
            })
            .unwrap_or((8, 16))
    }

    /// The cell size in physical pixels: the renderer's, or the one measured
    /// before it existed.
    pub(super) fn cell_size(ws: &WindowState) -> Option<(f32, f32)> {
        ws.renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .or(ws.startup_cell)
    }

    /// The surface size in physical pixels: the renderer's, or the one chosen
    /// before it existed.
    pub(super) fn surface_size(ws: &WindowState) -> Option<(u32, u32)> {
        ws.renderer
            .as_ref()
            .map(|r| r.surface_size())
            .or(ws.startup_surface)
    }

    pub(super) fn content_bands(&self, ws: &WindowState) -> ContentBands {
        ContentBands {
            tab_bar_h: self.tab_bar_h(ws),
            status_bar_h: self.status_bar_h(ws),
            vertical_title_edit_h: self.vertical_title_edit_h(ws),
        }
    }

    pub(super) fn area(&self, ws: &WindowState) -> Rect {
        self.area_with_bands(ws, self.content_bands(ws))
    }

    fn area_with_bands(&self, ws: &WindowState, bands: ContentBands) -> Rect {
        let surface = Self::surface_size(ws).unwrap_or((800, 600));
        // Delegate to the pure helper, threading
        // `cfg.tab_bar_width` so a user-configured strip width
        // is honored.
        let mut area = content_rect_for_with_strip(
            surface,
            bands,
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
            self.cfg.tab_bar_width,
        );
        area.3 = (area.3 - self.search_bar_h(ws)).max(0.0);
        area
    }

    /// The columns and rows that fit in `terminal`, a pane's terminal (or a
    /// whole area with nothing carved from it), inside its padding.
    pub(super) fn grid_of(&self, ws: &WindowState, terminal: Rect) -> (usize, usize) {
        let (cw, ch) = Self::cell_size(ws).unwrap_or((8.0, 16.0));
        let (_, _, w, h) = terminal;
        grid_dims_px((w, h), (cw, ch), (self.cfg.padding_x, self.cfg.padding_y))
    }

    pub(super) fn pty_geometry_for_grid(
        &self,
        ws: &WindowState,
        columns: usize,
        rows: usize,
    ) -> PtyGeometry {
        let fractional_cell = Self::cell_size(ws).unwrap_or((8.0, 16.0));
        let (pixel_width, pixel_height) = pty_pixel_size(fractional_cell, (columns, rows));
        PtyGeometry::new(columns, rows, pixel_width, pixel_height)
    }

    fn pty_geometry_for_terminal(&self, ws: &WindowState, terminal: Rect) -> PtyGeometry {
        let (columns, rows) = self.grid_of(ws, terminal);
        self.pty_geometry_for_grid(ws, columns, rows)
    }

    pub(super) fn prospective_split_geometry(
        &self,
        ws: &WindowState,
        dir: Dir,
        new_first: bool,
    ) -> PtyGeometry {
        // The new pane's leaf in a tab one pane larger, with no lane of its own.
        let area = self.area(ws);
        let leaf = ws
            .mux
            .prospective_split_rect(dir, new_first, area)
            .unwrap_or(area);
        let metrics = self
            .layout_style(ws)
            .metrics(ws.mux.active_pane_count().saturating_add(1));
        let terminal = crate::pane_partition::partition_leaf(leaf, metrics, None).terminal;
        self.pty_geometry_for_terminal(ws, terminal)
    }

    pub(super) fn restore_geometries(
        &self,
        ws: &WindowState,
        session: &crate::session::Session,
    ) -> Vec<Vec<PtyGeometry>> {
        // A restored tab has no lanes: each pane's terminal is its leaf less
        // its titlebar.
        let bands = self.content_bands(ws).for_restore(
            self.cfg.tab_bar,
            session.tabs.len(),
            Self::cell_size(ws).map_or(16.0, |(_, height)| height),
        );
        let area = self.area_with_bands(ws, bands);
        let style = self.layout_style(ws);
        let terminal = |leaf: Rect, panes: usize| {
            crate::pane_partition::partition_leaf(leaf, style.metrics(panes), None).terminal
        };
        session
            .tabs
            .iter()
            .map(|tab| {
                let rectangles = tab.root.leaf_rects(area);
                let pane_count = rectangles.len();
                let mut geometries: Vec<_> = rectangles
                    .into_iter()
                    .map(|leaf| self.pty_geometry_for_terminal(ws, terminal(leaf, pane_count)))
                    .collect();
                if tab.zoomed
                    && let Some(focused_index) =
                        (!geometries.is_empty()).then(|| tab.focus.min(geometries.len() - 1))
                    && let Some(focused) = geometries.get_mut(focused_index)
                {
                    *focused = self.pty_geometry_for_terminal(ws, terminal(area, 1));
                }
                geometries
            })
            .collect()
    }

    pub(super) fn pane_snapshots_match_current_layout(&self, ws: &WindowState) -> bool {
        if ws.renderer.is_none() || ws.pane_snapshots.len() != ws.pane_snapshot_keys.len() {
            return false;
        }
        let area = self.area(ws);
        let layout = self.pane_layout(ws, area);
        let current = layout.iter().filter_map(|(id, partition)| {
            let pane = ws.mux.panes.get(id)?;
            let (columns, screen_lines) = self.grid_of(ws, partition.terminal);
            Some((*id, pane.term.output_generation(), columns, screen_lines))
        });
        pane_snapshot_keys_match(&ws.pane_snapshot_keys, current)
            && ws
                .pane_snapshot_keys
                .iter()
                .zip(&ws.pane_snapshots)
                .all(|((id, ..), snap)| {
                    ws.mux.panes.get(id).is_some_and(|pane| {
                        snap.captures_card_marks() == pane.inline_cards.needs_marks()
                    })
                })
    }

    /// Which way `split_auto` should cut the focused pane.
    ///
    /// Terminator's rule is the pane's LONGER axis: a wide pane splits into a
    /// left/right pair, a tall one into top/bottom. Every user-facing
    /// description promises this (`docs/CONFIG.md` "pick by aspect ratio", the
    /// palette's "Split automatically", the context-menu row, and the default
    /// Ctrl+Shift+A binding).
    ///
    /// Ties go to a vertical cut, the same as `split_down`, so the default
    /// binding keeps that meaning where the aspect ratio favours neither side.
    pub(super) fn auto_split_dir(&self, ws: &WindowState) -> Dir {
        let area = self.area(ws);
        let rect = ws
            .mux
            .active_focus()
            .and_then(|focus| {
                ws.mux
                    .leaves(ws.mux.active, area)
                    .into_iter()
                    .find(|(id, _)| *id == focus)
            })
            .map_or(area, |(_, leaf)| leaf);
        if rect.2 > rect.3 {
            Dir::Horizontal
        } else {
            Dir::Vertical
        }
    }

    /// How this window measures its panes: its cells and padding, and the
    /// titlebar a pane shows beside another.
    pub(super) fn layout_style(&self, ws: &WindowState) -> LayoutStyle {
        let cell = Self::cell_size(ws).unwrap_or((8.0, 16.0));
        let padding = (self.cfg.padding_x, self.cfg.padding_y);
        let titlebar = self
            .cfg
            .show_titlebar
            .then(|| ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(20.0));
        LayoutStyle::new(cell, padding, titlebar, self.cfg.title_at_bottom)
            .or_else(|| LayoutStyle::new((8.0, 16.0), padding, titlebar, self.cfg.title_at_bottom))
            .unwrap_or_else(LayoutStyle::fallback)
    }

    /// The active tab's panes laid out over `area`, each divided between its
    /// titlebar, terminal and preview lane.
    pub(super) fn pane_layout(&self, ws: &WindowState, area: Rect) -> Vec<(u64, PanePartition)> {
        ws.mux.layout(ws.mux.active, area, self.layout_style(ws))
    }

    /// The partition of `pane_id` in the active tab.
    pub(super) fn pane_partition(
        &self,
        ws: &WindowState,
        area: Rect,
        pane_id: u64,
    ) -> Option<PanePartition> {
        self.pane_layout(ws, area)
            .into_iter()
            .find(|(id, _)| *id == pane_id)
            .map(|(_, partition)| partition)
    }

    /// The terminal of `pane_id` in the active tab: its grid with the
    /// padding around it, clear of the titlebar and lane.
    pub(super) fn pane_terminal(&self, ws: &WindowState, area: Rect, pane_id: u64) -> Option<Rect> {
        self.pane_partition(ws, area, pane_id)
            .map(|partition| partition.terminal)
    }

    /// The focused pane's terminal.
    pub(super) fn focused_terminal(&self, ws: &WindowState, area: Rect) -> Option<Rect> {
        self.pane_terminal(ws, area, ws.mux.active_focus()?)
    }

    /// Resize every pane's PTY to match its tile in the layout.
    pub(super) fn resize_all(&mut self, ws: &mut WindowState) {
        // Never synthesize a layout from the helper fallbacks while the
        // renderer is temporarily absent during GPU recovery. Existing PTYs
        // retain their last good geometry; the atomic renderer rebuild calls
        // this once with the real surface and fractional cell metrics.
        if ws.renderer.is_none() {
            return;
        }
        let (cw, ch) = self.cell_px(ws);
        let fractional_cell = ws
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((f32::from(cw), f32::from(ch)));
        let area = self.area(ws);
        let mut plan: Vec<(u64, usize, usize, u16, u16)> = Vec::new();
        let style = self.layout_style(ws);
        for ti in 0..ws.mux.tabs.len() {
            // Each PTY gets the rows and columns of its pane's terminal, clear
            // of its titlebar and lane.
            for (id, partition) in ws.mux.layout(ti, area, style) {
                let (cols, rows) = self.grid_of(ws, partition.terminal);
                let (pixel_width, pixel_height) = pty_pixel_size(fractional_cell, (cols, rows));
                plan.push((id, cols, rows, pixel_width, pixel_height));
            }
        }
        let mut native_resize_failed = false;
        for (id, cols, rows, pixel_width, pixel_height) in plan {
            if let Some(p) = ws.mux.panes.get_mut(&id)
                && let Err(error) = p.term.try_resize_geometry(PtyGeometry::new(
                    cols,
                    rows,
                    pixel_width,
                    pixel_height,
                ))
            {
                native_resize_failed = true;
                log::warn!(
                    "pane {id} PTY resize failed; a bounded deadline retry is scheduled: {error:#}"
                );
            }
        }
        ws.pty_resize_retry
            .record_result(std::time::Instant::now(), native_resize_failed);
        // The window, its font or its splits changed what a zoomed lane
        // shows.
        self.lanes_moved(ws);
    }

    pub(super) fn apply_window_resize(&mut self, ws: &mut WindowState, width: u32, height: u32) {
        // Never reconfigure a dead surface. A Resized event after a GPU device
        // loss would otherwise drive a surface configure on the lost device. The
        // grid resize below still runs so the layout is correct if the user
        // reopens.
        let gpu_lost = self.gpu.as_ref().is_some_and(|g| g.is_lost());
        if !gpu_lost
            && !ws.frame_recovery.renderer_rebuild_pending()
            && let Some(r) = ws.renderer.as_mut()
        {
            r.resize(width, height);
        }
        self.reclamp_context_menu(ws, (width as f32, height as f32));
        self.resize_all(ws);
        let show_chip = match self.cfg.resize_overlay {
            kettle_config::ResizeOverlayMode::Never => false,
            kettle_config::ResizeOverlayMode::Always => true,
            kettle_config::ResizeOverlayMode::AfterFirst => {
                ws.seen_first_resize
                    && ws.spawned_at.elapsed() > std::time::Duration::from_millis(1500)
            }
        };
        ws.seen_first_resize = true;
        if show_chip {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            ws.resize_overlay = Some((cols as u16, rows as u16, std::time::Instant::now()));
        }
    }

    pub(super) fn commit_window_resize(&mut self, ws: &mut WindowState, width: u32, height: u32) {
        ws.frame_recovery.expedite(std::time::Instant::now());
        self.apply_window_resize(ws, width, height);
        // Record the new grid size into the asciicast trace.
        if self.recorder.is_some() {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            if let Some(rec) = self.recorder.as_mut() {
                rec.record_resize(cols as u16, rows as u16);
            }
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn dpi_resize_renderer_ready(&self, ws: &WindowState) -> bool {
        ws.renderer.is_some()
            && !self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost())
            && !ws.frame_recovery.renderer_rebuild_pending()
    }

    /// Visible terminal cursor cell for IME composition/candidate placement.
    pub(super) fn terminal_cursor_cell(&self, ws: &WindowState) -> Option<(usize, usize)> {
        let pane = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id))?;
        let term = pane.term.term.lock().ok()?;
        let point = term.renderable_content().cursor.point;
        Some((point.line.0.max(0) as usize, point.column.0))
    }

    pub(super) fn overlay_text_line_height(&self, ws: &WindowState) -> f32 {
        ws.renderer
            .as_ref()
            .map(|renderer| renderer.overlay_text_line_height())
            .unwrap_or(self.menu_cell(ws).1)
    }

    pub(super) fn overlay_text_cell_width(&self, ws: &WindowState) -> f32 {
        ws.renderer
            .as_ref()
            .map(|renderer| renderer.overlay_text_cell_width())
            .unwrap_or(self.menu_cell(ws).0)
    }
}
