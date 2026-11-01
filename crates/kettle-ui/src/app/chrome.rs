//! Tab/status chrome and local update facts.

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
        let tab_top_h = if matches!(self.cfg.tab_bar_pos, TabBarPos::Top) {
            self.tab_bar_h(ws)
        } else {
            0.0
        };
        cursor_in_status_bar_band(
            ws.cursor.y as f32,
            h,
            sh as f32,
            self.cfg.status_bar,
            tab_top_h,
            self.search_bar_h(ws),
        )
    }

    /// Combined chrome-band hit-test. True when the cursor is over the tab
    /// bar or the status bar, which belong in the "OS arrow cursor" group.
    pub(super) fn cursor_in_chrome_band(&self, ws: &WindowState) -> bool {
        self.cursor_in_tab_bar(ws)
            || self.cursor_in_status_bar(ws)
            || self.update_toast_projection(ws).is_some_and(|toast| {
                toast
                    .geometry
                    .hit_test(ws.cursor.x as f32, ws.cursor.y as f32)
                    .is_some()
            })
    }

    pub(super) fn cursor_in_tab_bar(&self, ws: &WindowState) -> bool {
        let h = self.tab_bar_h(ws);
        if h <= 0.0 {
            return false;
        }
        rect_contains(self.tab_band(ws, h), ws.cursor.x as f32, ws.cursor.y as f32)
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

    pub(super) fn vertical_title_edit_h(&self, ws: &WindowState) -> f32 {
        if self.cfg.tab_bar_pos.is_vertical() && ws.editing_title.is_some() {
            self.tab_bar_h(ws)
        } else {
            0.0
        }
    }

    pub(super) fn tab_bar(&self, ws: &WindowState) -> TabBar {
        let height = self.tab_bar_h(ws);
        if height <= 0.0 {
            return TabBar::hidden();
        }
        self.tab_bar_at_height(ws, height)
    }

    pub(super) fn tab_bar_at_height(&self, ws: &WindowState, height: f32) -> TabBar {
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
        // Every consumer of segment geometry reads it from this bar, so the
        // drag-to-reorder handler and the click hit-test agree on where
        // segments end.
        let strip = tab_segment_strip_width(sw, plus_w, arrow_w).max(plus_w);
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
        let now = std::time::Instant::now();
        let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
        let segments: Vec<TabSeg> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let x = layout.xs.get(i).copied().flatten().unwrap_or(PARKED_TAB);
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
                // A tab narrowed below its close button's width has no room
                // for one: the button would reach past it onto the arrows.
                let closable = self.cfg.close_button_on_tab && seg_w >= height;
                let close = tab_close_rect(x + seg_w - height, y, height, closable);
                TabSeg {
                    idx: i,
                    rect,
                    title_rect: tab_title_rect(rect, close, closable),
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
    /// vertically, each `cfg.tab_bar_width` × `tab_bar_h`, with the new-tab
    /// `+` below them. Rows that do not fit scroll; see [`side_tab_rows`].
    pub(super) fn tab_bar_vertical(&self, ws: &WindowState, band: Rect, height: f32) -> TabBar {
        let (strip_x, _, strip_w, sh) = band;
        let labels = ws.mux.tab_labels(&self.ui_text);
        let active = ws.mux.active;
        let now = std::time::Instant::now();
        let silence = std::time::Duration::from_millis(self.cfg.tab_silence_threshold_ms);
        // While a title is edited, its editor takes the window's top row
        // across both the strip and the panes; the strip starts below it.
        let lead = self.vertical_title_edit_h(ws);
        let strip_rows = ((sh - lead) / height).floor().max(0.0) as usize;
        let rows = strip_rows;
        let layout = side_tab_rows(labels.len(), active, rows);
        let row_y = |row: usize| lead + row as f32 * height;
        // Rows scrolled out of view are parked off-screen, kept in the segment
        // list as the horizontal bar keeps its own.
        let shown = layout.first..layout.first + layout.shown;
        let segments: Vec<TabSeg> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let seg_y = if shown.contains(&i) {
                    row_y(layout.top + i - layout.first)
                } else {
                    PARKED_TAB
                };
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
                let closable = self.cfg.close_button_on_tab && strip_w >= height;
                let close = tab_close_rect(strip_x + strip_w - height, seg_y, height, closable);
                TabSeg {
                    idx: i,
                    rect,
                    title_rect: tab_title_rect(rect, close, closable),
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
        // `+` is left out of a strip with no row to spare for it.
        let plus_y = if layout.plus < rows {
            row_y(layout.plus)
        } else {
            PARKED_TAB
        };
        let arrow = |row: usize| {
            if layout.arrows {
                (strip_x, row_y(row), strip_w, height)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            }
        };
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
            // tab's landing slot, or at the near edge of the rows shown when
            // that slot is scrolled out of view.
            insert_marker: ws.dock_preview.map(|idx| {
                let slot = idx.min(labels.len()).clamp(shown.start, shown.end);
                let iy = row_y(layout.top + slot - layout.first)
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
            // `⌃` above the rows shown and `⌄` below them step to the previous
            // and next tab, which scrolls the strip.
            scroll_left: arrow(0),
            scroll_right: arrow(layout.plus.saturating_sub(1)),
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
        let tab_top_h = if matches!(self.cfg.tab_bar_pos, TabBarPos::Top) {
            self.tab_bar_h(ws)
        } else {
            0.0
        };
        let y = status_bar_y(
            surface_h,
            h,
            self.cfg.status_bar,
            tab_top_h,
            self.search_bar_h(ws),
        );
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
            self.ui_text.theme_name(&self.cfg.theme_name)
        );
        kettle_render::StatusBar { height: h, y, text }
    }

    /// The update notice this process shows, if any.
    pub(super) fn update_notice(&self) -> Option<crate::update_notice::Notice> {
        crate::update_notice::active_notice(&self.update_facts, self.update_restart_dismissed)
    }

    /// Read local update facts off the event loop, throttled on focus changes.
    pub(super) fn request_update_facts(&mut self, force: bool) {
        let now = std::time::Instant::now();
        if !force
            && self
                .update_facts_requested
                .is_some_and(|at| now.duration_since(at) < UPDATE_FACTS_THROTTLE)
        {
            return;
        }
        self.update_facts_requested = Some(now);
        if !self.update_facts_refresh.begin() {
            return;
        }
        let proxy = self.proxy.clone();
        let install = self.startup.update_install.clone();
        let policy = self.cfg.update_policy;
        if std::thread::Builder::new()
            .name("kettle-update-facts".into())
            .spawn(move || {
                let facts =
                    crate::update_check::read_facts(&install, policy, env!("CARGO_PKG_VERSION"));
                let _ = proxy.send_event(UserEvent::UpdateFacts(Box::new(facts)));
            })
            .is_err()
        {
            self.update_facts_refresh.complete();
            log::warn!("could not start update facts read");
        }
    }

    /// Refresh update facts without changing terminal geometry.
    pub(super) fn apply_update_facts(
        &mut self,
        ws: &mut WindowState,
        mut facts: crate::update_notice::UpdateFacts,
    ) {
        self.update_local_dismissals.apply(&mut facts);
        self.update_toast_unavailable = None;
        if self.update_facts != facts {
            let previous = self.update_notice();
            self.update_facts = facts;
            if previous != self.update_notice() {
                self.update_toast_retry = None;
            }
            self.redraw_for_update_notice(ws);
        }
    }

    fn redraw_for_update_notice(&mut self, ws: &mut WindowState) {
        for window in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
            window.accessibility_pending = true;
            if let Some(w) = &window.window {
                w.request_redraw();
            }
        }
    }

    /// Open the release page of the active notice. Opening does not dismiss
    /// it. False when no notice is active.
    pub(super) fn open_update_release(&mut self, ws: &mut WindowState) -> bool {
        let Some(url) = self
            .update_notice()
            .and_then(|notice| crate::update_notice::release_url(&notice.tag))
        else {
            return false;
        };
        self.open_url(ws, &url, LinkSource::Kettle);
        true
    }

    /// Dismiss the active notice: Available and Failed for every process,
    /// per tag; Restart pending for this process until it exits. False when
    /// no notice is active.
    pub(super) fn dismiss_update_notice(&mut self, ws: &mut WindowState) -> bool {
        self.dismiss_update_toast(ws);
        use crate::update_notice::NoticeKind;
        let Some(notice) = self.update_notice() else {
            return false;
        };
        match notice.kind {
            NoticeKind::Available => {
                crate::update_check::record_dismissed(&notice.tag);
                self.update_local_dismissals.available = Some(notice.tag.clone());
                self.update_facts.dismissed_available = Some(notice.tag);
            }
            NoticeKind::Failed => {
                crate::update_check::record_dismissed_failed(&notice.tag);
                self.update_local_dismissals.failed = Some(notice.tag.clone());
                self.update_facts.dismissed_failed = Some(notice.tag);
            }
            NoticeKind::RestartPending => self.update_restart_dismissed = true,
        }
        self.redraw_for_update_notice(ws);
        true
    }
}

/// Horizontal tab-bar layout. Tabs divide `strip` evenly and **fill the bar**
/// (two tabs each take half). There is intentionally no max width; tabs always
/// maximize width. When `scroll` is on and they would shrink below `min_w`, the
/// bar overflows: tabs stay at `min_w`, `‹ ›` arrow buttons (each `arrow_w`
/// wide) reserve the strip ends, and only whole tabs that fit between them are
/// shown, chosen so the active tab is visible (pinned to the trailing edge once
/// it scrolls past the first page). Pure (no `&self`, renderer, or winit), so
/// it is unit-testable.
pub(super) struct TabStripLayout {
    /// Per-tab width for every tab EXCEPT the last one.
    pub(super) width: f32,
    /// Width of the LAST tab. Equal to `width` in every layout except the
    /// full-width-aligned one, where the last tab yields the trailing `▾`/`+`
    /// button reservation so the other tab boundaries land on the same grid as
    /// vertical pane splits.
    pub(super) last_width: f32,
    /// On-screen left x per tab, or `None` when scrolled out of view.
    pub(super) xs: Vec<Option<f32>>,
    /// Left x of the `‹` / `›` scroll buttons when the bar overflows.
    pub(super) arrow_left: Option<f32>,
    pub(super) arrow_right: Option<f32>,
}

pub(super) fn tab_strip_layout(
    n: usize,
    active: usize,
    strip: f32,
    trailing_reserve: f32,
    arrow_w: f32,
    min_w: f32,
    scroll: bool,
) -> TabStripLayout {
    let n = n.max(1);
    let strip = strip.max(1.0);
    let min_w = min_w.max(1.0);
    let reserve = trailing_reserve.max(0.0);
    // Preferred layout: divide the FULL bar width (the button-trimmed `strip`
    // PLUS the trailing `▾`/`+` reservation) evenly, so tab boundaries fall on
    // the same grid as vertical pane splits — with 2 tabs the tab1/tab2
    // boundary lands at the window centre, in line with a 50/50 split's
    // divider. Only the LAST tab is shortened to yield the button reservation
    // (its right edge stops at `strip`); every other tab is `even_full` wide.
    // Used whenever the shortened last tab still clears `min_w` (so no tab is
    // driven below the floor). `reserve == 0` degrades to plain even fill.
    let full = strip + reserve;
    let even_full = full / n as f32;
    let last_full = even_full - reserve;
    if last_full >= min_w {
        let xs = (0..n).map(|i| Some(i as f32 * even_full)).collect();
        return TabStripLayout {
            width: even_full,
            last_width: last_full,
            xs,
            arrow_left: None,
            arrow_right: None,
        };
    }
    let even = strip / n as f32;
    if !scroll || even >= min_w {
        // Full-width alignment would shrink the last tab below `min_w`, so fall
        // back to the original behavior: tabs divide the button-trimmed `strip`
        // evenly and fill it, left-packed. (With scrolling off and many tabs,
        // width may fall below min_w — they just keep shrinking, no overflow.)
        let width = even;
        let xs = (0..n).map(|i| Some(i as f32 * width)).collect();
        return TabStripLayout {
            width,
            last_width: width,
            xs,
            arrow_left: None,
            arrow_right: None,
        };
    }
    // Overflow: tabs at `min_w`, arrows reserve both ends, whole tabs only. A
    // strip with less than `min_w` between its arrows still shows the active
    // tab, narrowed to that room, rather than drawn over the arrows and buttons.
    let arrow_w = arrow_w.clamp(1.0, strip / 2.0);
    let room = (strip - 2.0 * arrow_w).max(0.0);
    let width = min_w.min(room);
    let count = if width > 0.0 {
        ((room / width).floor() as usize).clamp(1, n)
    } else {
        1
    };
    let first = first_shown_tab(n, active, count);
    let x0 = arrow_w;
    let xs = (0..n)
        .map(|i| {
            if i >= first && i < first + count {
                Some(x0 + (i - first) as f32 * width)
            } else {
                None
            }
        })
        .collect();
    TabStripLayout {
        width,
        last_width: width,
        xs,
        arrow_left: Some(0.0),
        arrow_right: Some(strip - arrow_w),
    }
}

/// The first of `count` consecutive tabs, out of `n`, that an overflowing tab
/// bar shows: page one while it holds the active tab, else the window that
/// ends at the active tab. Both bar directions scroll this way, so moving the
/// selection is how a bar scrolls.
fn first_shown_tab(n: usize, active: usize, count: usize) -> usize {
    let count = count.clamp(1, n.max(1));
    if active < count {
        0
    } else {
        (active + 1 - count).min(n.max(1) - count)
    }
}

/// Where a tab bar parks the tabs it does not show: far off-screen on its
/// main axis, kept in the segment list so indices stay put.
pub(super) const PARKED_TAB: f32 = -100_000.0;

/// Whether a rendered tab segment is on screen rather than parked.
pub(super) fn tab_shown(seg: &kettle_render::TabSeg) -> bool {
    seg.rect.0 > PARKED_TAB / 2.0 && seg.rect.1 > PARKED_TAB / 2.0
}

/// Rows of a side tab bar with `rows` rows for its tabs and `+` (the update
/// chip's row not counted). While every tab and `+` fit, the tabs stack from
/// the top and `+` follows the last. Otherwise `⌃` and `⌄` rows bracket as
/// many whole tabs as fit, chosen by [`first_shown_tab`] so the active tab is
/// always shown, with `+` below `⌄`. A strip too short for the arrows shows
/// the active tab and `+` alone, and one with a single row (or less) the
/// active tab alone: `plus` is then `rows` or more, a row the strip lacks.
/// Pure, like [`tab_strip_layout`].
#[derive(Debug, PartialEq)]
pub(super) struct SideTabRows {
    /// The first tab shown, and how many are shown from it.
    pub(super) first: usize,
    pub(super) shown: usize,
    /// The row of the first tab shown: 1 below `⌃`, else 0.
    pub(super) top: usize,
    /// The row of `+`; `⌄` is the row above it when `arrows`.
    pub(super) plus: usize,
    pub(super) arrows: bool,
}

pub(super) fn side_tab_rows(n: usize, active: usize, rows: usize) -> SideTabRows {
    let n = n.max(1);
    if n < rows {
        return SideTabRows {
            first: 0,
            shown: n,
            top: 0,
            plus: n,
            arrows: false,
        };
    }
    if rows < 4 {
        let shown = rows.saturating_sub(1).clamp(1, n);
        return SideTabRows {
            first: first_shown_tab(n, active, shown),
            shown,
            top: 0,
            plus: shown,
            arrows: false,
        };
    }
    let shown = rows - 3;
    SideTabRows {
        first: first_shown_tab(n, active, shown),
        shown,
        top: 1,
        plus: shown + 2,
        arrows: true,
    }
}

/// Split the trailing new-tab button into `(arrow_left, plus_right)`
/// hit-rects — the `▾` dropdown on the LEFT, the `+` on the RIGHT. `arrow_w` is
/// clamped to the button width so a degenerate (tiny) button can't yield a
/// negative `+` width. Pure → unit-tested and shared by the renderer geometry
/// + the click hit-test (single source of truth).
pub(super) fn split_new_tab_button(
    button: kettle_render::Rect4,
    arrow_w: f32,
) -> (kettle_render::Rect4, kettle_render::Rect4) {
    let (x, y, w, h) = button;
    let aw = arrow_w.clamp(0.0, w);
    ((x, y, aw, h), (x + aw, y, (w - aw).max(0.0), h))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NewTabHoverTarget {
    Menu,
    Plus,
}

pub(super) fn new_tab_hover_target(bar: &TabBar, x: f32, y: f32) -> Option<NewTabHoverTarget> {
    if bar.new_tab_menu.2 > 0.0 && rect_contains(bar.new_tab_menu, x, y) {
        Some(NewTabHoverTarget::Menu)
    } else if rect_contains(bar.new_tab, x, y) {
        Some(NewTabHoverTarget::Plus)
    } else {
        None
    }
}

/// Whether a pointer event belongs to the tab-bar chrome. `bar.height` is the
/// row height, not the full extent of a vertical strip, so using
/// `bar.y..bar.y + bar.height` silently makes every vertical row after the
/// first (including the new-tab button) non-interactive. `band` is the layout's
/// actual horizontal row or vertical column and is shared with rendering.
pub(super) fn tab_bar_pointer_region_contains(bar: &TabBar, x: f32, y: f32) -> bool {
    bar.height > 0.0 && rect_contains(bar.band, x, y)
}

/// Pure: when the mouse is over chrome (tab bar, status bar, update banner, or
/// any modal overlay), the OS cursor should be the standard arrow rather than
/// the text I-beam. Matches iTerm2 / WezTerm / Ghostty / kitty: chrome surfaces
/// are clickable, not selectable, so the I-beam is visually misleading there.
/// Returns `Some(Default)` for chrome, `None` to let the content-area caller
/// decide between `Pointer` (URL-hover) and `Text`.
pub(super) fn chrome_cursor_icon(in_chrome_band: bool, modal_open: bool) -> Option<CursorIcon> {
    if in_chrome_band || modal_open {
        Some(CursorIcon::Default)
    } else {
        None
    }
}

/// Pure: when the cursor is over a tab's `✕` close-button zone, override the
/// chrome `Default` with `Pointer` — the same "hand" icon every browser
/// (Chrome / Firefox / Safari) uses to telegraph "this glyph is clickable."
/// Composes with `chrome_cursor_icon`: close-hover wins over the bar's
/// `Default` so the affordance is visible the moment the cursor lands on
/// the close zone. Returns `None` when not over a close button so the
/// content-area decision still gets a chance.
pub(super) fn tab_close_hover_icon(over_close: bool) -> Option<CursorIcon> {
    if over_close {
        Some(CursorIcon::Pointer)
    } else {
        None
    }
}

/// Pure: while a tab-drag gesture is armed or in
/// progress, override every other cursor decision with the "holding
/// something" hand: `Grab` the moment a press arms the FSM (the gesture
/// could still resolve to a plain click), `Grabbing` once movement promotes
/// it to a real drag. In-window reorder and cross-window tear share
/// `detach::DragState`, so one helper covers both. `None` from `Idle`
/// defers to every other rule. No `detachable-tabs` re-check needed: the
/// press handler only arms the FSM when the config gate is on, so a
/// non-`Idle` state already implies it (see
/// `detachable_tabs_config_gates_all_detach_paths`).
pub(super) fn tab_drag_cursor_icon(state: &crate::detach::DragState) -> Option<CursorIcon> {
    match state {
        crate::detach::DragState::Idle => None,
        crate::detach::DragState::ArmedInside { .. } => Some(CursorIcon::Grab),
        crate::detach::DragState::DraggingInside { .. }
        | crate::detach::DragState::DraggingOutside { .. } => Some(CursorIcon::Grabbing),
    }
}

/// Pure: which tab segment's close-button (`✕`) rect contains the cursor,
/// if any. Walks `segments` once and returns the first hit (segments don't
/// overlap in the tab bar layout; cycle-tab-bar invariant). Used both to
/// drive the OS pointer-cursor swap and the renderer's hover-background
/// quad — the visual affordance that makes the `✕` read as a button rather
/// than a trailing character in the title text.
pub(super) fn hovered_close_button(
    segments: &[kettle_render::TabSeg],
    px: f32,
    py: f32,
) -> Option<usize> {
    let in_rect = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        px >= rx && px < rx + rw && py >= ry && py < ry + rh
    };
    segments.iter().find(|s| in_rect(s.close)).map(|s| s.idx)
}

#[cfg(test)]
pub(super) fn cursor_in_tab_bar_band(y: f32, bar_h: f32, surface_h: f32, pos: TabBarPos) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    // Left/Right strips span the full window height, so every in-window y is in
    // the band. `cursor_in_tab_bar` checks x against the strip's edge.
    match pos {
        TabBarPos::Top => y >= 0.0 && y < bar_h,
        TabBarPos::Bottom => y >= (surface_h - bar_h) && y <= surface_h,
        TabBarPos::Left | TabBarPos::Right => y >= 0.0 && y <= surface_h,
    }
}

pub(super) fn status_bar_y(
    surface_h: f32,
    bar_h: f32,
    pos: kettle_config::StatusBarMode,
    tab_top_h: f32,
    search_h: f32,
) -> f32 {
    match pos {
        kettle_config::StatusBarMode::Top => tab_top_h,
        kettle_config::StatusBarMode::Bottom => surface_h - search_h - bar_h,
        kettle_config::StatusBarMode::Off => 0.0,
    }
}

/// Sibling of `cursor_in_tab_bar_band` for the status bar, so the
/// chrome-cursor logic treats both bars uniformly. The status strip is outside
/// every pane rect and the tab-bar band, so without this check hovering it
/// falls through to the "over a pane" branch and shows the I-beam.
pub(super) fn cursor_in_status_bar_band(
    y: f32,
    bar_h: f32,
    surface_h: f32,
    pos: kettle_config::StatusBarMode,
    tab_top_h: f32,
    search_h: f32,
) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    let bar_y = status_bar_y(surface_h, bar_h, pos, tab_top_h, search_h);
    match pos {
        kettle_config::StatusBarMode::Off => false,
        kettle_config::StatusBarMode::Top => y >= bar_y && y < bar_y + bar_h,
        kettle_config::StatusBarMode::Bottom => y >= bar_y && y <= bar_y + bar_h,
    }
}

/// Width of the strip that horizontal tab segments tile across, given the
/// surface width and the trailing new-tab button geometry.
///
/// `tab_bar()` lays segments out across this width, and the drag-to-reorder
/// handler hit-tests those rendered segments, so the drag target can't drift
/// from what is drawn. `arrow_w` is `0.0` when the dropdown is absent
/// (vertical bars). Floored at `plus_w` so a very narrow bar still reserves room
/// for the `+` button.
pub(super) fn tab_segment_strip_width(surface_w: f32, plus_w: f32, arrow_w: f32) -> f32 {
    (surface_w - plus_w - arrow_w).max(plus_w)
}

/// The shortest a focus change waits before reading the update facts again.
const UPDATE_FACTS_THROTTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// The ✕ hit zone for one tab segment, or an EMPTY rect when the button is
/// configured off.
///
/// With `close-button-on-tab = false`, a full-size hit rect would still be
/// probed by the press handler, the `ctl` press handler, and the pointer-cursor
/// hover test. Clicking a tab's trailing square would then close it through an
/// invisible button, killing every pane in that tab. A zero-size rect fixes all
/// three, because every consumer tests with a strict `px < rx + rw`, which no
/// zero-width rect can satisfy. `tab_title_rect` reads `close.0` only when the
/// button is visible, so the reclaimed label space is unaffected.
pub(super) fn tab_close_rect(
    trailing_x: f32,
    y: f32,
    height: f32,
    close_visible: bool,
) -> kettle_render::Rect4 {
    if close_visible {
        (trailing_x, y, height, height)
    } else {
        (trailing_x, y, 0.0, 0.0)
    }
}

/// Text lane inside a tab segment. The close button is visible chrome, so the
/// label must be centered in the lane that remains before that button rather
/// than centered against the full segment and visually drifting under `✕`.
pub(super) fn tab_title_rect(
    segment: kettle_render::Rect4,
    close: kettle_render::Rect4,
    close_visible: bool,
) -> kettle_render::Rect4 {
    let (x, y, w, h) = segment;
    let left = x + 8.0;
    let right = if close_visible { close.0 } else { x + w - 8.0 };
    (left, y, (right - left).max(0.0), h)
}

/// Should the new-tab `▾` dropdown arrow be shown? Always yes. Besides the
/// shells, the dropdown carries Settings / Command palette / About rows
/// (Windows Terminal's bottom section), so it is never a pointless one-item
/// menu, even on a bash-only Ubuntu.
fn new_tab_dropdown_visible() -> bool {
    true
}
