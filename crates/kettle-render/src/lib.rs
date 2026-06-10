//! GPU renderer: wgpu surface + glyphon (cosmic-text) glyph atlas for text,
//! plus an instanced quad pipeline for cell backgrounds, cursor, selection,
//! search highlights, split dividers, focus borders and the tab bar.
//!
//! Multiple panes are tiled in a single frame: each pane gets its own
//! cosmic-text buffer clipped to its rectangle; all backgrounds/UI go through
//! one instanced quad pass and all text through one glyphon prepare/render.
//!
//! Pipeline order per frame (matters for transparency + dim overlays):
//! 1. **Quads** — cell backgrounds, tab-bar chrome, focus borders,
//!    cursor block / beam / underline / hollow outline. Active-tab + focused-
//!    pane accents flip to theme `palette[3]` (yellow) while broadcast
//!    mode is on so the user can see input is fan-out.
//! 2. **Images** — kitty graphics / Sixel / iTerm2 OSC 1337 placements
//!    composited per-pane with scrollback-anchored Y coords.
//! 3. **Text** — glyphon `prepare` + `render`. Per-cell SGR resolution +
//!    `Flags::DIM` half-blend + WCAG minimum-contrast lift (`minimum-contrast`
//!    config) all happen here so they compose cleanly against the
//!    backgrounds laid down by step 1.
//! 4. **Overlay quads** — a *second* instanced pass for post-text chrome:
//!    unfocused-pane dimming (theme bg at `1 - unfocused-split-opacity`)
//!    and the per-pane scrollback scrollbar thumb. Drawn after text so the
//!    dim actually covers glyphs.
//!
//! Modules (private; see source):
//! - `color` — palette/cube/named-color resolution against the active
//!   `Theme`, WCAG luminance + contrast-lift, SGR `Dim` half-blend,
//!   OSC 4/10/11/12 query-reply formatting.
//! - `quad` — `QuadPipeline` + `QuadInstance`. Reused twice per frame
//!   (one instance for the main pass, one for the post-text overlay).
//! - `imgpipe` — sampled-texture image-blit pipeline, used for kitty /
//!   Sixel / iTerm2 placements.
//!
//! Headless paths: [`capture_png`] builds an offscreen device + texture
//! chain, renders one representative frame, and copies it back via
//! `copy_texture_to_buffer` — powers `kettle --screenshot`.
//! [`offscreen_selftest`] compiles the WGSL shaders + sets up a tiny
//! pipeline without ever creating a Surface, so CI can validate the GPU
//! path under Xvfb without a real display.

mod bg_image;
mod color;
mod imgpipe;
mod quad;

pub use bg_image::{BgImage, decode_bg_image, decode_bg_image_with_blur};

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

// Cycle 721 (2026-05-23): named constants for the right-click
// context-menu chrome. Pre-721 these magic numbers (12.0 row-pad,
// 8.0 sep-h, 40.0 horiz-pad, 180.0 min-w, 80.0 surface-breathing)
// were duplicated across 16 sites in `kettle-render/src/lib.rs` +
// `kettle-ui/src/app.rs`; the duplication made the cycle-682 +
// cycle-714 layout-math changes a 16-line search-and-replace
// instead of a 1-line edit. Re-exported so `kettle-ui` can pull
// them in via `use kettle_render::menu;` instead of redeclaring.
pub mod menu {
    /// Vertical padding inside each context-menu row. Cell-height +
    /// MENU_ROW_PAD = total row height (~28-32 px on default cell
    /// metrics — a comfortable click target).
    pub const ROW_PAD: f32 = 12.0;
    /// Separator row height. Smaller than a regular row so the menu
    /// reads as grouped without wasting vertical space.
    pub const SEP_H: f32 = 8.0;
    /// Horizontal padding inside the panel: `max_chars * cw + H_PAD`.
    /// Gives the longest label breathing room and lets short labels
    /// (Copy) still feel like a real menu surface.
    pub const H_PAD: f32 = 40.0;
    /// Minimum panel width — overrides the chars-based math when the
    /// longest label is tiny.
    pub const MIN_W: f32 = 180.0;
    /// Top + bottom breathing room reserved when clamping the panel
    /// height to the surface (cycle 714 scrollable submenus). Keeps
    /// the menu from kissing the window edge.
    pub const PANEL_BREATHING: f32 = 80.0;
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

/// One row of the right-click context menu. Action labels are owned
/// `String` so the UI can build them on-demand (e.g. conditionally
/// enable Copy based on whether a selection exists); the renderer
/// stays agnostic of the `Action` enum that drives them.
pub struct ContextMenuRow {
    pub label: String,
    /// `true` when the row is a horizontal separator rather than a
    /// selectable item. The renderer draws a thin divider line and
    /// the UI skips it during keyboard / mouse highlight changes.
    pub separator: bool,
    /// Greyed-out (e.g. Copy with no selection). Still drawn, still
    /// gives the user a sense of "this is an option that's not
    /// available right now," but not selectable.
    pub enabled: bool,
}

/// Right-click context menu (Terminator / GNOME Terminal / iTerm2
/// parity). Drawn as a floating panel anchored at the click point;
/// the UI clamps the anchor so the panel fits the surface. The
/// renderer reads this slice each frame and draws if `Some` — the
/// UI is the single source of truth for state + dispatch.
pub struct ContextMenu {
    /// Top-left of the panel in surface pixels (already clamped).
    pub anchor: (f32, f32),
    pub rows: Vec<ContextMenuRow>,
    /// Index of the currently highlighted (selectable) row. Always
    /// points at an enabled, non-separator row when the menu is
    /// non-empty.
    pub highlight: usize,
    /// Cycle 714 (Terminator menu UX, C5): index of the first row
    /// the renderer should paint. Rows `0..scroll_offset` are
    /// scrolled off-panel; the renderer also stops drawing when
    /// the accumulated row height exceeds `panel_h_clamped`. Zero
    /// means "show from the top" (the pre-cycle-714 default).
    pub scroll_offset: usize,
    /// Cycle 714. Panel height after the surface clamp (App-side
    /// `context_menu_geometry` already applies the clamp); the
    /// renderer reuses it to decide which rows are visible + to
    /// position the ▲/▼ arrows. Zero means "no clamp", in which
    /// case the renderer falls back to the natural panel height.
    pub panel_h_clamped: f32,
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
    /// Cycle 708 (Terminator parity, `layoutlauncher.py`):
    /// `Some(typed)` while the layout picker is open. Same UX
    /// surface as the command palette but the hint string lists
    /// layout names from `Session::list_layouts`.
    pub layout_picker_query: Option<String>,
    pub layout_picker_hint: String,
    /// Cycle 372 (Terminator parity, edit-title overlay UX): the
    /// in-progress title-edit text + a scope label for the prompt
    /// (e.g. "Edit window title:" / "Edit tab title:" /
    /// "Edit pane title:"). `None` when no edit is in progress.
    ///
    /// Cycle 395: optional anchor_y. When `Some(y)`, the overlay
    /// renders at that surface y-position (used to anchor near
    /// the clicked pane's titlebar for EditPaneTitle). When `None`,
    /// renders at the window-bottom (window/tab scopes).
    pub edit_title: Option<(String, String, Option<f32>)>,
    /// Window has keyboard focus (solid vs hollow cursor, pane dimming).
    pub window_focused: bool,
    /// Cursor is in its "on" blink phase.
    pub cursor_visible: bool,
    /// Visual-bell intensity, 0.0 (none) .. 1.0 (just rang).
    pub bell: f32,
    /// `Some` while the right-click context menu is open. Rendered on
    /// top of everything else so an overlapping pane border doesn't
    /// occlude the menu.
    pub context_menu: Option<ContextMenu>,
    /// Cycle 300 vi-mode cursor (sub-cycle 3 of 4). `Some((row,col))`
    /// while the user is in vi-mode; the renderer paints a 1-cell
    /// outlined block at that grid position in the focused pane.
    /// Different chrome from the terminal cursor (block vs outline)
    /// so the user can tell the two modes apart at a glance.
    pub vi_cursor: Option<(usize, usize)>,
    /// Cycle 301 vi-mode visual selection (sub-cycle 4). `Some` when
    /// the user has pressed `v` to start a selection. The renderer
    /// highlights cells from `vi_visual_anchor` to `vi_cursor`
    /// (inclusive both ends) using theme.selection_background.
    pub vi_visual_anchor: Option<(usize, usize)>,
    /// Cycle 660 (sub-cycle 3 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
    /// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)): when
    /// `Some`, render a centered modal dialog over a dimming
    /// backdrop. The renderer paints the prompt + button row;
    /// the button at `focus_idx` gets the accent-border treatment.
    pub confirm_dialog: Option<ConfirmDialogOverlay>,
    /// Cycle 756: `Some` while the in-app settings overlay is open. Painted
    /// centered, above panes but below the confirm dialog.
    pub settings: Option<SettingsOverlay>,
    /// Cycle 794: `Some((tag, url))` while the "a newer kettle release is
    /// available" banner is showing. Rendered as a passive, lowest-priority
    /// bottom bar — any real modal (search/palette/…) takes the bar instead,
    /// and it returns when they close. Dismissed with Esc, opened with Enter.
    pub update_available: Option<(String, String)>,
}

/// Cycle 660: renderer-side projection of `App::confirm_dialog`.
/// Stripped of dispatch state — just the bits needed to paint.
#[derive(Debug, Clone)]
pub struct ConfirmDialogOverlay {
    /// Prompt text shown at the top of the modal.
    pub prompt: String,
    /// Button labels in display order (Cancel typically first).
    pub buttons: Vec<ConfirmDialogButton>,
    /// Which button has focus (idx into `buttons`).
    pub focus_idx: usize,
}

/// Cycle 660: paint-side button shape. `destructive: true` gets
/// the red-accent treatment (Close / Delete buttons).
#[derive(Debug, Clone)]
pub struct ConfirmDialogButton {
    pub label: String,
    pub destructive: bool,
}

/// Cycle 756: renderer-side projection of `App::settings_nav` + the resolved
/// field values. The UI computes labels/values (reading `Config`); the renderer
/// just paints a centered panel — a row of category tabs, then label/value
/// rows for the active category, with the focused row highlighted.
#[derive(Debug, Clone)]
pub struct SettingsOverlay {
    /// Category tab names, in order.
    pub categories: Vec<String>,
    /// Index of the active category (its tab is highlighted, its rows shown).
    pub active_category: usize,
    /// The active category's fields as (label, current-value) pairs.
    pub rows: Vec<SettingsRow>,
    /// Index into `rows` of the focused field (gets the accent highlight).
    pub focused_row: usize,
}

/// Cycle 756: one settings row — a human label and its current value string.
#[derive(Debug, Clone)]
pub struct SettingsRow {
    pub label: String,
    pub value: String,
}

/// Pixel rectangle `(x, y, w, h)`.
pub type Rect4 = (f32, f32, f32, f32);

/// Activity state of a tab — `Normal` draws no indicator, `Output`
/// draws a small cyan dot, `Bell` draws a yellow dot. Terminator-
/// parity affordance ("you've got new output in an inactive tab")
/// cycle 246. Renderer-side enum so the UI doesn't need to leak its
/// `kettle_ui::mux::TabActivity` type across crate boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabActivity {
    #[default]
    Normal,
    Output,
    Bell,
    /// Cycle 252: inactive tab had unseen output but went quiet for
    /// at least `tab-silence-threshold-ms`. Terminator's "Silence
    /// Watcher" affordance — useful for tail-following long jobs
    /// (`tail -f`, build watchers) where the *absence* of output is
    /// the signal the user wants. Drawn as a dim chrome-gray dot,
    /// distinct from `Output` (cyan) and `Bell` (yellow).
    Silent,
}

/// One tab segment in the tab bar.
pub struct TabSeg {
    pub idx: usize,
    /// Full segment rect.
    pub rect: Rect4,
    /// Close-button (✕) hit rect within the segment.
    pub close: Rect4,
    pub title: String,
    pub active: bool,
    /// Inactive-tab activity (cycle 246). Always `Normal` on the
    /// active segment so the focused-tab accent isn't doubled-up by
    /// a redundant dot.
    pub activity: TabActivity,
}

/// The tab bar geometry — computed once in the UI, used for both drawing
/// (here) and click hit-testing (app), so there is a single source of truth.
/// Cycle 296: thin status-bar strip at the top or bottom of the
/// surface. Disabled by default; when on, the App sets `height` > 0
/// and supplies a pre-formatted single-line string.
///
/// Content is a free-form `String` so the App can compose whatever
/// it wants (the cycle-295 default: "HH:MM:SS · theme · pane title").
/// Renderer just draws background + text; layout / refresh / content
/// composition all live in the App.
pub struct StatusBar {
    /// Height in px (0 = hidden).
    pub height: f32,
    /// Top-left y of the strip. 0 for top position, `surface_h - h`
    /// for bottom.
    pub y: f32,
    /// Pre-formatted content (single line).
    pub text: String,
}

impl StatusBar {
    pub fn hidden() -> Self {
        StatusBar {
            height: 0.0,
            y: 0.0,
            text: String::new(),
        }
    }
}

pub struct TabBar {
    /// Bar height in px (0 = hidden).
    pub height: f32,
    /// Top-left Y of the bar (0 for top position, `surface_h - h` for bottom).
    pub y: f32,
    pub segments: Vec<TabSeg>,
    /// The trailing "new tab" (+) button rect.
    pub new_tab: Rect4,
    /// Cycle 805: the `▾` dropdown-arrow rect, immediately LEFT of `new_tab`.
    /// Clicking it opens the new-tab shell chooser. Zero-area `(0,0,0,0)` when
    /// the dropdown is disabled (vertical tab bars) — the renderer then draws a
    /// plain `+` and the hit-test skips the arrow branch.
    pub new_tab_menu: Rect4,
    /// Cycle 178: visual indicator that broadcast / group-input mode is
    /// on. Without this, the user can forget broadcast is enabled and
    /// type to one pane expecting it to stay local — every keystroke
    /// goes to every pane in the active tab silently. The renderer
    /// tints the active tab segment with a warning accent when set;
    /// inactive tabs (which aren't affected by broadcast) stay normal.
    pub broadcast: bool,
    /// Index of the segment whose `✕` close button is currently
    /// hovered by the mouse — used to draw a hover background so the
    /// user can tell the trailing glyph is a real button. Browser /
    /// Chrome / Firefox convention: the ✕ shows on every tab but the
    /// background only appears on hover. Computed in the UI's cursor-
    /// sync path so the renderer has zero geometry knowledge.
    pub hovered_close_idx: Option<usize>,
    /// Cycle 255 ghost-drag indicator: `Some(cursor_x)` while a
    /// left-button drag is in progress in the tab bar. The renderer
    /// draws a translucent overlay copy of the dragged (active) tab
    /// segment centered at `cursor_x`, so the user sees what's being
    /// moved while the underlying segments snap into place via
    /// `Mux::move_active_tab`. `None` while no drag is active.
    pub drag_cursor_x: Option<f32>,
}

impl TabBar {
    pub fn hidden() -> Self {
        TabBar {
            height: 0.0,
            y: 0.0,
            segments: Vec::new(),
            new_tab: (0.0, 0.0, 0.0, 0.0),
            new_tab_menu: (0.0, 0.0, 0.0, 0.0),
            broadcast: false,
            hovered_close_idx: None,
            drag_cursor_x: None,
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
    ///
    /// Borrowed (cycle 852, audit): the backing `Vec` lives in the per-frame
    /// `guards` collection for the whole frame — exactly like `term` borrows the
    /// `MutexGuard` — so the renderer reads it without a second per-pane clone.
    pub images: &'a [kettle_core::Placement],
    /// Cycle 382 (Terminator parity, per-pane-titlebar Bucket-D
    /// sub-cycle 3 follow-up): the pane's title — rendered into
    /// the cycle-379 titlebar background quad when
    /// cfg.show_titlebar = true. Borrowed from `guards` (cycle 852).
    pub title: &'a str,
    /// Cycle 386 (Terminator parity, per-pane-titlebar Bucket-D
    /// sub-cycle 6): pane terminal size in cols × rows. Appended
    /// to the titlebar title text as `WxH` unless
    /// cfg.title_hide_sizetext is true.
    pub size_cols: u16,
    pub size_rows: u16,
    /// Cycle 386 (Terminator parity, sub-cycle 6): bell-state
    /// indicator for the pane. When true and cfg.icon_bell is
    /// also true, a small dot renders in the titlebar.
    pub bell: bool,
    /// Cycle 406 (Terminator parity, titlebar Bucket-D sub-cycle 8):
    /// optional named broadcast group. When `Some(name)`, the
    /// titlebar prefixes `[name]` (group label in brackets)
    /// before the pane title. Borrowed from `guards` (cycle 852).
    pub group_name: Option<&'a str>,
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
    /// Cycle 827 (audit): pooled scratch for `build_pane`'s per-cell style runs,
    /// reused across frames. Both the Vec backing store AND each run's `String`
    /// buffer are recycled (the builder writes into slots by index rather than
    /// pushing fresh `String`s), so a busy colored pane no longer mints dozens–
    /// hundreds of `String` allocations on the 60 fps render hot path. Same
    /// high-water-mark pooling as `pane_buffers`.
    span_scratch: Vec<(String, Rgb, bool, bool)>,
    /// Cycle 827: pooled scratch for `build_pane`'s line-break indices.
    span_breaks_scratch: Vec<usize>,
    /// Cycle 853 (audit): pooled scratch for the per-frame cell/UI quad list
    /// (`render_frame_with_status` filled a fresh `Vec` of `panes*16+256`
    /// `QuadInstance`s every frame). Taken + cleared at the top of the frame,
    /// returned after the GPU upload — same high-water pooling as `span_scratch`.
    quad_scratch: Vec<QuadInstance>,
    /// Cycle 382 (Terminator parity, per-pane-titlebar Bucket-D
    /// sub-cycle 3): one TextBuffer per pane for the title text
    /// drawn in the cycle-379 titlebar quad. Reused across redraws
    /// to amortize allocation; trimmed/grown alongside pane_buffers.
    pane_titlebar_buffers: Vec<TextBuffer>,
    tab_buffers: Vec<TextBuffer>,
    hint_buffers: Vec<TextBuffer>,
    /// One text buffer per row of the right-click context menu. Reused
    /// across openings to amortize allocation; trimmed when the row
    /// count shrinks for a smaller menu.
    context_menu_buffers: Vec<TextBuffer>,
    /// Cycle 756: one text buffer per display line of the settings overlay
    /// (title, category tabs, field rows, footer). Grown + truncated like the
    /// context-menu pool.
    settings_buffers: Vec<TextBuffer>,
    tabbar_buffer: TextBuffer,
    /// Cycle 805: the `▾` new-tab dropdown-arrow glyph, in its own buffer
    /// (drawn left of `+`) so it lands precisely in `new_tab_menu` and the `+`
    /// stays put in `new_tab`. Unused when the dropdown is disabled.
    new_tab_arrow_buffer: TextBuffer,
    /// Single shared `✕` glyph buffer reused for every tab's close
    /// button. Rendered separately from the title text so we can:
    /// 1. Color it independently (dim at rest, bright red on hover).
    /// 2. Position it precisely inside `seg.close` rather than letting
    ///    the title's last character drift across segment widths.
    ///
    /// One buffer, N positions via per-tab `TextArea` instances.
    tab_close_buffer: TextBuffer,
    search_buffer: TextBuffer,
    /// Cycle 296: status-bar text. Single line, reused every frame
    /// via `set_text` — same one-buffer pattern `tabbar_buffer` uses
    /// for tab labels. Stays at length 0 when the status bar is off.
    status_bar_buffer: TextBuffer,

    quads: QuadPipeline,
    /// Second quad pass drawn *after* text (pane dimming, scrollbar).
    overlay_quads: QuadPipeline,
    /// Third quad pass drawn after the overlay quads — reserved for
    /// the right-click context menu's shadow / panel / border /
    /// highlight quads. Lives in its own pass so the menu's text
    /// (rendered by `menu_text_renderer` below) lands *on top of* the
    /// panel bg rather than underneath it. Cycle 251 split this out
    /// after v1.3.0+v1.3.1 shipped a blank menu — opaque panel-bg
    /// quad in `overlay_quads` was painted on top of the menu text
    /// (which was bundled with all other text in the single
    /// `text_renderer.render` call between `quads.draw` and
    /// `overlay_quads.draw`).
    menu_quads: QuadPipeline,
    /// Dedicated TextRenderer for the context-menu rows. Shares
    /// `atlas` + `viewport` with `text_renderer` (glyphon allows
    /// multiple renderers against one atlas); rendered as the final
    /// pass so menu labels sit above the panel bg.
    menu_text_renderer: TextRenderer,
    imgs: imgpipe::ImagePipeline,
    /// Cycle 388 (Terminator parity, bg-image Bucket-D sub-cycles
    /// 3+4): decoded background-image cache. Tuple of
    /// (cfg.background_image path, decoded ImageData). Invalidated
    /// + re-decoded when the config path changes.
    // Cycle 892 (audit): key is `(path, blur_radius)` — keying on the path
    // alone meant toggling `background-blur` was ignored on reload unless
    // the image path *also* changed. The value is the decoded RGBA (up to
    // ~256 MiB); it is freed (`= None`) when the config moves away from
    // `background-type = image` so a large wallpaper doesn't sit resident
    // for the rest of the session after the user turns it off.
    //
    // Cycle 918: the inner `Option<ImageData>` is `None` when the current
    // (path, blur) was tried and FAILED to decode. Caching the failed key
    // (a) stops rendering the previous wallpaper after the path changes to a
    // broken one, and (b) stops re-attempting the failing decode every frame.
    bg_image_cache: Option<(String, u32, Option<kettle_core::ImageData>)>,
    /// Cycle 919 (audit L2): when the current bg-image (path, blur) FAILED to
    /// decode, the earliest `Instant` to retry — throttling self-heal to ≥3s so
    /// a broken/corrupt path isn't re-decoded every frame. `None` once a decode
    /// succeeds (the loaded wallpaper never re-decodes) or while no bg image is
    /// configured.
    bg_image_retry_at: Option<std::time::Instant>,

    /// `Arc<str>` so `render_frame_with_status`'s per-frame
    /// `self.font_family.clone()` (needed to satisfy the borrow checker while
    /// `&mut self.font_system` is held alongside ~20 `Family::Name(&family)`
    /// reads) is a refcount bump, not a heap alloc + memcpy at 60fps. `Arc<str>`
    /// derefs to `str`, so every `Family::Name(&family)` site is unchanged
    /// (cycle 845, audit).
    font_family: Arc<str>,
    font_size: f32,
    metrics: Metrics,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Cycle 636 (Terminator parity, `cell_width` / `cell_height`):
    /// multiplicative scale applied to the measured cell metrics.
    /// `(1.0, 1.0)` is the default — measured dimensions unchanged.
    /// `(1.0, 1.5)` would space lines 50% taller; useful for users
    /// with strong vision needs or fonts whose default leading is
    /// too tight. Range clamped to `[0.5, 3.0]` at the config-parse
    /// layer (kettle-config/src/lib.rs:cell-width/cell-height arms).
    pub cell_scale_w: f32,
    pub cell_scale_h: f32,
    pub scale: f32,
    /// Cycle 654 (sub-cycle 3 of
    /// [`TERMINATOR-TERMINALSHOT-DESIGN.md`](../../../docs/TERMINATOR-TERMINALSHOT-DESIGN.md)):
    /// when `Some`, the next `render_frame` call should also do a
    /// surface-readback into a staging buffer + dispatch a PNG
    /// encode off-thread. v1 of this field is the storage only —
    /// sub-cycle 4 wires the actual readback. `App::dispatch`
    /// for `Action::TakeScreenshot` (cycle 640) sets this via
    /// `set_pending_screenshot()` after computing the path via
    /// the cycle-650 `session_screenshot_path` helper.
    pub pending_screenshot: Option<ScreenshotRequest>,
}

/// Cycle 654: a queued screenshot request. Sub-cycle 4 of
/// terminalshot design will consume this in `render_frame`.
#[derive(Debug, Clone)]
pub struct ScreenshotRequest {
    /// Where to save the PNG. Caller already computed this via
    /// `session_screenshot_path(unix_secs, pid, cache_dir)`.
    pub out_path: std::path::PathBuf,
    /// If `Some`, crop the captured frame to this pixel rect
    /// (the focused pane's geometry). If `None`, capture the
    /// whole window.
    pub crop: Option<(f32, f32, f32, f32)>,
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
        let adapter = request_adapter_or_fallback(
            &instance,
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            },
            "Renderer::new",
        )
        .await?;
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
        // Cycle 148: pick a real alpha-aware mode when the user wants
        // transparency. The previous `caps.alpha_modes[0]` just took
        // whatever the backend listed first — usually `Opaque`, which
        // ignores the alpha channel from `Color { a: ... }` on the
        // clear ops. So `background-opacity = 0.5` rendered as fully
        // opaque on most surfaces. Prefer `PreMultiplied` (the
        // standard for compositing) when opacity < 1.0, falling back
        // through `PostMultiplied` → `Inherit` → `Auto` → whatever's
        // first if nothing fancier is available. Opaque configs
        // stay opaque (matching the surface's default behavior).
        let want_transparency = cfg.background_opacity < 1.0;
        let alpha_mode = if want_transparency {
            [
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::PostMultiplied,
                wgpu::CompositeAlphaMode::Inherit,
                wgpu::CompositeAlphaMode::Auto,
            ]
            .into_iter()
            .find(|m| caps.alpha_modes.contains(m))
            .unwrap_or(caps.alpha_modes[0])
        } else {
            caps.alpha_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            // Cycle 688 (sub-cycle 4 of TERMINATOR-TERMINALSHOT-DESIGN.md):
            // add COPY_SRC so the cycle-654 pending_screenshot path
            // can read back the live surface. Most desktop adapters
            // support this fine; mobile may need a fallback to a
            // separate intermediate texture (deferred polish).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode,
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
        // Cycle 747: physical-pixel metrics — logical font size × DPI scale.
        let metrics = metrics_for(font_size, scale);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let tabbar_buffer = TextBuffer::new(&mut font_system, metrics);
        let new_tab_arrow_buffer = TextBuffer::new(&mut font_system, metrics);
        let tab_close_buffer = TextBuffer::new(&mut font_system, metrics);
        let search_buffer = TextBuffer::new(&mut font_system, metrics);
        let status_bar_buffer = TextBuffer::new(&mut font_system, metrics);
        let (cell_w, cell_h) =
            measure_cell(&mut font_system, &mut measure, &cfg.font_family, metrics);
        // Cycle 636: honor cfg.cell_width / cell_height multipliers
        // (Terminator parity). Values are pre-clamped to [0.5, 3.0]
        // at parse time so the cell can't degenerate to 0 here.
        let cell_scale_w = cfg.cell_width.max(0.01);
        let cell_scale_h = cfg.cell_height.max(0.01);
        let cell_w = cell_w * cell_scale_w;
        let cell_h = cell_h * cell_scale_h;

        let quads = QuadPipeline::new(&device, format);
        let overlay_quads = QuadPipeline::new(&device, format);
        let menu_quads = QuadPipeline::new(&device, format);
        let menu_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
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
            span_scratch: Vec::new(),
            quad_scratch: Vec::new(),
            span_breaks_scratch: Vec::new(),
            pane_titlebar_buffers: Vec::new(),
            tab_buffers: Vec::new(),
            hint_buffers: Vec::new(),
            context_menu_buffers: Vec::new(),
            settings_buffers: Vec::new(),
            tabbar_buffer,
            new_tab_arrow_buffer,
            tab_close_buffer,
            search_buffer,
            status_bar_buffer,
            quads,
            overlay_quads,
            menu_quads,
            menu_text_renderer,
            imgs,
            bg_image_cache: None,
            bg_image_retry_at: None,
            font_family: cfg.font_family.as_str().into(),
            font_size,
            metrics,
            cell_w,
            cell_h,
            cell_scale_w,
            cell_scale_h,
            scale,
            pending_screenshot: None,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        // Cycle 394 (Terminator parity, bg-image Bucket-D sub-cycle 8):
        // explicit resize handler for the background-image render
        // path. The cycle-388 bg-image cache stores the DECODED
        // image (not a window-sized texture); the cycle-390 UV-mode
        // dispatch recomputes the image rect from the current
        // surface dims every frame via build_frame. So a resize
        // implicitly takes effect on the next frame — no manual
        // texture re-upload needed.
        //
        // This comment closes the docs/TERMINATOR-BG-IMAGE-DESIGN.md
        // sub-cycle 8 with the "implicit per-frame recompute"
        // contract documented so a future contributor sees that
        // the per-frame recompute IS the impl.
        //
        // Floor at 1 (`surface.configure(0, …)` panics) and ceiling
        // at the device's max-texture-dimension-2d. The default wgpu
        // Limits cap that at 8192 px; an oversized window (stretched
        // across multiple 4K monitors, an 8K display, or a tiling
        // WM tile that exceeds the surface limit) used to make
        // `surface.configure` silently fail validation, leaving a
        // stale surface that paints nothing on the next frame. Clip
        // the surface to the device's announced limit so we still
        // render the visible top-left region cleanly even if the
        // user has unusually large geometry. Cycle 137 sibling to
        // cycle 119's `cap_axis_cells` (which fixed the same
        // class of bug on the `--screenshot` path).
        let max = self.device.limits().max_texture_dimension_2d.max(1);
        self.config.width = width.clamp(1, max);
        self.config.height = height.clamp(1, max);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = clamp_font_size(size);
        // Cycle 747: re-derive physical metrics at the current DPI scale so a
        // font-size change (zoom, reload) keeps HiDPI scaling applied.
        self.metrics = metrics_for(self.font_size, self.scale);
        self.remeasure_cell();
    }

    /// The current *logical* font size (the user-facing pt value, before the
    /// cycle-747 DPI multiply). Zoom keybinds step this rather than
    /// back-deriving it from the now-physical `cell_h`, which would otherwise
    /// double-apply the scale factor.
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Cycle 747: update the device-pixel scale factor (DPI). Wired to winit's
    /// `ScaleFactorChanged` — fired at startup and whenever the window moves to
    /// a monitor with a different scale. Recomputes physical metrics from the
    /// unchanged *logical* `font_size` and re-measures the cell, so glyphs keep
    /// the same visual size across DPI changes (and fixes tiny text that was
    /// the result of `scale` being stored but never applied). No-op when the
    /// scale is unchanged. The caller must re-grid afterward (cell_w/cell_h
    /// change), e.g. via `App::resize_all`.
    pub fn set_scale(&mut self, scale: f32) {
        let s = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if (self.scale - s).abs() < f32::EPSILON {
            return;
        }
        self.scale = s;
        self.metrics = metrics_for(self.font_size, s);
        self.remeasure_cell();
    }

    /// Update the primary font family and re-measure the cell. Called by
    /// `reload_config` so a `font-family = …` change in the user's config
    /// actually takes effect at runtime — without this, the renderer kept
    /// the family it was constructed with forever and only the `font-size`
    /// part of a reload was visible (silent partial-apply, same family as
    /// the cycle-44+ "reload doesn't re-flow downstream caches" gap).
    pub fn set_font_family(&mut self, family: String) {
        if self.font_family.as_ref() == family.as_str() {
            return;
        }
        self.font_family = family.into();
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
        // Cycle 636: apply the configured cell_width/cell_height
        // multipliers AFTER measurement so a font-family or font-
        // size change preserves the user's chosen scale.
        self.cell_w = cw * self.cell_scale_w.max(0.01);
        self.cell_h = ch * self.cell_scale_h.max(0.01);
    }

    /// Cycle 654 (sub-cycle 3 of terminalshot design): queue a
    /// screenshot request to be honored on the next `render_frame`.
    /// Replaces any pending request — only the latest one wins on
    /// rapid-fire triggers. The renderer consumes + clears this slot
    /// during the next paint.
    pub fn set_pending_screenshot(&mut self, req: ScreenshotRequest) {
        self.pending_screenshot = Some(req);
    }

    /// Cycle 654: peek + clear. Sub-cycle 4 will call this from
    /// inside `render_frame` after the wgpu surface is presented +
    /// the copy_texture_to_buffer is issued.
    pub fn take_pending_screenshot(&mut self) -> Option<ScreenshotRequest> {
        self.pending_screenshot.take()
    }

    /// Cycle 636 (Terminator parity, `cell_width` / `cell_height`):
    /// update the cell-scale multipliers + re-measure. Called by the
    /// App's `reload_config` path when the user reloads with a new
    /// `cell-width` / `cell-height` value. No-op when the requested
    /// scale matches the current one.
    pub fn set_cell_scale(&mut self, w: f32, h: f32) {
        let w = w.max(0.01);
        let h = h.max(0.01);
        if (self.cell_scale_w - w).abs() < f32::EPSILON
            && (self.cell_scale_h - h).abs() < f32::EPSILON
        {
            return;
        }
        self.cell_scale_w = w;
        self.cell_scale_h = h;
        self.remeasure_cell();
    }

    /// Render a full frame of tiled panes plus the tab bar and search overlay.
    pub fn render_frame(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
    ) -> Result<()> {
        self.render_frame_with_status(panes, tabbar, cfg, overlay, &StatusBar::hidden())
    }

    /// Cycle 296: extended `render_frame` variant that also draws the
    /// status-bar strip. The bare `render_frame` shim passes a hidden
    /// status bar, so existing call sites that don't yet know about
    /// the new feature still compile.
    pub fn render_frame_with_status(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
        status: &StatusBar,
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

        // Cycle 379+382 (Terminator parity, per-pane-titlebar):
        // hoisted alongside buffer allocation so the cycle-382
        // text-setting block can reference it. Same condition as
        // the cycle-379 quad render (cfg.show_titlebar && >1 pane).
        let pane_titlebar_h: f32 = if cfg.show_titlebar && panes.len() > 1 {
            ch + 6.0
        } else {
            0.0
        };
        // Ensure one text buffer per pane.
        while self.pane_buffers.len() < panes.len() {
            let b = TextBuffer::new(&mut self.font_system, metrics);
            self.pane_buffers.push(b);
        }
        // Cycle 382: parallel grow for per-pane titlebar buffers.
        while self.pane_titlebar_buffers.len() < panes.len() {
            let b = TextBuffer::new(&mut self.font_system, metrics);
            self.pane_titlebar_buffers.push(b);
        }
        // Cycle 749: release buffers for panes that have closed. The grow
        // loops above only ever extend, so without this the two vecs sat at
        // the session's high-water pane count — a 6-way split that you close
        // back to one pane left 5 idle TextBuffers (with their shaped glyph
        // runs) allocated for the rest of the session. Truncation is safe:
        // every later loop indexes by enumerate position `< panes.len()`.
        self.pane_buffers.truncate(panes.len());
        self.pane_titlebar_buffers.truncate(panes.len());
        // Cycle 382: write each pane's title into its titlebar
        // buffer NOW (before the later loops borrow self
        // immutably). pane_titlebar_h was computed earlier as
        // either 0.0 or ch+6.0; only do the mutation when active.
        if pane_titlebar_h > 0.0 {
            for (i, pv) in panes.iter().enumerate() {
                let (_, _, rw, _) = pv.rect;
                let title: &str = if pv.title.trim().is_empty() {
                    "kettle"
                } else {
                    pv.title
                };
                // Cycle 386: titlebar text = "  TITLE [WxH] [●]"
                // where:
                //   - [WxH] is shown unless cfg.title_hide_sizetext
                //   - [●] is shown when cfg.icon_bell && pv.bell
                // Cycle 682 (named-groups sub-cycle 6): when
                //   `pane.group_name = Some("fleet")`, prepend
                //   the group pill: "  [fleet] TITLE …".
                //   The render-side bracket gives it a visual
                //   weight without needing a separate quad
                //   shape (sub-cycle 7 can promote to a real
                //   colored chip).
                let mut label = String::new();
                if let Some(g) = pv.group_name
                    && !g.is_empty()
                {
                    label.push_str(&format!("  [{g}]"));
                }
                label.push_str(&format!("  {title}"));
                if !cfg.title_hide_sizetext {
                    label.push_str(&format!("  {}x{}", pv.size_cols, pv.size_rows));
                }
                if cfg.icon_bell && pv.bell {
                    label.push_str("  \u{1F514}");
                }
                let buf = &mut self.pane_titlebar_buffers[i];
                buf.set_metrics(&mut self.font_system, metrics);
                buf.set_size(&mut self.font_system, Some(rw), Some(pane_titlebar_h));
                buf.set_text(
                    &mut self.font_system,
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Basic,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Cycle 761: pre-size the per-frame quad/image vectors so the render
        // hot path doesn't repeatedly reallocate as they grow (borders +
        // per-pane chrome + cell-background quads dominate `quads`). Capacities
        // are rough upper-of-typical estimates; growth still happens for
        // outliers but the common 60fps path avoids the realloc churn.
        // Cycle 853 (audit): reuse the pooled quad scratch (cleared, capacity
        // retained from the prior frame) instead of allocating a fresh Vec every
        // frame. Returned to `self.quad_scratch` after the GPU upload below.
        let mut quads: Vec<QuadInstance> = std::mem::take(&mut self.quad_scratch);
        quads.clear();
        quads.reserve(panes.len() * 16 + 256);
        // Third quad pass — drawn after `over` so the right-click
        // context menu's bg/shadow/border/highlight sit on top of
        // every other UI element. The menu's text is rendered by
        // `menu_text_renderer` after this pass so the labels land on
        // top of the panel bg. Cycle 251.
        //
        // Cycle 915 (audit): the four per-frame buffers below (menu_q / over /
        // img_items / live) are INTENTIONALLY allocated fresh each frame, unlike
        // the pooled `quad_scratch` / `span_scratch`. They are small and usually
        // near-empty (no open context menu, a handful of panes, no cell images),
        // so the allocation is trivial; high-water pooling is reserved for the
        // large per-cell `quads` / `spans` buffers where it actually pays off.
        // The asymmetry is deliberate, not an oversight.
        let mut menu_q: Vec<QuadInstance> = Vec::with_capacity(64);
        // Drawn *after* text: unfocused-pane dimming + scrollbar thumbs.
        let mut over: Vec<QuadInstance> = Vec::with_capacity(panes.len() * 4 + 8);
        let mut img_items: Vec<(f32, f32, f32, f32, kettle_core::ImageData)> =
            Vec::with_capacity(16);
        let mut live: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // Cycle 388 (Terminator parity, bg-image Bucket-D sub-cycles
        // 3+4): when cfg.background_type = Image + cfg.background_image
        // is set, decode-once + cache + prepend a fullscreen image
        // item BEFORE any cell-images so the wallpaper renders at the
        // back. The cycle-381 decode_bg_image helper handles the
        // file-not-found / decode-error paths gracefully.
        use kettle_config::BackgroundType;
        if matches!(cfg.background_type, BackgroundType::Image) && !cfg.background_image.is_empty()
        {
            let want = cfg.background_image.clone();
            // Cycle 396: route through decode_bg_image_with_blur
            // so cfg.background_blur takes effect at load time.
            // Radius 8 is a reasonable default for the on/off
            // toggle Terminator's bool config exposes; a future
            // sub-cycle could expose a `background_blur_radius`
            // numeric for finer control.
            let blur_radius: u32 = if cfg.background_blur { 8 } else { 0 };
            // Cycle 892 (audit): reload when the path OR the blur radius
            // changes. Before, blur lived outside the cache key, so toggling
            // `background-blur` on a still-loaded image was silently ignored.
            let need_reload = match self.bg_image_cache.as_ref() {
                None => true,
                // Reload when the (path, blur) key changed, OR — cycle 919 (audit
                // L2) — when the cached entry is a FAILED decode (inner `None`)
                // and the throttle has elapsed: a transient read error / an
                // in-place file fix self-heals, but THROTTLED (≥3s between
                // attempts) so a broken or corrupt path is NOT re-decoded every
                // frame (the per-frame thrash cycle 918 removed). A successful
                // decode clears the throttle, so the happy path never re-decodes.
                Some((p, b, img)) => {
                    p != &want
                        || *b != blur_radius
                        || (img.is_none()
                            && self
                                .bg_image_retry_at
                                .is_none_or(|t| std::time::Instant::now() >= t))
                }
            };
            if need_reload {
                // Cycle 918: store the (path, blur) key even when decode fails
                // (inner `None`). Without this the stale wallpaper kept rendering
                // for a now-broken path AND `need_reload` stayed true, re-decoding
                // the failing file every frame.
                let decoded = bg_image::decode_bg_image_with_blur(&want, blur_radius).map(|d| {
                    use std::sync::Arc;
                    kettle_core::ImageData {
                        width: d.width,
                        height: d.height,
                        rgba: Arc::new(d.rgba),
                    }
                });
                // Cycle 919 (audit L2): on failure, throttle the next retry; on
                // success, clear it so the loaded wallpaper never re-decodes.
                self.bg_image_retry_at = if decoded.is_none() {
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(3))
                } else {
                    None
                };
                self.bg_image_cache = Some((want.clone(), blur_radius, decoded));
            }
            if let Some((_, _, Some(data))) = self.bg_image_cache.as_ref() {
                // Cycle 390 (Terminator parity, bg-image Bucket-D
                // sub-cycle 5): UV-mode variants. background-image-mode
                // controls how the decoded image fills the surface.
                //
                //   stretch_and_fill (default): one quad covering the
                //                               whole surface (image
                //                               is stretched).
                //   tile:                       tile the original-size
                //                               image across the
                //                               surface (preserves
                //                               aspect; visible seams
                //                               at the tile boundaries).
                //   center / scale:             single image quad at
                //                               its natural size,
                //                               centered. `scale`
                //                               adds proportional fit.
                let img_w = data.width as f32;
                let img_h = data.height as f32;
                live.insert(std::sync::Arc::as_ptr(&data.rgba) as usize);
                match cfg.background_image_mode.as_str() {
                    "tile" if bg_tiles_within_cap(sw, sh, img_w, img_h) => {
                        // Tile starts from (0, 0); rows go top-to-bottom.
                        let mut y = 0.0;
                        while y < sh {
                            let mut x = 0.0;
                            while x < sw {
                                let tw = img_w.min(sw - x);
                                let th = img_h.min(sh - y);
                                img_items.push((x, y, tw, th, data.clone()));
                                x += img_w;
                            }
                            y += img_h;
                        }
                    }
                    "tile" => {
                        // Cycle 825 (audit): a tiny source image (e.g. a 1×1
                        // pixel) tiles into a huge number of CPU quads + Arc
                        // clones EVERY frame — ~8.3M on a 4K surface — hanging
                        // the render thread. Past `MAX_BG_TILES`, fall back to a
                        // single stretched quad instead of melting the renderer.
                        img_items.push((0.0, 0.0, sw, sh, data.clone()));
                    }
                    "center" => {
                        // Cycle 391: align_horiz/vert nudge the
                        // centered position. left/top → 0, right/
                        // bottom → max-edge, center/middle (default)
                        // → centered.
                        let w = img_w.min(sw);
                        let h = img_h.min(sh);
                        let x = match cfg.background_image_align_horiz.as_str() {
                            "left" => 0.0,
                            "right" => (sw - w).max(0.0),
                            _ => ((sw - w) * 0.5).max(0.0),
                        };
                        let y = match cfg.background_image_align_vert.as_str() {
                            "top" => 0.0,
                            "bottom" => (sh - h).max(0.0),
                            _ => ((sh - h) * 0.5).max(0.0),
                        };
                        img_items.push((x, y, w, h, data.clone()));
                    }
                    "scale" => {
                        // Cycle 391: aspect-preserving fit within
                        // the surface; align_horiz/vert position
                        // the scaled image.
                        let scale = (sw / img_w).min(sh / img_h);
                        let w = img_w * scale;
                        let h = img_h * scale;
                        let x = match cfg.background_image_align_horiz.as_str() {
                            "left" => 0.0,
                            "right" => (sw - w).max(0.0),
                            _ => ((sw - w) * 0.5).max(0.0),
                        };
                        let y = match cfg.background_image_align_vert.as_str() {
                            "top" => 0.0,
                            "bottom" => (sh - h).max(0.0),
                            _ => ((sh - h) * 0.5).max(0.0),
                        };
                        img_items.push((x, y, w, h, data.clone()));
                    }
                    _ => {
                        // "stretch_and_fill" + any unknown value:
                        // single quad covering the whole surface.
                        img_items.push((0.0, 0.0, sw, sh, data.clone()));
                    }
                }
            }
        } else if self.bg_image_cache.is_some() {
            // Cycle 892 (audit): config no longer requests an image background
            // (type switched away, or path cleared) — drop the decoded RGBA so
            // an up-to-256-MiB wallpaper isn't pinned for the rest of the
            // session. Re-enabling re-decodes via the need_reload path above.
            self.bg_image_cache = None;
            self.bg_image_retry_at = None; // cycle 919 (L2): reset the self-heal throttle
        }

        // Cycle 296: status-bar background. The text is uploaded
        // alongside `tabbar_buffer.set_text` further down so the same
        // text-renderer pass handles both. Just a chrome-dim panel
        // here (1 quad).
        if status.height > 0.0 {
            quads.push(rect(
                0.0,
                status.y,
                sw,
                status.height,
                theme.palette[8],
                1.0,
            ));
            // One-px line on the side facing the pane grid so the
            // strip reads as distinct chrome, not as terminal output.
            // The line goes on the BOTTOM of a top-positioned status
            // bar and the TOP of a bottom-positioned one.
            let line_y = if status.y < 1.0 {
                status.height - 1.0
            } else {
                status.y
            };
            quads.push(rect(0.0, line_y, sw, 1.0, theme.background, 0.7));
        }

        // Tab bar background + per-segment chrome (text added later).
        if tabbar.height > 0.0 {
            let by = tabbar.y;
            // Cycle 672 (vertical-tabs sub-cycle 5): when the strip
            // is vertical (TabBarPos::Left/Right), paint the bar
            // background as a column matching the strip rect
            // instead of a full-width horizontal stripe.
            if cfg.tab_bar_pos.is_vertical() {
                // Derive the strip's x + width from the first
                // segment (cycle-668 hands us correct per-segment
                // rects). new_tab anchors at the same x/w.
                let (sx, _, swid, _) = tabbar
                    .segments
                    .first()
                    .map(|s| s.rect)
                    .unwrap_or(tabbar.new_tab);
                quads.push(rect(sx, 0.0, swid, sh, theme.palette[8], 1.0));
            } else {
                quads.push(rect(0.0, by, sw, tabbar.height, theme.palette[8], 1.0));
            }
            for s in &tabbar.segments {
                // Cycle 672 (vertical-tabs sub-cycle 5): use the
                // segment's own y/h (from cycle 668) instead of
                // the strip-wide `by`/`tabbar.height`. For
                // horizontal layouts the values match; for
                // vertical they're per-row.
                let (x, seg_y, w, seg_h) = s.rect;
                if s.active {
                    quads.push(rect(x, seg_y, w, seg_h, default_bg, 1.0));
                    // Active accent bar on the left edge.
                    // Cycle 178: when broadcast / group-input mode is on,
                    // use a warning-yellow accent (theme palette index 3,
                    // the standard ANSI "yellow" slot) for the active tab
                    // so the user can't forget broadcast is enabled and
                    // type to one pane expecting it to stay local. Other
                    // tabs are unaffected — broadcast is scoped to the
                    // active tab (cycle-112 invariant), so only the
                    // active segment's accent flips.
                    let accent = if tabbar.broadcast {
                        theme.palette[3]
                    } else {
                        // Cycle 293/937: explicit `accent-color` wins, then
                        // Peacock auto (per-cwd), then the THEME's signature
                        // accent (Mocha's mauve — matching the icon; palette[4]
                        // for themes without one).
                        cfg.resolved_accent(theme)
                    };
                    quads.push(rect(x, seg_y, 2.0, seg_h, accent, 1.0));
                }
                // Thin separator on the right (horizontal) or
                // bottom (vertical) of each segment. For vertical,
                // the cycle-668 layout stacks rows top-to-bottom,
                // so the separator goes ALONG the bottom edge of
                // each row instead of the right edge.
                if cfg.tab_bar_pos.is_vertical() {
                    quads.push(rect(x, seg_y + seg_h - 1.0, w, 1.0, theme.background, 0.5));
                } else {
                    quads.push(rect(x + w - 1.0, seg_y, 1.0, seg_h, theme.background, 0.5));
                }
                // Activity indicator dot (cycle 246) — a small disc-
                // approximation in the lower-left of any *inactive*
                // segment whose tab has produced output (cyan) or
                // rung the terminal bell (yellow) since the user last
                // looked at it. Terminator's Activity / Urgent
                // Watcher affordance, surfaced inline on the tab bar
                // so a user driving long-running jobs in background
                // tabs sees the cue without polling each tab.
                let dot_color = match s.activity {
                    TabActivity::Bell => Some(theme.palette[3]),
                    TabActivity::Output => Some(theme.palette[6]),
                    // Cycle 252: Silent is the "your watched output
                    // stopped" state. Dim palette[8] — same color
                    // the inactive-pane border + chrome surfaces use,
                    // so it reads as "low-urgency, FYI" rather than
                    // the Output-arrived nudge.
                    TabActivity::Silent => Some(theme.palette[8]),
                    TabActivity::Normal => None,
                };
                if let Some(c) = dot_color {
                    let r = (seg_h * 0.18).clamp(3.0, 6.0);
                    let dx = x + 6.0;
                    let dy = seg_y + seg_h - r * 2.0 - 4.0;
                    // Render the dot as a small square — wgpu doesn't
                    // have a circle primitive here and a 4×4 / 6×6
                    // square at high opacity reads as a "bullet" at
                    // typical tab-bar sizes (kitty / iTerm2 do the
                    // same in their text-only inactive-tab indicators).
                    quads.push(rect(dx, dy, r * 2.0, r * 2.0, c, 1.0));
                }
                // Close-button chip — drawn at *all* times so the user
                // can see the close zone is a button without having to
                // hover-discover it. Chrome / Firefox / Safari tab
                // convention: the `✕` always has a subtle background
                // chip, and hover bumps it to the destructive-action
                // color (red). The chip is a small rounded-feeling
                // square (no shader for actual rounded corners; we get
                // the chip feel from the pad + opacity choice).
                //
                // Cycle 349 (Terminator parity, terminatorlib/config.py:81
                // `close_button_on_tab`): when false, skip the close
                // chip + the ✕ glyph entirely. Tab is still closable
                // via Ctrl+Shift+W; just the visual chrome is removed.
                if !cfg.close_button_on_tab {
                    continue;
                }
                let (cx, cy, ccw, cch) = s.close;
                let pad = 5.0_f32;
                let inner_w = (ccw - pad * 2.0).max(0.0);
                let inner_h = (cch - pad * 2.0).max(0.0);
                let hovered = tabbar.hovered_close_idx == Some(s.idx);
                let (chip_color, chip_alpha) = if hovered {
                    // Hover: bright destructive-action red.
                    (theme.palette[1], 0.85)
                } else if s.active {
                    // Inactive close on the *active* tab — slightly
                    // more visible since the active tab has a brighter
                    // surface and the chip needs more contrast.
                    (theme.palette[8], 0.55)
                } else {
                    // Inactive tab: very subtle chip, just enough to
                    // distinguish the close button from the title text.
                    (theme.foreground, 0.12)
                };
                if inner_w > 0.0 && inner_h > 0.0 {
                    quads.push(rect(
                        cx + pad,
                        cy + pad,
                        inner_w,
                        inner_h,
                        chip_color,
                        chip_alpha,
                    ));
                }
            }
            // New-tab (+) button background. Cycle 672: use the
            // new_tab rect's own y/h (which cycle-668 set to the
            // strip-bottom row for vertical layouts).
            // Cycle 805: paint the union of [▾ | +] when the dropdown arrow is
            // present so there's no unpainted gap behind it; otherwise just the
            // `+` button.
            let (nx, ny, nw, nh) = tabbar.new_tab;
            let (mx, _, mw, _) = tabbar.new_tab_menu;
            let (bx, bw) = if mw > 0.0 { (mx, mw + nw) } else { (nx, nw) };
            quads.push(rect(bx, ny, bw, nh, theme.palette[8], 1.0));
            // Cycle 255: drag-in-progress ghost. While the user holds a
            // left button down on the tab bar (cycle 249), paint a
            // translucent overlay copy of the active segment centered
            // at the cursor x. The underlying segments still snap to
            // their target positions via `move_active_tab`; the ghost
            // gives the bar a "you're picking this tab up" affordance
            // so the snap doesn't read as a confusing teleport. Push
            // to `over` (post-text) so the ghost sits above the live
            // segment text. Drawn only when both a drag is active
            // *and* there's an active segment to copy from.
            if let Some(cx) = tabbar.drag_cursor_x
                && let Some(active_seg) = tabbar.segments.iter().find(|s| s.active)
            {
                let (_, _, seg_w, seg_h) = active_seg.rect;
                // Clamp the ghost's left edge so the box doesn't slide
                // entirely off either end of the bar — same idea as
                // cycle 245's context-menu anchor clamp.
                let half = seg_w * 0.5;
                let max_x = (sw - seg_w).max(0.0);
                let ghost_x = (cx - half).clamp(0.0, max_x);
                // Soft drop shadow under the ghost (same trick as the
                // cycle-251 context menu).
                over.push(rect(
                    ghost_x + 3.0,
                    by + 3.0,
                    seg_w,
                    seg_h,
                    Rgb::new(0, 0, 0),
                    0.30,
                ));
                // Ghost background — theme.background at 0.85 opacity
                // so the bar shows through enough that it reads as a
                // floating preview rather than a real new tab.
                over.push(rect(ghost_x, by, seg_w, seg_h, theme.background, 0.85));
                // Accent strip on the left edge, same color the live
                // active segment uses (palette[3] yellow under
                // broadcast, cycle-293 accent-color → palette[4]
                // otherwise — keeps the ghost visually identical to
                // the source segment).
                let accent = if tabbar.broadcast {
                    theme.palette[3]
                } else {
                    cfg.resolved_accent(theme)
                };
                over.push(rect(ghost_x, by, 2.0, seg_h, accent, 1.0));
            }
        }

        // Per-pane grid + dividers/border.
        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.rect;
            // Pane separators / focus border. Both colors are config-
            // overridable: `split-divider-color` for inactive panes
            // (defaults to theme `palette[8]`, the dim color) and
            // `focused-split-color` for the focused pane (defaults to
            // theme `palette[4]`, the accent blue).
            //
            // Cycle 184: when broadcast / group-input mode is on, the
            // focused-pane border flips to theme palette[3] (yellow,
            // the same warning slot the tab-bar accent uses in cycle
            // 178). The tab-bar indicator alone wasn't enough: with
            // `tab-bar = auto` and only one tab open (the default
            // single-window case), the tab bar is hidden and the
            // user has no visual cue that broadcast is active.
            // Per-pane border-color shift works regardless of tab-bar
            // state. Inactive panes keep their normal divider color
            // — broadcast is scoped to the active tab (cycle-112
            // invariant) and the focused-pane border is the single
            // most-visible chrome element on every layout.
            let border = if pv.focused {
                if tabbar.broadcast {
                    theme.palette[3]
                } else {
                    // Cycle 293/937: cascade order is
                    //   focused-split-color (explicit override)
                    //   → resolved accent (explicit accent-color → Peacock
                    //     auto → the theme's signature accent, Mocha mauve)
                    // Backward-compat: anyone who set `focused-split-color`
                    // before cycle 293 keeps their pinned color.
                    cfg.focused_split_color
                        .unwrap_or_else(|| cfg.resolved_accent(theme))
                }
            } else {
                cfg.split_divider_color.unwrap_or(theme.palette[8])
            };
            // Cycle 353 (Terminator parity, terminatorlib/config.py:74
            // `handle_size`): split-divider width in px. -1 means
            // "use theme default" (1.0 here); positive values 0-20 are
            // honored directly. Clamping was already done at parse
            // time (cycle 339).
            let bw = if cfg.handle_size < 0 {
                1.0
            } else {
                cfg.handle_size as f32
            };
            quads.push(rect(rx, ry, rw, bw, border, 1.0));
            quads.push(rect(rx, ry + rh - bw, rw, bw, border, 1.0));
            quads.push(rect(rx, ry, bw, rh, border, 1.0));
            quads.push(rect(rx + rw - bw, ry, bw, rh, border, 1.0));

            // Cycle 379: per-pane titlebar background quad. Drawn
            // ABOVE the pane's border + BELOW the pane's content.
            // Color picks from the cfg.title_*_bg_color variants
            // based on focus + broadcast group state.
            if pane_titlebar_h > 0.0 {
                // Cycle 387 + 710: see `pick_titlebar_bg`.
                let bar_bg = pick_titlebar_bg(cfg, theme, pv.focused, tabbar.broadcast);
                // Cycle 385 (Terminator parity, titlebar Bucket-D
                // sub-cycle 9): title_at_bottom flips the bar from
                // (top of pane) to (bottom of pane). Cells shift
                // is still applied at the top — that's the
                // intentional follow-up. Today the bar lands at the
                // user's chosen position; the small top-pad gap
                // when title_at_bottom is true is a layout-shift
                // follow-up.
                let bar_y = if cfg.title_at_bottom {
                    ry + rh - bw - pane_titlebar_h
                } else {
                    ry + bw
                };
                quads.push(rect(
                    rx + bw,
                    bar_y,
                    rw - 2.0 * bw,
                    pane_titlebar_h,
                    bar_bg,
                    1.0,
                ));
            }

            self.build_pane(
                i,
                pv,
                cfg,
                &family,
                overlay.window_focused,
                overlay.cursor_visible,
                overlay.vi_cursor,
                overlay.vi_visual_anchor,
                &mut quads,
                pane_titlebar_h,
                // Cycle 891: the whole-surface clear color so build_pane can
                // detect when an unfocused pane needs its own bg backdrop.
                default_bg,
            );

            // Image placements, anchored history-aware so they scroll.
            {
                let g = pv.term.grid();
                let top = g.history_size() as i64 - g.display_offset() as i64;
                let nrows = g.screen_lines() as i64;
                // Cycle 791 (audit C1): most panes carry 0–1 image placements,
                // so skip the per-frame `Vec` alloc + sort in that common case
                // (z-order is meaningless for fewer than two) and iterate the
                // slice directly; only collect + sort when 2+ placements
                // actually need ordering. One closure keeps the body single-
                // sourced across both paths.
                let mut draw = |p: &kettle_core::Placement| {
                    let row = p.abs_line - top;
                    if row + p.cell_rows as i64 <= 0 || row >= nrows {
                        return;
                    }
                    live.insert(std::sync::Arc::as_ptr(&p.img.rgba) as usize);
                    img_items.push((
                        rx + pad_x + p.col as f32 * cw,
                        // Cycle 383: image placements also shift
                        // below the titlebar so a kitty/sixel
                        // image at row 0 doesn't overlap the bar.
                        ry + pad_y + pane_titlebar_h + row as f32 * ch,
                        p.cell_cols as f32 * cw,
                        p.cell_rows as f32 * ch,
                        p.img.clone(),
                    ));
                };
                if pv.images.len() > 1 {
                    // Draw in ascending z so higher z-index images land on top.
                    let mut ordered: Vec<&kettle_core::Placement> = pv.images.iter().collect();
                    ordered.sort_by_key(|p| p.z);
                    for p in ordered {
                        draw(p);
                    }
                } else {
                    for p in pv.images {
                        draw(p);
                    }
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
                    ry + pad_y + pane_titlebar_h + ln.row as f32 * ch + ch - 1.5,
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
                        ry + pad_y + pane_titlebar_h + hl.row as f32 * ch,
                        hl.width as f32 * cw,
                        ch,
                        if hl.active {
                            // Cycle 920: the active match follows the theme's
                            // yellow (Mocha #f9e2af) unless overridden, so it
                            // matches the inactive highlight's theme.selection_bg
                            // instead of a hardcoded TokyoNight amber.
                            cfg.search_background.unwrap_or(theme.palette[3])
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
                        ry + pad_y + pane_titlebar_h + hint.row as f32 * ch,
                        n * cw,
                        ch,
                        if hint.dim {
                            theme.palette[8]
                        } else {
                            cfg.search_background.unwrap_or(theme.palette[3])
                        },
                        if hint.dim { 0.6 } else { 0.96 },
                    ));
                }
            }

            // Post-text overlay: dim unfocused panes; per-pane scrollbar.
            //
            // Cycle 356 (Terminator parity, terminatorlib/config.py:84-85
            // `inactive_color_offset` + `inactive_bg_color_offset`):
            // when EITHER offset is < 1.0, layer a dim over the
            // unfocused pane. Uses the BG offset for the overlay
            // alpha (since the visible effect on the bg is most of
            // the dim). The FG offset is kept reserved for the
            // glyph-level desaturation that's a Bucket-D follow-up
            // (would need to recolor each glyph's fg, which means
            // re-running the text-shaper for unfocused panes).
            let inactive_bg_dim = (1.0 - cfg.inactive_bg_color_offset).clamp(0.0, 0.95);
            let split_opacity_dim = (1.0 - cfg.unfocused_split_opacity).clamp(0.0, 0.95);
            let composed_dim = inactive_bg_dim.max(split_opacity_dim);
            if !pv.focused && panes.len() > 1 && composed_dim > 0.0 {
                over.push(rect(rx, ry, rw, rh, theme.background, composed_dim));
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
        // Cycle 808 (audit): how far ABOVE the surface bottom the shared
        // bottom-bar text sits. Stays 0 for the modal bottom bars (search /
        // confirm / broadcast), which intentionally cover any bottom chrome
        // while they hold focus; the passive update banner sets it so it
        // stacks above a bottom-anchored tab / status bar instead of clobbering
        // it. See `update_banner_top`.
        let mut bottom_bar_offset = 0.0_f32;
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
        } else if let Some(q) = &overlay.layout_picker_query {
            // Cycle 708 (Terminator parity, layoutlauncher.py):
            // layout picker overlay. Same bar shape as the
            // palette but the hint string lists layouts.
            have_search = true;
            let bar_h = ch + 10.0;
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[6], 0.96));
            let label = format!(
                "  ▤ layout: {q}_   ▸ {}   (Enter spawn · Tab/↑↓ select · Esc cancel)",
                overlay.layout_picker_hint
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
        } else if let Some((label_prefix, q, anchor_y)) = &overlay.edit_title {
            // Cycle 372 (Terminator parity, edit-title overlay UX):
            // a thin bar at the bottom of the window mirroring the
            // shape of the cycle-X palette + ssh-input overlays.
            // Uses palette[3] (yellow) so it's visually distinct
            // from the palette (5) and ssh (4) bars.
            //
            // Cycle 395: pane scope anchors near the clicked pane;
            // window/tab scopes fall back to window-bottom.
            have_search = true;
            let bar_h = ch + 10.0;
            let bar_y = anchor_y.unwrap_or(sh - bar_h);
            quads.push(rect(0.0, bar_y, sw, bar_h, theme.palette[3], 0.96));
            let label = format!("  ✎ {label_prefix} {q}_   (Enter apply · Esc cancel)");
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
        } else if let Some(dlg) = &overlay.confirm_dialog {
            // Cycle 660 (sub-cycle 3 of TERMINATOR-CONFIRM-DIALOG-DESIGN.md):
            // a bottom-bar projection of the modal. v1 of the
            // renderer skips the fancy centered-panel + backdrop
            // dimming + per-button accent border — the bottom bar
            // gives the user immediate "a modal is open" feedback
            // with prompt + button labels + focus indicator
            // (the focused button gets a ▶ prefix). The full
            // centered-modal painting is sub-cycle 3.5 (renderer
            // polish); for now this lands the wiring so cycle 661
            // can hook up the dispatch.
            have_search = true;
            let bar_h = ch + 10.0;
            // Red-ish accent (palette[1]) to signal "destructive
            // confirmation pending" vs the cycle-X palette/ssh/
            // edit-title yellows/blues/cyans.
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[1], 0.96));
            let mut buttons_label = String::new();
            for (i, btn) in dlg.buttons.iter().enumerate() {
                if !buttons_label.is_empty() {
                    buttons_label.push_str("   ");
                }
                let marker = if i == dlg.focus_idx { "▶ " } else { "  " };
                buttons_label.push_str(marker);
                buttons_label.push_str(&btn.label);
            }
            let label = format!(
                "  ⚠ {}      {}      (Tab/←→ focus · Enter confirm · Esc cancel)",
                dlg.prompt, buttons_label
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
        } else if let Some((tag, url)) = &overlay.update_available {
            // Cycle 794: passive "newer release available" banner — lowest
            // priority, so any real modal above takes the bar and this returns
            // when they close. Green accent (palette[2]) = good news.
            have_search = true;
            let bar_h = ch + 10.0;
            // Cycle 808 (audit): stack above a bottom-anchored tab / status bar
            // so the passive banner doesn't paint over (or, with the App's
            // matching hit-test, steal clicks from) it. `status.y > 0` marks a
            // bottom status bar (top sits at y == 0).
            let bottom_tabbar_h = if matches!(cfg.tab_bar_pos, kettle_config::TabBarPos::Bottom) {
                tabbar.height
            } else {
                0.0
            };
            let bottom_status_h = if status.height > 0.0 && status.y > 0.0 {
                status.height
            } else {
                0.0
            };
            bottom_bar_offset = bottom_tabbar_h + bottom_status_h;
            let bar_y = update_banner_top(sh, bar_h, bottom_tabbar_h, bottom_status_h);
            quads.push(rect(0.0, bar_y, sw, bar_h, theme.palette[2], 0.96));
            let label = format!(
                "  ⬆ kettle {tag} available — {url}    (click: open · right-click: dismiss)"
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
            // Cycle 788 (audit B4): shrink the pool when tabs close, matching
            // `pane_buffers`/`settings_buffers` — otherwise it stuck at the
            // peak tab count for the whole session (open 50, close to 5 → 50
            // shaped-text buffers retained).
            self.tab_buffers.truncate(tabbar.segments.len());
            for (bi, s) in tabbar.segments.iter().enumerate() {
                let (_, _, w, _) = s.rect;
                // chars that fit: segment minus the ✕ zone, ~cell_w each.
                // `max(0.0)` so a segment narrower than the ✕ zone can't go
                // negative, and `cw.max(1.0)` guards a degenerate cell width
                // — both keep the `as usize` cast in its defined range
                // rather than relying on float→int saturation.
                //
                // Cycle 804: the budget now tracks the *actual* segment width
                // instead of a hard 24-char cap, so a wide tab shows its full
                // title (and only ellipsizes when the title genuinely doesn't
                // fit). We reserve `fixed_w` for the non-title part of the
                // format (the leading space + e.g. "{n}: ") so the title
                // ellipsizes to keep the WHOLE label inside the segment rather
                // than letting the prefix push it past the right edge.
                let n = (s.idx + 1).to_string();
                let avail = ((w - tabbar.height).max(0.0) / cw.max(1.0)) as usize;
                let fixed_w =
                    1 + kettle_config::template::fill(&cfg.tab_format, &[("n", &n), ("title", "")])
                        .chars()
                        .count();
                let maxc = avail.saturating_sub(fixed_w).max(3);
                let title = truncate(&s.title, maxc);
                let body =
                    kettle_config::template::fill(&cfg.tab_format, &[("n", &n), ("title", &title)]);
                // Title only — the ✕ is rendered separately below so we
                // can color it independently from the title text and
                // give it a real button chip background.
                let label = format!(" {body}");
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
            // Shared `✕` glyph buffer for every tab's close button.
            // Sized once; positioned per-tab via TextArea below.
            self.tab_close_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.tab_close_buffer.set_size(
                &mut self.font_system,
                Some(tabbar.height),
                Some(tabbar.height),
            );
            self.tab_close_buffer.set_text(
                &mut self.font_system,
                "✕",
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.tab_close_buffer
                .shape_until_scroll(&mut self.font_system, false);
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
            // Cycle 805: the `▾` dropdown arrow, shaped in its own buffer so it
            // lands inside `new_tab_menu` (left of `+`). Skipped when disabled.
            if tabbar.new_tab_menu.2 > 0.0 {
                self.new_tab_arrow_buffer
                    .set_metrics(&mut self.font_system, metrics);
                self.new_tab_arrow_buffer.set_size(
                    &mut self.font_system,
                    Some(tabbar.new_tab_menu.2),
                    Some(tabbar.height),
                );
                self.new_tab_arrow_buffer.set_text(
                    &mut self.font_system,
                    " ▾",
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.new_tab_arrow_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Cycle 296: upload status-bar text. Single buffer, single
        // line; sized to surface width so cosmic-text doesn't wrap
        // an overlong status string.
        if status.height > 0.0 && !status.text.is_empty() {
            self.status_bar_buffer
                .set_metrics(&mut self.font_system, metrics);
            self.status_bar_buffer.set_size(
                &mut self.font_system,
                Some(sw - 16.0),
                Some(status.height),
            );
            self.status_bar_buffer.set_text(
                &mut self.font_system,
                &status.text,
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.status_bar_buffer
                .shape_until_scroll(&mut self.font_system, false);
        }

        // Context-menu row labels (one buffer per row, separators skipped).
        if let Some(menu) = &overlay.context_menu {
            while self.context_menu_buffers.len() < menu.rows.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.context_menu_buffers.push(b);
            }
            // Cycle 788 (audit B2): shrink to the current row count so a small
            // menu after a large one (common with dynamic Lua menus) doesn't
            // keep the peak's worth of shaped-glyph buffers. The field doc
            // promised this trim; the code never did it until now.
            self.context_menu_buffers.truncate(menu.rows.len());
            // Approximate widest label so the panel fits without
            // wrapping; the renderer doesn't try to measure precisely
            // because the labels are short and we pad generously.
            let max_chars = menu
                .rows
                .iter()
                .filter(|r| !r.separator)
                .map(|r| r.label.chars().count())
                .max()
                .unwrap_or(0) as f32;
            // Panel sizing — more generous than v1.3.0's tight box so
            // the menu reads as a polished surface rather than a wall
            // of text. Horizontal pad 40 px (was 32), min width 180 px
            // (was 140) so even a single-character action label gives
            // the panel real presence.
            let panel_w = (max_chars * cw + 40.0).max(180.0);
            // Row height matches a comfortable click target (~28-32 px
            // on default cell metrics) — was 6 px of pad which gave a
            // cramped 18-19 px row.
            let row_h = ch + 12.0;
            for (i, row) in menu.rows.iter().enumerate() {
                if row.separator {
                    continue;
                }
                let buf = &mut self.context_menu_buffers[i];
                buf.set_metrics(&mut self.font_system, metrics);
                buf.set_size(&mut self.font_system, Some(panel_w), Some(row_h));
                buf.set_text(
                    &mut self.font_system,
                    &row.label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Cycle 756: settings-overlay row buffers (one per display line).
        if let Some(set) = &overlay.settings {
            let lines = settings_display_lines(set);
            while self.settings_buffers.len() < lines.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.settings_buffers.push(b);
            }
            self.settings_buffers.truncate(lines.len());
            // Panel width fits the content but never exceeds the surface
            // (so it stays usable in a small window); see the matching clamp
            // in the quad/area pass below.
            let panel_w = (settings_panel_cols(&lines) * cw + 48.0).min((sw - 40.0).max(120.0));
            let row_h = ch + 6.0;
            for (i, line) in lines.iter().enumerate() {
                let buf = &mut self.settings_buffers[i];
                buf.set_metrics(&mut self.font_system, metrics);
                buf.set_size(&mut self.font_system, Some(panel_w), Some(row_h));
                buf.set_text(
                    &mut self.font_system,
                    line,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Quick-select hint label glyphs (one buffer per label).
        if !overlay.hint_labels.is_empty() {
            while self.hint_buffers.len() < overlay.hint_labels.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.hint_buffers.push(b);
            }
            // Cycle 788 (audit B3): quick-select labels every visible link, so
            // densities swing widely (50 → 5 → 100); shrink the pool to the
            // current label count instead of pinning it at the peak.
            self.hint_buffers.truncate(overlay.hint_labels.len());
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
        // Menu text lives in its own areas vec so we can hand it to a
        // dedicated `menu_text_renderer.prepare(...)` call after the
        // main `text_renderer.prepare(...)`. Cycle 251 — drawing the
        // menu's bg / shadow / border / highlight before the menu's
        // text in the same pass painted text right under bg; this
        // split fixes that by giving the menu its own
        // bg→border→highlight→text pipeline at the end of the render
        // pass.
        // Cycle 761: pre-size for the menu / settings-overlay rows it collects.
        let mut menu_areas: Vec<TextArea> = Vec::with_capacity(48);
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
                // Cycle 383: shift cell text below the titlebar
                // when active. Same offset used inside build_pane
                // (which renders cells/cursor/images/links).
                top: ry + pad_y + pane_titlebar_h,
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
        // Cycle 382 (Terminator parity, per-pane-titlebar Bucket-D
        // sub-cycle 3): per-pane title text. Push the TextAreas
        // referencing the cycle-382 buffers (already populated
        // during the cycle-379 build_pane pass — see
        // build_pane_titlebar_text).
        if pane_titlebar_h > 0.0 {
            for (i, pv) in panes.iter().enumerate() {
                let (rx, ry, rw, rh) = pv.rect;
                // Cycle 387: matching fg variant for the three states.
                // Cycle 920: derive from the theme so the title text stays
                // readable + on-theme. The focused + broadcast bars are the
                // theme's (light) blue `palette[4]`, so their text is the dark
                // `theme.cursor_text`; the inactive bar is the dark `palette[8]`
                // surface, so its text is the light `theme.foreground`. Explicit
                // `title-*-fg-color` config still overrides.
                let fg = if pv.focused {
                    cfg.title_transmit_fg_color.unwrap_or(theme.cursor_text)
                } else if tabbar.broadcast {
                    cfg.title_receive_fg_color.unwrap_or(theme.cursor_text)
                } else {
                    cfg.title_inactive_fg_color.unwrap_or(theme.foreground)
                };
                // Cycle 385: text-area position mirrors the
                // cycle-385 bar position so the title text follows
                // the bar to the bottom when title_at_bottom is
                // true. 2px top padding matches cycle-382.
                let text_top = if cfg.title_at_bottom {
                    ry + rh - pane_titlebar_h + 2.0
                } else {
                    ry + 2.0
                };
                let text_bot = if cfg.title_at_bottom {
                    ry + rh
                } else {
                    ry + pane_titlebar_h
                };
                areas.push(TextArea {
                    buffer: &self.pane_titlebar_buffers[i],
                    left: rx,
                    top: text_top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: rx as i32,
                        // Cycle 761: clamp to ≥0 so a pane flush against the
                        // window top can't hand glyphon a negative clip bound.
                        top: (text_top - 2.0).max(0.0) as i32,
                        right: (rx + rw) as i32,
                        bottom: text_bot as i32,
                    },
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
            }
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
                // `✕` close glyph — separate text area so we can color
                // it independently of the title. Bright on hover, dim
                // at rest (still readable, but visually subordinate to
                // the title text). Centered inside `seg.close`.
                //
                // Cycle 349: skipped when cfg.close_button_on_tab is
                // false (matches the quad branch above).
                if !cfg.close_button_on_tab {
                    continue;
                }
                let (cx, _, ccw, _) = s.close;
                let hovered = tabbar.hovered_close_idx == Some(s.idx);
                let close_fg = if hovered {
                    // Cycle 920: dark glyph (theme.cursor_text) on the theme-red
                    // close chip (palette[1]) — higher contrast than white on the
                    // Mocha pink-red, and tracks the theme instead of a literal.
                    theme.cursor_text
                } else {
                    // Rest: dim chrome — readable but secondary.
                    theme.palette[8]
                };
                areas.push(TextArea {
                    buffer: &self.tab_close_buffer,
                    left: cx + (ccw - cw) * 0.5,
                    top: tabbar.y + 4.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: cx as i32,
                        top: ty,
                        right: (cx + ccw) as i32,
                        bottom: tb,
                    },
                    default_color: GColor::rgb(close_fg.r, close_fg.g, close_fg.b),
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
            // Cycle 805: the `▾` dropdown arrow glyph, at `new_tab_menu` (left
            // of `+`). Only present when the dropdown is enabled.
            if tabbar.new_tab_menu.2 > 0.0 {
                let (ax, _, aw, _) = tabbar.new_tab_menu;
                areas.push(TextArea {
                    buffer: &self.new_tab_arrow_buffer,
                    left: ax + 4.0,
                    top: tabbar.y + 4.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: ax as i32,
                        top: ty,
                        right: (ax + aw) as i32,
                        bottom: tb,
                    },
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
            }
        }
        if have_search {
            let bar_h = ch + 10.0;
            areas.push(TextArea {
                buffer: &self.search_buffer,
                left: 0.0,
                // Cycle 808: `bottom_bar_offset` lifts the passive update
                // banner's text above any bottom-anchored chrome (0 for the
                // modal bars, which keep the flush-bottom position).
                top: sh - bar_h - bottom_bar_offset + 5.0,
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
        // Cycle 296: status-bar text area. Left-padded 8 px, baseline
        // nudged 3 px below the strip top so descenders don't clip.
        if status.height > 0.0 && !status.text.is_empty() {
            areas.push(TextArea {
                buffer: &self.status_bar_buffer,
                left: 8.0,
                top: status.y + 3.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: status.y as i32,
                    right: self.config.width as i32,
                    bottom: (status.y + status.height) as i32,
                },
                default_color: GColor::rgb(fg.r, fg.g, fg.b),
                custom_glyphs: &[],
            });
        }
        // Hint labels over the focused pane (chips drawn above as quads).
        if let Some((frx, fry, frw, frh)) = focus_origin {
            // Cycle 920: hint-label text follows the theme background (dark on
            // the theme-yellow chip) unless overridden.
            let lab = cfg.search_foreground.unwrap_or(theme.background);
            for (i, hint) in overlay.hint_labels.iter().enumerate() {
                areas.push(TextArea {
                    buffer: &self.hint_buffers[i],
                    left: frx + pad_x + hint.col as f32 * cw,
                    // Cycle 383: hint labels also shift below the
                    // titlebar so they land over the cell they
                    // mark, not over the title text.
                    top: fry + pad_y + pane_titlebar_h + hint.row as f32 * ch,
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

        // Right-click context menu — drawn in its own final pass
        // (cycle 251). v1.3.0/v1.3.1 put the menu's panel-bg quad in
        // `over` (drawn AFTER text), with the opaque bg covering the
        // menu text underneath. Now: chrome quads go to `menu_q`
        // (drawn after `over` via `self.menu_quads.draw`); row labels
        // go to `menu_areas` (drawn via a dedicated
        // `self.menu_text_renderer.render` call after the menu
        // quads). The bg-under-text order finally matches reality.
        if let Some(menu) = &overlay.context_menu {
            let chrome = menu_chrome_quads(menu, theme, cfg.resolved_accent(theme), cw, ch);
            menu_q.extend(chrome);
            // Row labels — collected into `menu_areas` so the second
            // TextRenderer can prepare them as their own batch.
            let max_chars = menu
                .rows
                .iter()
                .filter(|r| !r.separator)
                .map(|r| r.label.chars().count())
                .max()
                .unwrap_or(0) as f32;
            let panel_w = (max_chars * cw + 40.0).max(180.0);
            let row_h = ch + 12.0;
            let sep_h = 8.0_f32;
            let (ax, ay) = menu.anchor;
            // Cycle 714: skip scrolled-off rows + stop drawing when
            // the next row would extend past the clamped panel
            // height. Keeps text rendering in lockstep with the
            // chrome-quad loop above.
            let natural_h: f32 = menu
                .rows
                .iter()
                .map(|r| if r.separator { sep_h } else { row_h })
                .sum();
            let panel_h_eff = if menu.panel_h_clamped > 0.0 {
                menu.panel_h_clamped.min(natural_h)
            } else {
                natural_h
            };
            let start = menu.scroll_offset.min(menu.rows.len());
            let mut row_y = ay;
            for (i, row) in menu.rows.iter().enumerate().skip(start) {
                let h = if row.separator { sep_h } else { row_h };
                if row_y + h > ay + panel_h_eff {
                    break;
                }
                if row.separator {
                    row_y += sep_h;
                    continue;
                }
                // Disabled rows blend toward the panel bg so a greyed
                // Copy reads as ~55% transparent without alpha-blending
                // through to whatever lives under the panel.
                let fg = if row.enabled {
                    theme.foreground
                } else {
                    Rgb::new(
                        ((theme.foreground.r as u16 + theme.background.r as u16 * 5) / 6) as u8,
                        ((theme.foreground.g as u16 + theme.background.g as u16 * 5) / 6) as u8,
                        ((theme.foreground.b as u16 + theme.background.b as u16 * 5) / 6) as u8,
                    )
                };
                menu_areas.push(TextArea {
                    buffer: &self.context_menu_buffers[i],
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

        // Cycle 756: settings overlay — a centered modal panel drawn on top via
        // the menu pipeline (dim backdrop + panel + accent border + focused-row
        // highlight as quads; one TextArea per display line).
        if let Some(set) = &overlay.settings {
            let lines = settings_display_lines(set);
            let row_h = ch + 6.0;
            let panel_w = (settings_panel_cols(&lines) * cw + 48.0).min((sw - 40.0).max(120.0));
            let panel_h = (lines.len() as f32 * row_h + 24.0).min((sh - 40.0).max(80.0));
            let px = ((sw - panel_w) * 0.5).max(0.0);
            let py = ((sh - panel_h) * 0.5).max(0.0);
            // Cycle 937: the settings overlay's accent follows the resolved
            // chrome accent (the theme signature / Peacock / explicit), so it
            // matches the focus border + active tab rather than always-blue.
            let acc = cfg.resolved_accent(theme);
            // Dim backdrop over the whole window so the panel reads as modal.
            menu_q.push(rect(0.0, 0.0, sw, sh, theme.background, 0.55));
            // Panel background (near-opaque) + accent border.
            menu_q.push(rect(px, py, panel_w, panel_h, theme.background, 0.99));
            menu_q.push(rect(px, py, panel_w, 2.0, acc, 1.0));
            menu_q.push(rect(px, py + panel_h - 2.0, panel_w, 2.0, acc, 1.0));
            menu_q.push(rect(px, py, 2.0, panel_h, acc, 1.0));
            menu_q.push(rect(px + panel_w - 2.0, py, 2.0, panel_h, acc, 1.0));
            // Focused field-row highlight.
            let hi_line = SETTINGS_FIELD_START + set.focused_row;
            let hi_y = py + 12.0 + hi_line as f32 * row_h;
            menu_q.push(rect(px + 6.0, hi_y, panel_w - 12.0, row_h, acc, 0.22));
            let sfg = theme.foreground;
            for (i, _line) in lines.iter().enumerate() {
                if i >= self.settings_buffers.len() {
                    break;
                }
                menu_areas.push(TextArea {
                    buffer: &self.settings_buffers[i],
                    left: px + 16.0,
                    top: py + 12.0 + i as f32 * row_h + 3.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: px as i32,
                        top: py as i32,
                        right: (px + panel_w) as i32,
                        bottom: (py + panel_h) as i32,
                    },
                    default_color: GColor::rgb(sfg.r, sfg.g, sfg.b),
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
        // Second TextRenderer prepare — context-menu rows. Empty
        // `menu_areas` is fine; glyphon's prepare handles a zero-area
        // batch as a no-op.
        self.menu_text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            menu_areas,
            &mut self.swash,
        )?;
        self.quads
            .upload(&self.device, &self.queue, [sw, sh], &quads);
        // Cycle 853: return the scratch to the pool (keeps its capacity for next
        // frame). Last use of `quads` is the upload just above.
        self.quad_scratch = quads;
        self.imgs
            .upload(&self.device, &self.queue, [sw, sh], &img_items);
        self.imgs.gc(&live);
        self.overlay_quads
            .upload(&self.device, &self.queue, [sw, sh], &over);
        self.menu_quads
            .upload(&self.device, &self.queue, [sw, sh], &menu_q);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            // Cycle 798 (audit): ANY non-success state — Outdated (resize /
            // format change), **Lost** (GPU device reset, laptop sleep/wake,
            // monitor hot-swap, driver TDR), Timeout — reconfigures the
            // surface and skips this frame; the next redraw paints on the
            // fresh surface. Pre-798 only `Outdated` reconfigured and `Lost`
            // fell into a bare `return Ok(())`, so after a device-lost the
            // surface was never recovered: every subsequent frame returned
            // Lost again and the window froze permanently. Reconfiguring on
            // the catch-all is the standard wgpu recovery and is harmless for
            // the rarer fatal states (a fresh configure simply fails the same
            // way next frame rather than wedging).
            _ => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
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
                            // Cycle 380 (Terminator parity, terminatorlib/
                            // config.py:106 + 117 `background_darkness` +
                            // `background_type`): when bg-type=transparent,
                            // compose the configured darkness with the
                            // cycle-X background-opacity. background_darkness
                            // is documented as 0.0 = fully dark (no
                            // transparency) .. 1.0 = no tint; we treat
                            // 1.0 - darkness as the additional alpha-
                            // reduction so a config like darkness=0.4
                            // gives a 60% extra-transparent surface on
                            // top of background-opacity.
                            a: composed_bg_alpha(cfg),
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
            // Cycle 251: the right-click context menu owns the last
            // two passes — chrome quads (shadow / bg / border /
            // highlight) then row labels — so the menu sits above
            // every other UI element AND the row labels sit above the
            // menu's own panel bg. Both calls are cheap no-ops when
            // the menu is closed (empty uploads / zero areas).
            self.menu_quads.draw(&mut pass);
            self.menu_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Cycle 688 (sub-cycle 4 of TERMINATOR-TERMINALSHOT-DESIGN.md):
        // if a screenshot request is queued (cycle-654), copy the
        // surface texture to a staging buffer BEFORE present (after
        // present the texture isn't readable). The PNG encode + write
        // happens off-thread; this path is best-effort: errors log
        // but don't fail the frame.
        let screenshot_req = self.pending_screenshot.take();
        if let Some(req) = &screenshot_req
            && let Err(e) = self.capture_live_surface(&frame, req)
        {
            log::warn!("take_screenshot capture failed: {e}");
        }
        frame.present();
        self.atlas.trim();
        Ok(())
    }

    /// Cycle 688 (sub-cycle 4 of TERMINATOR-TERMINALSHOT-DESIGN.md):
    /// copy the current swap-chain texture to a staging buffer,
    /// then map + encode PNG + write to `req.out_path`. Synchronous
    /// (device.poll(Wait)) to keep the implementation simple; a
    /// future polish can move the encode off-thread.
    fn capture_live_surface(
        &self,
        frame: &wgpu::SurfaceTexture,
        req: &ScreenshotRequest,
    ) -> Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let tex = &frame.texture;
        let size = tex.size();
        let width = size.width;
        let height = size.height;
        // wgpu requires 256-byte alignment on bytes_per_row.
        let bytes_per_pixel = 4u32; // BGRA8 / RGBA8 — both 4 bpp.
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
        let buffer_size = (padded_bytes_per_row * height) as u64;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-screenshot-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kettle-screenshot-copy"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        let buffer_slice = staging.slice(..);
        let done = Arc::new(AtomicBool::new(false));
        let done_set = done.clone();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            if let Err(e) = result {
                log::warn!("screenshot map_async failed: {e:?}");
            }
            done_set.store(true, Ordering::SeqCst);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        if !done.load(Ordering::SeqCst) {
            return Err(anyhow!("screenshot readback timed out"));
        }
        let mapped = buffer_slice.get_mapped_range();

        // Compact rows (strip wgpu's 256-byte row padding) +
        // convert BGRA → RGBA if needed. Surface format is
        // typically Bgra8UnormSrgb on most desktop adapters;
        // PNG expects RGBA.
        let surface_format = tex.format();
        let bgra = matches!(
            surface_format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut rgba: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let start = (row * padded_bytes_per_row) as usize;
            let row_pixels = &mapped[start..start + unpadded_bytes_per_row as usize];
            if bgra {
                for chunk in row_pixels.chunks_exact(4) {
                    rgba.push(chunk[2]);
                    rgba.push(chunk[1]);
                    rgba.push(chunk[0]);
                    rgba.push(chunk[3]);
                }
            } else {
                rgba.extend_from_slice(row_pixels);
            }
        }
        drop(mapped);
        staging.unmap();

        // Apply optional crop.
        let (out_w, out_h, out_pixels) = if let Some((cx, cy, cw, ch)) = req.crop {
            let cx = cx.max(0.0) as u32;
            let cy = cy.max(0.0) as u32;
            let cw = cw.max(1.0) as u32;
            let ch = ch.max(1.0) as u32;
            let x_end = (cx + cw).min(width);
            let y_end = (cy + ch).min(height);
            let cropped_w = x_end.saturating_sub(cx);
            let cropped_h = y_end.saturating_sub(cy);
            let mut cropped: Vec<u8> = Vec::with_capacity((cropped_w * cropped_h * 4) as usize);
            for y in cy..y_end {
                let row_start = (y * width * 4) as usize;
                let row_pixels = &rgba[row_start..row_start + (width * 4) as usize];
                let col_start = (cx * 4) as usize;
                let col_end = (x_end * 4) as usize;
                cropped.extend_from_slice(&row_pixels[col_start..col_end]);
            }
            (cropped_w, cropped_h, cropped)
        } else {
            (width, height, rgba)
        };

        // PNG encode + write.
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(out_w, out_h, out_pixels)
            .ok_or_else(|| anyhow!("screenshot ImageBuffer::from_raw failed"))?;
        img.save(&req.out_path)
            .map_err(|e| anyhow!("PNG save failed: {e}"))?;
        log::info!("screenshot saved: {}", req.out_path.display());
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
        vi_cursor: Option<(usize, usize)>,
        vi_visual_anchor: Option<(usize, usize)>,
        quads: &mut Vec<QuadInstance>,
        // Cycle 383 (Terminator parity, per-pane-titlebar Bucket-D
        // sub-cycle 2 complete): extra top offset for cell content
        // so it doesn't overlap the cycle-379 titlebar bar. When
        // titlebar is off this is 0.0 (zero overhead).
        pane_titlebar_h: f32,
        // Cycle 891 (audit): the whole-surface clear color (the FOCUSED
        // pane's OSC 11 bg, or the theme bg). When this pane's own
        // default bg differs from it we must paint a backdrop — the
        // per-cell loop skips quads for default-bg cells on the
        // assumption the clear already painted them, which is false for
        // an unfocused pane carrying its own OSC 11 background.
        surface_bg: Rgb,
    ) {
        let theme = &cfg.theme;
        let (rx, ry, rw, rh) = pv.rect;
        let ox = rx + cfg.padding_x;
        let oy = ry + cfg.padding_y + pane_titlebar_h;
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
        // Cycle 912 (R1 completion): `display_iter` + `content.selection` yield
        // GRID-ABSOLUTE lines (negative when scrolled into history); the per-cell
        // bg/underline/strikeout quads and the selection-bg quad position by
        // VIEWPORT row, so convert with `viewport_row = grid_line + display_offset`
        // (alacritty's `point_to_viewport`). The text itself flows correctly off
        // relative line-break deltas, so only the quad Y needs this. No-op when
        // not scrolled (display_offset == 0).
        let display_off = content.display_offset as i32;
        let screen_rows = term.grid().screen_lines() as i32;
        // Match the surface clear-color so a cell whose bg resolves to the
        // active default (OSC 11 override or theme bg) doesn't paint a
        // redundant quad over the already-correct backdrop.
        let default_bg = term_colors[257]
            .map(|c| Rgb::new(c.r, c.g, c.b))
            .unwrap_or(theme.background);

        // Cycle 891 (audit): when this pane's default bg differs from the
        // surface clear color (e.g. an UNFOCUSED pane running a program that
        // set its own OSC 11 background, while the focused pane defines the
        // clear color), paint a backdrop over the pane interior. Without it
        // the per-cell loop below skips a quad for every default-bg cell —
        // correct for the focused pane (the clear already painted them) but
        // wrong here, so those cells leaked the *other* pane's background.
        //
        // Cover the interior only: inside the border (`bw`) and below/above
        // the titlebar strip, so we don't paint over the focus border or the
        // per-pane titlebar quad (both drawn by the caller before this call).
        // Alpha mirrors the surface clear so window transparency / darkness
        // applies to this pane's bg exactly as it does to the focused one.
        if default_bg != surface_bg {
            let bw = if cfg.handle_size < 0 {
                1.0
            } else {
                cfg.handle_size as f32
            };
            if let Some((bx, by, bwid, bhgt)) =
                pane_backdrop_rect(pv.rect, bw, pane_titlebar_h, cfg.title_at_bottom)
            {
                quads.push(rect(
                    bx,
                    by,
                    bwid,
                    bhgt,
                    default_bg,
                    composed_bg_alpha(cfg) as f32,
                ));
            }
        }

        // Cycle 827 (audit): take the pooled scratch (with last frame's String
        // buffers) instead of allocating fresh. `n` is the LOGICAL run count;
        // `spans` may hold extra slots from a busier prior frame, which we reuse
        // (clear + refill) before falling back to a push. Stored back to `self`
        // at the end of this method so the capacity recycles next frame.
        let mut spans = std::mem::take(&mut self.span_scratch);
        let mut span_line_breaks = std::mem::take(&mut self.span_breaks_scratch);
        span_line_breaks.clear();
        let mut n = 0usize;
        let mut cur_row = 0i32;
        // The style of the run currently being appended to (`spans[n - 1]`), or
        // `None` when the next char must open a new run.
        let mut cur: Option<(Rgb, bool, bool)> = None;

        // Cycle 939 (Terminator parity, cursor_fg_color / cursor_bg_color): a
        // focused SOLID block cursor renders the block in `theme.cursor`
        // (cursor-color / cursor-bg-color) with the glyph UNDER it recolored to
        // `theme.cursor_text` (cursor-fg-color) — the standard "inverted cursor"
        // model. Identify that grid-absolute cell so the span builder recolors
        // exactly its glyph. Only the full Block shape covers the glyph (beam /
        // underline leave it visible, so they aren't recolored).
        let recolor_cursor_cell: Option<(i32, usize)> = {
            let cp = content.cursor.point;
            let cvrow = cp.line.0 + display_off;
            if pv.focused
                && window_focused
                && cursor_visible
                && content.cursor.shape == EShape::Block
                && (0..screen_rows).contains(&cvrow)
            {
                Some((cp.line.0, cp.column.0))
            } else {
                None
            }
        };
        // Cycle 942 (audit): an OSC 12 runtime cursor color moves the block
        // out from under the theme's cursor/cursor_text pair, so the
        // recolored glyph follows reverse-video (its own cell bg) instead of
        // `theme.cursor_text` (which was tuned against `theme.cursor`).
        // Resolved once; the cursor-draw below resolves the same slot.
        let cursor_rt_override = color::resolve_query(258, theme, term_colors);
        // Cycle 942 (audit): a wide (CJK/emoji) glyph under the cursor needs
        // a TWO-cell block — recoloring the glyph to cursor_text while the
        // 1-cell block covered only its left half left the right half drawn
        // in cursor_text on the default bg (invisible on Mocha, where
        // cursor_text == background). And a cursor parked on the SPACER half
        // re-anchors to the lead glyph one cell left. Discovered during the
        // cell walk (the display iterator is single-pass); `None` = narrow
        // cell, draw as before.
        let mut cursor_wide_quad: Option<(usize, f32)> = None;

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let row = point.line.0;
            let col = point.column.0;
            // Cycle 912: viewport row for quad placement; `row` (grid-absolute,
            // negative when scrolled) stays for the relative line-break deltas.
            let vrow = row + display_off;
            if row != cur_row {
                cur = None; // runs never span a line break
                for _ in cur_row..row {
                    span_line_breaks.push(n);
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
            // Cycle 355 (Terminator parity, terminatorlib/config.py:111
            // `allow_bold`): when false, suppress bold attr entirely.
            // Useful on fonts without a bold companion.
            let bold = cfg.allow_bold && flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            let hidden = flags.contains(Flags::HIDDEN);
            // Cycle 355 (Terminator parity, terminatorlib/config.py:130
            // `bold_is_bright`): when true + bold + fg comes from
            // palette[0..8], remap to palette[8..16] (the xterm
            // bright variant). Color::bright_for_bold returns the
            // mapped color or the original if it's not a low-palette
            // index. No-op when bold isn't set.
            if bold && cfg.bold_is_bright {
                fg = color::bright_for_bold(fg, theme);
            }
            // Recolor the glyph sitting under a focused solid block cursor.
            // The second arm catches a cursor parked on the spacer half of a
            // wide glyph: the glyph lives one cell LEFT (the WIDE_CHAR lead),
            // and the block must cover both columns starting there.
            let lead_of_cursor_spacer =
                recolor_cursor_cell == Some((row, col + 1)) && flags.contains(Flags::WIDE_CHAR);
            if recolor_cursor_cell == Some((row, col)) || lead_of_cursor_spacer {
                if flags.contains(Flags::WIDE_CHAR) {
                    cursor_wide_quad = Some((col, 2.0));
                }
                fg = if cursor_rt_override.is_some() {
                    bg
                } else {
                    theme.cursor_text
                };
            }

            if bg != default_bg {
                quads.push(rect(
                    ox + col as f32 * cw,
                    oy + vrow as f32 * ch,
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
                let y = oy + vrow as f32 * ch;
                quads.push(rect(x, y + ch - 2.0, cw, 1.0, line_color, 1.0));
                if flags.contains(Flags::DOUBLE_UNDERLINE) {
                    quads.push(rect(x, y + ch - 4.0, cw, 1.0, line_color, 1.0));
                }
            }
            if flags.contains(Flags::STRIKEOUT) {
                quads.push(rect(
                    ox + col as f32 * cw,
                    oy + vrow as f32 * ch + ch * 0.5,
                    cw,
                    1.0,
                    fg,
                    1.0,
                ));
            }
            let dc = if hidden { ' ' } else { cell.c };
            match cur {
                Some((f, cb, ci)) if f == fg && cb == bold && ci == italic => {
                    // Same style — extend the current run (the last live span).
                    spans[n - 1].0.push(dc);
                }
                _ => {
                    // New run: reuse the pooled slot's String if one exists
                    // (clearing keeps its capacity), else push a fresh entry.
                    if n < spans.len() {
                        let slot = &mut spans[n];
                        slot.0.clear();
                        slot.0.push(dc);
                        slot.1 = fg;
                        slot.2 = bold;
                        slot.3 = italic;
                    } else {
                        let mut s = String::new();
                        s.push(dc);
                        spans.push((s, fg, bold, italic));
                    }
                    n += 1;
                    cur = Some((fg, bold, italic));
                }
            }
        }

        // Selection.
        if let Some(sel) = content.selection {
            let (s, e) = (sel.start, sel.end);
            for r in s.line.0..=e.line.0 {
                // Cycle 912: selection lines are grid-absolute; map to the
                // viewport row and clip to the visible screen. The old `r < 0`
                // guard DROPPED any selection scrolled up into history, and a
                // positive `r` was drawn at the wrong (un-offset) viewport y.
                let vrow = r + display_off;
                if vrow < 0 || vrow >= screen_rows {
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
                    oy + vrow as f32 * ch,
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
        // Cycle 150: also require cursor_visible. The old check fell
        // through to draw the hollow-outline branch on an unfocused
        // window even when DEC ?25l had hidden the cursor. So a
        // program that called `printf '\e[?25l'` (vim, less, fzf…)
        // and the user clicked away — the unfocused-pane outline
        // still showed. cursor_visible now gates everything; the
        // hollow-outline-for-HollowBlock-shape case stays inside the
        // visible branch since DECSCUSR shapes and DEC ?25 hide are
        // independent (a program can use HollowBlock to mean "I'm
        // not in this pane" while still wanting the cursor visible).
        // Cycle 916 (file-by-file audit): the cursor point is grid-absolute
        // (kettle never enters alacritty vi-mode), so when scrolled back
        // (display_offset > 0) it must convert to a viewport row like the cells
        // and selection already do (cycle 912) — else a phantom cursor block
        // paints over scrollback after the text has scrolled away. The old
        // `cp.line.0 >= 0` guard was dead (a writing cursor's absolute line is
        // always >= 0); the real visibility test is whether its viewport row is
        // on screen.
        let cvrow = cp.line.0 + display_off;
        let draw_cursor = shape != EShape::Hidden
            && (0..screen_rows).contains(&cvrow)
            && pv.focused
            && cursor_visible;
        if draw_cursor {
            // Cycle 942: a wide glyph under a solid block cursor widens the
            // block to both columns (and a spacer-parked cursor re-anchors to
            // the lead glyph's cell). `cursor_wide_quad` is only ever set on
            // the focused solid-Block path, so beam/underline/hollow shapes
            // and unfocused windows are untouched.
            let (bcol, bcells) = cursor_wide_quad.unwrap_or((cp.column.0, 1.0));
            let bx = ox + bcol as f32 * cw;
            let by = oy + cvrow as f32 * ch;
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
            } else {
                let (cwidth, alpha, cheight, yoff) = match shape {
                    EShape::Beam => (cw * 0.15, 1.0, ch, 0.0),
                    EShape::Underline => (cw, 1.0, 2.0, ch - 2.0),
                    // Cycle 939: a focused block cursor is SOLID (was a 0.55
                    // translucent tint) — the glyph under it is recolored to
                    // `theme.cursor_text` in the span builder, the standard
                    // inverted-cursor model + Terminator cursor_fg/bg parity.
                    // Cycle 942: `bcells` widens it over a wide (CJK/emoji)
                    // glyph so the recolored right half isn't left uncovered.
                    EShape::Block | EShape::HollowBlock | EShape::Hidden => {
                        (cw * bcells, 1.0, ch, 0.0)
                    }
                };
                quads.push(rect(bx, by + yoff, cwidth, cheight, cursor_color, alpha));
            }
        }

        // Cycle 301 vi-mode visual selection (sub-cycle 4). Drawn
        // BEFORE the vi cursor so the cursor's hollow block reads on
        // top of the selection's solid fill. Selection spans
        // [anchor..cursor] inclusive; the anchor / cursor order is
        // normalized to (start, end) ordered ascending.
        if pv.focused
            && let (Some((arow, acol)), Some((crow, ccol))) = (vi_visual_anchor, vi_cursor)
        {
            let (start, end) = if (arow, acol) <= (crow, ccol) {
                ((arow, acol), (crow, ccol))
            } else {
                ((crow, ccol), (arow, acol))
            };
            // Char-visual semantics (Alacritty default): start..end
            // sweeps cells row by row.
            let mut r = start.0;
            while r <= end.0 {
                if let Some((first, last)) = vi_selection_row_span(r, start, end, cols) {
                    let bx = ox + first as f32 * cw;
                    let by = oy + r as f32 * ch;
                    let bw = (last - first + 1) as f32 * cw;
                    quads.push(rect(bx, by, bw, ch, theme.selection_background, 0.55));
                }
                r += 1;
            }
        }

        // Cycle 300 vi-mode cursor (sub-cycle 3 of 4). When the user
        // is in vi-mode, draw a magenta hollow block at the vi
        // cursor's grid position over the focused pane. Distinct
        // from the terminal cursor (different color + always hollow,
        // even in focused-block mode) so the user can tell vi-mode
        // is on at a glance. Drawn only on the focused pane —
        // multi-pane setups don't paint vi cursors over inactive
        // panes.
        if pv.focused
            && let Some((vrow, vcol)) = vi_cursor
        {
            let vi_color = theme.palette[5]; // magenta — distinct from
            // broadcast yellow (3),
            // accent blue (4), text fg.
            let bx = ox + vcol as f32 * cw;
            let by = oy + vrow as f32 * ch;
            // Hollow block outline (4 quads). Same shape as the
            // HollowBlock terminal cursor above but a dedicated
            // color so the two never visually merge.
            quads.push(rect(bx, by, cw, 1.0, vi_color, 1.0));
            quads.push(rect(bx, by + ch - 1.0, cw, 1.0, vi_color, 1.0));
            quads.push(rect(bx, by, 1.0, ch, vi_color, 1.0));
            quads.push(rect(bx + cw - 1.0, by, 1.0, ch, vi_color, 1.0));
            // Faint fill so the block reads even on busy text.
            quads.push(rect(bx, by, cw, ch, vi_color, 0.20));
        }

        // Lay out the text buffer. Cycle 870: advance lines by the grid's
        // `cell_h` (which includes the cfg.cell_height multiplier) so the text
        // rows stay locked to the cursor/quad row step — see `pane_metrics`.
        let pm = pane_metrics(self.metrics.font_size, self.cell_h);
        let buf = &mut self.pane_buffers[idx];
        buf.set_metrics(&mut self.font_system, pm);
        buf.set_size(
            &mut self.font_system,
            Some((rw - cfg.padding_x * 2.0).max(1.0)),
            Some((rh - cfg.padding_y * 2.0).max(1.0)),
        );
        let ff = font_features(cfg);
        let default_attrs = Attrs::new()
            .family(Family::Name(family))
            .font_features(ff.clone());
        // Cycle 806 (audit perf): borrow each span's text as `&str` into the
        // rich-text vec instead of cloning it to an owned `String`. `spans` is
        // built above and not touched again, so it outlives this
        // `set_rich_text` call and the slices stay valid. This drops one heap
        // allocation per style run per pane per frame — a busy 120×50 pane of
        // colored output churned dozens–hundreds of `String`s (plus a `"\n"`
        // per wrapped row) every single frame. The `Vec<(&str, Attrs)>` type
        // also makes a future re-introduction of `text.clone()` a compile
        // error rather than a silent regression.
        // Only the first `n` slots are live this frame (the pool may carry more
        // from a busier prior frame); `set_rich_text` sees exactly those.
        let rich = build_rich_spans(&spans[..n], &span_line_breaks, cfg, &ff, &default_attrs);
        // Advanced shaping applies OpenType features (ligatures, ss##,
        // cv##, …). Drop to Basic only when ligatures are off *and* there
        // are no explicit features to honor — the fast path with no shaping.
        let shaping = if cfg.font_ligatures || !cfg.font_features.is_empty() {
            Shaping::Advanced
        } else {
            Shaping::Basic
        };
        // Hand the vec to `set_rich_text` by value: it takes an
        // `IntoIterator<Item = (&str, Attrs)>`, so consuming `rich` also drops
        // the per-run `Attrs` clone the old `.iter().map(|(s, a)| (s.as_str(),
        // a.clone()))` adapter used to make.
        buf.set_rich_text(&mut self.font_system, rich, &default_attrs, shaping, None);
        buf.shape_until_scroll(&mut self.font_system, false);
        // Return the scratch (with its grown String buffers) to the pool for the
        // next frame/pane. `rich` is already consumed, so `spans` is free.
        self.span_scratch = spans;
        self.span_breaks_scratch = span_line_breaks;
    }
}

/// OpenType features to shape pane text with: the coarse ligature toggle
/// expressed as `liga/clig/calt/dlig = 0` when off, then the user's explicit
/// `font-feature` overrides applied on top (so they can re-enable or tune
/// individual features). Cited: Ghostty `font-feature`, kitty `font_features`.
/// Upper bound on tiles a `tile` background may emit per frame before falling
/// back to a single stretched quad. ~60-px tiles on a 4K surface (3840×2160 →
/// 64×34 ≈ 2176) stay under it; only pathologically small source images
/// (≤ ~30 px) trip the cap. Cycle 825 (audit).
const MAX_BG_TILES: f32 = 4096.0;

/// Whether a `tile` background's source image yields a sane number of tiles for
/// the surface, or so many (a tiny source image) that the per-frame quad +
/// Arc-clone storm would hang the renderer and it should stretch instead. Zero
/// dims are treated as 1 px so we never divide by zero. Cycle 825 (audit).
fn bg_tiles_within_cap(surface_w: f32, surface_h: f32, img_w: f32, img_h: f32) -> bool {
    let tiles_x = (surface_w / img_w.max(1.0)).ceil().max(1.0);
    let tiles_y = (surface_h / img_h.max(1.0)).ceil().max(1.0);
    tiles_x * tiles_y <= MAX_BG_TILES
}

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

/// The inclusive `(first_col, last_col)` the vi-mode visual selection
/// highlights on grid row `r`, given the normalized `(start, end)` endpoints and
/// the pane's column count, or `None` when the row's span is empty.
///
/// Cycle 820 (audit): an intermediate (non-end) row now extends to the pane's
/// real last column (`cols - 1`), not a hardcoded `256`. On a pane wider than
/// 256 columns — common on 4K/ultrawide with a small font (a 3840-px pane at a
/// ~7-px cell is ~548 cols) — the middle rows of a multi-row visual selection
/// used to highlight only to column 256 while the selection still yanked the
/// full rows, a visible highlight/behavior mismatch.
fn vi_selection_row_span(
    r: usize,
    start: (usize, usize),
    end: (usize, usize),
    cols: usize,
) -> Option<(usize, usize)> {
    let first = if r == start.0 { start.1 } else { 0 };
    let last = if r == end.0 {
        end.1
    } else {
        cols.saturating_sub(1)
    };
    (last >= first).then_some((first, last))
}

/// Build a pane's rich-text runs as `(&str, Attrs)` pairs, **borrowing** each
/// span's text from `spans` rather than cloning it (cycle 806 hot-path
/// allocation fix). `"\n"` line-break markers are interleaved at the recorded
/// `line_breaks` indices, each carrying `default_attrs` (the row breaks
/// inherit the pane's default styling, exactly as before).
///
/// The returned `&'a str` slices are tied to `spans`, which every caller keeps
/// alive until after the `Buffer::set_rich_text` call consumes the vec — so
/// the per-frame allocation count stays flat no matter how many colored style
/// runs a pane has. Pre-sized to `spans.len() + line_breaks.len()` so it never
/// reallocs mid-build (cycle 761).
fn build_rich_spans<'a>(
    spans: &'a [(String, Rgb, bool, bool)],
    line_breaks: &[usize],
    cfg: &'a Config,
    ff: &'a FontFeatures,
    default_attrs: &Attrs<'a>,
) -> Vec<(&'a str, Attrs<'a>)> {
    let mut rich: Vec<(&'a str, Attrs<'a>)> = Vec::with_capacity(spans.len() + line_breaks.len());
    let mut nb = 0usize;
    for (i, (text, fg, bold, italic)) in spans.iter().enumerate() {
        while nb < line_breaks.len() && line_breaks[nb] == i {
            rich.push(("\n", default_attrs.clone()));
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
        rich.push((text.as_str(), a));
    }
    rich
}

/// Truncate `s` to at most `n` **display columns** (not chars), adding `…`
/// when something was cut. CJK characters and emoji are wide (2 cells
/// each), so a char-count truncation overflows the tab segment / title
/// when these are present; this honors the cell width that the renderer
/// Cycle 710 (regression fix): pick the per-pane titlebar background
/// color from the cycle-387 (focus, broadcast) state.
///
/// The focused branch used to fall back to a hardcoded
/// Terminator-bright `Rgb::new(0xc8, 0x00, 0x03)` which screamed
/// against dark themes like Tokyo Night Storm. The pane border (lib.rs
/// ~1209) and screenshot accent (lib.rs ~3136) already cascade through
/// `focused_split_color → accent_color → palette[4]` for theme-aware
/// focus signaling, so this mirrors that cascade. An explicit
/// `title_transmit_bg_color = #hex` still wins — anyone who pinned the
/// Terminator look keeps it.
///
/// Cycle 920: receive (broadcast) and inactive now ALSO derive from the theme
/// (they were hardcoded Terminator/legacy literals — `#0076c9` blue and
/// `#c0bebf` grey — that clashed with a dark theme like the Catppuccin Mocha
/// default). Broadcast mirrors the focused cascade (accent → `palette[4]`);
/// inactive falls back to the theme's surface `palette[8]`. Explicit
/// `title-*-bg-color` config still wins.
///
/// Pure so the cascade is drift-guarded without standing up wgpu.
pub(crate) fn pick_titlebar_bg(
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
    focused: bool,
    broadcast: bool,
) -> Rgb {
    if focused {
        cfg.title_transmit_bg_color
            .or(cfg.focused_split_color)
            .unwrap_or_else(|| cfg.resolved_accent(theme))
    } else if broadcast {
        cfg.title_receive_bg_color
            .unwrap_or_else(|| cfg.resolved_accent(theme))
    } else {
        cfg.title_inactive_bg_color.unwrap_or(theme.palette[8])
    }
}

/// Cap a cell count so `requested * cell_px + chrome_px <= 8192` —
/// the wgpu per-side texture limit. Returns at least 1 so a degenerate
/// clamp (huge font + huge padding) doesn't produce a zero-cell PNG.
/// Pure so the arithmetic is unit-tested without standing up wgpu.
pub fn cap_axis_cells(requested: u32, cell_px: f32, chrome_px: f32) -> u32 {
    const MAX_TEXTURE_PX: f32 = 8192.0;
    let cell = cell_px.max(1.0); // never divide by zero
    let safe_body = (MAX_TEXTURE_PX - chrome_px).max(cell);
    let cap = (safe_body / cell).floor() as u32;
    requested.min(cap).max(1)
}

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

/// Cycle 747: build glyphon [`Metrics`] for a *logical* `font_size` at a given
/// device-pixel `scale` (the window's `scale_factor`). glyphon shapes and
/// rasterizes in the same coordinate space as the wgpu surface, which winit
/// sizes in **physical** pixels — so a logical `font_size` must be multiplied
/// by the scale factor or text renders at `1/scale` of its intended size on
/// HiDPI displays. That was the "tiny font at 200% Windows scaling" bug:
/// `scale` was stored but never applied, so a 13pt font drew at ~6.5px on a 2×
/// monitor. The line height keeps the historical 1.25 ratio. `scale` is
/// sanitized (NaN / ≤0 → 1.0) so a bogus value can't produce zero-sized cells.
pub fn metrics_for(font_size: f32, scale: f32) -> Metrics {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let px = font_size * s;
    Metrics::new(px, px * 1.25)
}

/// Cycle 870: metrics for a terminal PANE's text buffer. The glyph size stays
/// `font_size` (the DPI-scaled px from `metrics_for`), but the LINE HEIGHT is
/// the grid's actual row height `cell_h` — which already folds in the 1.25
/// line-height ratio AND any `cfg.cell_height` multiplier (cycle 636). The
/// cursor and selection/vi quads step by `cell_h` per row (`by = oy + line *
/// ch`), so the text must advance by the same `cell_h`; laying it out at the
/// unscaled `metrics.line_height` instead drifts a fraction of a row per line —
/// a full row off near the bottom of a tall window whenever `cell_height != 1`.
pub fn pane_metrics(font_size: f32, cell_h: f32) -> Metrics {
    Metrics::new(font_size, cell_h)
}

/// Cycle 753: request a GPU adapter, preferring real hardware but transparently
/// retrying with a **software rasterizer** (Mesa llvmpipe / lavapipe, or WARP on
/// Windows) when no hardware adapter is available. Before this, all four adapter
/// sites passed `force_fallback_adapter: false` and hard-errored with "no
/// suitable GPU adapter" — so kettle could not start under **WSLg**, headless
/// VMs, minimal Linux installs, or GPU-less CI runners, where a software Vulkan/GL
/// ICD is the only option. Hardware is tried first so a machine with a real GPU
/// never silently drops to the slower software path; the fallback engages only
/// when the first request fails, and emits a `log::warn` so the degraded mode is
/// visible in `RUST_LOG`. `context` labels the call site in the error/warning.
async fn request_adapter_or_fallback(
    instance: &wgpu::Instance,
    options: &wgpu::RequestAdapterOptions<'_, '_>,
    context: &str,
) -> Result<wgpu::Adapter> {
    if let Ok(adapter) = instance.request_adapter(options).await {
        return Ok(adapter);
    }
    log::warn!(
        "{context}: no hardware GPU adapter; retrying with software fallback \
         (llvmpipe / lavapipe / WARP) — expected under WSLg / headless / VM / CI"
    );
    let fallback = wgpu::RequestAdapterOptions {
        power_preference: options.power_preference,
        compatible_surface: options.compatible_surface,
        force_fallback_adapter: true,
    };
    instance
        .request_adapter(&fallback)
        .await
        .map_err(|e| anyhow!("{context}: no GPU adapter, even software fallback: {e:?}"))
}

/// Cycle 756: number of header display-lines before the field rows in the
/// settings panel (title, category tabs, blank). The focused-row highlight
/// quad and the per-line text areas both index off this.
const SETTINGS_FIELD_START: usize = 3;

/// Cycle 756: build the settings panel's display lines from its renderer-side
/// projection — title, a category-tab strip (active category bracketed), a
/// blank, one `"▸ label        value"` line per field (focused row marked),
/// a blank, then the keybind footer. Shared by the buffer-text pass and the
/// quad/area pass so they stay in lockstep (same row count + ordering).
fn settings_display_lines(set: &SettingsOverlay) -> Vec<String> {
    let mut lines = Vec::with_capacity(set.rows.len() + SETTINGS_FIELD_START + 2);
    let cat = set
        .categories
        .get(set.active_category)
        .map(|s| s.as_str())
        .unwrap_or("");
    lines.push(format!("⚙  Settings — {cat}"));
    let tabs: Vec<String> = set
        .categories
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i == set.active_category {
                format!("[ {c} ]")
            } else {
                format!("  {c}  ")
            }
        })
        .collect();
    lines.push(tabs.join(" "));
    lines.push(String::new());
    for (i, row) in set.rows.iter().enumerate() {
        let mark = if i == set.focused_row { "▸ " } else { "  " };
        lines.push(format!("{mark}{:<26}{}", row.label, row.value));
    }
    lines.push(String::new());
    lines.push("↑↓ field    ←→ change    Tab category    Esc close".to_string());
    lines
}

/// Cycle 784: the settings panel's width in character cells — the widest
/// display line, so the panel grows to fit its content. Both render passes
/// (buffer-text + quad/highlight) call this off the same `settings_display_lines`
/// output, keeping them in lockstep. The old hardcoded 44 cols clipped the
/// ~50-cell footer hint ("Esc close" rendered as "Esc clo") and overflowed the
/// in-capture "‹press a chord — Esc to cancel›" prompt (~59 cells with its
/// 26-col label) onto the next row. A 44-col floor keeps a sparse category from
/// rendering as a cramped panel.
fn settings_panel_cols(lines: &[String]) -> f32 {
    use unicode_width::UnicodeWidthStr;
    lines.iter().map(|l| l.width()).max().unwrap_or(44).max(44) as f32
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

/// Build the right-click context-menu chrome quads — shadow, panel
/// background, 1-px border on each edge, per-row highlight bg + 2-px
/// accent strip, and inter-row separator lines. Pure: takes the menu
/// state + theme + cell metrics, returns the quads in draw order
/// (shadow first so the bg paints over it; bg second so the border
/// sits on its edge; etc.).
///
/// Shared between [`Renderer::render_frame`] and [`capture_png_with`]
/// so the live menu and the headless visual-regression screenshot
/// produce identical pixels. Cycle 251.
fn menu_chrome_quads(
    menu: &ContextMenu,
    theme: &kettle_config::Theme,
    accent: Rgb,
    cw: f32,
    ch: f32,
) -> Vec<QuadInstance> {
    let mut out: Vec<QuadInstance> = Vec::new();
    let max_chars = menu
        .rows
        .iter()
        .filter(|r| !r.separator)
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0) as f32;
    let panel_w = (max_chars * cw + 40.0).max(180.0);
    let row_h = ch + 12.0;
    let sep_h = 8.0_f32;
    // Cycle 714 (Terminator menu UX, C5): natural panel height
    // (sum of every row) may exceed the surface. App-side
    // `context_menu_geometry` already computed the clamped
    // height; if non-zero we honor it, otherwise fall back to
    // the natural sum (pre-cycle-714 behavior — no clamp).
    let natural_h: f32 = menu
        .rows
        .iter()
        .map(|r| if r.separator { sep_h } else { row_h })
        .sum();
    let panel_h = if menu.panel_h_clamped > 0.0 {
        menu.panel_h_clamped.min(natural_h)
    } else {
        natural_h
    };
    let clipped_top = menu.scroll_offset > 0;
    let clipped_bottom = panel_h < natural_h;
    let (ax, ay) = menu.anchor;

    // Soft drop shadow — offset 4 px down-right at low opacity for
    // depth (GTK / iTerm2 convention).
    out.push(rect(
        ax + 4.0,
        ay + 4.0,
        panel_w,
        panel_h,
        Rgb::new(0, 0, 0),
        0.35,
    ));
    // Panel background — theme.background opaque so the menu inherits
    // the pane bg color the user is calibrated for.
    out.push(rect(ax, ay, panel_w, panel_h, theme.background, 1.0));
    // 1-px border in dim chrome, each edge separate so a future tweak
    // can color them individually if needed.
    out.push(rect(ax, ay, panel_w, 1.0, theme.palette[8], 0.65));
    out.push(rect(
        ax,
        ay + panel_h - 1.0,
        panel_w,
        1.0,
        theme.palette[8],
        0.65,
    ));
    out.push(rect(ax, ay, 1.0, panel_h, theme.palette[8], 0.65));
    out.push(rect(
        ax + panel_w - 1.0,
        ay,
        1.0,
        panel_h,
        theme.palette[8],
        0.65,
    ));

    // Per-row highlight + separators. Cycle 714: skip scrolled-off
    // rows; stop drawing when we'd go past panel_h.
    let mut row_y = ay;
    let start = menu.scroll_offset.min(menu.rows.len());
    for (i, row) in menu.rows.iter().enumerate().skip(start) {
        let h = if row.separator { sep_h } else { row_h };
        if row_y + h > ay + panel_h {
            break;
        }
        if row.separator {
            out.push(rect(
                ax + 12.0,
                row_y + sep_h * 0.5 - 0.5,
                panel_w - 24.0,
                1.0,
                theme.palette[8],
                0.55,
            ));
            row_y += sep_h;
            continue;
        }
        if i == menu.highlight && row.enabled {
            // Soft accent tint across the row.
            out.push(rect(ax + 1.0, row_y, panel_w - 2.0, row_h, accent, 0.18));
            // 2-px accent strip on the left of the highlighted row —
            // same pattern as the cycle-178 active-tab accent and
            // cycle-184 focused-pane border.
            out.push(rect(ax + 1.0, row_y, 2.0, row_h, accent, 1.0));
        }
        row_y += row_h;
    }
    // Cycle 714 (Terminator menu UX, C5): ▲/▼ scroll arrows when
    // the natural list is clipped above or below. Drawn as small
    // accent-colored bars rather than glyphs so they don't need a
    // separate text-buffer path. The text-area loop in render_frame
    // also bakes literal ▲ / ▼ unicode into the row labels for
    // accessibility, but the chrome quad here gives the visual cue
    // even when the row glyph is otherwise occupied (e.g. the first
    // visible row's label might wrap).
    if clipped_top {
        // Centered 12-px wide accent bar near the top edge.
        let bar_w = 12.0;
        let bar_h = 3.0;
        let bx = ax + (panel_w - bar_w) * 0.5;
        let by = ay + 2.0;
        out.push(rect(bx, by, bar_w, bar_h, accent, 0.85));
    }
    if clipped_bottom {
        let bar_w = 12.0;
        let bar_h = 3.0;
        let bx = ax + (panel_w - bar_w) * 0.5;
        let by = ay + panel_h - 2.0 - bar_h;
        out.push(rect(bx, by, bar_w, bar_h, accent, 0.85));
    }
    out
}

fn srgb(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Cycle 380 (Terminator parity, terminatorlib/config.py:106 + 117):
/// compose the kettle background-opacity (cycle-X) with Terminator's
/// `background_darkness` + `background_type`. Logic:
///
///   bg-type = solid (default):  alpha = background_opacity
///   bg-type = transparent:      alpha = background_opacity * background_darkness
///   bg-type = image:            alpha = background_opacity (image render
///                               will land in a later bg-image sub-cycle;
///                               for now darkness applies same as transparent
///                               so users get the dim tint stage early)
///
/// All inputs already clamped at parse time so no defensive math needed.
/// Cycle 891 (audit): the interior rectangle of a pane to paint with its
/// own default background, given the pane `(x, y, w, h)`, border width
/// `bw`, titlebar strip height `pane_titlebar_h` (0 when off), and whether
/// the titlebar sits at the bottom. Returns the rect *inside* the border
/// and clear of the titlebar so the backdrop never overpaints the focus
/// border or the per-pane titlebar quad. `None` when the interior would be
/// empty (degenerate pane / border ≥ half the size).
fn pane_backdrop_rect(
    pane: (f32, f32, f32, f32),
    bw: f32,
    pane_titlebar_h: f32,
    title_at_bottom: bool,
) -> Option<(f32, f32, f32, f32)> {
    let (rx, ry, rw, rh) = pane;
    let (title_top, title_bot) = if title_at_bottom {
        (0.0, pane_titlebar_h)
    } else {
        (pane_titlebar_h, 0.0)
    };
    let bx = rx + bw;
    let by = ry + bw + title_top;
    let bwid = (rw - 2.0 * bw).max(0.0);
    let bhgt = (rh - 2.0 * bw - title_top - title_bot).max(0.0);
    (bwid > 0.0 && bhgt > 0.0).then_some((bx, by, bwid, bhgt))
}

fn composed_bg_alpha(cfg: &kettle_config::Config) -> f64 {
    use kettle_config::BackgroundType;
    match cfg.background_type {
        BackgroundType::Solid => cfg.background_opacity as f64,
        BackgroundType::Transparent | BackgroundType::Image => {
            (cfg.background_opacity as f64) * (cfg.background_darkness as f64)
        }
    }
}

fn measure_cell(
    fs: &mut FontSystem,
    buf: &mut TextBuffer,
    family: &str,
    metrics: Metrics,
) -> (f32, f32) {
    buf.set_metrics(fs, metrics);
    // Cycle 865 (audit): size the measure box relative to the (physical)
    // metrics, not a fixed 1000×100. At a large font on a high-DPI display the
    // physical font size can be ~200px, so the 10-glyph probe is ~1300px wide
    // and wrapped against the old 1000px box — `line_w` then reflected only the
    // first wrapped line and `cell_w` came out too narrow, mis-gridding the
    // terminal. A monospace `M` is ~0.6em, so 10 fit in ~6em; 20em + slack is
    // ample headroom that can never wrap regardless of size/scale.
    let box_w = metrics.font_size * 20.0 + 100.0;
    let box_h = metrics.line_height * 2.0 + 100.0;
    buf.set_size(fs, Some(box_w), Some(box_h));
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
/// Which synthetic scene to render in [`capture_png_with`]. Cycle 251.
///
/// The default screenshot path renders a single-pane, single-tab,
/// no-overlay representative frame — what `kettle --screenshot` ships
/// today. `ContextMenu` adds a synthetic right-click context menu over
/// the rendered pane so the menu's render path can be visually verified
/// without opening the windowed app. Visible only via the
/// `kettle --screenshot-menu PATH` CLI flag.
/// The kettle version label baked into the `--screenshot` demo scene's
/// `cargo test` compile line. Wired to the crate (= workspace) version
/// so the README hero / UX showcase screenshots can never re-stale to a
/// hardcoded string the way the original `kettle v0.1.0` did — by the
/// v2.x series that frozen literal made the hero image look years out of
/// date even though the pixels still matched the (equally frozen) scene.
/// `env!` resolves at compile time, so a release version bump regenerates
/// a correct screenshot with zero code churn (cycle 771).
pub(crate) const SCREENSHOT_DEMO_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugScene {
    /// Existing `--screenshot` behavior (cycle 168).
    #[default]
    Default,
    /// Render with a synthetic right-click context menu open over the
    /// pane. The menu carries the eight items kettle ships (Copy,
    /// Paste, sep, Split Right, Split Down, Close Pane, sep, New
    /// Tab) with the first enabled row highlighted, anchored at a
    /// fixed position so the resulting PNG is byte-deterministic
    /// across runs.
    ContextMenu,
}

/// Top edge (px from the surface top) of the passive "update available"
/// banner, given the surface height, the banner's own height, and the heights
/// of any **bottom-anchored** tab / status bars it must stack above.
///
/// Cycle 808 (audit): the banner is a non-modal bottom strip. When the user
/// puts the tab bar or status bar at the bottom (`tab-bar-pos = bottom` /
/// `status-bar = bottom`), drawing the banner flush at `surface_h - banner_h`
/// painted *over* that bar and — paired with the click handler that treated
/// the whole bottom band as the banner — stole its clicks (you couldn't switch
/// tabs while the banner showed). Stacking the banner above the bottom chrome
/// fixes both. Pure + shared so the renderer's draw and the App's hit-test
/// agree to the pixel; pass `0.0` for chrome that isn't bottom-anchored.
pub fn update_banner_top(
    surface_h: f32,
    banner_h: f32,
    bottom_tabbar_h: f32,
    bottom_status_h: f32,
) -> f32 {
    surface_h - banner_h - bottom_tabbar_h - bottom_status_h
}

/// Back-compat wrapper for the cycle-168 `capture_png` callers (the CLI
/// smoke + the cycle-236 `--screenshot` end-to-end CI step). Always
/// renders [`DebugScene::Default`].
pub fn capture_png(
    cfg: &Config,
    cols: u32,
    rows: u32,
    out: &std::path::Path,
) -> Result<(u32, u32)> {
    capture_png_with(cfg, cols, rows, out, DebugScene::Default)
}

/// Resolve the wgpu adapter kettle would use on this machine and
/// return a human-readable diagnostic string. Same setup as
/// [`capture_png_with`] — `wgpu::Instance::default()` + a default
/// `RequestAdapterOptions` — so the reported adapter is what the
/// live renderer / `--screenshot` / `--screenshot-menu` paths would
/// pick on this host.
///
/// Used by `kettle --gpu-info` so a user filing a "blank window" /
/// "no GPU adapter" bug report can attach the adapter / backend /
/// driver / texture-limit details without a windowed run. The same
/// answer would otherwise require launching the binary, hitting the
/// failure mode, and digging through `RUST_LOG=info` output.
pub fn gpu_info() -> Result<String> {
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = request_adapter_or_fallback(
            &instance,
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
            "gpu_info",
        )
        .await?;
        let info = adapter.get_info();
        let limits = adapter.limits();
        Ok(format!(
            "Backend:        {:?}\n\
             Adapter:        {}\n\
             Adapter type:   {:?}\n\
             Driver:         {}\n\
             Driver info:    {}\n\
             Vendor (PCI):   0x{:04x}\n\
             Device (PCI):   0x{:04x}\n\
             Max texture:    {} px / side\n\
             Max buffer:     {} bytes\n\
             Max bind groups: {}",
            info.backend,
            if info.name.is_empty() {
                "<unnamed>".to_string()
            } else {
                info.name
            },
            info.device_type,
            if info.driver.is_empty() {
                "<unknown>".to_string()
            } else {
                info.driver
            },
            if info.driver_info.is_empty() {
                "<unknown>".to_string()
            } else {
                info.driver_info
            },
            info.vendor,
            info.device,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
            limits.max_bind_groups,
        ))
    })
}

/// Render a screenshot PNG; returns the **actual** (cols, rows) used after
/// the cycle-119 texture-limit cap so the CLI can report what was rendered
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

/// Cycle 294: extended `capture_png_with` variant that adds an
/// optional bottom-left caption overlay (an "annotated screenshot" —
/// useful for docs, README hero images, and bug reports that want
/// to caption a screenshot with a version / repro / env note).
///
/// When `annotation` is `Some(text)`, after every existing render
/// pass kettle paints a translucent dark rect across the bottom 24px
/// of the image plus the text rendered in `theme.foreground`. When
/// `None`, this is identical to `capture_png_with`.
///
/// Hooked into the `--screenshot --annotate TEXT` CLI surface.
/// iTerm2's *persistent* annotations (in-terminal sticky notes
/// attached to scrollback positions) are a separate, multi-cycle
/// feature; this is just the screenshot caption.
pub fn capture_png_with_annotation(
    cfg: &Config,
    cols: u32,
    rows: u32,
    out: &std::path::Path,
    scene: DebugScene,
    annotation: Option<&str>,
) -> Result<(u32, u32)> {
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = request_adapter_or_fallback(
            &instance,
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
            "capture_png",
        )
        .await?;
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
        // Second pipelines for the `DebugScene::ContextMenu` overlay.
        // Allocated unconditionally (small, cheap) so the render pass
        // can always call `draw` / `render` on them — empty uploads
        // are a no-op. Mirrors the live `Renderer` (cycle 251).
        let mut menu_quads_pipe = QuadPipeline::new(&device, format);
        let mut menu_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let theme = &cfg.theme;
        let fam = cfg.font_family.clone();
        // Same clamp Renderer::new (cycle 118) and set_font_size apply.
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
        // `--rows ≤ 200` (cycle 69), but at a 72pt clamped font size the
        // cell can be ~35×90px — so 200 cols × 90px = 18000px wide
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
        let (wf, hf) = (w as f32, h as f32);
        let split_x = (wf / 2.0).round();

        let base = Attrs::new().family(Family::Name(&fam));
        let mut q: Vec<QuadInstance> = Vec::new();

        // --- Tab bar (redesigned: active accent + per-tab ✕ + trailing +).
        //
        // Tab labels are defined once and reused for BOTH the chrome geometry
        // and the text buffer below, so the highlighted segment + separators
        // always line up with the glyphs. Cycle 859 (audit): the old fixed
        // 240px segments were ~2× wider than the ~120px labels, so the second
        // tab's text floated inside the first tab's highlight.
        let tab0_label = " 1: zsh  ✕   ";
        let tab1_label = "2: ssh prod  ✕";
        let tabplus_label = "     +";
        // Monospace: one cell == `cw`, so a label's pixel width is its char
        // count × `cw`.
        let tab_text_left = 8.0_f32;
        let w0 = tab0_label.chars().count() as f32 * cw;
        let w1 = tab1_label.chars().count() as f32 * cw;
        q.push(rect(0.0, 0.0, wf, tab_h, theme.palette[8], 1.0));
        // Active tab 0: themed background + left accent bar, sized to its label.
        // Cycle 293/937: cascade through the resolved accent so peacock + the
        // theme's signature accent (Mocha mauve) show in --screenshot too.
        let screenshot_accent = cfg.resolved_accent(theme);
        q.push(rect(tab_text_left, 0.0, w0, tab_h, theme.background, 1.0));
        q.push(rect(tab_text_left, 0.0, 2.0, tab_h, screenshot_accent, 1.0));
        // Inactive tab 1: a mostly-solid dark box (slight bar tint so it reads
        // as "muted" vs the active tab) — without its own background the dim
        // label would sit grey-on-grey against the `palette[8]` bar
        // (cycle 859, audit).
        q.push(rect(
            tab_text_left + w0,
            0.0,
            w1,
            tab_h,
            theme.background,
            0.9,
        ));
        // Subtle separators at each tab's right edge.
        q.push(rect(
            tab_text_left + w0 - 1.0,
            0.0,
            1.0,
            tab_h,
            theme.palette[8],
            0.7,
        ));
        q.push(rect(
            tab_text_left + w0 + w1 - 1.0,
            0.0,
            1.0,
            tab_h,
            theme.palette[8],
            0.7,
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
        // Cycle 293/937: focused_split_color → resolved accent (explicit →
        // Peacock → theme signature), same order as the live renderer.
        let foc = cfg
            .focused_split_color
            .unwrap_or_else(|| cfg.resolved_accent(theme));
        let ly = tab_h;
        let lh = hf - tab_h;
        q.push(rect(0.0, ly, split_x, 1.0, foc, 1.0));
        q.push(rect(0.0, ly + lh - 1.0, split_x, 1.0, foc, 1.0));
        q.push(rect(0.0, ly, 1.0, lh, foc, 1.0));
        q.push(rect(split_x - 1.0, ly, 1.0, lh, foc, 1.0));

        // Block cursor sitting at the end of the left pane's idle prompt
        // (`kevim@kettle:~/Repos/kettle$ ` = 29 columns, so the cursor's empty
        // input cell is column 29). Cycle 859 (audit): the prompt text was
        // lengthened but this column wasn't, leaving the cursor stranded
        // mid-path on the "e" of `~/Repos/kettle`. Keep `cur_col` in sync with
        // the final prompt line in the `left` buffer below.
        let cur_row = 6.0;
        let cur_col = 29.0;
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

        let mut tab_buf = TextBuffer::new(&mut font_system, metrics);
        tab_buf.set_size(&mut font_system, Some(wf), Some(tab_h));
        tab_buf.set_rich_text(
            &mut font_system,
            [
                (tab0_label, fg.clone()),
                (tab1_label, dim.clone()),
                (tabplus_label, grn.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        tab_buf.shape_until_scroll(&mut font_system, false);

        // The demo `cargo test` compile line carries the live crate
        // version (cycle 771) — never a hardcoded literal — so the hero /
        // showcase screenshots track the real product version forever.
        let compile_line = format!("kettle v{SCREENSHOT_DEMO_VERSION}\n");
        let mut left = TextBuffer::new(&mut font_system, metrics);
        left.set_size(&mut font_system, Some(split_x - pad), Some(lh));
        left.set_rich_text(
            &mut font_system,
            [
                ("kevim@kettle", grn.clone()),
                (":", fg.clone()),
                ("~/Repos/kettle", blu.clone()),
                // Keep this command short enough that it never wraps even in the
                // narrow showcase split (~50-col left pane) — a wrap would push
                // every line down one and strand the hardcoded `cur_row` cursor
                // on a blank line (cycle 859, audit).
                ("$ cargo test\n", fg.clone()),
                ("   Compiling ", dim.clone()),
                (compile_line.as_str(), dim.clone()),
                ("    Finished ", grn.clone()),
                ("`test` profile [optimized]\n", fg.clone()),
                ("     Running ", grn.clone()),
                ("unittests\n", fg.clone()),
                ("test result: ", fg.clone()),
                ("ok", grn.clone()),
                (". 550 passed; 0 failed\n\n", fg.clone()),
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
                ("\n  splits · tabs · search · settings ✓\n", dim.clone()),
                ("  keybinds · sixel · kitty · OSC 8 ✓", dim.clone()),
            ],
            &base,
            Shaping::Advanced,
            None,
        );
        right.shape_until_scroll(&mut font_system, false);

        // Cycle 294: optional caption overlay at the bottom of the
        // image. When `annotation` is Some, paint a translucent dark
        // strip across the bottom 24px + render the caption text in
        // theme.foreground. Useful for docs, README hero images, and
        // bug reports that want to caption a screenshot with a
        // version / repro / env note.
        let mut annotate_buf = TextBuffer::new(&mut font_system, metrics);
        let annotate_h = (ch + 8.0).max(24.0);
        if let Some(text) = annotation {
            annotate_buf.set_size(&mut font_system, Some(wf - 16.0), Some(annotate_h));
            annotate_buf.set_text(&mut font_system, text, &base, Shaping::Advanced, None);
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

        let mut areas = vec![
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
        // Cycle 294: append the annotation TextArea if set. Bottom-
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

        // `DebugScene::ContextMenu`: build a synthetic context menu at
        // a fixed anchor (so the resulting PNG is byte-deterministic)
        // with the same eight items the live `App::context_menu_items`
        // ships. Quads go through the shared `menu_chrome_quads`
        // helper; text areas are built inline here because the
        // capture-path text-buffer pool is local to this function.
        // Cycle 251.
        let mut menu_text_buffers: Vec<TextBuffer> = Vec::new();
        let mut menu_q: Vec<QuadInstance> = Vec::new();
        let mut menu_areas: Vec<TextArea> = Vec::new();
        if scene == DebugScene::ContextMenu {
            // 8 items mirroring `App::context_menu_items`. Copy is
            // *disabled* in the synthetic scene because there is no
            // selection (matches the more-common state a user opens
            // the menu in). Highlight starts on Paste (idx 1), the
            // first enabled non-separator row.
            let rows = vec![
                ContextMenuRow {
                    label: "Copy".into(),
                    separator: false,
                    enabled: false,
                },
                ContextMenuRow {
                    label: "Paste".into(),
                    separator: false,
                    enabled: true,
                },
                ContextMenuRow {
                    label: String::new(),
                    separator: true,
                    enabled: false,
                },
                ContextMenuRow {
                    label: "Split Right".into(),
                    separator: false,
                    enabled: true,
                },
                ContextMenuRow {
                    label: "Split Down".into(),
                    separator: false,
                    enabled: true,
                },
                ContextMenuRow {
                    label: "Close Pane".into(),
                    separator: false,
                    enabled: true,
                },
                ContextMenuRow {
                    label: String::new(),
                    separator: true,
                    enabled: false,
                },
                ContextMenuRow {
                    label: "New Tab".into(),
                    separator: false,
                    enabled: true,
                },
            ];
            let menu = ContextMenu {
                // Anchor at a fixed offset from the top-left chrome.
                // Keeps the resulting PNG deterministic regardless of
                // window dimensions (--cols / --rows from CLI).
                anchor: (pad + cw * 2.0, tab_h + pad + ch * 2.0),
                rows,
                highlight: 1,
                // Cycle 714: deterministic screenshot fixture stays
                // unscrolled + unclamped (the harness paints all 8
                // rows in their natural height).
                scroll_offset: 0,
                panel_h_clamped: 0.0,
            };
            menu_q.extend(menu_chrome_quads(
                &menu,
                theme,
                cfg.resolved_accent(theme),
                cw,
                ch,
            ));

            // Text areas — one TextBuffer per non-separator row.
            // Positioning mirrors the live renderer's menu block.
            let max_chars = menu
                .rows
                .iter()
                .filter(|r| !r.separator)
                .map(|r| r.label.chars().count())
                .max()
                .unwrap_or(0) as f32;
            let panel_w = (max_chars * cw + 40.0).max(180.0);
            let row_h = ch + 12.0;
            let sep_h = 8.0_f32;
            let (ax, ay) = menu.anchor;
            // Allocate buffers first (one per row, separators get an
            // empty placeholder so indices align with `menu.rows`).
            for row in &menu.rows {
                let mut buf = TextBuffer::new(&mut font_system, metrics);
                if !row.separator {
                    buf.set_metrics(&mut font_system, metrics);
                    buf.set_size(&mut font_system, Some(panel_w), Some(row_h));
                    buf.set_text(
                        &mut font_system,
                        &row.label,
                        &Attrs::new().family(Family::Name(&fam)),
                        Shaping::Advanced,
                        None,
                    );
                    buf.shape_until_scroll(&mut font_system, false);
                }
                menu_text_buffers.push(buf);
            }
            // Now build TextAreas referring to the freshly-shaped
            // buffers. Borrow rules: collect indices first, then push
            // areas in a second pass so the borrow checker sees a
            // single shared borrow at the time of `menu_areas.push`.
            let mut row_y = ay;
            for (i, row) in menu.rows.iter().enumerate() {
                if row.separator {
                    row_y += sep_h;
                    continue;
                }
                let fg = if row.enabled {
                    theme.foreground
                } else {
                    Rgb::new(
                        ((theme.foreground.r as u16 + theme.background.r as u16 * 5) / 6) as u8,
                        ((theme.foreground.g as u16 + theme.background.g as u16 * 5) / 6) as u8,
                        ((theme.foreground.b as u16 + theme.background.b as u16 * 5) / 6) as u8,
                    )
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
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: srgb(bg.r),
                            g: srgb(bg.g),
                            b: srgb(bg.b),
                            // Cycle 380: route through composed_bg_alpha
                            // so the screenshot path also honors
                            // background-type + background-darkness.
                            // Cycle 149: honor cfg.background_opacity
                            // here too. The live-window clear op
                            // already did (line ~862), but the
                            // screenshot path hardcoded `a: 1.0` —
                            // so `kettle --screenshot --config
                            // /transparent.conf` produced an opaque
                            // PNG regardless. PNG is RGBA8 and stores
                            // the alpha channel directly; honoring
                            // the config makes the screenshot match
                            // what the live window shows.
                            a: composed_bg_alpha(cfg),
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
            // Cycle 251: menu chrome + menu text, same pass order as
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
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = match request_adapter_or_fallback(
            &instance,
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
            "offscreen_selftest",
        )
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
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn gpu_pipelines_compile_and_render_offscreen() {
        match super::offscreen_selftest() {
            Ok(true) => {}
            Ok(false) => eprintln!("no GPU adapter on this host; skipped"),
            Err(e) => panic!("offscreen GPU self-test failed: {e}"),
        }
    }

    /// Render one solid quad of a known *dark* sRGB color (#1a1b23) covering
    /// an sRGB target and read pixel (0,0) back. `Ok(None)` on a GPU-less host.
    fn srgb_quad_roundtrip_sample() -> Result<Option<[u8; 3]>> {
        pollster::block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = match request_adapter_or_fallback(
                &instance,
                &wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::None,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                },
                "srgb_quad_roundtrip",
            )
            .await
            {
                Ok(a) => a,
                Err(_) => return Ok(None),
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-srgb-test"),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow!("device: {e:?}"))?;

            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let mut quads = QuadPipeline::new(&device, format);
            quads.upload(
                &device,
                &queue,
                [8.0, 8.0],
                &[QuadInstance {
                    pos: [0.0, 0.0],
                    size: [8.0, 8.0],
                    color: [26.0 / 255.0, 27.0 / 255.0, 35.0 / 255.0, 1.0],
                }],
            );
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("kettle-srgb-target"),
                size: wgpu::Extent3d {
                    width: 8,
                    height: 8,
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
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded = (8u32 * 4).div_ceil(align) * align;
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kettle-srgb-readback"),
                size: (padded * 8) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut enc =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("kettle-srgb-pass"),
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
            }
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(8),
                    },
                },
                wgpu::Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(std::iter::once(enc.finish()));

            let slice = staging.slice(..);
            let done = Arc::new(AtomicBool::new(false));
            let done_set = done.clone();
            slice.map_async(wgpu::MapMode::Read, move |_| {
                done_set.store(true, Ordering::SeqCst);
            });
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            if !done.load(Ordering::SeqCst) {
                return Err(anyhow!("srgb readback timed out"));
            }
            let mapped = slice.get_mapped_range();
            let px = [mapped[0], mapped[1], mapped[2]];
            drop(mapped);
            staging.unmap();
            Ok(Some(px))
        })
    }

    /// Cycle 797 drift guard. A dark sRGB quad (#1a1b23) drawn to an sRGB
    /// target must read back ≈ #1a1b23, NOT the gamma-lifted ~#5a5f68 that the
    /// missing sRGB→linear decode in the quad shader produced (full-screen
    /// TUIs like AstroNvim set an explicit bg on every cell, so the lift
    /// washed out the whole screen). Allows a few units per channel for the
    /// linear↔sRGB round-trip + 8-bit quantization.
    #[test]
    fn quad_pipeline_does_not_gamma_lift_on_srgb_target() {
        match srgb_quad_roundtrip_sample() {
            Ok(None) => eprintln!("no GPU adapter on this host; skipped"),
            Ok(Some([r, g, b])) => {
                assert!(
                    r < 40 && g < 40 && b < 48,
                    "quad gamma-lifted: got #{r:02x}{g:02x}{b:02x}, expected ≈ #1a1b23 \
                     (regression: the sRGB→linear decode in quad.rs's shader was removed)"
                );
                assert!(
                    r > 10 && b > 20,
                    "quad crushed too dark: #{r:02x}{g:02x}{b:02x}"
                );
            }
            Err(e) => panic!("srgb round-trip render failed: {e}"),
        }
    }
}

#[cfg(test)]
mod screenshot_demo_tests {
    use super::SCREENSHOT_DEMO_VERSION;

    /// Cycle 771 drift guard. The README hero / UX showcase screenshots are
    /// generated from the hardcoded `DebugScene::Default` scene, whose demo
    /// `cargo test` compile line used to bake a literal `kettle v0.1.0` into
    /// the rendered pixels. By the v2.x series that frozen string made the
    /// hero image look years out of date even though the PNG still matched
    /// the (equally frozen) scene. The version is now sourced from the crate
    /// (= workspace) version via `env!`, so a release bump regenerates a
    /// correct screenshot for free. Guard that wiring so a future edit can't
    /// silently reintroduce a hardcoded / stale version label.
    #[test]
    fn screenshot_demo_version_tracks_crate_version() {
        assert_eq!(
            SCREENSHOT_DEMO_VERSION,
            env!("CARGO_PKG_VERSION"),
            "the --screenshot demo version must track the crate version, not a literal"
        );
        assert_ne!(
            SCREENSHOT_DEMO_VERSION, "0.1.0",
            "the hero/showcase screenshot must not advertise the legacy v0.1.0"
        );
        assert!(
            !SCREENSHOT_DEMO_VERSION.starts_with("0."),
            "the demo screenshot should advertise the real (>=1.0) product version, got {SCREENSHOT_DEMO_VERSION}"
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::field_reassign_with_default,
    reason = "stepwise field set reads more clearly than a 80-field struct literal here"
)]
mod pick_titlebar_bg_tests {
    use super::pick_titlebar_bg;
    use kettle_config::{Config, Rgb, Theme};

    /// Cycle 710 drift guard. The focused titlebar must NEVER fall
    /// through to the historic hardcoded `#c80003` Terminator red.
    ///
    /// Cascade order (cycle 937 folds accent-color + the theme accent into
    /// `Config::resolved_accent`):
    ///   1. explicit `title_transmit_bg_color = #hex`
    ///   2. `focused_split_color` (cycle 271 split-border override)
    ///   3. resolved accent = explicit `accent-color` → Peacock auto →
    ///      `theme.accent` (the theme's signature accent — Catppuccin Mocha's
    ///      mauve; `palette[4]` for themes without one)
    ///
    /// Unfocused panes stay on their pre-cycle-710 neutral fallbacks
    /// so the gray + blue (broadcast) defaults don't regress.
    #[test]
    fn focused_titlebar_uses_accent_cascade_when_unset() {
        let theme = Theme::by_name("Default"); // falls back to Catppuccin Mocha
        let mut cfg = Config::default();
        cfg.title_transmit_bg_color = None;
        cfg.focused_split_color = None;
        cfg.accent_color = None;
        // Default fallback is the theme's signature accent (Mocha mauve), not
        // the hardcoded `#c80003` red nor a bare `palette[4]`.
        let bg = pick_titlebar_bg(&cfg, &theme, true, false);
        assert_eq!(bg, theme.accent);
        assert_eq!(
            theme.accent,
            Rgb::new(0xcb, 0xa6, 0xf7),
            "Mocha accent = mauve"
        );
        assert_ne!(
            bg,
            Rgb::new(0xc8, 0x00, 0x03),
            "the hardcoded Terminator red MUST NOT be the focused-titlebar fallback"
        );
        // 3. accent_color wins over palette[4].
        let accent = Rgb::new(0x00, 0xaa, 0x00);
        cfg.accent_color = Some(accent);
        assert_eq!(pick_titlebar_bg(&cfg, &theme, true, false), accent);
        // 2. focused_split_color wins over accent_color.
        let split = Rgb::new(0xff, 0x88, 0x00);
        cfg.focused_split_color = Some(split);
        assert_eq!(pick_titlebar_bg(&cfg, &theme, true, false), split);
        // 1. explicit title_transmit_bg_color wins over all (preserves
        //    the Terminator-look pin for any user who set it).
        let pinned = Rgb::new(0xc8, 0x00, 0x03);
        cfg.title_transmit_bg_color = Some(pinned);
        assert_eq!(pick_titlebar_bg(&cfg, &theme, true, false), pinned);
    }

    /// Cycle 920: unfocused + non-broadcast derives from the theme's surface
    /// `palette[8]` (was a hardcoded `#c0bebf` grey that clashed with dark
    /// themes like the Catppuccin Mocha default). An explicit
    /// `title-inactive-bg-color` still wins.
    #[test]
    fn unfocused_titlebar_derives_from_theme_surface() {
        let theme = Theme::by_name("Default");
        let mut cfg = Config::default();
        cfg.title_inactive_bg_color = None;
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, false, false),
            theme.palette[8]
        );
        let pinned = Rgb::new(0x33, 0x33, 0x33);
        cfg.title_inactive_bg_color = Some(pinned);
        assert_eq!(pick_titlebar_bg(&cfg, &theme, false, false), pinned);
    }

    /// Cycle 920/937: unfocused + broadcast mirrors the focused cascade
    /// (`title-receive-bg-color → resolved accent`) — was a hardcoded `#0076c9`
    /// Terminator blue. The resolved accent defaults to the theme's signature
    /// accent (Mocha mauve). An explicit value still wins.
    #[test]
    fn broadcast_titlebar_derives_from_theme_accent() {
        let theme = Theme::by_name("Default"); // Catppuccin Mocha
        let mut cfg = Config::default();
        cfg.title_receive_bg_color = None;
        cfg.accent_color = None;
        assert_eq!(pick_titlebar_bg(&cfg, &theme, false, true), theme.accent);
        // accent_color wins over the theme fallback.
        let accent = Rgb::new(0x12, 0x34, 0x56);
        cfg.accent_color = Some(accent);
        assert_eq!(pick_titlebar_bg(&cfg, &theme, false, true), accent);
    }
}

#[cfg(test)]
mod cap_axis_cells_tests {
    use super::cap_axis_cells;

    #[test]
    fn cap_axis_cells_respects_8192_texture_limit() {
        // Small cells × small request: no-op (request passes through).
        assert_eq!(cap_axis_cells(80, 8.0, 16.0), 80);
        // 72pt-ish cell (~90px tall): 200 rows × 90 = 18000 > 8192.
        // Cap: (8192 - chrome) / 90 ≈ 90 rows.
        let c = cap_axis_cells(200, 90.0, 0.0);
        assert!(c <= 91, "200×90px should cap near 91 rows, got {c}");
        assert!(c >= 80, "but shouldn't collapse below ~80, got {c}");
        // Chrome (window padding + tab bar) shrinks the body budget.
        let c2 = cap_axis_cells(200, 90.0, 200.0);
        assert!(c2 < c, "more chrome means fewer body cells: {c2} < {c}");
        // Floor at 1: even with absurd inputs that would yield 0 or
        // negative, the result is at least 1 (so a degenerate
        // screenshot is a tiny image, not a panic).
        assert_eq!(cap_axis_cells(50, 1e6, 0.0), 1);
        assert_eq!(cap_axis_cells(50, 50.0, 1e6), 1);
        // Zero / NaN-cell-px clamped via the .max(1.0) inside; doesn't
        // divide by zero.
        assert_eq!(cap_axis_cells(1, 0.0, 0.0), 1);
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
mod hidpi_scale_tests {
    use super::{measure_cell, metrics_for, pane_metrics};
    use glyphon::{Buffer as TextBuffer, FontSystem};

    /// Cycle 870: the pane text buffer must advance lines by the grid's `cell_h`
    /// (which includes the `cfg.cell_height` multiplier) so the cursor and
    /// selection/vi quads — which step by `cell_h` per row — stay locked to the
    /// text. Laying out at the unscaled `metrics.line_height` drifts a fraction
    /// of a row per line, a full row off near the bottom when cell_height != 1.
    #[test]
    fn pane_metrics_line_height_tracks_cell_h_not_font_line_height() {
        let base = metrics_for(16.0, 1.0); // line_height = 20.0
        let cell_h = base.line_height * 1.4; // e.g. cfg.cell_height = 1.4
        let pm = pane_metrics(base.font_size, cell_h);
        assert_eq!(pm.line_height, cell_h, "text line step must equal cell_h");
        assert_eq!(
            pm.font_size, base.font_size,
            "glyph size must NOT be scaled by cell_height"
        );
        // At cell_height == 1.0 the pane line height equals the base metric.
        let pm1 = pane_metrics(base.font_size, base.line_height);
        assert_eq!(pm1.line_height, base.line_height);
    }

    /// Cycle 747 core invariant: a logical font size renders at
    /// `font_size × scale` physical pixels. This is the bug that made text
    /// tiny on a 200%-scaled Windows 11 display — `scale` was stored but the
    /// metrics ignored it, so a 13pt font drew at ~6.5px on a 2× monitor.
    #[test]
    fn metrics_scale_with_dpi_factor() {
        // 1× display: physical == logical.
        let m1 = metrics_for(13.0, 1.0);
        assert!((m1.font_size - 13.0).abs() < f32::EPSILON);
        assert!((m1.line_height - 13.0 * 1.25).abs() < f32::EPSILON);
        // 2× (200% Windows scaling / Retina): physical is doubled.
        let m2 = metrics_for(13.0, 2.0);
        assert!((m2.font_size - 26.0).abs() < f32::EPSILON);
        assert!((m2.line_height - 26.0 * 1.25).abs() < f32::EPSILON);
        // 1.5× (150%, common Surface scaling).
        let m15 = metrics_for(20.0, 1.5);
        assert!((m15.font_size - 30.0).abs() < f32::EPSILON);
    }

    /// A bogus scale (0, negative, NaN, inf) must not zero or NaN the cell —
    /// it falls back to 1× rather than producing degenerate metrics.
    #[test]
    fn metrics_sanitize_bad_scale() {
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            let m = metrics_for(13.0, bad);
            assert!((m.font_size - 13.0).abs() < f32::EPSILON, "scale {bad}");
        }
    }

    /// End-to-end: the measured cell box scales (≈) with the DPI factor, so
    /// the grid (cols×rows from physical window size ÷ physical cell) stays
    /// consistent. Uses the embedded font — no GPU required.
    #[test]
    fn measured_cell_doubles_at_2x() {
        let mut fs = FontSystem::new();
        for face in kettle_config::font::all() {
            fs.db_mut().load_font_data(face.to_vec());
        }
        let fam = "JetBrains Mono";
        let m1 = metrics_for(16.0, 1.0);
        let mut b1 = TextBuffer::new(&mut fs, m1);
        let (w1, h1) = measure_cell(&mut fs, &mut b1, fam, m1);
        let m2 = metrics_for(16.0, 2.0);
        let mut b2 = TextBuffer::new(&mut fs, m2);
        let (w2, h2) = measure_cell(&mut fs, &mut b2, fam, m2);
        // Allow a little slack for hinting/rounding, but it must be ~2×, not 1×.
        assert!(
            (w2 / w1 - 2.0).abs() < 0.15,
            "cell width should ≈ double at 2× scale: {w1} → {w2}"
        );
        assert!(
            (h2 / h1 - 2.0).abs() < 0.15,
            "cell height should ≈ double at 2× scale: {h1} → {h2}"
        );
    }

    /// Cycle 865 (audit): at a large font on a high-DPI display the 10-glyph
    /// measure probe (~1300px at 72pt×3) exceeded the old fixed 1000px measure
    /// box and wrapped, so `cell_w` came out too narrow and mis-gridded the
    /// terminal. With the metrics-relative box it must scale linearly.
    #[test]
    fn measured_cell_does_not_wrap_at_large_font_highdpi() {
        let mut fs = FontSystem::new();
        for face in kettle_config::font::all() {
            fs.db_mut().load_font_data(face.to_vec());
        }
        let fam = "JetBrains Mono";
        let m1 = metrics_for(72.0, 1.0);
        let mut b1 = TextBuffer::new(&mut fs, m1);
        let (w1, _) = measure_cell(&mut fs, &mut b1, fam, m1);
        // 72pt × 3 = 216px physical; the ~1300px probe would have wrapped the
        // old 1000px box. Width must still scale ~3×.
        let m3 = metrics_for(72.0, 3.0);
        let mut b3 = TextBuffer::new(&mut fs, m3);
        let (w3, _) = measure_cell(&mut fs, &mut b3, fam, m3);
        assert!(
            (w3 / w1 - 3.0).abs() < 0.15,
            "cell width must scale ~3× without wrapping: {w1} → {w3}"
        );
    }
}

#[cfg(test)]
mod pane_buffer_lifecycle_tests {
    /// Cycle 749 drift guard. The per-pane text-buffer vecs are grown with
    /// `while len < panes.len()` and must be truncated back down when panes
    /// close, or they sit at the session's high-water pane count holding idle
    /// glyph buffers. A behavioral test would need a full GPU `Renderer`, so
    /// pin the invariant at the source level (same shape as term.rs's
    /// detach-never-joins guard): both truncate calls must stay present.
    #[test]
    fn render_frame_truncates_pane_buffers_on_shrink() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("self.pane_buffers.truncate(panes.len())"),
            "pane_buffers must be truncated to panes.len() so closed panes \
             don't leak their text buffers"
        );
        assert!(
            src.contains("self.pane_titlebar_buffers.truncate(panes.len())"),
            "pane_titlebar_buffers must be truncated to panes.len() too"
        );
    }

    /// Cycle 891 (audit): an unfocused pane carrying its own OSC 11
    /// background must paint a backdrop over its interior, because the
    /// per-cell loop skips default-bg cells (they'd otherwise leak the
    /// focused pane's clear color). The backdrop rect must stay INSIDE the
    /// border and clear of the titlebar strip so it never overpaints the
    /// focus border or per-pane titlebar.
    #[test]
    fn pane_backdrop_rect_stays_inside_border_and_titlebar() {
        use super::pane_backdrop_rect;
        // 200x150 pane at (10, 20), 2px border, 18px titlebar at the top.
        let pane = (10.0, 20.0, 200.0, 150.0);
        let (x, y, w, h) = pane_backdrop_rect(pane, 2.0, 18.0, false).unwrap();
        // Inside the left/top border, below the top titlebar.
        assert_eq!(x, 12.0);
        assert_eq!(y, 40.0); // 20 + 2 (border) + 18 (titlebar)
        assert_eq!(w, 196.0); // 200 - 2*2
        assert_eq!(h, 128.0); // 150 - 2*2 - 18
        // Backdrop must end at/above the bottom border.
        assert!(y + h <= pane.1 + pane.3 - 2.0 + f32::EPSILON);

        // Titlebar at the bottom: interior shifts to leave the bottom strip.
        let (_, yb, _, hb) = pane_backdrop_rect(pane, 2.0, 18.0, true).unwrap();
        assert_eq!(yb, 22.0); // 20 + 2 (border), no top titlebar
        assert_eq!(hb, 128.0); // 150 - 2*2 - 18 (bottom titlebar)

        // No titlebar (h = 0): interior is the full pane minus border.
        let (_, y0, _, h0) = pane_backdrop_rect(pane, 1.0, 0.0, false).unwrap();
        assert_eq!(y0, 21.0);
        assert_eq!(h0, 148.0);

        // Degenerate pane (border ≥ half the size) → None, no quad pushed.
        assert!(pane_backdrop_rect((0.0, 0.0, 3.0, 3.0), 2.0, 0.0, false).is_none());
    }

    /// Cycle 892 (audit): the background-image cache must (a) key on blur
    /// radius so toggling `background-blur` reloads, and (b) be freed when
    /// the config moves away from `background-type = image`. Pinned at the
    /// source level — exercising it needs a full GPU `Renderer`.
    #[test]
    fn bg_image_cache_keys_on_blur_and_frees_on_disable() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("Option<(String, u32, Option<kettle_core::ImageData>)>"),
            "bg_image_cache must key on (path, blur_radius) so a blur toggle \
             reloads even when the path is unchanged"
        );
        assert!(
            src.contains("p != &want") && src.contains("*b != blur_radius"),
            "need_reload must compare blur radius, not just the path"
        );
        assert!(
            src.contains("} else if self.bg_image_cache.is_some() {")
                && src.contains("self.bg_image_cache = None;"),
            "the decoded wallpaper must be freed when background-type leaves \
             image / the path is cleared"
        );
        // Cycle 919 (audit L2): a FAILED decode self-heals on a THROTTLE — the
        // reload condition includes `img.is_none()` gated on `bg_image_retry_at`,
        // so a transient error / in-place fix recovers without re-decoding a
        // broken path every frame.
        assert!(
            src.contains("img.is_none()") && src.contains("self.bg_image_retry_at"),
            "a failed bg-image decode must retry (img.is_none()) but throttled \
             via bg_image_retry_at — self-heal without per-frame thrash"
        );
        // Cycle 918: on a needed reload the key is stored UNCONDITIONALLY (inner
        // None on decode failure), and only a successfully-decoded entry renders.
        // Together these stop a stale wallpaper rendering for a broken new path
        // and stop re-decoding the failing file every frame.
        assert!(
            src.contains("self.bg_image_cache = Some((want.clone(), blur_radius, decoded));"),
            "a failed decode must still cache the (path, blur) key to avoid a \
             per-frame re-decode of the broken path"
        );
        assert!(
            src.contains("if let Some((_, _, Some(data))) = self.bg_image_cache.as_ref()"),
            "only a successfully-decoded cache entry may render (no stale image)"
        );
    }

    /// Cycle 891 (audit): the unfocused-pane backdrop is gated on the pane's
    /// own default bg differing from the surface clear color, and pinned to
    /// the build_pane path. Source-level guard (behavioral check needs GPU).
    #[test]
    fn unfocused_pane_backdrop_is_wired_into_build_pane() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("if default_bg != surface_bg {"),
            "build_pane must paint a backdrop when this pane's default bg \
             differs from the surface clear color"
        );
        assert!(
            src.contains("pane_backdrop_rect(pv.rect, bw, pane_titlebar_h, cfg.title_at_bottom)"),
            "the backdrop must use the border/titlebar-aware geometry helper"
        );
    }

    /// Cycle 788 drift guard (audit B2/B3/B4). The overlay text-buffer pools
    /// are grown with `while len < N` exactly like the pane pools and must be
    /// truncated back down too, or each ratchets to its session high-water mark
    /// (peak menu rows / hint labels / tab count) holding idle shaped-glyph
    /// buffers. Pin all five truncate calls at the source level (a behavioral
    /// test would need a full GPU `Renderer`).
    #[test]
    fn render_frame_truncates_overlay_buffer_pools_on_shrink() {
        let src = include_str!("lib.rs");
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

    /// Cycle 827 drift guard (audit). `build_pane`'s per-cell style-run scratch
    /// must be POOLED on `self` (taken + returned) and reuse each run's `String`
    /// buffer by index (clear + refill), not `Vec::new()` + `to_string()` per
    /// frame — otherwise a busy colored pane mints dozens–hundreds of `String`
    /// allocations on the 60 fps hot path. A behavioral test needs a full GPU
    /// `Renderer`; pin the pattern at the source level.
    #[test]
    fn build_pane_pools_the_span_scratch() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("std::mem::take(&mut self.span_scratch)"),
            "span scratch must be taken from the self-pool, not allocated fresh"
        );
        assert!(
            src.contains("self.span_scratch = spans;"),
            "span scratch must be returned to the pool for the next frame"
        );
        // Cycle 853: the per-frame quad list is pooled the same way.
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

    /// Cycle 791 drift guard (audit C1). Image-placement draw must keep the
    /// `len > 1` fast-path so the common 0–1-image pane doesn't pay a per-frame
    /// `Vec` alloc + sort, AND must still z-sort the 2+ case so higher-z images
    /// land on top. A behavioral test needs a full GPU `Renderer`; pin both at
    /// the source level (same shape as the buffer-truncate guards above).
    #[test]
    fn image_placement_draw_keeps_len_fastpath_and_z_sort() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("if pv.images.len() > 1"),
            "image placement draw must fast-path the 0–1 case to skip the \
             per-frame Vec alloc + sort"
        );
        assert!(
            src.contains("ordered.sort_by_key(|p| p.z)"),
            "2+ image placements must still be z-sorted so higher z lands on top"
        );
    }

    /// Cycle 845 drift guard (audit). `render_frame_with_status` clones
    /// `self.font_family` every frame (to hold an owned handle while
    /// `&mut self.font_system` is borrowed across ~20 `Family::Name(&family)`
    /// reads). The field must stay `Arc<str>` so that clone is a refcount bump,
    /// not a per-frame heap alloc + memcpy at 60fps. A behavioral test needs a
    /// GPU `Renderer`; pin the field type at the source level.
    /// Cycle 852 drift guard. `PaneView` must *borrow* its per-frame
    /// images/title/group_name from the frame's `guards` collection (exactly as
    /// `term` borrows the `MutexGuard`), not own clones — otherwise `redraw()`
    /// double-clones every visible pane's image `Vec` + title `String` every
    /// frame. A behavioral test needs the full app frame loop; pin the borrowed
    /// field types at the source.
    #[test]
    fn paneview_borrows_per_frame_data() {
        let src = include_str!("lib.rs");
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
        let src = include_str!("lib.rs");
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

    #[test]
    fn truncate_honors_budgets_beyond_24_columns() {
        // Cycle 804: the tab-title budget used to be hard-capped at 24 chars
        // regardless of how wide the tab was. `truncate` itself never had that
        // cap, so these guard that a wide budget shows the full title and only
        // ellipsizes on genuine overflow — i.e. that a future re-clamp to 24
        // would be caught.
        let long = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
        let long_len = long.chars().count(); // all ASCII → 1 col each
        // A budget wider than the title returns it verbatim (no 24 cap).
        assert_eq!(truncate(long, 200), long);
        assert_eq!(truncate(long, long_len), long);
        // A 30-col title at budget 30 is unchanged (would fail under a 24 cap).
        let t30 = "abcdefghijklmnopqrstuvwxyz0123"; // 30 ASCII cols
        assert_eq!(t30.chars().count(), 30);
        assert_eq!(truncate(t30, 30), t30);
        // Still ellipsizes when the title actually exceeds the (large) budget.
        let t40 = "0123456789012345678901234567890123456789"; // 40 cols
        let cut = truncate(t40, 30);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), 30);
    }
}

#[cfg(test)]
mod update_banner_top_tests {
    use super::update_banner_top;

    /// Cycle 808 drift guard (audit). The passive update banner must stack
    /// above any BOTTOM-anchored tab / status bar so it neither paints over
    /// nor steals clicks from it. The renderer (draw) and the App (hit-test)
    /// share this pure helper, so they can't drift apart.
    #[test]
    fn stacks_above_bottom_chrome() {
        // No bottom chrome → flush at the surface bottom (1000 - 30).
        assert_eq!(update_banner_top(1000.0, 30.0, 0.0, 0.0), 970.0);
        // Bottom tab bar (28) → banner clears it.
        assert_eq!(update_banner_top(1000.0, 30.0, 28.0, 0.0), 942.0);
        // Bottom status bar (20) → banner clears it.
        assert_eq!(update_banner_top(1000.0, 30.0, 0.0, 20.0), 950.0);
        // Both at the bottom → banner clears the stack of both.
        assert_eq!(update_banner_top(1000.0, 30.0, 28.0, 20.0), 922.0);
    }
}

#[cfg(test)]
mod bg_tile_cap_tests {
    use super::bg_tiles_within_cap;

    /// Cycle 825 drift guard: a small source image must NOT tile into a
    /// per-frame quad storm — past the cap it falls back to a stretched quad.
    #[test]
    fn tiny_source_image_falls_back_to_stretch() {
        // A reasonable 64×64 tile on 4K (≈2176 tiles) still tiles.
        assert!(bg_tiles_within_cap(3840.0, 2160.0, 64.0, 64.0));
        // A 1×1 tile on 4K (~8.3M tiles) trips the cap.
        assert!(!bg_tiles_within_cap(3840.0, 2160.0, 1.0, 1.0));
        // A 16×16 tile on 4K (~32k tiles) also trips it.
        assert!(!bg_tiles_within_cap(3840.0, 2160.0, 16.0, 16.0));
        // Degenerate zero dims are treated as 1 px (no divide-by-zero) → cap.
        assert!(!bg_tiles_within_cap(3840.0, 2160.0, 0.0, 0.0));
        // A source as large as the surface is a single tile.
        assert!(bg_tiles_within_cap(1920.0, 1080.0, 1920.0, 1080.0));
    }
}

#[cfg(test)]
mod vi_selection_row_span_tests {
    use super::vi_selection_row_span;

    /// Cycle 820 drift guard: middle rows of a multi-row vi visual selection
    /// extend to the real last column, not a hardcoded 256.
    #[test]
    fn middle_rows_use_real_width_not_256() {
        let (start, end) = ((10, 5), (12, 8));
        let cols = 600; // wider than the old 256 bound
        // First row: from the anchor column to the real last column.
        assert_eq!(vi_selection_row_span(10, start, end, cols), Some((5, 599)));
        // Middle row: full width [0, cols-1] — was [0, 256] before the fix.
        assert_eq!(vi_selection_row_span(11, start, end, cols), Some((0, 599)));
        // Last row: from 0 to the cursor column.
        assert_eq!(vi_selection_row_span(12, start, end, cols), Some((0, 8)));
        // Single-row selection stays within its endpoints.
        assert_eq!(
            vi_selection_row_span(10, (10, 3), (10, 7), cols),
            Some((3, 7))
        );
        // Empty span → None (guards the draw against a zero/negative width).
        assert_eq!(vi_selection_row_span(10, (10, 9), (10, 3), cols), None);
    }
}

#[cfg(test)]
mod build_rich_spans_tests {
    use super::{Attrs, Family, Rgb, build_rich_spans, font_features};
    use kettle_config::Config;

    /// Cycle 806 drift guard (audit perf): the per-pane rich-text builder must
    /// **borrow** each span's text (`&str`) rather than clone it into an owned
    /// `String`. A clone allocates once per style run per pane per frame; this
    /// pins the zero-copy property via pointer identity (a clone would land at
    /// a different heap address) and confirms the `"\n"` row markers stay
    /// interleaved at the recorded break indices.
    #[test]
    fn build_rich_spans_borrows_text_and_interleaves_breaks() {
        let cfg = Config::default();
        let ff = font_features(&cfg);
        let default_attrs = Attrs::new()
            .family(Family::Name("TestFamily"))
            .font_features(ff.clone());
        let spans = vec![
            ("alpha".to_string(), Rgb::new(10, 20, 30), false, false),
            ("beta".to_string(), Rgb::new(40, 50, 60), true, true),
        ];
        // One line break recorded before span index 1 → "alpha","\n","beta".
        let line_breaks = vec![1usize];
        let rich = build_rich_spans(&spans, &line_breaks, &cfg, &ff, &default_attrs);

        let texts: Vec<&str> = rich.iter().map(|(s, _)| *s).collect();
        assert_eq!(texts, vec!["alpha", "\n", "beta"]);
        // Zero-copy: span entries point at the SAME heap buffer as the source
        // `String`s. If this ever fails, someone re-introduced a `text.clone()`.
        assert_eq!(rich[0].0.as_ptr(), spans[0].0.as_ptr());
        assert_eq!(rich[2].0.as_ptr(), spans[1].0.as_ptr());
    }

    /// A leading break (index 0) and no spans-after edge cases: the marker is
    /// emitted before the first span, and a run with zero breaks is verbatim.
    #[test]
    fn build_rich_spans_handles_leading_break_and_no_breaks() {
        let cfg = Config::default();
        let ff = font_features(&cfg);
        let default_attrs = Attrs::new()
            .family(Family::Name("TestFamily"))
            .font_features(ff.clone());
        let spans = vec![("only".to_string(), Rgb::new(1, 2, 3), false, false)];

        let lead = build_rich_spans(&spans, &[0usize], &cfg, &ff, &default_attrs);
        assert_eq!(
            lead.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec!["\n", "only"]
        );

        let none = build_rich_spans(&spans, &[], &cfg, &ff, &default_attrs);
        assert_eq!(
            none.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec!["only"]
        );
        assert_eq!(none[0].0.as_ptr(), spans[0].0.as_ptr());
    }
}

#[cfg(test)]
mod settings_panel_cols_tests {
    use super::settings_panel_cols;
    use unicode_width::UnicodeWidthStr;

    // Cycle 784: the settings panel must be wide enough for its two widest
    // lines — the footer hint and the in-capture chord prompt — both of which
    // exceed the old hardcoded 44 cols. Live sweep saw "Esc close" clipped to
    // "Esc clo" and the capture prompt overflowing onto the next row.
    #[test]
    fn settings_panel_fits_footer_and_capture_prompt() {
        let footer = "↑↓ field    ←→ change    Tab category    Esc close";
        // 26-col left-padded label + the capture-mode value (see app.rs).
        let capture = format!(
            "▸ {:<26}{}",
            "Split right", "‹press a chord — Esc to cancel›"
        );
        let cols = settings_panel_cols(&[
            footer.to_string(),
            capture.clone(),
            "  Font size".to_string(),
        ]);
        assert!(
            cols as usize >= footer.width(),
            "panel ({cols}) clips footer ({})",
            footer.width()
        );
        assert!(
            cols as usize >= capture.width(),
            "panel ({cols}) clips capture prompt ({})",
            capture.width()
        );
        // The footer alone already exceeds the old 44-col hardcode.
        assert!(footer.width() > 44, "regression-guard premise broke");
    }

    #[test]
    fn settings_panel_has_a_floor() {
        // A hypothetical sparse category never renders narrower than 44 cols.
        assert_eq!(settings_panel_cols(&["x".to_string()]) as usize, 44);
        assert_eq!(settings_panel_cols(&[]) as usize, 44);
    }
}
