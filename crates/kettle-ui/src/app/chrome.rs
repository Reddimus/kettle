//! The tab bar, status bar and update banner: their sizes, hit tests and drawing.

use super::*;

impl App {
    /// True when the cursor is over the status bar. Used
    /// by `cursor_in_chrome_band` so the OS arrow icon overrides
    /// the terminal I-beam over the status strip too.
    pub(super) fn cursor_in_status_bar(&self, ws: &WindowState) -> bool {
        let h = self.status_bar_h(ws);
        if h <= 0.0 {
            return false;
        }
        let (_, sh) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        cursor_in_status_bar_band(ws.cursor.y as f32, h, sh as f32, self.cfg.status_bar)
    }

    pub(super) fn cursor_in_update_banner(&self, ws: &WindowState) -> bool {
        let Some((x, y, w, h)) = self.update_banner_rect(ws) else {
            return false;
        };
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        px >= x && px < x + w && py >= y && py < y + h
    }

    /// Combined chrome-band hit-test. True when the cursor is over the tab
    /// bar, the status bar, or the update banner, which all belong in the
    /// "OS arrow cursor" group.
    pub(super) fn cursor_in_chrome_band(&self, ws: &WindowState) -> bool {
        self.cursor_in_tab_bar(ws)
            || self.cursor_in_status_bar(ws)
            || self.cursor_in_update_banner(ws)
    }

    pub(super) fn cursor_in_tab_bar(&self, ws: &WindowState) -> bool {
        let h = self.tab_bar_h(ws);
        if h <= 0.0 {
            return false;
        }
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        // For Left/Right strips, the cursor must be within
        // `cfg.tab_bar_width` of the configured edge.
        match self.cfg.tab_bar_pos {
            TabBarPos::Left => {
                let x = ws.cursor.x as f32;
                (0.0..self.cfg.tab_bar_width).contains(&x)
            }
            TabBarPos::Right => {
                let x = ws.cursor.x as f32;
                x >= sw as f32 - self.cfg.tab_bar_width && x <= sw as f32
            }
            TabBarPos::Top | TabBarPos::Bottom => {
                cursor_in_tab_bar_band(ws.cursor.y as f32, h, sh as f32, self.cfg.tab_bar_pos)
            }
        }
    }

    pub(super) fn tab_bar_h(&self, ws: &WindowState) -> f32 {
        let show = match self.cfg.tab_bar {
            // Title-edit needs a real chrome strip so the input never covers
            // terminal rows. Honor `off` during normal operation, but materialize
            // the strip while the modal owns keyboard focus.
            TabBarMode::Off => ws.editing_title.is_some(),
            // A live dock preview MATERIALIZES the bar on a single-tab auto
            // window. The strip appears under the hovering torn window so the
            // drop target is visible before the drop (Chrome's always-on
            // strip affordance, on demand).
            TabBarMode::Auto => {
                ws.mux.tabs.len() > 1 || ws.dock_preview.is_some() || ws.editing_title.is_some()
            }
            TabBarMode::Always => true,
        };
        if show {
            Self::cell_size(ws).map_or(24.0, |(_, ch)| ch + 8.0)
        } else {
            0.0
        }
    }

    /// Status-bar height (0 when off, cell_h + 6 px when
    /// enabled). Pair with `cfg.status_bar` (StatusBarMode) for
    /// position. Slightly shorter than the tab bar so the two strips
    /// read as distinct horizontal bands.
    pub(super) fn status_bar_h(&self, ws: &WindowState) -> f32 {
        match self.cfg.status_bar {
            kettle_config::StatusBarMode::Off => 0.0,
            _ => Self::cell_size(ws).map_or(22.0, |(_, ch)| ch + 6.0),
        }
    }

    /// Height of the passive update banner while it is visible. The renderer
    /// uses the same `cell_h + 10px` formula, so the PTY grid, pixels, and
    /// hit-test agree instead of letting the banner cover the bottom row.
    pub(super) fn update_banner_h(&self, ws: &WindowState) -> f32 {
        // Renderer chrome gives an active Search lane priority over this
        // passive notice. Match that precedence in PTY sizing instead of
        // reserving an invisible extra strip between content and Search.
        if self.update_available.is_some() && !ws.search.open {
            ws.renderer
                .as_ref()
                .map(|r| r.cell_h + 10.0)
                .unwrap_or(26.0)
        } else {
            0.0
        }
    }

    pub(super) fn vertical_title_edit_h(&self, ws: &WindowState) -> f32 {
        if self.cfg.tab_bar_pos.is_vertical() && ws.editing_title.is_some() {
            self.tab_bar_h(ws)
        } else {
            0.0
        }
    }

    pub(super) fn update_banner_rect(&self, ws: &WindowState) -> Option<Rect> {
        self.update_available.as_ref()?;
        let (sw, sh) = ws.renderer.as_ref()?.surface_size();
        let banner_h = self.update_banner_h(ws);
        if banner_h <= 0.0 {
            return None;
        }
        let bottom_tabbar_h = if matches!(self.cfg.tab_bar_pos, TabBarPos::Bottom) {
            self.tab_bar_h(ws)
        } else {
            0.0
        };
        let bottom_status_h = if matches!(self.cfg.status_bar, kettle_config::StatusBarMode::Bottom)
        {
            self.status_bar_h(ws)
        } else {
            0.0
        };
        let y =
            kettle_render::update_banner_top(sh as f32, banner_h, bottom_tabbar_h, bottom_status_h);
        Some((0.0, y, sw as f32, banner_h))
    }

    pub(super) fn tab_bar(&self, ws: &WindowState) -> TabBar {
        let height = self.tab_bar_h(ws);
        if height <= 0.0 {
            return TabBar::hidden();
        }
        let band = self.tab_band(ws, height);
        let sw = band.2;
        let y = band.1;
        let is_vertical = self.cfg.tab_bar_pos.is_vertical();
        // Left/Right strips route through a separate vertical-stacked path;
        // Top/Bottom use the `tab_strip_layout` flow below.
        if is_vertical {
            return self.tab_bar_vertical(ws, band, height);
        }
        if ws.editing_title.is_some() {
            let strip_w = if is_vertical {
                self.cfg.tab_bar_width
            } else {
                0.0
            };
            let strip_x = match self.cfg.tab_bar_pos {
                TabBarPos::Right => sw - strip_w,
                _ => 0.0,
            };
            return TabBar {
                height,
                y,
                segments: Vec::new(),
                new_tab: (strip_x, y, strip_w, height),
                new_tab_menu: (0.0, 0.0, 0.0, 0.0),
                hovered_new_tab: false,
                hovered_new_tab_menu: false,
                broadcast: ws.mux.is_broadcast_on(),
                hovered_close_idx: None,
                drag_cursor_x: None,
                drag_cursor_y: None,
                // The title-edit modal owns the bar — no drag can be live.
                tear_lift: 0.0,
                insert_marker: None,
                band,
                scroll_left: (0.0, 0.0, 0.0, 0.0),
                scroll_right: (0.0, 0.0, 0.0, 0.0),
            };
        }
        let labels = ws.mux.tab_labels(&self.ui_text);
        let n = labels.len().max(1);
        // Trailing "▾ +" button: a `▾` dropdown arrow (left) + the `+` (right),
        // each `height` wide. The strip must reserve the WHOLE
        // button (arrow + plus), not just `plus_w`, or the last tab segment
        // overlaps it.
        let plus_w = height;
        // Hide the ▾ shell-dropdown when only one shell is
        // available (e.g. stock Ubuntu = just bash). A zero-width arrow drops it
        // from both the render pass (`new_tab_menu.2 > 0.0`) and the click
        // hit-test, and the `+` button reclaims the space.
        let arrow_w = if new_tab_dropdown_visible() {
            height
        } else {
            0.0
        };
        let button_w = plus_w + arrow_w;
        // The drag-to-reorder handler derives its strip width from
        // the same helper, so the two can't disagree on where segments end.
        let strip = tab_segment_strip_width(sw, plus_w, arrow_w);
        let (arrow_rect, plus_rect) =
            split_new_tab_button((sw - button_w, y, button_w, height), arrow_w);
        // Tabs divide the strip evenly and fill the bar; once they would
        // shrink below `tab_min_width` the bar overflows and — when `scroll_tabbar`
        // — scrolls with `‹ ›` arrows, showing only whole tabs (active kept in
        // view). See `tab_strip_layout`. Scroll arrows are `height`-wide squares.
        let active = ws.mux.active;
        let layout = tab_strip_layout(
            n,
            active,
            strip,
            button_w,
            height,
            self.cfg.tab_min_width,
            self.cfg.scroll_tabbar,
        );
        // Tabs scrolled out of view are parked off-screen (kept in the segment
        // list, idx-indexed, so reorder / dock / ghost logic is undisturbed).
        const OFFSCREEN_X: f32 = -100_000.0;
        let now = std::time::Instant::now();
        let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
        let segments: Vec<TabSeg> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let x = layout.xs.get(i).copied().flatten().unwrap_or(OFFSCREEN_X);
                // Per-tab activity dot. Active tabs short-circuit to
                // Normal (the focused-tab accent already signals "you are here").
                let activity = ws
                    .mux
                    .tabs
                    .get(i)
                    .map(|tab| {
                        match crate::mux::classify_tab_activity(
                            i == active,
                            tab.bell,
                            tab.last_output_at,
                            tab.last_seen_at,
                            now,
                            silence,
                        ) {
                            crate::mux::TabActivity::Normal => RenderTabActivity::Normal,
                            crate::mux::TabActivity::Output => RenderTabActivity::Output,
                            crate::mux::TabActivity::Bell => RenderTabActivity::Bell,
                            crate::mux::TabActivity::Silent => RenderTabActivity::Silent,
                        }
                    })
                    .unwrap_or(RenderTabActivity::Normal);
                // The last tab yields the trailing `▾`/`+` button reservation
                // in the full-width-aligned layout, so it is narrower than the
                // rest; every other layout has `last_width == width`.
                let seg_w = if i + 1 == n {
                    layout.last_width
                } else {
                    layout.width
                };
                let rect = (x, y, seg_w, height);
                let close =
                    tab_close_rect(x + seg_w - height, y, height, self.cfg.close_button_on_tab);
                TabSeg {
                    idx: i,
                    rect,
                    title_rect: tab_title_rect(rect, close, self.cfg.close_button_on_tab),
                    // ✕ hit zone = the trailing `height`-wide square.
                    close,
                    title: label.text.clone(),
                    path: label.path.clone(),
                    active: i == active,
                    activity,
                }
            })
            .collect();
        let to_btn = |ox: Option<f32>| ox.map_or((0.0, 0.0, 0.0, 0.0), |x| (x, y, height, height));
        TabBar {
            height,
            y,
            segments,
            new_tab: plus_rect,
            new_tab_menu: arrow_rect,
            hovered_new_tab: ws.hovered_new_tab,
            hovered_new_tab_menu: ws.hovered_new_tab_menu,
            // Broadcast indicator on the active tab.
            broadcast: ws.mux.is_broadcast_on(),
            // Hover-on-✕ chip: renderer paints a red highlight behind
            // the close glyph; UI's `sync_cursor_icon` flips the OS
            // cursor to Pointer at the same time.
            hovered_close_idx: ws.hovered_close_idx,
            // While a tab-bar drag is in progress, hand
            // the renderer the cursor x so it paints a translucent
            // ghost of the dragged segment under the cursor.
            drag_cursor_x: if ws.tab_drag_active && ws.tab_drag_press.is_none() {
                Some(ws.cursor.x as f32)
            } else {
                None
            },
            drag_cursor_y: None,
            // Tear-off UX: pre-tear ghost escalation.
            tear_lift: self.tear_lift_for(ws),
            // Re-dock: vertical insertion line at the docked tab's landing slot,
            // anchored to the target tab's on-screen x when visible (overflow
            // scrolling), else to the leftmost visible edge.
            insert_marker: ws.dock_preview.map(|idx| {
                let x = layout
                    .xs
                    .get(idx)
                    .copied()
                    .flatten()
                    .or_else(|| layout.xs.iter().copied().flatten().next())
                    .unwrap_or(0.0)
                    .clamp(0.0, strip - kettle_render::tab_drag::INSERT_MARKER_PX);
                (x, y, kettle_render::tab_drag::INSERT_MARKER_PX, height)
            }),
            // Tear-off UX: the dock-highlight canvas.
            band,
            scroll_left: to_btn(layout.arrow_left),
            scroll_right: to_btn(layout.arrow_right),
        }
    }

    pub(super) fn title_edit_rect(&self, ws: &WindowState) -> kettle_render::Rect4 {
        let (w, h) = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((800, 600));
        let (sw, sh) = (w as f32, h as f32);
        let bar = self.tab_bar(ws);
        let row_h = ws.renderer.as_ref().map(|r| r.cell_h + 8.0).unwrap_or(24.0);
        let content_y = self.area(ws).1;
        title_edit_rect_for(
            self.cfg.tab_bar_pos.is_vertical(),
            bar.y,
            bar.height,
            content_y,
            sw,
            sh,
            row_h,
        )
    }

    /// Tab-bar layout for `TabBarPos::Left` / `Right`. Stacks segments
    /// vertically, each `cfg.tab_bar_width` × `tab_bar_h`. The new-tab `+`
    /// button follows the last tab, clamped to the bottom of the strip.
    pub(super) fn tab_bar_vertical(&self, ws: &WindowState, band: Rect, height: f32) -> TabBar {
        let (strip_x, _, strip_w, sh) = band;
        let labels = ws.mux.tab_labels(&self.ui_text);
        let active = ws.mux.active;
        let now = std::time::Instant::now();
        let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
        let segments: Vec<TabSeg> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let seg_y = i as f32 * height;
                let activity = ws
                    .mux
                    .tabs
                    .get(i)
                    .map(|tab| {
                        match crate::mux::classify_tab_activity(
                            i == active,
                            tab.bell,
                            tab.last_output_at,
                            tab.last_seen_at,
                            now,
                            silence,
                        ) {
                            crate::mux::TabActivity::Normal => RenderTabActivity::Normal,
                            crate::mux::TabActivity::Output => RenderTabActivity::Output,
                            crate::mux::TabActivity::Bell => RenderTabActivity::Bell,
                            crate::mux::TabActivity::Silent => RenderTabActivity::Silent,
                        }
                    })
                    .unwrap_or(RenderTabActivity::Normal);
                let rect = (strip_x, seg_y, strip_w, height);
                let close = tab_close_rect(
                    strip_x + strip_w - height,
                    seg_y,
                    height,
                    self.cfg.close_button_on_tab,
                );
                TabSeg {
                    idx: i,
                    rect,
                    title_rect: tab_title_rect(rect, close, self.cfg.close_button_on_tab),
                    // ✕ hit zone = the trailing-right square of the
                    // segment (same axis convention as horizontal).
                    close,
                    title: label.text.clone(),
                    path: label.path.clone(),
                    active: i == active,
                    activity,
                }
            })
            .collect();
        // `+` button at the bottom of the strip.
        let plus_y = (labels.len() as f32 * height).min(sh - height);
        TabBar {
            height,
            // `y` is the *band start* (top of the strip) for the
            // renderer; for vertical strips the band spans the
            // whole window height, so `y = 0`.
            y: 0.0,
            segments,
            new_tab: (strip_x, plus_y, strip_w, height),
            // No dropdown arrow on vertical bars — the bottom-of-
            // strip full-width `+` has nowhere sensible for a left-side arrow.
            new_tab_menu: (0.0, 0.0, 0.0, 0.0),
            hovered_new_tab: ws.hovered_new_tab,
            hovered_new_tab_menu: false,
            broadcast: ws.mux.is_broadcast_on(),
            hovered_close_idx: ws.hovered_close_idx,
            // A vertical strip's ghost rides the cursor's y down a fixed
            // column, so the main-axis coordinate is `y` here and `x` on the
            // horizontal bar. Using `x` here would pin the ghost to the top of
            // the strip and slide it sideways out of the bar.
            drag_cursor_x: None,
            drag_cursor_y: if ws.tab_drag_active && ws.tab_drag_press.is_none() {
                Some(ws.cursor.y as f32)
            } else {
                None
            },
            tear_lift: self.tear_lift_for(ws),
            // Re-dock: horizontal insertion line across the strip at the docked
            // tab's landing slot.
            insert_marker: ws.dock_preview.map(|idx| {
                let iy = (idx.min(labels.len()) as f32 * height)
                    .min(sh - kettle_render::tab_drag::INSERT_MARKER_PX)
                    .max(0.0);
                (
                    strip_x,
                    iy,
                    strip_w,
                    kettle_render::tab_drag::INSERT_MARKER_PX,
                )
            }),
            // Tear-off UX: the dock-highlight canvas is the whole vertical strip.
            band,
            // Vertical bars don't overflow-scroll (yet), so they have no arrows.
            scroll_left: (0.0, 0.0, 0.0, 0.0),
            scroll_right: (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Compose the status-bar contents (HH:MM:SS · theme ·
    /// focused-pane title). Returns `StatusBar::hidden` when the
    /// config has it off. The renderer's draw is a no-op on a
    /// hidden status bar so this is cheap even when never visible.
    /// Takes `ws` mutably only because `Mux::focused` does; nothing
    /// is mutated here.
    pub(super) fn build_status_bar(&mut self, ws: &mut WindowState) -> kettle_render::StatusBar {
        if matches!(self.cfg.status_bar, kettle_config::StatusBarMode::Off) {
            return kettle_render::StatusBar::hidden();
        }
        let h = self.status_bar_h(ws);
        let surface_h = ws
            .renderer
            .as_ref()
            .map(|r| r.surface_size().1 as f32)
            .unwrap_or(600.0);
        let y = match self.cfg.status_bar {
            kettle_config::StatusBarMode::Top => {
                if matches!(self.cfg.tab_bar_pos, TabBarPos::Top) {
                    self.tab_bar_h(ws)
                } else {
                    0.0
                }
            }
            kettle_config::StatusBarMode::Bottom => surface_h - self.search_bar_h(ws) - h,
            kettle_config::StatusBarMode::Off => 0.0,
        };
        // Compose text: local HH:MM:SS · theme · focused pane title.
        let (hh, mm, ss) = crate::wall_clock::now_local();
        let title = ws
            .mux
            .focused()
            .map(|p| p.title.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "kettle".to_string());
        // Cap the title at a character budget so a chatty
        // prompt that puts the full cwd in the window title
        // (e.g. `PROMPT_COMMAND='echo -ne "\033]0;$PWD\007"'`)
        // doesn't overflow the strip's 1-cell height. Pure helper
        // so we can drift-guard it in tests.
        let title_capped = cap_title_for_status_bar(&title, 60);
        let text = format!(
            "{hh:02}:{mm:02}:{ss:02}  ·  {}  ·  {title_capped}",
            self.cfg.theme_name
        );
        kettle_render::StatusBar { height: h, y, text }
    }

    /// Act on the update banner: dismiss it (recording the tag so it
    /// won't re-nag) and, when `open` is true, open the release page first.
    /// Returns `false` when no banner is showing (the caller decides whether
    /// that's a no-op or worth a debug log). Shared by the banner mouse
    /// handler and the `OpenUpdate` / `DismissUpdate` keyboard
    /// actions so all three paths stay in lockstep.
    pub(super) fn act_on_update_banner(&mut self, ws: &mut WindowState, open: bool) -> bool {
        let Some((tag, url)) = self.update_available.clone() else {
            return false;
        };
        if open {
            self.open_url(ws, &url, LinkSource::Kettle);
        }
        crate::update_check::record_dismissed(&tag);
        self.update_available = None;
        Self::queue_window_resizes(std::iter::once(&mut *ws).chain(self.windows.values_mut()));
        true
    }
}
