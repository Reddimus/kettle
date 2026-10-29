//! GPU renderer for pane grids, chrome, text, images, and overlays.
//!
//! Draw order is part of the contract: backgrounds and chrome, inline images,
//! pane text, scrollbars, then menu chrome and text. Changing
//! it can break transparency, clipping, or overlay readability. See
//! `docs/ARCHITECTURE.md` for the full pipeline.
//!
//! [`capture_png`] renders an offscreen representative frame;
//! [`offscreen_selftest`] validates shader and pipeline creation without a
//! window surface.

mod background;
mod batching;
mod capture;
mod completion;
mod fonts;
mod geometry;
mod gpu;
mod labels;
mod overlays;
mod screenshot;
mod search;
mod settings;
mod terminal;
mod theme;

use background::{BgImageAnim, BgImageWorker, starfield_time};
#[cfg(test)]
use background::{BgImageJob, BgImageResult};
#[cfg(test)]
use batching::cell_locked_glyph_offset;
use batching::{
    SharedPipelines, apply_quad_alpha_floor, clip_quads, completion_text_damage_key,
    context_menu_text_damage_key, emit_cell_locked_glyphs, pane_outline,
    prepared_text_areas_damage_key, push_ime_preedit_quads, rect, text_bounds_for_rect,
    text_layout_damage_key, text_overlay_requires_continuous_prepare,
};
pub use capture::{
    DebugScene, capture_png, capture_png_with, capture_png_with_annotation, offscreen_selftest,
    offscreen_selftest_with_config,
};
#[cfg(test)]
use capture::{SCREENSHOT_DEMO_VERSION, screenshot_meter, screenshot_text_buffer};
use completion::{
    COMPLETION_BORDER, COMPLETION_PAD_COLUMNS, COMPLETION_SCROLL_W, completion_header_columns,
    completion_header_count, completion_header_label, completion_match_span,
    completion_panel_geometry, completion_scroll_thumb, push_completion_selection_quads,
};
#[cfg(test)]
use completion::{
    COMPLETION_MAX_COLUMNS, COMPLETION_MIN_COLUMNS, COMPLETION_RAIL_W, CompletionPanelPlacement,
    MAX_COMPLETION_ROWS,
};
pub use completion::{
    CompletionOverlay, CompletionOverlayRow, completion_overlay_rect, completion_overlay_row_rects,
};
pub use fonts::{PreparedFonts, StartupFonts, clamp_font_size, metrics_for, pane_metrics};
#[cfg(test)]
use fonts::{TEXT_SYMBOL_PROBE, resolve_text_symbol_family};
use fonts::{font_features, load_bundled_font, measure_cell, single_column_text_presentation};
#[cfg(test)]
use geometry::snapshot_viewport_top;
use geometry::{
    MACOS_WINDOW_CORNER_RADIUS_POINTS, OUTLINE_BOTTOM_LEFT, OUTLINE_BOTTOM_RIGHT,
    background_image_rect, cell_locked_pen_x, centered_text_origin, fair_placement_quotas,
    glyph_grid_col, ime_preedit_rect, inline_image_clip, inline_placement_rect,
    live_surface_dimensions, pane_backdrop_rect, pane_bottom_window_corner_mask,
    placement_is_visible, placement_viewport_row, rect_covers_surface, rects_overlap,
    row_thumb_floor, selection_row_span, visible_selection_rows,
};
pub use geometry::{Rect4, cap_axis_cells, pane_grid_origin};
pub use gpu::{
    AdapterEscalation, AdapterSelection, FrameOutcome, GpuAdapterInfo, GpuAdapterKey, GpuContext,
    GpuFault, RendererRecoveryState, detect_gpus, enumerate_adapter_infos, escalation_for_attempt,
    gpu_info, pin_match_rank_fields,
};
#[cfg(test)]
use gpu::{
    BackendPlatform, MAX_RECOVERY_ADAPTER_ATTEMPTS, adapter_priority_for,
    backend_attempt_order_for, backend_rank_for, can_probe_native_backend_directly,
    effective_backend, has_gpu_pin, recovery_candidate_indices, should_query_preferred_adapter,
    try_adapter_candidates,
};
use gpu::{latch_gpu_fault, live_device_limits, resolve_headless_adapter};
use labels::{
    display_width, fit_single_line_label, middle_ellipsis, overlay_label_cols, take_cols_back,
    take_cols_front,
};
pub use labels::{fit_pane_titlebar_title, fit_tab_segment_title};
#[cfg(test)]
use labels::{fit_tab_path, fit_tab_title};
#[cfg(test)]
use overlays::input_bar_colors;
pub use overlays::{
    ConfirmDialogButton, ConfirmDialogOverlay, ContextMenu, ContextMenuRow, HintLabel, ImePreedit,
    MediaPasteReceiptGeometry, MediaPasteReceiptHit, MediaPasteReceiptKind,
    MediaPasteReceiptOverlay, NotificationGeometry, Overlay, StatusBar, TabActivity, TabBar,
    TabSeg, TitleEditOverlay, UpdateToastGeometry, UpdateToastOverlay, format_media_size,
    media_paste_receipt_geometry, menu, menu_row_chars, notification_geometry, tab_drag,
    theme_picker_prefix, title_edit_prefix, update_toast_geometry,
};
use overlays::{
    MAX_LANE_IMAGES, NEW_TAB_MENU_GLYPH, NEW_TAB_PLUS_GLYPH, context_menu_panel_width,
    media_paste_receipt_text, menu_chrome_quads, push_input_bar, scroll_arrow_glyphs,
    toast_buffer_text,
};
#[cfg(test)]
use overlays::{
    context_menu_clip_indicators, media_paste_receipt_compact_title, remote_warning_columns,
};
#[cfg(test)]
use screenshot::{
    MAX_SCREENSHOT_PERSISTENCE_JOBS, ScreenshotPersistencePermit, ScreenshotPersistencePool,
    create_screenshot_file, crop_screenshot, persist_screenshot_file_if,
    persist_screenshot_file_with_steps, screenshot_publication_error_message,
    screenshot_target_bytes, wait_for_screenshot_submission_with,
};
pub use screenshot::{
    ScreenshotCancellation, ScreenshotOutputPolicy, ScreenshotRecoveryWake, ScreenshotRequest,
};
use screenshot::{ScreenshotWorker, unpremultiply_rgba8};
#[cfg(test)]
use screenshot::{persist_screenshot_file, persist_screenshot_file_with_flush};
use search::{
    SEARCH_FOCUSED_FILL_ALPHA, SEARCH_SEGMENTS, cluster_spans, cluster_stops, search_bar_segments,
    search_editor_body, segment_clusters, shape_search_segment, stop_x,
};
pub use search::{
    SearchBarGeometry, SearchCaseMode, SearchControl, SearchEditorCluster, SearchOverlay,
    SearchStatus, search_bar_columns, search_bar_geometry,
};
#[cfg(test)]
use search::{
    SearchBarSegment, SearchBarWidths, drop_cols_front, grapheme_clusters, merge_clusters,
    starts_right_to_left,
};
#[cfg(test)]
use search::{search_bar_text, search_editor_label, search_focused_label};
use settings::{
    SETTINGS_FIELD_START, settings_display_lines, settings_fit_display_line,
    settings_painted_lines, settings_panel_cols, settings_panel_layout, settings_tab_span,
};
pub use settings::{
    SettingsHit, SettingsOverlay, SettingsPanelGeometry, SettingsRow, settings_hit_test,
    settings_panel_geometry,
};
#[cfg(test)]
use settings::{settings_fit_line, settings_row_line};
pub use terminal::{HighlightRect, LinkRect, PaintedCard, PaneView};
use terminal::{
    PendingCursorGlyph, cursor_glyph_damage_key, cursor_glyph_top, invalidate_pane_titlebar_texts,
};
#[cfg(test)]
use terminal::{
    ShapedRow, cursor_focus_gate, ime_cursor_shape, push_cell_decorations, push_underline_quads,
    run_attrs, search_highlight_at,
};
use theme::{
    CellHighlight, CompletionPalette, MinimumContrastCache, background_has_wallpaper,
    completion_palette_with_colors, composed_bg_alpha, confirm_dialog_border_color,
    desired_alpha_mode, dim_blend, final_scene_is_uniformly_opaque, gc, inactive_pane_amount,
    live_underlay_clear_color, needs_postmultiplied_presentation, pane_inactive_amount,
    pick_titlebar_bg, resolve_chrome_bg, resolved_cell_foreground_cached, srgb, srgb_encode,
    surface_clear_color, use_live_pane_bases,
};
#[cfg(test)]
use theme::{attributed_foreground, completion_palette, resolved_cell_foreground};
pub use theme::{bell_flash_alpha, confirm_dialog_text_color, window_requires_alpha_surface};
#[cfg(test)]
use theme::{completion_selection_surface, solid_blend};

mod bg_image;

mod card_marks;

mod card_scene;

mod inline_cards;

pub use card_scene::duration_label;

pub use inline_cards::{
    CardHarness, CardPlacement, CardPoster, CardRefusal, CardSpec, InlineCards, MAX_CARD_COLUMNS,
    MAX_PANE_CARDS, cli_card_text,
};

mod chrome;

mod color;

mod confirm;

pub use confirm::ConfirmDialogLayout;

mod cursor_patch;

mod cursor_policy;

mod glyphpipe;

#[cfg(test)]
mod headless_tests;

mod imgpipe;

mod media_lane;

mod media_transport;

mod media_viewport;

mod outline;

mod present;

mod quad;

mod rounded_surface;

mod snapshot;

mod starfield;

mod upload;

pub use cursor_patch::{CursorPatchFailure, CursorPatchIneligible, CursorPatchOutcome, PatchRect};

pub use upload::{RenderUploads, UploadCounts};

pub use bg_image::{
    BgFrame, BgImage, bg_current_frame, decode_bg_image, decode_bg_image_frames,
    decode_bg_image_frames_with_blur, decode_bg_image_with_blur,
};

pub use snapshot::{PaneSnapshot, SnapCell};

use std::sync::Arc;

use alacritty_terminal::term::cell::Flags;

use anyhow::{Result, anyhow};

use glyphon::cosmic_text::{
    Align, AttrsList, AttrsOwned, BufferLine, FeatureTag, FontFeatures, LineEnding, Wrap, fontdb,
};

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution,
    Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};

// `kettle_config::TextRenderer` (the grid|legacy mode enum) is aliased so it
// doesn't collide with glyphon's `TextRenderer` (the renderer) imported above.
use kettle_config::{Config, Rgb, ScrollbarMode, TextRenderer as TextRendererMode};

use kettle_i18n::{Language, Text, Translator};

use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle};

pub use color::{
    dim as dim_color, lightness, perceptual_wash_alpha, relative_luminance, reply_for_query,
    reply_for_text_area_size, resolve, resolve_query,
};

use glyphpipe::{GlyphClip, GlyphInstance, GlyphPipeline, RasterGlyph};

use outline::{OutlineInstance, OutlinePipeline};

use quad::{QuadInstance, QuadPipeline};

/// The production source of the renderer modules, excluding test-only items.
#[cfg(test)]
fn production_source() -> String {
    let production = [
        kettle_test_support::production_source(include_str!("lib.rs")),
        kettle_test_support::production_source(include_str!("terminal.rs")),
        kettle_test_support::production_source(include_str!("fonts.rs")),
        kettle_test_support::production_source(include_str!("overlays.rs")),
        kettle_test_support::production_source(include_str!("batching.rs")),
        kettle_test_support::production_source(include_str!("theme.rs")),
        kettle_test_support::production_source(include_str!("search.rs")),
        kettle_test_support::production_source(include_str!("completion.rs")),
        kettle_test_support::production_source(include_str!("settings.rs")),
        kettle_test_support::production_source(include_str!("geometry.rs")),
        kettle_test_support::production_source(include_str!("gpu.rs")),
        kettle_test_support::production_source(include_str!("background.rs")),
        kettle_test_support::production_source(include_str!("screenshot.rs")),
        kettle_test_support::production_source(include_str!("labels.rs")),
        kettle_test_support::production_source(include_str!("capture.rs")),
    ]
    .join("\n");
    assert!(
        !production.contains("fn production_source()"),
        "the production slice retained its own helper"
    );
    assert!(
        !production.contains("#[test]"),
        "the production slice retained a test function"
    );
    assert!(
        !production.contains("#[cfg(test)]"),
        "the production slice retained a test-only item"
    );
    production
}

#[cfg(test)]
mod font_reload_tests;

#[cfg(test)]
mod dim_blend_tests;

#[cfg(test)]
mod context_menu_row_width_tests;

pub use media_lane::{
    MediaCanvas, MediaLaneEdge, MediaLaneFrame, MediaLaneGeometry, MediaLaneHit, MediaLaneMode,
    MediaLanePanel, MediaLanePlay, MediaLaneScrubber, MediaLaneSender, MediaLaneSound,
    MediaLaneSource, MediaLaneTile, media_lane_geometry, media_lane_geometry_scaled,
};

pub use media_viewport::{MAX_MEDIA_ZOOM, MEDIA_ZOOM_STEP, MIN_MEDIA_ZOOM, MediaViewport};

pub struct Renderer {
    /// `None` only for the headless renderer tests build: it draws into the
    /// offscreen capture target alone.
    surface: Option<wgpu::Surface<'static>>,
    gpu: GpuContext,
    /// Shared CPU/GPU retained-resource scope for this renderer window.
    graphics_budget: kettle_core::GraphicsBudget,
    config: wgpu::SurfaceConfiguration,
    supported_alpha_modes: Vec<wgpu::CompositeAlphaMode>,

    font_system: FontSystem,
    /// The face to request for a codepoint Unicode renders as text by default,
    /// when this system has one. `None` leaves such cells on the platform
    /// fallback cascade.
    text_symbol_family: Option<&'static str>,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
    /// Cell-locked pane-text renderer (the default `text-renderer=grid`
    /// path). Pins every glyph to its grid cell so fallback/ligature/CJK glyphs
    /// can't drift off the `col*cell_w` grid that selection / cursor / hit-testing
    /// use. glyphon's `text_renderer` above still draws chrome / titlebars /
    /// menus / the cursor glyph (none of which are grid-locked).
    glyph_pipeline: GlyphPipeline,
    /// Pooled per-frame scratch for the glyph instances emitted into
    /// `glyph_pipeline` (high-water-mark reuse like `quad_scratch`).
    glyph_instances: Vec<GlyphInstance>,
    /// Pooled per-pane scissor ranges paired with `glyph_instances`, so each
    /// pane's text is clipped to its own rect at draw time (persists across idle
    /// frames alongside the instance buffer).
    glyph_clips: Vec<GlyphClip>,
    /// Pooled per-run scratch: byte offset of each cell's base scalar.
    /// Attached zero-width marks share their base cell's column. Reused
    /// across runs/frames to keep the glyph-emit walk allocation-free.
    glyph_char_starts: Vec<u32>,
    /// Cell-locked glyph instance buffer needs a forced upload after the GPU
    /// pipeline is cleared, even if pane text/layout hashes did not change.
    /// Without this, a font/scale/cache invalidation can drop the live draw
    /// count to zero and cursor-only frames keep presenting blank pane text.
    grid_glyphs_dirty: bool,
    /// Text-area / grid-glyph layout damage key. Cursor blink must not
    /// be part of this key: a blink changes cursor quads / cursor glyph only,
    /// never pane text glyph instances. Geometry, cell metrics, renderer mode,
    /// font shaping inputs, and pane viewport dimensions do belong here because
    /// cached glyph vertices/text areas become stale even when row contents did
    /// not reshape.
    last_text_layout_key: Option<u64>,
    /// Bundled Regular is loaded eagerly; styled faces are loaded on first
    /// bold/italic terminal content so first-window startup pays for one face,
    /// not the full family.
    bundled_style_faces_loaded: bool,
    /// Pane id currently occupying each per-pane buffer/cache slot.
    pane_buffer_ids: Vec<Option<u64>>,
    pane_buffers: Vec<TextBuffer>,
    /// Pooled scratch for `build_pane`'s per-cell style runs,
    /// reused across frames. Both the Vec backing store AND each run's `String`
    /// buffer are recycled (the builder writes into slots by index rather than
    /// pushing fresh `String`s), so a busy colored pane no longer mints dozens–
    /// hundreds of `String` allocations on the 60 fps render hot path. Same
    /// high-water-mark pooling as `pane_buffers`.
    span_scratch: Vec<(String, Rgb, bool, bool)>,
    /// Pooled scratch for `build_pane`'s line-break indices.
    span_breaks_scratch: Vec<usize>,
    /// Per-frame cache for WCAG foreground lifts. Visible grids repeat a small
    /// palette across thousands of cells, while the lift itself performs a
    /// bisection with many nonlinear color conversions.
    minimum_contrast_cache: MinimumContrastCache,
    chrome_colors: chrome::Colors,
    /// Per-pane, per-row content keys for the line-level shaping cache.
    /// `build_pane` hashes each grid row's style runs (text, fg, bold, italic).
    /// A row whose key matches last frame is skipped entirely, so its
    /// `BufferLine` keeps its shaped and laid-out caches and an idle blink
    /// repaint does not re-shape visible text. Grown and truncated in lockstep
    /// with `pane_buffers`, because the keys describe what is in the buffer at
    /// that index.
    pane_line_keys: Vec<Vec<u64>>,
    /// Per-pane key over the inputs that change how a row SHAPES
    /// without changing its run tuples — font-family variants, ligature
    /// toggle, font-features, shaping mode. On mismatch the pane's row keys
    /// are wiped so every row re-sets via `reset_new` (the only path that
    /// updates a `BufferLine`'s internal shaping mode).
    pane_style_keys: Vec<u64>,
    /// Pooled scratch for assembling one row's text.
    line_text_scratch: String,
    /// Chrome-label caches (titlebar / tab / status / glyph
    /// buttons) gate their `Buffer::set_text` (which re-shapes
    /// unconditionally) on text equality. Text-only keys are sound while the
    /// font family is stable; this key invalidates them all when it changes.
    chrome_style_key: u64,
    /// Hash of the chrome label text shaped last frame
    /// (titlebars, tab labels, status, resize chip). When it is unchanged AND
    /// no pane row reshaped AND no overlay is open, the whole-viewport glyphon
    /// `prepare` (which re-encodes EVERY visible glyph's vertices) is skipped
    /// and the cached vertex buffers are re-rendered as-is.
    last_chrome_hash: u64,
    /// Set before a fallible shared-atlas text prepare and cleared only after
    /// every text renderer succeeds. Buffer/damage caches are updated while
    /// the frame is assembled, so this latch forces a retry after an error
    /// instead of accepting partially retained vertices as current.
    text_prepare_dirty: bool,
    /// Whether any text overlay (settings, palette, search, menu, ...) was open
    /// the previous frame. The `need_prepare` damage gate forces a glyphon
    /// prepare while an overlay is open. On the frame an overlay closes, the
    /// gate would otherwise see no overlay and no change, skip the prepare, and
    /// keep drawing the closed panel's cached text until the next keystroke.
    /// This flag lets the close transition force one clearing prepare.
    last_overlay_open: bool,
    /// Dedicated renderer + 1-line buffer for the focused
    /// solid-block cursor's foreground glyph, drawn in its own pass on top of
    /// the cursor block quad. Decoupling it from the pane text buffer is what
    /// lets a blinking BLOCK cursor (the default) skip the whole-viewport
    /// `prepare` between content changes — the block toggles a quad + a single
    /// glyph, not a buffer reshape. Shares `atlas`/`viewport` like
    /// `menu_text_renderer`.
    cursor_glyph_renderer: TextRenderer,
    cursor_glyph_buffer: TextBuffer,
    /// Set during the focused pane's `build_pane` when a solid block cursor is
    /// visible; consumed (and reset) each frame in `render_frame_with_status`.
    pending_cursor_glyph: Option<PendingCursorGlyph>,
    /// Vertex/layout key last prepared into `cursor_glyph_renderer`. Stable
    /// cursor frames (including menu hover) can render its retained vertices;
    /// any main text prepare still forces a refresh in case the shared atlas
    /// repacked.
    last_cursor_glyph_key: Option<u64>,
    /// Last text shaped into each `pane_titlebar_buffers` slot.
    pane_titlebar_texts: Vec<String>,
    /// Last text shaped into each `tab_buffers` slot.
    tab_texts: Vec<String>,
    /// Last text shaped into each `hint_buffers` slot. Quick-select labels are
    /// byte-stable while the overlay is open, so this gate skips re-shaping up
    /// to ~100 of them on every blink/keystroke redraw (cheap under no-fallback
    /// shaping, real work under Advanced).
    hint_texts: Vec<String>,
    /// Last text shaped into `tab_close_buffer` / `tabbar_buffer`
    /// / `new_tab_arrow_buffer` / `status_bar_buffer`. The first three are
    /// constant glyphs, so after frame 1 these gates always hold.
    tab_close_text: String,
    tabbar_text: String,
    new_tab_arrow_text: String,
    /// Last text shaped into the overflow scroll-arrow buffers; it changes
    /// only when the bar moves between a side and the top or bottom.
    scroll_left_text: String,
    scroll_right_text: String,
    status_bar_text: String,
    /// Ghostty parity: the transient resize chip's text buffer and its
    /// equality gate (re-shaped only when the grid size changes).
    resize_overlay_buffer: TextBuffer,
    resize_overlay_text: String,
    update_toast_buffers: [TextBuffer; 3],
    update_toast_text: [String; 3],
    /// Input-method preedit buffer and equality gate.
    ime_buffer: TextBuffer,
    ime_quads: QuadPipeline,
    ime_text_renderer: TextRenderer,
    ime_prepared: bool,
    ime_text: String,
    /// Pooled scratch for the per-frame cell/UI quad list, so
    /// `render_frame_with_status` does not allocate a fresh `Vec` of
    /// `panes*16+256` `QuadInstance`s every frame. Taken and cleared at the
    /// top of the frame, returned after the GPU upload; same high-water pooling
    /// as `span_scratch`.
    quad_scratch: Vec<QuadInstance>,
    /// Where the focused pane's blinking cursor sits in the uploaded `quads`,
    /// so the off phase can skip it at draw time. The cursor is always built
    /// and uploaded; a blink edge changes only this draw. `None` when no
    /// cursor can blink (none drawn, or vi mode, which never blinks).
    cursor_quad_range: Option<std::ops::Range<u32>>,
    /// Text prepares run so far, one per glyphon prepare call (the pane, menu
    /// and cursor-glyph renderers). Steady frames, blink edges included,
    /// should add none.
    text_prepares: u64,
    chrome_prepares: u64,
    /// Frames presented to the window so far.
    frames_presented: u64,
    /// What the frame on screen drew, so `present_cursor_patch` can encode
    /// the same scene again. `None` from the start of every frame until it
    /// is presented. Headless tests record the capture in place of a present.
    last_scene: Option<cursor_patch::SceneFacts>,
    /// The cursor patch's combine pipeline and on/off targets, built at the
    /// first hand-off and freed on detach.
    cursor_patch: Option<cursor_patch::PatchPipeline>,
    /// The Core Animation layer the UI placed over the cursor.
    #[cfg(target_os = "macos")]
    cursor_layer: Option<cursor_patch::CursorLayerSurface>,
    /// Terminator parity, per-pane-titlebar: one TextBuffer per pane
    /// for the title text drawn in the titlebar quad (see
    /// `pick_titlebar_bg`). Reused across redraws to amortize
    /// allocation; trimmed/grown alongside pane_buffers.
    pane_titlebar_buffers: Vec<TextBuffer>,
    tab_buffers: Vec<TextBuffer>,
    hint_buffers: Vec<TextBuffer>,
    /// One text buffer per row of the right-click context menu. Reused
    /// across openings to amortize allocation; trimmed when the row
    /// count shrinks for a smaller menu.
    context_menu_buffers: Vec<TextBuffer>,
    /// Last shaped text per menu row; unchanged rows skip reshaping.
    context_menu_texts: Vec<String>,
    /// Dropdown-parity: one buffer per row's right-aligned shortcut
    /// hint (empty-hint rows shape nothing). Pooled like its sibling.
    context_menu_hint_buffers: Vec<TextBuffer>,
    /// Last text shaped into each `context_menu_hint_buffers` slot.
    context_menu_hint_texts: Vec<String>,
    /// One text buffer per display line of the settings overlay
    /// (title, category tabs, field rows, footer). Grown + truncated like the
    /// context-menu pool.
    settings_buffers: Vec<TextBuffer>,
    /// Last text shaped into each `settings_buffers` slot. Moving the focused
    /// row changes only 2 of N lines (the old/new `▸` mark). The whole-overlay
    /// `settings_lines_cache` gate below still recomputes `lines` on any
    /// overlay change, but this gate skips the reshape for every row whose text
    /// is unaffected.
    settings_texts: Vec<String>,
    /// Source overlay for `settings_lines_cache`; a mismatch invalidates it.
    settings_lines_source: Option<SettingsOverlay>,
    settings_lines_cache: Vec<String>,
    /// Label and secondary-description buffers for the focused pane's
    /// completion shelf. Separate pools keep the command token crisp while the
    /// explanation stays visually subordinate.
    completion_buffers: Vec<TextBuffer>,
    completion_texts: Vec<String>,
    /// Emphasis span shaped into each label buffer, and whether that buffer was
    /// shaped for the selected surface. Both change the glyph colors without
    /// changing the label text, so the reshape gate has to see them.
    completion_spans: Vec<Option<(usize, usize)>>,
    completion_selected: Vec<bool>,
    /// Explicit rich-text emphasis color shaped into each label. Unlike the
    /// surrounding label color, this does not come from `TextArea`, so a live
    /// theme or accent change must invalidate the row even when its text and
    /// byte span are unchanged.
    completion_emphasis_colors: Vec<Rgb>,
    completion_description_buffers: Vec<TextBuffer>,
    completion_description_texts: Vec<String>,
    /// Card header: `Completions · fish` on the left, the match count on the
    /// right. Part of the list container, never a candidate row.
    completion_header_buffer: TextBuffer,
    completion_header_text: String,
    completion_count_buffer: TextBuffer,
    completion_count_text: String,
    media_receipt_title_buffer: TextBuffer,
    media_receipt_title_text: String,
    media_receipt_detail_buffer: TextBuffer,
    media_receipt_detail_text: String,
    media_receipt_dismiss_buffer: TextBuffer,
    media_receipt_badge_buffer: TextBuffer,
    tabbar_buffer: TextBuffer,
    /// The `▾` new-tab dropdown-arrow glyph, in its own buffer
    /// (drawn left of `+`) so it lands precisely in `new_tab_menu` and the `+`
    /// stays put in `new_tab`. Unused when the dropdown is disabled.
    new_tab_arrow_buffer: TextBuffer,
    /// The tab bar's overflow scroll-arrow glyphs (`‹ ›` across a top or
    /// bottom bar, `⌃ ⌄` down a side one), each in its own buffer, reshaped
    /// only when the glyph changes. Drawn only while the bar overflows.
    scroll_left_buffer: TextBuffer,
    scroll_right_buffer: TextBuffer,
    /// Single shared `✕` glyph buffer reused for every tab's close
    /// button. Rendered separately from the title text so we can:
    /// 1. Color it independently (dim at rest, bright red on hover).
    /// 2. Position it precisely inside `seg.close` rather than letting
    ///    the title's last character drift across segment widths.
    ///
    /// One buffer, N positions via per-tab `TextArea` instances.
    tab_close_buffer: TextBuffer,
    search_buffer: TextBuffer,
    /// Last text shaped into `search_buffer`. Shared across the search /
    /// command-palette / layout-picker / ssh-launcher / edit-title /
    /// confirm-dialog / update-banner bars. Only one of those `else if`
    /// branches paints per frame, so a single cache is enough. When a
    /// different bar takes over, it re-shapes once, the same cost a fresh
    /// buffer pays on its first frame.
    search_buffer_text: String,
    /// One buffer per piece of search-lane text (see `search_bar_segments`),
    /// each drawn in its own slot, and the text last shaped into it.
    search_segment_buffers: Vec<TextBuffer>,
    search_segment_texts: Vec<String>,
    /// Glyph boundaries of the query painted last frame, as (query byte,
    /// window x). Empty while the search lane is closed.
    search_editor_stops: Vec<(usize, f32)>,
    /// The clusters of the query painted last frame, in query bytes and
    /// window x. Empty while the search lane is closed.
    search_editor_clusters: Vec<SearchEditorCluster>,
    /// Whether the query painted last frame reads right to left.
    search_editor_rtl: bool,
    /// Status-bar text. Single line, reused every frame
    /// via `set_text` — same one-buffer pattern `tabbar_buffer` uses
    /// for tab labels. Stays at length 0 when the status bar is off.
    status_bar_buffer: TextBuffer,

    pane_bases: QuadPipeline,
    /// Live-window copy of `pane_bases` with the compositor fallback opacity
    /// applied. Screenshots keep using `pane_bases`, so an unsupported native
    /// blur backend cannot silently change the alpha the user configured in a
    /// captured PNG.
    live_pane_bases: QuadPipeline,
    quads: QuadPipeline,
    /// Rounded pane outlines used only where a decorated native window clips
    /// an outer pane corner. Kept out of `QuadInstance`: cell backgrounds
    /// dominate that buffer, and paying a larger instance for every cell to
    /// fix at most two window corners would be the wrong hot-path trade-off.
    pane_outlines: OutlinePipeline,
    /// Pane backgrounds, images and cards dim before contrast-resolved text.
    pane_dimming: QuadPipeline,
    text_decorations: QuadPipeline,
    /// Preview-lane dimming and scrollbars draw after lane content.
    overlay_quads: QuadPipeline,
    /// Third quad pass drawn after the overlay quads, for overlay chrome
    /// (context menu, settings panel, confirm bar, resize chip, and similar).
    /// It draws between `overlay_quads` and `menu_text_renderer`, so the panels
    /// sit above pane dimming and the main text while their own labels land
    /// *on top of* the panel bg.
    menu_quads: QuadPipeline,
    /// Dedicated TextRenderer for the text drawn over `menu_quads`
    /// (context-menu rows, settings lines, the resize chip, ...). Shares
    /// `atlas` + `viewport` with `text_renderer` (glyphon allows multiple
    /// renderers against one atlas); rendered after `menu_quads` so menu labels
    /// sit above the panel bg.
    menu_text_renderer: TextRenderer,
    rounded_surfaces: rounded_surface::RoundedSurfacePipeline,
    transport_surface_count: u32,
    confirmation_text_renderer: TextRenderer,
    confirmation_text: confirm::ConfirmText,
    confirmation_labels_prepared: bool,
    imgs: imgpipe::ImagePipeline,
    card_base: QuadPipeline,
    card_cursors: QuadPipeline,
    card_cursor_quad_range: Option<std::ops::Range<u32>>,
    card_decoration: QuadPipeline,
    card_posters: Option<imgpipe::ImagePipeline>,
    /// The preview lanes: their panels' quads, their images (charged to the
    /// preview account like card posters, made on first use) and their text,
    /// drawn above the terminal and below every overlay.
    lane_quads: QuadPipeline,
    lane_img: Option<imgpipe::ImagePipeline>,
    lane_text_renderer: TextRenderer,
    /// Each lane's text, by pane, kept while its lane shows.
    lane_texts: std::collections::HashMap<u64, media_lane::LaneText>,
    /// Whether the lane text renderer last prepared any text, so closing the
    /// last lane prepares it once more, empty.
    lane_labels_prepared: bool,
    card_image_shared: imgpipe::ImageShared,
    card_frames: Vec<inline_cards::CardFrame>,
    card_scene: card_scene::CardScene,
    /// The cards the last frame drew, presented or not: the next frame
    /// draws them again, so their shelf items' pixels must stay.
    drawn_cards: Vec<PaintedCard>,
    /// The cards on screen: those of the last frame presented. A press there
    /// is the card's, so a frame that never reached the screen leaves them.
    painted_cards: Vec<PaintedCard>,
    /// For each of `painted_cards`, whether that frame showed its image.
    painted_shown: Vec<bool>,
    card_text_renderer: TextRenderer,
    card_label_buffers: Vec<TextBuffer>,
    card_label_texts: Vec<String>,
    card_labels_prepared: bool,
    /// Single-instance overlay pipeline drawn between menu chrome and menu
    /// text, so the receipt thumbnail cannot cover its own status labels.
    media_receipt_img: imgpipe::ImagePipeline,
    /// Dedicated pipeline for the **background image (wallpaper)**, drawn at
    /// the very back, between the surface clear and the cell/chrome `quads`
    /// pass. Cell backgrounds (selection, syntax, TUI panels), chrome (tab bar /
    /// status bar / per-pane titlebars), and pane borders therefore composite
    /// OPAQUELY on top of the wallpaper (the standard kitty / wezterm /
    /// alacritty layering). Inline kitty / sixel images stay in `imgs`, drawn
    /// *after* the quads so they sit over cell backgrounds.
    bg_imgs: imgpipe::ImagePipeline,
    /// Procedural starfield wallpaper (`background-type = starfield`),
    /// drawn in the same back-most slot as `bg_imgs`. Stateless on the GPU side
    /// (just a per-frame uniform); the only state is `starfield_started`.
    starfield: starfield::StarfieldPipeline,
    presentation: present::PresentationPipeline,
    /// Playback clock for the starfield drift — `elapsed()` feeds the shader's
    /// continuous `time` so motion is smooth-valued even though we repaint at a
    /// low fps cap.
    starfield_started: std::time::Instant,
    /// Terminator parity, bg-image: decoded background-image cache, keyed on
    /// `(path, blur_radius)` so a `background-blur` toggle re-decodes even when
    /// the image path is unchanged.
    // The value is bounded to 64 MiB per frame and 128 MiB per animation. It
    // is freed (`= None`) when config moves away from `background-type = image`
    // so a large wallpaper doesn't stay resident after the user turns it off.
    //
    // A failed decode is cached as `frames.is_empty()`. Caching the failed key
    // (a) stops rendering the previous wallpaper after the path changes to a
    // broken one, and (b) stops re-attempting the failing decode every frame.
    // Holds all frames of an animated background (GIF/APNG/WebP), one for a
    // still image, plus per-frame gaps and the playback clock origin, so the
    // render loop swaps in the already-decoded frame per `bg_current_frame`.
    bg_image_cache: Option<BgImageAnim>,
    /// When the current bg-image (path, blur) FAILED to
    /// decode, the earliest `Instant` to retry — throttling self-heal to ≥3s so
    /// a broken/corrupt path isn't re-decoded every frame. `None` once a decode
    /// succeeds (the loaded wallpaper never re-decodes) or while no bg image is
    /// configured.
    bg_image_retry_at: Option<std::time::Instant>,
    /// Lazy, single-consumer decode worker (same shape as
    /// `screenshot_worker`): offloads `bg_image::decode_bg_image_frames_with_blur`
    /// off the render thread so an animated/blurred wallpaper's decode+blur
    /// (tens of ms per frame, up to 128 frames) never stalls a frame the
    /// winit event loop and every window's render pass depend on.
    bg_image_worker: Option<BgImageWorker>,
    bg_image_redraw: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The `(path, blur_radius)` key of the job currently in flight on
    /// `bg_image_worker`, if any. Lets `apply_bg_image_worker_result` discard
    /// a stale result (config changed again before the first decode
    /// finished) instead of overwriting a newer request's outcome, and lets
    /// `request_bg_image_reload` avoid resubmitting the same job every frame
    /// while it's still decoding.
    bg_image_pending: Option<(String, u32)>,

    /// `Arc<str>` so `render_frame_with_status`'s per-frame
    /// `self.font_family.clone()` (needed to satisfy the borrow checker while
    /// `&mut self.font_system` is held alongside ~20 `Family::Name(&family)`
    /// reads) is a refcount bump, not a heap alloc + memcpy at 60fps. `Arc<str>`
    /// derefs to `str`, so every `Family::Name(&family)` site is unchanged.
    font_family: Arc<str>,
    font_size: f32,
    metrics: Metrics,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Terminator parity (`cell_width` / `cell_height`):
    /// multiplicative scale applied to the measured cell metrics.
    /// `(1.0, 1.0)` is the default — measured dimensions unchanged.
    /// `(1.0, 1.5)` would space lines 50% taller; useful for users
    /// with strong vision needs or fonts whose default leading is
    /// too tight. Range clamped to `[0.5, 3.0]` at the config-parse
    /// layer (kettle-config/src/lib.rs:cell-width/cell-height arms).
    pub cell_scale_w: f32,
    pub cell_scale_h: f32,
    pub scale: f32,
    /// Multi-window (Peacock): the per-window accent the App resolved
    /// (theme pool slot + live dedupe across windows and processes). `None`
    /// falls back to the static `cfg.resolved_accent(theme)` — pinned hex or
    /// the theme signature. The offscreen `--screenshot` renderer never sets
    /// one, so hero renders stay cfg-governed.
    accent_override: Option<Rgb>,
    /// Whether the current OS window has rounded content corners. The UI owns
    /// fullscreen/decorations state and refreshes this before every frame.
    rounded_window_corners: bool,
    /// Live-only base alpha supplied when the OS cannot provide requested
    /// behind-window blur. The scene is rendered over this near-opaque clear;
    /// offscreen screenshots deliberately keep their configured alpha.
    live_background_opacity_floor: Option<f32>,
    /// Pending live-scene screenshot request; completion runs through the bounded
    /// capture worker.
    /// When `Some`, the next `render_frame` call renders the prepared scene to
    /// a bounded offscreen texture, copies it into a staging buffer, and
    /// dispatches a PNG encode off-thread. `App::dispatch` for
    /// `Action::TakeScreenshot` sets this via `set_pending_screenshot()` after
    /// computing the path with `session_screenshot_path`.
    pub pending_screenshot: Option<ScreenshotRequest>,
    /// Lazy, single-consumer readback worker. Screenshot capture must never
    /// block the winit event-loop thread: Wayland can disconnect clients that
    /// stop dispatching while output globals are changing. The worker owns the
    /// finite GPU wait, mapping, PNG encoding, and file write.
    screenshot_worker: Option<ScreenshotWorker>,
}

#[cfg(test)]
mod live_screenshot_tests;

/// The async background-image decode worker. `render_frame` must never block
/// the render thread on `decode_bg_image_frames_with_blur` (see
/// `Renderer::request_bg_image_reload` / `Renderer::apply_bg_image_worker_result`).
/// Pure CPU + `std::sync::mpsc`, so unlike `ScreenshotWorker`, which needs a
/// real `wgpu::Device`, this is fully unit-testable without a GPU.
#[cfg(test)]
mod bg_image_worker_tests;

#[cfg(test)]
mod starfield_time_tests;

impl Renderer {
    pub fn render_frame(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
    ) -> Result<FrameOutcome> {
        self.render_frame_with_status(panes, tabbar, cfg, overlay, &StatusBar::hidden())
    }

    /// Extended `render_frame` variant that also draws the status-bar strip.
    /// The bare `render_frame` shim passes a hidden status bar.
    pub fn render_frame_with_status(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
        status: &StatusBar,
    ) -> Result<FrameOutcome> {
        self.render_frame_with_status_and_pre_present(panes, tabbar, cfg, overlay, status, || {})
    }

    /// Live-window renderer variant. `pre_present` is invoked after drawing
    /// and queue submission, immediately before the surface is presented. The
    /// winit caller uses this seam for `Window::pre_present_notify`; offscreen
    /// callers keep using the no-op wrappers above.
    pub fn render_frame_with_status_and_pre_present<F>(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
        status: &StatusBar,
        pre_present: F,
    ) -> Result<FrameOutcome>
    where
        F: FnOnce(),
    {
        // Uploads below replace what the GPU holds before this frame reaches
        // the screen, so no hand-off may re-encode the previous one.
        self.last_scene = None;
        self.set_background_compositing(cfg);
        let theme = &cfg.theme;
        // OSC 11 (set default background) override from the focused pane.
        // The engine stores it in `Colors[257]`; the renderer needs it for
        // the surface clear-color (chrome regions: window padding, gaps
        // between panes, tab-bar background) so a program-driven bg flip
        // reaches the *whole* window rather than just cells with explicit
        // `Named(Background)`. Same precedence the OSC 11 query path
        // returns — override wins, theme is fallback.
        let default_bg = panes
            .iter()
            .find(|p| p.focused)
            .and_then(|p| p.snap.colors[257])
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

        // Terminator parity, per-pane-titlebar: hoisted alongside
        // buffer allocation so the titlebar text-setting block below
        // can reference it. Same condition as the titlebar quad
        // render (see `pick_titlebar_bg`; cfg.show_titlebar && >1 pane).
        let pane_titlebar_h: f32 = if cfg.show_titlebar && panes.len() > 1 {
            ch + 6.0
        } else {
            0.0
        };
        // The chrome-label caches below compare TEXT only, which is sound while
        // the font family is stable, so invalidate them all once when it
        // changes (config reload with a new `font-family`).
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            family.hash(&mut h);
            let k = h.finish();
            if self.chrome_style_key != k {
                self.chrome_style_key = k;
                self.card_label_texts.clear();
                invalidate_pane_titlebar_texts(&mut self.pane_titlebar_texts);
                self.tab_texts.clear();
                self.tab_close_text.clear();
                self.tabbar_text.clear();
                self.new_tab_arrow_text.clear();
                self.scroll_left_text.clear();
                self.scroll_right_text.clear();
                self.status_bar_text.clear();
                self.resize_overlay_text.clear();
                self.update_toast_text.iter_mut().for_each(String::clear);
                // The overlay text caches are touched only while their overlay
                // is open, so a font-family reload that lands while one is
                // closed, or that keeps the same labels, would leave glyphs
                // shaped in the old family.
                // Quick-select hint buffers stay allocated; an empty label
                // forces each one to reshape on its next fill.
                for label in &mut self.hint_texts {
                    label.clear();
                }
                self.context_menu_texts.clear();
                self.context_menu_hint_texts.clear();
                self.settings_texts.clear();
                self.settings_lines_source = None;
                self.completion_texts.clear();
                self.completion_spans.clear();
                self.completion_selected.clear();
                self.completion_emphasis_colors.clear();
                self.completion_description_texts.clear();
                self.completion_header_text.clear();
                self.completion_count_text.clear();
                self.media_receipt_title_text.clear();
                self.media_receipt_detail_text.clear();
                self.lane_texts
                    .values_mut()
                    .for_each(media_lane::LaneText::invalidate);
                self.search_buffer_text.clear();
                self.search_segment_texts.iter_mut().for_each(String::clear);
            }
        }
        // Ensure one text buffer per pane.
        while self.pane_buffers.len() < panes.len() {
            let b = TextBuffer::new(&mut self.font_system, metrics);
            self.pane_buffers.push(b);
        }
        // The line-key / style-key pools live and die with `pane_buffers`,
        // because a key must always describe the content actually shaped into
        // the buffer at the same index.
        while self.pane_line_keys.len() < panes.len() {
            self.pane_line_keys.push(Vec::new());
        }
        while self.pane_style_keys.len() < panes.len() {
            self.pane_style_keys.push(0);
        }
        // Parallel grow for per-pane titlebar buffers.
        while self.pane_titlebar_buffers.len() < panes.len() {
            let b = TextBuffer::new(&mut self.font_system, metrics);
            self.pane_titlebar_buffers.push(b);
        }
        while self.pane_titlebar_texts.len() < panes.len() {
            self.pane_titlebar_texts.push(String::new());
        }
        while self.pane_buffer_ids.len() < panes.len() {
            self.pane_buffer_ids.push(None);
        }
        for (i, pane) in panes.iter().enumerate() {
            let pane_id = pane.id;
            if self.pane_buffer_ids[i] == Some(pane_id) {
                continue;
            }
            if let Some(j) = (i + 1..self.pane_buffer_ids.len())
                .find(|&j| self.pane_buffer_ids[j] == Some(pane_id))
            {
                self.pane_buffer_ids.swap(i, j);
                self.pane_buffers.swap(i, j);
                self.pane_line_keys.swap(i, j);
                self.pane_style_keys.swap(i, j);
                self.pane_titlebar_buffers.swap(i, j);
                self.pane_titlebar_texts.swap(i, j);
            } else {
                self.pane_buffer_ids[i] = Some(pane_id);
                self.pane_line_keys[i].clear();
                self.pane_style_keys[i] = 0;
                self.pane_titlebar_texts[i].clear();
            }
        }
        // Release buffers for panes that have closed. The grow loops above
        // only extend, so without this the pools stay at the session's
        // high-water pane count: a 6-way split closed back to one pane keeps
        // 5 idle TextBuffers (with their shaped glyph runs) for the rest of
        // the session. Truncation is safe: every later loop indexes by
        // enumerate position `< panes.len()`.
        self.pane_buffers.truncate(panes.len());
        self.pane_buffer_ids.truncate(panes.len());
        self.pane_line_keys.truncate(panes.len());
        self.pane_style_keys.truncate(panes.len());
        self.pane_titlebar_buffers.truncate(panes.len());
        self.pane_titlebar_texts.truncate(panes.len());
        // Write each pane's title into its titlebar
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
                // Label layout: "  [group]  TITLE  WxH  <bell>". The group pill
                // appears for a named group (the brackets give it visual weight
                // without a separate quad), WxH unless cfg.title_hide_sizetext,
                // and the bell when cfg.icon_bell && pv.bell. To fit the pane
                // width, `fit_pane_titlebar_title` sheds the size text, then the
                // group tag, then middle-ellipsizes the title (keeping the
                // program/leaf name).
                let group = pv.group_name.filter(|g| !g.is_empty());
                let size_text = (!cfg.title_hide_sizetext)
                    .then(|| format!("{}x{}", pv.size_cols, pv.size_rows));
                let bell = (cfg.icon_bell && pv.bell).then_some("\u{1F514}");
                let budget = (rw / cw.max(1.0)).floor().max(1.0) as usize;
                let label = fit_pane_titlebar_title(
                    group,
                    pv.title_prefix,
                    title,
                    pv.title_path,
                    size_text.as_deref(),
                    bell,
                    budget,
                );
                let buf = &mut self.pane_titlebar_buffers[i];
                buf.set_metrics(metrics);
                buf.set_size(Some(rw), Some(pane_titlebar_h));
                // `Buffer::set_text` re-shapes unconditionally, so gate it on
                // text change and a steady title costs nothing.
                if self.pane_titlebar_texts[i] != label {
                    // Use Advanced shaping, like every other chrome buffer. The
                    // title is shell-controlled OSC 0/2 text (agents such as
                    // Claude Code lead with a status glyph) and the label can
                    // carry the 🔔 bell. Basic skips cosmic-text's platform
                    // font fallback (Segoe UI Emoji/Symbol, Noto), so any
                    // glyph outside the bundled Nerd Font would render as tofu.
                    buf.set_text(
                        &label,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.pane_titlebar_texts[i] = label;
                }
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Pre-size the per-frame quad/image vectors so the 60fps hot path
        // avoids realloc churn (borders + per-pane chrome + cell-background
        // quads dominate `quads`). Capacities are rough upper-of-typical
        // estimates; outliers still grow. `quads` reuses the pooled
        // `quad_scratch` (cleared, capacity kept from the prior frame) and
        // returns to it after the GPU upload below.
        let mut quads: Vec<QuadInstance> = std::mem::take(&mut self.quad_scratch);
        self.cursor_quad_range = None;
        self.card_cursor_quad_range = None;
        quads.clear();
        quads.reserve(panes.len() * 16 + 256);
        let mut pane_outlines: Vec<OutlineInstance> = Vec::with_capacity(panes.len());
        let mut pane_bases: Vec<QuadInstance> = Vec::with_capacity(panes.len() + 1);
        if !background_has_wallpaper(cfg) {
            pane_bases.push(rect(
                0.0,
                0.0,
                sw,
                sh,
                default_bg,
                composed_bg_alpha(cfg) as f32,
            ));
        }
        // Third quad pass, drawn after `over` so the right-click context
        // menu's bg/shadow/border/highlight sit on top of every other UI
        // element. `menu_text_renderer` draws the menu labels after this pass,
        // on top of the panel bg.
        //
        // The per-frame buffers below (menu_q / over / image items /
        // image-live sets) are deliberately allocated fresh each frame, unlike
        // the pooled `quad_scratch` / `span_scratch`. They are small and usually
        // near-empty (no open context menu, a handful of panes, no cell images),
        // so the allocation is trivial; high-water pooling is reserved for the
        // large per-cell `quads` / `spans` buffers, where it pays off.
        use kettle_config::BackgroundType;
        let mut ime_q: Vec<QuadInstance> = Vec::with_capacity(2);
        let mut menu_q: Vec<QuadInstance> = Vec::with_capacity(64);
        let mut pane_dim = Vec::with_capacity(panes.len());
        let mut decorations = Vec::new();
        // Drawn after lane content: lane dimming and scrollbar thumbs.
        let mut over: Vec<QuadInstance> = Vec::with_capacity(panes.len() * 4 + 8);
        let mut img_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(16);
        let mut media_receipt_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(1);
        let mut lane_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(overlay.media_lanes.len());
        let mut lane_q: Vec<QuadInstance> = Vec::new();
        if let Some(shades) = overlay
            .media_viewer_rect
            .and_then(|viewer| media_lane::viewer_backdrop(viewer, (sw, sh)))
        {
            for (x, y, width, height) in shades {
                over.push(rect(x, y, width, height, Rgb::new(0, 0, 0), 0.35));
            }
        }
        // The wallpaper is always one retained item in its own back-most pass;
        // tile mode repeats UVs in the sampler instead of rebuilding a quad per
        // tile on every frame.
        let mut bg_img_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(1);
        // When `chrome-background = auto`, the average color of the
        // currently-displayed wallpaper frame, used to tint the chrome strips.
        // Computed once from the displayed frame below (only when auto is set).
        let mut bg_frame_avg: Option<Rgb> = None;
        // Starfield's fragment shader writes alpha 1 over the whole surface.
        // Image backgrounds prove this separately from the selected frame's
        // cached alpha scan and its destination geometry below.
        let mut opaque_wallpaper_covers_surface =
            matches!(cfg.background_type, BackgroundType::Starfield);

        // Terminator parity, bg-image: when cfg.background_type = Image +
        // cfg.background_image is set, decode once, cache, and push the
        // wallpaper into `bg_img_items`, its own back-most pass. The
        // `decode_bg_image_frames_with_blur` helper handles the
        // file-not-found / decode-error paths gracefully.
        if matches!(cfg.background_type, BackgroundType::Image) && !cfg.background_image.is_empty()
        {
            let want = cfg.background_image.clone();
            // The worker decodes through `decode_bg_image_frames_with_blur`,
            // so cfg.background_blur takes effect at load time. Radius 8 is a
            // reasonable default for the on/off toggle Terminator's bool config
            // exposes; a follow-up could expose a `background_blur_radius`
            // numeric for finer control.
            let blur_radius: u32 = if cfg.background_blur { 8 } else { 0 };
            // Reload when the path OR the blur radius changes, so toggling
            // `background-blur` on a still-loaded image takes effect.
            let need_reload = match self.bg_image_cache.as_ref() {
                None => true,
                // Reload when the (path, blur) key changed, OR when the cached
                // entry is a FAILED decode (`frames` empty) and the throttle has
                // elapsed. A transient read error or an in-place file fix
                // self-heals, but THROTTLED (≥3s between attempts) so a broken
                // or corrupt path is NOT re-decoded every frame. A successful
                // decode clears the throttle, so the happy path never re-decodes.
                Some(c) => {
                    c.path != want
                        || c.blur != blur_radius
                        || (c.frames.is_empty()
                            && self
                                .bg_image_retry_at
                                .is_none_or(|t| std::time::Instant::now() >= t))
                }
            };
            if need_reload {
                // (Re)decode ALL frames (one for a still image; many for an
                // animated GIF/APNG/WebP) off the render thread. An animated,
                // blurred wallpaper's decode+blur can take tens of ms PER FRAME
                // (up to 128 frames), which would stall the winit event loop and
                // every window's render pass. `request_bg_image_reload` just
                // (re)submits the job to `bg_image_worker`; the result lands on
                // a LATER frame via `apply_bg_image_worker_result` below.
                self.request_bg_image_reload(&want, blur_radius);
            }
            // Pick up a finished decode (if any) before selecting the frame
            // to display. A cheap `try_recv` no-op when nothing has finished.
            self.apply_bg_image_worker_result(&want, blur_radius);
            // Select the frame to display now. A still image (1 frame) or
            // `background-animation = off` shows frame 0; otherwise the
            // playback clock loops through frames at their own gaps. Focus does
            // NOT gate the index (so an output-driven repaint while unfocused
            // still shows the time-correct frame, no jump); focus only gates
            // whether the render loop PROACTIVELY wakes to animate, via
            // `background_is_animating` feeding the anim tick.
            let bg_frame: Option<(&kettle_core::ImageData, bool)> = self
                .bg_image_cache
                .as_ref()
                .filter(|c| !c.frames.is_empty())
                .map(|c| {
                    let idx = if c.frames.len() > 1
                        && cfg.background_animation != kettle_config::BackgroundAnimation::Off
                    {
                        bg_image::bg_current_frame(&c.gaps, c.started.elapsed().as_millis())
                    } else {
                        0
                    };
                    let idx = idx.min(c.frames.len() - 1);
                    (
                        &c.frames[idx],
                        c.opaque_frames.get(idx).copied().unwrap_or(false),
                    )
                });
            if let Some((data, frame_is_opaque)) = bg_frame {
                // Sample the displayed frame's average color for
                // `chrome-background = auto`. Sampled + alpha-aware, so it's a
                // few microseconds even on a 4K frame; only when auto is set.
                if cfg.chrome_background == kettle_config::ChromeBackground::Auto {
                    bg_frame_avg = Some(color::average_color(data.rgba.as_slice()));
                }
                // Terminator parity, bg-image: UV-mode variants.
                // background-image-mode
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
                if cfg.background_image_mode == "tile" {
                    bg_img_items.push(imgpipe::ImageItem::tiled(sw, sh, data.clone()));
                    opaque_wallpaper_covers_surface = frame_is_opaque;
                } else {
                    let rect @ [x, y, w, h] = background_image_rect(
                        cfg.background_image_mode.as_str(),
                        cfg.background_image_align_horiz.as_str(),
                        cfg.background_image_align_vert.as_str(),
                        [sw, sh],
                        [img_w, img_h],
                    );
                    bg_img_items.push(imgpipe::ImageItem::full(x, y, w, h, data.clone()));
                    opaque_wallpaper_covers_surface =
                        frame_is_opaque && rect_covers_surface(rect, [sw, sh]);
                }
            }
        } else {
            // Config no longer requests an image background
            // (type switched away, or path cleared) — drop the decoded RGBA so
            // a large wallpaper isn't pinned for the rest of the
            // session. Re-enabling re-decodes via the need_reload path above.
            self.bg_image_cache = None;
            self.bg_image_retry_at = None; // reset the self-heal throttle
            self.bg_image_pending = None;
            // The worker requests a redraw when a late result is published.
            if let Some(worker) = self.bg_image_worker.as_ref() {
                while worker.try_recv().is_some() {}
            }
        }

        // The opaque fill color for the window chrome strips (tab bar, status
        // bar, new-tab button). It differs from the theme's `palette[8]` only
        // when a wallpaper is in use AND `chrome-background` asks for it. See
        // `resolve_chrome_bg`.
        let chrome_strip_bg = resolve_chrome_bg(cfg, theme, bg_frame_avg);

        // Status-bar background. The text is uploaded alongside
        // `tabbar_buffer.set_text` further down so the same text-renderer
        // pass handles both.
        if status.height > 0.0 {
            quads.push(rect(0.0, status.y, sw, status.height, chrome_strip_bg, 1.0));
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
            // Vertical tabs: when the strip
            // is vertical (TabBarPos::Left/Right), paint the bar
            // background as a column matching the strip rect
            // instead of a full-width horizontal stripe.
            if cfg.tab_bar_pos.is_vertical() {
                // Derive the strip's x + width from the first
                // segment (the UI's tab-bar layout already hands us
                // correct per-segment rects). new_tab anchors at the same x/w.
                let (sx, _, swid, _) = tabbar
                    .segments
                    .first()
                    .map(|s| s.rect)
                    .unwrap_or(tabbar.new_tab);
                quads.push(rect(sx, 0.0, swid, sh, chrome_strip_bg, 1.0));
            } else {
                quads.push(rect(0.0, by, sw, tabbar.height, chrome_strip_bg, 1.0));
            }
            for s in &tabbar.segments {
                // Vertical tabs: use the
                // segment's own y/h (from the UI's tab-bar layout) instead of
                // the strip-wide `by`/`tabbar.height`. For
                // horizontal layouts the values match; for
                // vertical they're per-row.
                let (x, seg_y, w, seg_h) = s.rect;
                if s.active {
                    quads.push(rect(x, seg_y, w, seg_h, default_bg, 1.0));
                    // When broadcast / group-input mode is on,
                    // use a warning-yellow accent (theme palette index 3,
                    // the standard ANSI "yellow" slot) for the active tab
                    // so the user can't forget broadcast is enabled and
                    // type to one pane expecting it to stay local. Other
                    // tabs are unaffected — broadcast is scoped to the
                    // active tab, so only the
                    // active segment's accent flips.
                    let accent = if tabbar.broadcast {
                        theme.palette[3]
                    } else {
                        // Multi-window: the per-WINDOW accent
                        // (Peacock pool slot, live-deduped), falling back to
                        // explicit `accent-color` → theme signature.
                        self.ui_accent(cfg, theme)
                    };
                    quads.push(rect(x, seg_y, 2.0, seg_h, accent, 1.0));
                }
                // Thin separator on the right (horizontal) or
                // bottom (vertical) of each segment. For vertical,
                // the UI's tab-bar layout stacks rows top-to-bottom,
                // so the separator goes ALONG the bottom edge of
                // each row instead of the right edge.
                if cfg.tab_bar_pos.is_vertical() {
                    quads.push(rect(x, seg_y + seg_h - 1.0, w, 1.0, theme.background, 0.5));
                } else {
                    quads.push(rect(x + w - 1.0, seg_y, 1.0, seg_h, theme.background, 0.5));
                }
                // Activity indicator dot — a small disc-
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
                    // Silent is the "your watched output
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
                    // Render the dot as a small square. wgpu has no circle
                    // primitive here, and a 6x6 to 12x12 square at high
                    // opacity reads as a "bullet" at typical tab-bar sizes
                    // (kitty / iTerm2 do the same in their text-only
                    // inactive-tab indicators).
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
                // Terminator parity, terminatorlib/config.py:81
                // `close_button_on_tab`: when false, skip the close
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
            // New-tab control group. The dropdown and `+` keep independent
            // hit targets and hover surfaces, while the rest-state background
            // unifies them as one trailing action cluster.
            //
            // A permanent two-pixel accent cap closes the right edge. It
            // mirrors the active tab's two-pixel leading accent and prevents
            // the `+` from reading like an unfinished rectangle at the window
            // boundary. Two *physical* pixels stays crisp at every scale.
            // Use the
            // new_tab rect's own y/h (set to the
            // strip-bottom row for vertical layouts).
            // Paint the union of [▾ | +] when the dropdown arrow is
            // present so there's no unpainted gap behind it; otherwise just the
            // `+` button.
            let (nx, ny, nw, nh) = tabbar.new_tab;
            let (mx, _, mw, _) = tabbar.new_tab_menu;
            let (bx, bw) = if mw > 0.0 { (mx, mw + nw) } else { (nx, nw) };
            quads.push(rect(bx, ny, bw, nh, chrome_strip_bg, 1.0));
            let accent = if tabbar.broadcast {
                theme.palette[3]
            } else {
                self.ui_accent(cfg, theme)
            };
            if tabbar.hovered_new_tab_menu && mw > 0.0 {
                quads.push(rect(mx, ny, mw, nh, accent, 0.14));
            }
            if tabbar.hovered_new_tab {
                quads.push(rect(nx, ny, nw, nh, accent, 0.14));
            }
            if mw > 0.0 {
                quads.push(rect(nx, ny + 6.0, 1.0, (nh - 12.0).max(0.0), accent, 0.45));
            }
            let cap_x = if matches!(cfg.tab_bar_pos, kettle_config::TabBarPos::Left) {
                nx
            } else {
                nx + nw - 2.0
            };
            quads.push(rect(cap_x, ny, 2.0, nh, accent, 1.0));
            // Drag-in-progress ghost. While the user holds a
            // left button down on the tab bar, paint a
            // translucent overlay copy of the active segment centered
            // at the cursor x. The underlying segments still snap to
            // their target positions via `move_active_tab`; the ghost
            // gives the bar a "you're picking this tab up" affordance
            // so the snap doesn't read as a confusing teleport. Push
            // to `over` (post-text) so the ghost sits above the live
            // segment text. Drawn only when both a drag is active
            // *and* there's an active segment to copy from.
            if let Some(active_seg) = tabbar.segments.iter().find(|s| s.active)
                && let Some((ghost_x, ghost_y)) = {
                    let (seg_x, _, seg_w, seg_h) = active_seg.rect;
                    // Clamp the ghost so the box doesn't slide entirely off
                    // either end of the strip — same idea as
                    // `context_menu_geometry`'s anchor clamp. A vertical bar
                    // rides the cursor's y down a fixed column; a horizontal
                    // one rides x along a fixed row.
                    tabbar
                        .drag_cursor_x
                        .map(|cx| ((cx - seg_w * 0.5).clamp(0.0, (sw - seg_w).max(0.0)), by))
                        .or_else(|| {
                            tabbar.drag_cursor_y.map(|cy| {
                                (seg_x, (cy - seg_h * 0.5).clamp(0.0, (sh - seg_h).max(0.0)))
                            })
                        })
                }
            {
                let (_, _, seg_w, seg_h) = active_seg.rect;
                // Pre-tear escalation: the ghost "lifts off" as the cursor
                // approaches the tear threshold (bigger/darker shadow, fading
                // body), so a release reads as "this will tear" instead of
                // surprising the user with a new window. `tear_lift` is 0 for a
                // plain reorder.
                let lift = tabbar.tear_lift.clamp(0.0, 1.0);
                let shadow_off =
                    tab_drag::GHOST_SHADOW_OFFSET_PX + tab_drag::GHOST_SHADOW_OFFSET_LIFT_PX * lift;
                let shadow_alpha =
                    tab_drag::GHOST_SHADOW_ALPHA + tab_drag::GHOST_SHADOW_ALPHA_LIFT * lift;
                let bg_alpha = tab_drag::GHOST_BG_ALPHA - tab_drag::GHOST_BG_ALPHA_LIFT * lift;
                // Soft drop shadow under the ghost (same trick as
                // `menu_chrome_quads`'s context menu).
                over.push(rect(
                    ghost_x + shadow_off,
                    ghost_y + shadow_off,
                    seg_w,
                    seg_h,
                    Rgb::new(0, 0, 0),
                    shadow_alpha,
                ));
                // Ghost background — theme.background, translucent enough
                // that the bar shows through and it reads as a floating
                // preview rather than a real new tab.
                over.push(rect(
                    ghost_x,
                    ghost_y,
                    seg_w,
                    seg_h,
                    theme.background,
                    bg_alpha,
                ));
                // Accent strip on the left edge, in the same color the live
                // active segment uses (palette[3] yellow under broadcast,
                // otherwise `ui_accent`), so the ghost matches its source
                // segment.
                let accent = if tabbar.broadcast {
                    theme.palette[3]
                } else {
                    self.ui_accent(cfg, theme)
                };
                over.push(rect(
                    ghost_x,
                    ghost_y,
                    tab_drag::GHOST_ACCENT_W_PX,
                    seg_h,
                    accent,
                    1.0,
                ));
            }
            // Re-dock insertion marker: an accent line between segments
            // showing where a torn-off window's tab will dock. Pushed to
            // `over` so it sits above segment backgrounds AND text (a thin
            // line under text would vanish behind a long title). Rect comes
            // oriented from the UI (vertical line for horizontal bars,
            // horizontal line for vertical bars).
            if let Some((ix, iy, iw, ih)) = tabbar.insert_marker {
                let accent = self.ui_accent(cfg, theme);
                // Dock-target highlight: wash the whole latched band in
                // translucent accent plus a border on the pane-facing edge.
                // Without it the marker line is the ONLY latch signal off
                // Windows (the torn-window alpha trick is Windows-only), and on
                // Linux that reads as "nothing happened". Presence is derived
                // from `insert_marker` so the two signals cannot drift apart.
                let (bx0, by0, bw0, bh0) = tabbar.band;
                if bw0 > 0.0 && bh0 > 0.0 {
                    over.push(rect(
                        bx0,
                        by0,
                        bw0,
                        bh0,
                        accent,
                        tab_drag::DOCK_HIGHLIGHT_WASH_ALPHA,
                    ));
                    // Border on the pane-facing edge: bottom for a top bar,
                    // top for a bottom bar, right for a left bar, left for
                    // a right bar — taken from `tab-bar-position` directly
                    // (shape inference misreads a tall `tab-bar-width` next
                    // to a short window).
                    let bp = tab_drag::DOCK_HIGHLIGHT_BORDER_PX;
                    let (ex, ey, ew, eh) = match cfg.tab_bar_pos {
                        kettle_config::TabBarPos::Top => (bx0, by0 + bh0 - bp, bw0, bp),
                        kettle_config::TabBarPos::Bottom => (bx0, by0, bw0, bp),
                        kettle_config::TabBarPos::Left => (bx0 + bw0 - bp, by0, bp, bh0),
                        kettle_config::TabBarPos::Right => (bx0, by0, bp, bh0),
                    };
                    over.push(rect(
                        ex,
                        ey,
                        ew,
                        eh,
                        accent,
                        tab_drag::DOCK_HIGHLIGHT_BORDER_ALPHA,
                    ));
                }
                over.push(rect(ix, iy, iw, ih, accent, 1.0));
                // Square end-caps (the activity-dot "bullet" idiom — this
                // renderer has no curves) so the marker reads as a placed
                // pin rather than a stray hairline.
                let cap = tab_drag::INSERT_MARKER_CAP_PX;
                if iw < ih {
                    let cx0 = ix + iw * 0.5 - cap * 0.5;
                    over.push(rect(cx0, iy - cap * 0.5, cap, cap, accent, 1.0));
                    over.push(rect(cx0, iy + ih - cap * 0.5, cap, cap, accent, 1.0));
                } else {
                    let cy0 = iy + ih * 0.5 - cap * 0.5;
                    over.push(rect(ix - cap * 0.5, cy0, cap, cap, accent, 1.0));
                    over.push(rect(ix + iw - cap * 0.5, cy0, cap, cap, accent, 1.0));
                }
            }
        }

        // Per-pane grid + dividers/border.
        // Idle perf: true if ANY pane reshaped a row this frame.
        let mut any_pane_text_changed = false;
        // Reset the focused-cursor glyph; the focused pane's `build_pane` re-sets
        // it this frame if a solid block cursor is visible.
        self.pending_cursor_glyph = None;
        self.minimum_contrast_cache.clear();
        // Inline image instances share one renderer/window budget. Allocate it
        // across panes before iterating so pane order cannot monopolize all
        // slots. Offscreen placements consume no quota.
        let placement_limit = self.graphics_budget.limits().placements;
        let visible_placement_counts = panes
            .iter()
            .map(|pane| {
                pane.images
                    .iter()
                    .filter(|placement| placement_is_visible(pane.snap, placement))
                    .count()
            })
            .collect::<Vec<_>>();
        let placement_quotas = fair_placement_quotas(&visible_placement_counts, placement_limit);
        let mut card_frames = std::mem::take(&mut self.card_frames);
        card_frames.resize_with(panes.len(), inline_cards::CardFrame::default);
        card_frames.truncate(panes.len());
        let mut card_scene = std::mem::take(&mut self.card_scene);
        card_scene.clear();
        let mut drawn_cards = std::mem::take(&mut self.drawn_cards);
        drawn_cards.clear();
        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.rect;
            // Pane separators / focus border. Both colors are config-
            // overridable: `split-divider-color` for inactive panes
            // (defaults to theme `palette[8]`, the dim color) and
            // `focused-split-color` for the focused pane (defaults to the
            // resolved accent below).
            //
            // When broadcast / group-input mode is on, the focused-pane border
            // flips to theme palette[3] (yellow, the same warning slot the
            // tab-bar accent uses). The tab-bar indicator alone is not enough,
            // because with `tab-bar = auto` and one tab open (the default
            // single-window case) the tab bar is hidden. Inactive panes keep
            // their normal divider color, since broadcast is scoped to the
            // active tab and the focused-pane border is the single most-visible
            // chrome element on every layout.
            let border = if pv.focused {
                if tabbar.broadcast {
                    theme.palette[3]
                } else {
                    // Cascade order is
                    //   focused-split-color (explicit override)
                    //   → resolved accent (explicit accent-color → Peacock
                    //     auto → the theme's signature accent, Mocha mauve)
                    // Backward-compat: anyone who set `focused-split-color`
                    // keeps their pinned color.
                    cfg.focused_split_color
                        .unwrap_or_else(|| self.ui_accent(cfg, theme))
                }
            } else {
                cfg.split_divider_color.unwrap_or(theme.palette[8])
            };
            // Terminator parity (terminatorlib/config.py:74
            // `handle_size`): split-divider width in px. -1 means
            // "use theme default" (1.0 here), 0 draws no border (see
            // below), and 1-50 are used as-is. Clamping was already done
            // at parse time.
            let bw = if cfg.handle_size < 0 {
                1.0
            } else {
                cfg.handle_size as f32
            };
            let corner_mask =
                pane_bottom_window_corner_mask(pv.rect, (sw, sh), self.rounded_window_corners);
            if bw <= 0.0 {
                // A zero handle size means no pane border. Do not send a
                // degenerate outline to the shader: derivative AA can otherwise
                // give a mathematical zero-width centreline visible coverage.
            } else if corner_mask == 0 {
                quads.push(rect(rx, ry, rw, bw, border, 1.0));
                quads.push(rect(rx, ry + rh - bw, rw, bw, border, 1.0));
                quads.push(rect(rx, ry, bw, rh, border, 1.0));
                quads.push(rect(rx + rw - bw, ry, bw, rh, border, 1.0));
            } else {
                pane_outlines.push(pane_outline(
                    pv.rect,
                    border,
                    bw,
                    MACOS_WINDOW_CORNER_RADIUS_POINTS * self.scale,
                    corner_mask,
                ));
            }

            // Per-pane titlebar background quad. Drawn
            // ABOVE the pane's border + BELOW the pane's content.
            // Color picks from the cfg.title_*_bg_color variants
            // based on focus + broadcast group state.
            if pane_titlebar_h > 0.0 {
                let bar_bg = pick_titlebar_bg(
                    cfg,
                    theme,
                    self.ui_accent(cfg, theme),
                    pv.focused,
                    tabbar.broadcast,
                );
                // title_at_bottom selects the titlebar edge; terminal geometry comes from
                // the shared pane partition.
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

            let pane_has_search = overlay
                .search
                .as_ref()
                .map_or(pv.focused, |search| search.target_pane == Some(pv.id));
            let grid_origin = pane_grid_origin(pv.terminal, (pad_x, pad_y));
            let card_frame = &mut card_frames[i];
            if let Some(cards) = pv.inline_cards {
                cards.recognize_into(pv.snap, card_frame);
                let drawn_before = card_scene.drawn.len();
                if let Some(pane_body) = pane_backdrop_rect(pv.terminal, bw)
                    && let Some(clip) = inline_image_clip(
                        pane_body,
                        grid_origin,
                        (pv.snap.columns, pv.snap.screen_lines),
                        (cw, ch),
                    )
                {
                    card_scene.append(
                        cards,
                        card_frame,
                        pv.snap,
                        &card_scene::CardGeometry {
                            grid_origin: [grid_origin.0, grid_origin.1],
                            cell: [cw, ch],
                            clip,
                            tr: pv.tr,
                        },
                        &card_scene::CardColors {
                            background: theme.background,
                            frame: self.ui_accent(cfg, theme),
                            selection: theme.selection_background,
                        },
                    );
                }
                drawn_cards.extend(card_scene.drawn[drawn_before..].iter().map(
                    |&(nonce, rect)| PaintedCard {
                        pane: pv.id,
                        nonce,
                        rect,
                    },
                ));
            } else {
                card_frame.clear();
            }
            any_pane_text_changed |= self.build_pane(
                i,
                pv,
                cfg,
                pane_inactive_amount(pv, panes.len(), cfg),
                &family,
                overlay.window_focused,
                pv.focused && overlay.ime_preedit.is_some(),
                if pane_has_search {
                    &overlay.highlights
                } else {
                    &[]
                },
                &mut quads,
                &mut decorations,
                &mut pane_bases,
                card_frame,
                &mut card_scene.base,
                &mut card_scene.cursors,
            );

            // Image placements, anchored history-aware so they scroll.
            {
                let quota = placement_quotas[i];
                let image_clip = pane_backdrop_rect(pv.terminal, bw).and_then(|pane_body| {
                    inline_image_clip(
                        pane_body,
                        grid_origin,
                        (pv.snap.columns, pv.snap.screen_lines),
                        (cw, ch),
                    )
                });
                let mut draw = |p: &kettle_core::Placement| {
                    let Some(image_clip) = image_clip else {
                        return;
                    };
                    let Some(row) = placement_viewport_row(pv.snap, p) else {
                        return;
                    };
                    let (image_x, image_y, image_width, image_height) =
                        inline_placement_rect(grid_origin.0, grid_origin.1, row, cw, ch, p);
                    // Placements shift below the titlebar so a Kitty/Sixel
                    // image at row zero cannot overlap the pane chrome.
                    img_items.push(imgpipe::ImageItem::placement(
                        [image_x, image_y, image_width, image_height],
                        p.img.clone(),
                        p.source_rect,
                        p.source_crop,
                        image_clip,
                    ));
                };
                if quota > 1 {
                    // Select in the app's class-interleaved order first, then
                    // sort that fair subset for correct pane-local compositing.
                    let mut ordered = pv
                        .images
                        .iter()
                        .filter(|placement| placement_is_visible(pv.snap, placement))
                        .take(quota)
                        .collect::<Vec<_>>();
                    ordered.sort_by_key(|placement| placement.z);
                    for placement in ordered {
                        draw(placement);
                    }
                } else if quota == 1
                    && let Some(placement) = pv
                        .images
                        .iter()
                        .find(|placement| placement_is_visible(pv.snap, placement))
                {
                    draw(placement);
                }
            }

            // Hyperlink underlines, focused pane only (brighter on hover).
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
                    grid_origin.0 + ln.col as f32 * cw,
                    grid_origin.1 + ln.row as f32 * ch + ch - 1.5,
                    ln.width as f32 * cw,
                    1.5,
                    col,
                    1.0,
                ));
            }

            // Search coordinates belong to the pane captured when the lane
            // opened, even if focus changes through automation or pane reap.
            if pane_has_search {
                for hl in &overlay.highlights {
                    quads.push(rect(
                        grid_origin.0 + hl.col as f32 * cw,
                        grid_origin.1 + hl.row as f32 * ch,
                        hl.width as f32 * cw,
                        ch,
                        if hl.active {
                            // The active match uses the theme's yellow
                            // (Mocha #f9e2af) unless `search-background`
                            // overrides it, so it follows the theme like the
                            // inactive highlight's theme.selection_bg.
                            cfg.search_background.unwrap_or(theme.palette[3])
                        } else {
                            theme.selection_background
                        },
                        1.0,
                    ));
                }
                // Quick-select hint label chips, with the menus on top of
                // everything, inline cards included: a card's label sits on
                // its poster and provenance tab. Hint mode closes any menu
                // first, so nothing else is up there. A chip is cut to its
                // pane, as its label is, so one at the last column never
                // covers the next pane.
                for hint in &overlay.hint_labels {
                    let n = hint.label.chars().count().max(1) as f32;
                    let left = grid_origin.0 + hint.col as f32 * cw;
                    let top = grid_origin.1 + hint.row as f32 * ch;
                    let (x0, y0) = (left.max(rx), top.max(ry));
                    let (x1, y1) = ((left + n * cw).min(rx + rw), (top + ch).min(ry + rh));
                    if x1 > x0 && y1 > y0 {
                        menu_q.push(rect(
                            x0,
                            y0,
                            x1 - x0,
                            y1 - y0,
                            if hint.dim {
                                theme.palette[8]
                            } else {
                                cfg.search_background.unwrap_or(theme.palette[3])
                            },
                            if hint.dim { 0.6 } else { 0.96 },
                        ));
                    }
                }
            }
            if !pv.focused && panes.len() > 1 {
                let amount = inactive_pane_amount(cfg);
                if amount > 0.0 {
                    let (x, y, w, h) = pv.terminal;
                    pane_dim.push(rect(x, y, w, h, theme.background, amount));
                    for lane in overlay.media_lanes.iter().filter(|lane| lane.pane == pv.id) {
                        let (x, y, w, h) = lane.rect;
                        over.push(rect(x, y, w, h, theme.background, amount));
                    }
                }
            }
            if cfg.scrollbar != ScrollbarMode::Never {
                // The scrollbar runs the height of the terminal it scrolls,
                // clear of the titlebar and lane.
                let (rx, ry, rw, rh) = pv.terminal;
                let s = pv.snap;
                let (rows, hist, off) = (s.screen_lines, s.history_size, s.display_offset);
                let has_scroll = hist > 0 && rows + hist > rows;
                // Compact overlay scrollbar. Width and minimum thumb are logical
                // pixels scaled to the surface DPI; input uses the same scale but
                // a larger invisible hit strip. Foreground-derived chrome keeps
                // reliable contrast across light and dark themes. The two-state
                // opacity still needs no fade timer or idle redraws.
                if has_scroll || cfg.scrollbar == ScrollbarMode::Always {
                    let bar_w = cfg.scrollbar_width.clamp(2.0, 40.0) * self.scale;
                    let edge = 2.0 * self.scale;
                    let bx = rx + rw - bar_w - edge;
                    let active = off > 0 || (pv.focused && overlay.scrollbar_active);
                    let (track_a, thumb_a) = if active { (0.14, 0.82) } else { (0.07, 0.42) };
                    let track_w = (1.5 * self.scale).clamp(1.0, bar_w);
                    if active || cfg.scrollbar == ScrollbarMode::Always {
                        over.push(rect(
                            bx + (bar_w - track_w) / 2.0,
                            ry,
                            track_w,
                            rh,
                            theme.foreground,
                            track_a,
                        ));
                    }
                    // Thumb — only when there is actually something to scroll.
                    if let Some((ty, th)) = kettle_core::scrollbar::thumb_with_min(
                        rows,
                        hist,
                        off,
                        rh,
                        24.0 * self.scale,
                    ) {
                        over.push(rect(bx, ry + ty, bar_w, th, theme.foreground, thumb_a));
                    }
                }
            }
        }

        // The card a click would open: an accent outline just inside its
        // visible part, over its frame. Static, so there is no motion to
        // reduce.
        // Only over that same card, drawn there by this frame: one that
        // moved, or another card in its place, leaves no outline behind.
        if let Some(card) = overlay.card_hover
            && drawn_cards.contains(&card)
        {
            let [x, y, width, height] = card.rect;
            let edge = (2.0 * self.scale)
                .round()
                .max(1.0)
                .min(width / 2.0)
                .min(height / 2.0);
            let accent = self.ui_accent(cfg, theme);
            over.push(rect(x, y, width, edge, accent, 1.0));
            over.push(rect(x, y + height - edge, width, edge, accent, 1.0));
            over.push(rect(x, y + edge, edge, height - 2.0 * edge, accent, 1.0));
            over.push(rect(
                x + width - edge,
                y + edge,
                edge,
                height - 2.0 * edge,
                accent,
                1.0,
            ));
        }
        self.card_frames = card_frames;
        self.drawn_cards = drawn_cards;
        self.card_scene = card_scene;
        if self.card_posters.is_none() && !self.card_scene.posters.is_empty() {
            // Preview pressure cannot prevent a text window. Retry admission
            // on the next actual frame after another preview releases space.
            self.card_posters = self.card_image_shared.layer_with_instance_limit(
                &self.gpu.device,
                kettle_core::GraphicsBudget::previews(),
                4096,
            );
        }
        if let Some(posters) = &mut self.card_posters {
            posters.prepare_frame(&self.gpu.device, &self.card_scene.posters);
            posters.upload(
                &self.gpu.device,
                &self.gpu.queue,
                [sw, sh],
                &self.card_scene.posters,
            );
            self.card_scene
                .apply_upload_results(posters.drawn_item_indices());
        } else {
            self.card_scene.apply_upload_results(std::iter::empty());
        }

        // Terminator parity: the drop hint for a pane being dragged elsewhere
        // in its tab. Pushed to `over` — above pane content and the unfocused
        // dim, below the tab bar and every modal — so it reads as a preview of
        // the layout rather than a piece of chrome. Same wash-plus-border
        // treatment as the tab-bar dock target, so the two drag affordances
        // look like the same idea.
        if let Some((hx, hy, hw, hh)) = overlay.pane_drop_hint
            && hw > 0.0
            && hh > 0.0
        {
            let accent = self.ui_accent(cfg, theme);
            over.push(rect(
                hx,
                hy,
                hw,
                hh,
                accent,
                tab_drag::DOCK_HIGHLIGHT_WASH_ALPHA,
            ));
            // A border on all four sides, unlike the tab bar's single
            // pane-facing edge: this rect floats inside a pane rather than
            // sitting against the window edge, so one edge would not read as an
            // outline at all. Clamped so a hint narrower than two borders still
            // paints something instead of two overlapping bars.
            let bp = tab_drag::DOCK_HIGHLIGHT_BORDER_PX
                .min(hw / 2.0)
                .min(hh / 2.0);
            for (bx, by, bw, bh) in [
                (hx, hy, hw, bp),
                (hx, hy + hh - bp, hw, bp),
                (hx, hy, bp, hh),
                (hx + hw - bp, hy, bp, hh),
            ] {
                over.push(rect(bx, by, bw, bh, accent, 1.0));
            }
        }

        // Visual bell: a brief wash over the pane that rang (replaces an
        // audible beep). It sits in the `quads` pass, under the text, so
        // glyphs stay crisp and only cell backgrounds lift. The peak is
        // `bell-flash-intensity`, a step of CIE L* rather than an alpha: the
        // quad pipeline blends in linear light, where "alpha 0.10 of the
        // foreground" was a +21.7 L* jump on a dark theme and a -3 L* dip on
        // a light one. `pv.bell_flash` is the decay ramp; it scales the step
        // before the alpha is derived so the fade is perceptually linear too.
        // `0.0` opts out of the flash while leaving the rest of the bell
        // (window attention, tab dot, titlebar glyph) alone.
        for pv in panes {
            if pv.bell_flash <= 0.0 {
                continue;
            }
            let (rx, ry, rw, rh) = pv.rect;
            let alpha = bell_flash_alpha(theme, cfg, pv.bell_flash);
            if alpha > 0.0 {
                quads.push(rect(rx, ry, rw, rh, theme.foreground, alpha));
            }
        }

        // Search uses a responsive RESERVED lane. The app subtracts the public
        // `search_bar_geometry(...).reserved_height` from pane layout; paint and
        // hit-testing then consume these exact same rectangles. Other legacy
        // modals remain single-line overlays.
        let confirmation_layout = overlay.confirm_dialog.as_ref().map(|dialog| {
            ConfirmDialogLayout::new(
                (sw, sh),
                (
                    self.overlay_text_cell_width(),
                    self.overlay_text_line_height(),
                ),
                self.scale,
                dialog,
            )
        });
        let mut have_search = false;
        let mut search_rect = (0.0, sh - (ch + 10.0), sw, ch + 10.0);
        // The search lane draws each control's text in its own area:
        // (segment index, left, top, clip bounds).
        let mut search_segment_places: Vec<(usize, f32, f32, TextBounds, Option<Rgb>)> = Vec::new();
        self.search_editor_stops.clear();
        self.search_editor_clusters.clear();
        self.search_editor_rtl = false;
        if overlay.confirm_dialog.is_none()
            && let Some(search) = overlay.search.as_ref()
        {
            let cw = self.overlay_text_cell_width();
            let geometry = search_bar_geometry(sw, sh, cw, ch, &search.tr);
            search_rect = geometry.rect;
            let row_h = geometry.reserved_height / geometry.rows.max(1) as f32;
            let accent = cfg.search_background.unwrap_or(theme.palette[3]);

            quads.push(rect(
                geometry.rect.0,
                geometry.rect.1,
                geometry.rect.2,
                geometry.rect.3,
                theme.palette[8],
                0.98,
            ));
            // A distinct editor well survives low-contrast themes. All button
            // rectangles get a subtle surface; the focused one uses the same
            // accent as the active terminal match, and the one under the
            // pointer is lifted so it reads as clickable.
            quads.push(rect(
                geometry.editor.0,
                geometry.editor.1 + 2.0,
                geometry.editor.2,
                (geometry.editor.3 - 4.0).max(1.0),
                theme.background,
                0.72,
            ));
            // Hover lifts a button toward the text color; a press held over it
            // sinks toward the background instead.
            let tint_of = |control: SearchControl| -> Option<(Rgb, f32)> {
                let over = search.hovered == Some(control);
                if over && search.pressed == Some(control) {
                    Some((theme.background, 0.45))
                } else if over {
                    Some((theme.foreground, 0.14))
                } else {
                    None
                }
            };
            for control in SearchControl::ALL {
                let control_rect = geometry.control_rect(control);
                if control != SearchControl::Editor {
                    quads.push(rect(
                        control_rect.0,
                        control_rect.1 + 2.0,
                        control_rect.2,
                        (control_rect.3 - 4.0).max(1.0),
                        if search.focused == control {
                            accent
                        } else {
                            theme.background
                        },
                        if search.focused == control {
                            SEARCH_FOCUSED_FILL_ALPHA
                        } else {
                            0.28
                        },
                    ));
                    if let Some((tint, alpha)) = tint_of(control) {
                        quads.push(rect(
                            control_rect.0,
                            control_rect.1 + 2.0,
                            control_rect.2,
                            (control_rect.3 - 4.0).max(1.0),
                            tint,
                            alpha,
                        ));
                    }
                }
            }
            // Shape every label first: the caret and selection are placed
            // from the editor's shaped glyphs, not from a cell grid, so they
            // stay on the text whatever width a fallback font gives it.
            // cosmic-text centers each line's glyphs in the line height, so a
            // line as tall as the row, placed at the row's top, is centered.
            let segment_metrics = Metrics::new(metrics.font_size, row_h.max(metrics.font_size));
            let linear = self.config.format.is_srgb();
            let mut editor_left = geometry.editor.0 + cw;
            let mut editor_clusters = Vec::new();
            for (index, segment) in search_bar_segments(search, geometry, cw)
                .into_iter()
                .enumerate()
            {
                let shaped = segment.shaped_text();
                let text_changed = self.search_segment_texts[index] != shaped;
                if text_changed {
                    self.search_segment_texts[index] = shaped.into_owned();
                }
                shape_search_segment(
                    &mut self.font_system,
                    &mut self.search_segment_buffers[index],
                    &segment,
                    segment_metrics,
                    Family::Name(&family),
                    text_changed,
                );
                if segment.control == Some(SearchControl::Editor) {
                    editor_left = segment.left;
                    self.search_editor_rtl = segment.rtl;
                    editor_clusters =
                        segment_clusters(&self.search_segment_buffers[index], &segment);
                }
                let bar =
                    self.chrome_colors
                        .surface(theme.background, theme.palette[8], 0.98, linear);
                let background = match segment.control {
                    Some(SearchControl::Editor) => {
                        self.chrome_colors
                            .surface(bar, theme.background, 0.72, linear)
                    }
                    Some(control) => {
                        let (fill, alpha) = if search.focused == control {
                            (accent, SEARCH_FOCUSED_FILL_ALPHA)
                        } else {
                            (theme.background, 0.28)
                        };
                        let fill = self
                            .chrome_colors
                            .surface(bar, fill, f64::from(alpha), linear);
                        tint_of(control).map_or(fill, |(tint, alpha)| {
                            self.chrome_colors
                                .surface(fill, tint, f64::from(alpha), linear)
                        })
                    }
                    None => bar,
                };
                let color = Some(self.chrome_colors.text(
                    if segment.control == Some(search.focused)
                        && search.focused != SearchControl::Editor
                    {
                        theme.cursor_text
                    } else {
                        theme.foreground
                    },
                    background,
                ));
                let (x, y, w, h) = segment.rect;
                if w > 0.0 && h > 0.0 && !self.search_segment_texts[index].is_empty() {
                    search_segment_places.push((
                        index,
                        segment.left,
                        y,
                        TextBounds {
                            left: x as i32,
                            top: y as i32,
                            right: (x + w) as i32,
                            bottom: (y + h) as i32,
                        },
                        color,
                    ));
                }
            }

            // The visible query's clusters and caret positions, in query bytes
            // and window pixels. The UI hit-tests clicks against the same lists.
            let editor_cols = search_bar_columns(geometry.editor.2, cw).max(1);
            let (_, skip) = search_editor_body(search, editor_cols);
            let clusters = editor_clusters;
            let stops = cluster_stops(&clusters);
            self.search_editor_clusters = clusters
                .iter()
                .map(|cluster| SearchEditorCluster {
                    start: skip + cluster.start,
                    end: skip + cluster.end,
                    left: editor_left + cluster.left,
                    right: editor_left + cluster.right,
                    ..*cluster
                })
                .collect();
            self.search_editor_stops = stops
                .iter()
                .map(|&(byte, x)| (skip + byte, editor_left + x))
                .collect();
            // Keep marks a pixel inside the editor. A slot under two pixels
            // wide collapses the range instead of inverting it.
            let lowest = geometry.editor.0 + 1.0;
            let highest = (geometry.editor.0 + geometry.editor.2 - 1.0).max(lowest);
            let inside = |x: f32| x.max(lowest).min(highest);
            let x_at =
                |byte: usize| inside(editor_left + stop_x(&stops, byte.saturating_sub(skip)));
            // The selection goes under the caret, so a caret at either end of
            // it stays whole.
            if let Some((a, b)) = search.selection {
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                for (left, right) in cluster_spans(
                    &clusters,
                    start.saturating_sub(skip),
                    end.saturating_sub(skip),
                ) {
                    let (left, right) = (inside(editor_left + left), inside(editor_left + right));
                    if right > left {
                        quads.push(rect(
                            left,
                            geometry.editor.1 + 2.0,
                            right - left,
                            (geometry.editor.3 - 4.0).max(1.0),
                            theme.selection_background,
                            1.0,
                        ));
                    }
                }
            }
            if search.focused == SearchControl::Editor {
                // A two-pixel underline leaves query glyphs unobscured while
                // still giving the editor a strong keyboard-focus affordance.
                quads.push(rect(
                    geometry.editor.0,
                    geometry.editor.1 + geometry.editor.3 - 2.0,
                    geometry.editor.2,
                    2.0,
                    accent,
                    1.0,
                ));
                // The caret is a bar on the boundary between two glyphs.
                quads.push(rect(
                    x_at(search.cursor_byte.min(search.query.len())) - 1.0,
                    geometry.editor.1 + ((geometry.editor.3 - ch) * 0.5).max(0.0),
                    2.0,
                    ch.min(geometry.editor.3),
                    theme.foreground,
                    1.0,
                ));
            }
        } else if overlay.confirm_dialog.is_none()
            && let Some(q) = &overlay.palette_query
        {
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            push_input_bar(
                &mut quads,
                search_rect,
                theme,
                theme.palette[5],
                &mut self.chrome_colors,
            );
            let label = format!("  ⌘ {q}_   {}", overlay.palette_hint);
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // Same equality gate as the other chrome buffers. Only one arm of
            // this `if`/`else if` chain runs per frame, so a single cache is
            // enough (see `search_buffer_text`'s doc comment).
            if self.search_buffer_text != label {
                self.search_buffer.set_text(
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.search_buffer_text = label;
            }
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else if overlay.confirm_dialog.is_none()
            && let Some(q) = &overlay.theme_picker_query
        {
            // Theme picker input. The ranked themes live in the shared
            // context-menu panel above this lane, and the window behind them
            // already wears the selected theme.
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            push_input_bar(
                &mut quads,
                search_rect,
                theme,
                theme.palette[5],
                &mut self.chrome_colors,
            );
            let label = format!(
                "{}{q}_   {}",
                theme_picker_prefix(&overlay.tr),
                overlay.theme_picker_hint
            );
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // Same equality gate as the other chrome buffers. Only one arm of
            // this `if`/`else if` chain runs per frame, so a single cache is
            // enough (see `search_buffer_text`'s doc comment).
            if self.search_buffer_text != label {
                self.search_buffer.set_text(
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.search_buffer_text = label;
            }
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else if overlay.confirm_dialog.is_none()
            && let Some(q) = &overlay.layout_picker_query
        {
            // Terminator parity, layoutlauncher.py:
            // layout picker input. The ranked layouts live in the shared
            // context-menu panel above this lane.
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            push_input_bar(
                &mut quads,
                search_rect,
                theme,
                theme.palette[6],
                &mut self.chrome_colors,
            );
            let label = format!(
                "  ▤ {} {q}_   {}",
                overlay.tr.text(Text::PickerLayoutPrompt),
                overlay.layout_picker_hint
            );
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // Same equality gate as the other chrome buffers. Only one arm of
            // this `if`/`else if` chain runs per frame, so a single cache is
            // enough (see `search_buffer_text`'s doc comment).
            if self.search_buffer_text != label {
                self.search_buffer.set_text(
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.search_buffer_text = label;
            }
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else if overlay.confirm_dialog.is_none()
            && let Some(q) = &overlay.ssh_query
        {
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            push_input_bar(
                &mut quads,
                search_rect,
                theme,
                theme.palette[4],
                &mut self.chrome_colors,
            );
            let label = format!("  ssh ❯ {q}_   {}", overlay.ssh_hint);
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // Same equality gate as the other chrome buffers. Only one arm of
            // this `if`/`else if` chain runs per frame, so a single cache is
            // enough (see `search_buffer_text`'s doc comment).
            if self.search_buffer_text != label {
                self.search_buffer.set_text(
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.search_buffer_text = label;
            }
            self.search_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else if overlay.confirm_dialog.is_none()
            && let Some(edit) = &overlay.edit_title
        {
            // Terminator parity, edit-title overlay UX:
            // a thin bar in app chrome mirroring the shape of the
            // palette + ssh-input overlays without covering terminal rows.
            // Its palette[3] edge tells it from the palette (5) and ssh (4)
            // bars; the fill is the shared readable neutral.
            have_search = true;
            search_rect = edit.rect;
            push_input_bar(
                &mut quads,
                edit.rect,
                theme,
                theme.palette[3],
                &mut self.chrome_colors,
            );
            let label = format!(
                "{}{}_   {}",
                title_edit_prefix(&edit.label),
                edit.input,
                edit.hint
            );
            let label = fit_single_line_label(&label, overlay_label_cols(edit.rect.2, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer
                .set_size(Some(edit.rect.2), Some(edit.rect.3));
            // Same equality gate as the other chrome buffers. Only one arm of
            // this `if`/`else if` chain runs per frame, so a single cache is
            // enough (see `search_buffer_text`'s doc comment).
            if self.search_buffer_text != label {
                self.search_buffer.set_text(
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.search_buffer_text = label;
            }
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
            // The label cache lives and dies with `tab_buffers`.
            while self.tab_texts.len() < tabbar.segments.len() {
                self.tab_texts.push(String::new());
            }
            self.tab_texts.truncate(tabbar.segments.len());
            // Shrink the pool when tabs close, matching
            // `pane_buffers`/`settings_buffers`. Otherwise it stays at the
            // session's peak tab count (open 50, close to 5 → 50 shaped-text
            // buffers retained).
            self.tab_buffers.truncate(tabbar.segments.len());
            for (bi, s) in tabbar.segments.iter().enumerate() {
                let (_, _, title_w, title_h) = s.title_rect;
                // chars that fit: explicit title lane, ~cell_w each. The lane
                // excludes fixed tab chrome such as the close button, so
                // fitting and visual centering share the same coordinate space.
                //
                // The budget tracks the *actual* segment width, so a wide tab
                // shows its full title and ellipsizes only when it doesn't fit.
                // `fixed_w` reserves room for the non-title part of the format
                // (e.g. "{n}: "), so the title ellipsizes to keep the WHOLE
                // label inside the segment.
                let title = fit_tab_segment_title(
                    &s.title,
                    s.path.as_deref(),
                    s.idx,
                    &cfg.tab_format,
                    title_w,
                    cw,
                );
                let n = (s.idx + 1).to_string();
                let body =
                    kettle_config::template::fill(&cfg.tab_format, &[("n", &n), ("title", &title)]);
                // Title only — the ✕ is rendered separately below so we
                // can color it independently from the title text and
                // give it a real button chip background.
                let label = body;
                let buf = &mut self.tab_buffers[bi];
                buf.set_metrics(metrics);
                buf.set_size(Some(title_w), Some(title_h));
                // Re-shape only when the label actually changed.
                if self.tab_texts[bi] != label {
                    buf.set_text(
                        &label,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.tab_texts[bi] = label;
                }
                buf.shape_until_scroll(&mut self.font_system, false);
            }
            // Shared `✕` glyph buffer for every tab's close button.
            // Sized once; positioned per-tab via TextArea below.
            self.tab_close_buffer.set_metrics(metrics);
            self.tab_close_buffer
                .set_size(Some(tabbar.height), Some(tabbar.height));
            // Constant glyph, shaped once per font family.
            if self.tab_close_text != "✕" {
                self.tab_close_buffer.set_text(
                    "✕",
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.tab_close_text = "✕".into();
            }
            self.tab_close_buffer
                .shape_until_scroll(&mut self.font_system, false);
            // `+` button glyph.
            self.tabbar_buffer.set_metrics(metrics);
            self.tabbar_buffer
                .set_size(Some(tabbar.new_tab.2), Some(tabbar.height));
            // Constant glyph, shaped once per font family.
            if self.tabbar_text != NEW_TAB_PLUS_GLYPH {
                self.tabbar_buffer.set_text(
                    NEW_TAB_PLUS_GLYPH,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.tabbar_text = NEW_TAB_PLUS_GLYPH.into();
            }
            self.tabbar_buffer
                .shape_until_scroll(&mut self.font_system, false);
            // The `▾` dropdown arrow, shaped in its own buffer so it
            // lands inside `new_tab_menu` (left of `+`). Skipped when disabled.
            if tabbar.new_tab_menu.2 > 0.0 {
                self.new_tab_arrow_buffer.set_metrics(metrics);
                self.new_tab_arrow_buffer
                    .set_size(Some(tabbar.new_tab_menu.2), Some(tabbar.height));
                // Constant glyph, shaped once per font family.
                if self.new_tab_arrow_text != NEW_TAB_MENU_GLYPH {
                    self.new_tab_arrow_buffer.set_text(
                        NEW_TAB_MENU_GLYPH,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.new_tab_arrow_text = NEW_TAB_MENU_GLYPH.into();
                }
                self.new_tab_arrow_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            }
            // Overflow scroll-arrow glyphs, `‹` / `›` across a top or bottom
            // bar and `⌃` / `⌄` down a side one, each shaped in its own buffer
            // and sized to its button rect. Present only while the bar
            // overflows.
            let (back, forward) = scroll_arrow_glyphs(cfg.tab_bar_pos.is_vertical());
            if tabbar.scroll_left.2 > 0.0 {
                self.scroll_left_buffer.set_metrics(metrics);
                self.scroll_left_buffer
                    .set_size(Some(tabbar.scroll_left.2), Some(tabbar.scroll_left.3));
                if self.scroll_left_text != back {
                    self.scroll_left_buffer.set_text(
                        back,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.scroll_left_text = back.into();
                }
                self.scroll_left_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            }
            if tabbar.scroll_right.2 > 0.0 {
                self.scroll_right_buffer.set_metrics(metrics);
                self.scroll_right_buffer
                    .set_size(Some(tabbar.scroll_right.2), Some(tabbar.scroll_right.3));
                if self.scroll_right_text != forward {
                    self.scroll_right_buffer.set_text(
                        forward,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.scroll_right_text = forward.into();
                }
                self.scroll_right_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Status-bar text. Single buffer, single
        // line; sized to surface width so cosmic-text doesn't wrap
        // an overlong status string.
        if status.height > 0.0 && !status.text.is_empty() {
            self.status_bar_buffer.set_metrics(metrics);
            self.status_bar_buffer
                .set_size(Some(sw - 16.0), Some(status.height));
            // The status line changes at most once a second (the HH:MM:SS
            // clock), so don't re-shape it on every painted frame.
            if self.status_bar_text != status.text {
                self.status_bar_buffer.set_text(
                    &status.text,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.status_bar_text.clear();
                self.status_bar_text.push_str(&status.text);
            }
            self.status_bar_buffer
                .shape_until_scroll(&mut self.font_system, false);
        }

        // Ghostty `resize-overlay` parity: shape the transient size chip's
        // text ("120×40"). Drawn later in the menu pass so it sits above pane
        // content; the equality gate means a live resize only re-shapes when
        // the GRID size actually changed.
        if let Some((rcols, rrows)) = overlay.resize_overlay {
            let label = format!("{rcols}×{rrows}");
            // Metrics/size stay OUTSIDE the text gate: a DPI change can re-show
            // the chip with an UNCHANGED label, and gating them would leave the
            // glyphs shaped at the old monitor's scale. Both calls early-out
            // when unchanged, like the other chrome buffers.
            self.resize_overlay_buffer.set_metrics(metrics);
            self.resize_overlay_buffer
                .set_size(Some(sw), Some(ch * 2.0));
            if self.resize_overlay_text != label {
                self.resize_overlay_buffer.set_text(
                    &label,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.resize_overlay_text = label;
            }
            self.resize_overlay_buffer
                .shape_until_scroll(&mut self.font_system, false);
        }

        if let Some(toast) = &overlay.update_toast {
            let texts = [&toast.message, &toast.details, &toast.close];
            let rects = [
                toast.geometry.message,
                toast.geometry.details,
                toast.geometry.close,
            ];
            let text_cell_width = self.overlay_text_cell_width();
            for (i, (text, rect)) in texts.into_iter().zip(rects).enumerate() {
                let buffer = &mut self.update_toast_buffers[i];
                buffer.set_metrics(metrics);
                buffer.set_size(Some(rect.2), Some(rect.3));
                let (displayed, wrap) = toast_buffer_text(text, rect.2, text_cell_width, i == 0);
                buffer.set_wrap(wrap);
                if self.update_toast_text[i] != displayed {
                    buffer.set_text(
                        &displayed,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.update_toast_text[i] = displayed.into_owned();
                }
                buffer.shape_until_scroll(&mut self.font_system, false);
            }
        }

        // Context-menu row labels (one buffer per row, separators skipped)
        // + right-aligned shortcut hints (dropdown-parity).
        if let Some(menu) = &overlay.context_menu {
            while self.context_menu_buffers.len() < menu.rows.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.context_menu_buffers.push(b);
            }
            // The label cache lives and dies with `context_menu_buffers` but
            // grows off ITS OWN length (like `tab_texts`/`tab_buffers`), not the
            // buffer pool's. The font-family invalidation above clears only the
            // `_texts` caches, not the heavier buffer pools, so the two could
            // otherwise reach different lengths and panic on indexing below.
            while self.context_menu_texts.len() < menu.rows.len() {
                // Empty sentinel — same trick `hint_buffers`/`hint_texts` use —
                // so a fresh slot's first fill isn't skipped by the equality
                // gate below (unless the row's own label is also empty).
                self.context_menu_texts.push(String::new());
            }
            while self.context_menu_hint_buffers.len() < menu.rows.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.context_menu_hint_buffers.push(b);
            }
            while self.context_menu_hint_texts.len() < menu.rows.len() {
                self.context_menu_hint_texts.push(String::new());
            }
            // Shrink to the current row count so a small menu after a large
            // one (common with dynamic Lua menus) doesn't keep the peak's worth
            // of shaped-glyph buffers.
            self.context_menu_buffers.truncate(menu.rows.len());
            self.context_menu_texts.truncate(menu.rows.len());
            self.context_menu_hint_buffers.truncate(menu.rows.len());
            self.context_menu_hint_texts.truncate(menu.rows.len());
            // The App's clamped width is authoritative when present; all
            // render passes share this helper so text, chrome, and hit testing
            // cannot drift by a fractional cell after ellipsis.
            let panel_w = context_menu_panel_width(menu, cw);
            // Row height matches a comfortable click target (~28-32 px
            // on default cell metrics).
            let row_h = ch + 12.0;
            for (i, row) in menu.rows.iter().enumerate() {
                if row.separator {
                    continue;
                }
                let buf = &mut self.context_menu_buffers[i];
                buf.set_metrics(metrics);
                buf.set_size(Some(panel_w), Some(row_h));
                // Re-shape only when the row's label actually changed. An open
                // menu's rows are byte-stable, so blink/hover-driven redraws
                // need no reshape.
                if self.context_menu_texts[i] != row.label {
                    buf.set_text(
                        &row.label,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.context_menu_texts[i].clone_from(&row.label);
                }
                buf.shape_until_scroll(&mut self.font_system, false);
                if !row.hint.is_empty() {
                    let hb = &mut self.context_menu_hint_buffers[i];
                    hb.set_metrics(metrics);
                    hb.set_size(Some(panel_w), Some(row_h));
                    if self.context_menu_hint_texts[i] != row.hint {
                        hb.set_text(
                            &row.hint,
                            &Attrs::new().family(Family::Name(&family)),
                            Shaping::Advanced,
                            None,
                        );
                        self.context_menu_hint_texts[i].clone_from(&row.hint);
                    }
                    hb.shape_until_scroll(&mut self.font_system, false);
                }
            }
        }

        // Settings-overlay row buffers (one per display line).
        if let Some(set) = &overlay.settings {
            // `settings_display_lines` runs a `format!()` per display line, so
            // memoize its output against the last `SettingsOverlay` it was
            // computed from. The panel, like the context menu, sits open across
            // blink/hover-driven redraws with nothing actually changing.
            if self.settings_lines_source.as_ref() != Some(set) {
                self.settings_lines_cache = settings_display_lines(set);
                self.settings_lines_source = Some(set.clone());
            }
            let lines = &self.settings_lines_cache;
            while self.settings_buffers.len() < lines.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.settings_buffers.push(b);
            }
            // Grows off ITS OWN length, not `settings_buffers`', for the same
            // reason as `context_menu_texts` above. The font-family invalidation
            // clears only the `_texts` cache, so the two pools must each regrow
            // independently or indexing below could panic.
            while self.settings_texts.len() < lines.len() {
                self.settings_texts.push(String::new());
            }
            self.settings_buffers.truncate(lines.len());
            self.settings_texts.truncate(lines.len());
            // Panel width fits the content but never exceeds the surface
            // (so it stays usable in a small window); see the matching clamp
            // in the quad/area pass below.
            let panel_w = (settings_panel_cols(set, lines) * cw + 48.0).min((sw - 40.0).max(120.0));
            let row_h = ch + 6.0;
            for (i, line) in lines.iter().enumerate() {
                let buf = &mut self.settings_buffers[i];
                buf.set_metrics(metrics);
                let text_w = (panel_w - 32.0).max(cw);
                let fitted = settings_fit_display_line(set, i, line, panel_w, cw);
                buf.set_size(Some(text_w), Some(row_h));
                buf.set_wrap(Wrap::None);
                // Moving the focused row only changes 2 of N lines (the old/new
                // `▸` mark). This per-row gate spares the other N-2 rows a
                // reshape even on a frame where the memoization above DID
                // recompute `lines`.
                if self.settings_texts[i] != fitted {
                    buf.set_text(
                        &fitted,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.settings_texts[i] = fitted;
                }
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }

        if let Some(completion) = &overlay.completion
            && let Some(geometry) = completion_panel_geometry(completion, (cw, ch))
        {
            let accent = self.ui_accent(cfg, theme);
            let palette = self.chrome_colors.completion(theme, accent);
            let count = geometry.rows;
            while self.completion_buffers.len() < count {
                let mut buffer = TextBuffer::new(&mut self.font_system, metrics);
                buffer.set_wrap(Wrap::None);
                self.completion_buffers.push(buffer);
            }
            while self.completion_texts.len() < count {
                self.completion_texts.push(String::new());
            }
            while self.completion_spans.len() < count {
                self.completion_spans.push(None);
            }
            while self.completion_selected.len() < count {
                self.completion_selected.push(false);
            }
            while self.completion_emphasis_colors.len() < count {
                self.completion_emphasis_colors.push(Rgb::new(0, 0, 0));
            }
            while self.completion_description_buffers.len() < count {
                let mut buffer = TextBuffer::new(&mut self.font_system, metrics);
                buffer.set_wrap(Wrap::None);
                self.completion_description_buffers.push(buffer);
            }
            while self.completion_description_texts.len() < count {
                self.completion_description_texts.push(String::new());
            }
            self.completion_buffers.truncate(count);
            self.completion_texts.truncate(count);
            self.completion_spans.truncate(count);
            self.completion_selected.truncate(count);
            self.completion_emphasis_colors.truncate(count);
            self.completion_description_buffers.truncate(count);
            self.completion_description_texts.truncate(count);

            let count_source = completion_header_count(completion);
            let (header_columns, count_columns) =
                completion_header_columns(&geometry, &count_source);
            let header_line =
                fit_single_line_label(&completion_header_label(completion), header_columns);
            let count_line = fit_single_line_label(&count_source, count_columns);
            self.completion_header_buffer.set_metrics(metrics);
            self.completion_header_buffer.set_size(
                Some((header_columns as f32 * cw).max(1.0)),
                Some(geometry.header.3),
            );
            if self.completion_header_text != header_line {
                self.completion_header_buffer.set_text(
                    &header_line,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.completion_header_text = header_line;
            }
            self.completion_header_buffer
                .shape_until_scroll(&mut self.font_system, false);
            self.completion_count_buffer.set_metrics(metrics);
            self.completion_count_buffer.set_size(
                Some((count_columns as f32 * cw).max(1.0)),
                Some(geometry.header.3),
            );
            if self.completion_count_text != count_line {
                self.completion_count_buffer.set_text(
                    &count_line,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.completion_count_text = count_line;
            }
            self.completion_count_buffer
                .shape_until_scroll(&mut self.font_system, false);

            for line_index in 0..count {
                let candidate_index = geometry.first + line_index;
                let candidate = &completion.candidates[candidate_index];
                let selected = completion.selected == Some(candidate_index);
                // Completion labels are dominated by paths, whose tail is the
                // discriminating part; keep both ends.
                let line = middle_ellipsis(&candidate.label, geometry.label_columns);
                let span = completion
                    .token
                    .as_deref()
                    .and_then(|token| completion_match_span(&line, token));
                let emphasis_color = if selected {
                    palette.selected_emphasis
                } else {
                    palette.emphasis
                };
                let buffer = &mut self.completion_buffers[line_index];
                buffer.set_metrics(metrics);
                buffer.set_size(Some(geometry.label_w), Some(geometry.row_h));
                if self.completion_texts[line_index] != line
                    || self.completion_spans[line_index] != span
                    || self.completion_selected[line_index] != selected
                    || (span.is_some()
                        && self.completion_emphasis_colors[line_index] != emphasis_color)
                {
                    let base = Attrs::new().family(Family::Name(&family));
                    match span {
                        // Only the matched run carries an explicit color. The
                        // text around it keeps the text area's default, so the
                        // selected/unselected label color stays in one place.
                        Some((start, end)) => {
                            let emphasis = base.clone().color(gc(emphasis_color));
                            buffer.set_rich_text(
                                [
                                    (&line[..start], base.clone()),
                                    (&line[start..end], emphasis),
                                    (&line[end..], base.clone()),
                                ],
                                &base,
                                Shaping::Advanced,
                                None,
                            );
                        }
                        None => buffer.set_text(&line, &base, Shaping::Advanced, None),
                    }
                    self.completion_texts[line_index] = line;
                    self.completion_spans[line_index] = span;
                    self.completion_selected[line_index] = selected;
                    self.completion_emphasis_colors[line_index] = emphasis_color;
                }
                buffer.shape_until_scroll(&mut self.font_system, false);

                let description =
                    fit_single_line_label(&candidate.description, geometry.description_columns);
                let buffer = &mut self.completion_description_buffers[line_index];
                buffer.set_metrics(metrics);
                buffer.set_size(Some(geometry.description_w.max(1.0)), Some(geometry.row_h));
                if self.completion_description_texts[line_index] != description {
                    buffer.set_text(
                        &description,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.completion_description_texts[line_index] = description;
                }
                buffer.shape_until_scroll(&mut self.font_system, false);
            }
        }

        let notifications = notification_geometry(
            overlay.update_toast.as_ref().map(|toast| toast.geometry),
            overlay.media_paste_receipt.as_ref(),
            overlay.completion.as_ref(),
            (cw, ch),
            self.overlay_text_cell_width(),
            self.overlay_text_line_height(),
        );
        if overlay.confirm_dialog.is_none()
            && let Some(receipt) = &overlay.media_paste_receipt
            && let Some(geometry) = notifications.receipt
        {
            let (title, detail) =
                media_paste_receipt_text(receipt, &geometry, self.overlay_text_cell_width());
            self.media_receipt_title_buffer.set_metrics(metrics);
            self.media_receipt_title_buffer
                .set_size(Some(geometry.title_rect.2), Some(geometry.title_rect.3));
            if self.media_receipt_title_text != title {
                self.media_receipt_title_buffer.set_text(
                    &title,
                    &Attrs::new()
                        .family(Family::Name(&family))
                        .weight(Weight::BOLD),
                    Shaping::Advanced,
                    None,
                );
                self.media_receipt_title_text = title;
            }
            self.media_receipt_title_buffer
                .shape_until_scroll(&mut self.font_system, false);

            self.media_receipt_dismiss_buffer.set_metrics(metrics);
            self.media_receipt_dismiss_buffer
                .set_size(Some(geometry.dismiss_rect.2), Some(geometry.dismiss_rect.3));
            self.media_receipt_dismiss_buffer
                .shape_until_scroll(&mut self.font_system, false);

            if matches!(receipt.kind, MediaPasteReceiptKind::Video { .. }) {
                self.media_receipt_badge_buffer.set_metrics(metrics);
                self.media_receipt_badge_buffer.set_size(
                    geometry.preview_rect.map(|rect| rect.2),
                    geometry.preview_rect.map(|rect| rect.3),
                );
                self.media_receipt_badge_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            }

            if let Some(detail_rect) = geometry.detail_rect {
                self.media_receipt_detail_buffer.set_metrics(metrics);
                self.media_receipt_detail_buffer
                    .set_size(Some(detail_rect.2), Some(detail_rect.3));
                if self.media_receipt_detail_text != detail {
                    self.media_receipt_detail_buffer.set_text(
                        &detail,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.media_receipt_detail_text = detail;
                }
                self.media_receipt_detail_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            } else {
                self.media_receipt_detail_text.clear();
            }
        } else {
            self.media_receipt_title_text.clear();
            self.media_receipt_detail_text.clear();
        }

        // Each shown lane's text, kept by pane; a lane gone takes its text.
        self.lane_texts
            .retain(|pane, _| overlay.media_lanes.iter().any(|lane| lane.pane == *pane));
        for lane in &overlay.media_lanes {
            let Some(geometry) = media_lane_geometry_scaled(
                lane,
                (cw, ch),
                (self.overlay_text_cell_width(), self.metrics.line_height),
                self.scale,
            ) else {
                continue;
            };
            let glyph_width = self.overlay_text_cell_width();
            let font_system = &mut self.font_system;
            self.lane_texts
                .entry(lane.pane)
                .or_insert_with(|| media_lane::LaneText::new(font_system, metrics))
                .shape(font_system, metrics, &family, lane, &geometry, glyph_width);
        }

        // Quick-select hint label glyphs (one buffer per label).
        if !overlay.hint_labels.is_empty() {
            while self.hint_buffers.len() < overlay.hint_labels.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.hint_buffers.push(b);
                // A fresh buffer holds no shaped text; the empty sentinel
                // keeps the equality gate below from skipping its first fill.
                self.hint_texts.push(String::new());
            }
            // Quick-select labels every visible link, so
            // densities swing widely (50 → 5 → 100); shrink the pool to the
            // current label count instead of pinning it at the peak.
            self.hint_buffers.truncate(overlay.hint_labels.len());
            self.hint_texts.truncate(overlay.hint_labels.len());
            for (i, hint) in overlay.hint_labels.iter().enumerate() {
                let n = hint.label.chars().count().max(1) as f32;
                let buf = &mut self.hint_buffers[i];
                // Metrics/size stay outside the text gate (same DPI-change
                // hazard as the resize chip above); both early-out when
                // unchanged. The gate spares a ~100-label reshape on every
                // blink-driven redraw while the overlay is open.
                buf.set_metrics(metrics);
                buf.set_size(Some(n * cw + 2.0), Some(ch));
                if self.hint_texts[i] != hint.label {
                    buf.set_text(
                        &hint.label,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.hint_texts[i].clone_from(&hint.label);
                }
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }
        if let Some(preedit) = &overlay.ime_preedit {
            self.ime_buffer.set_metrics(metrics);
            self.ime_buffer.set_size(None, Some(ch));
            if self.ime_text != preedit.text {
                self.ime_buffer.set_text(
                    &preedit.text,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.ime_text.clone_from(&preedit.text);
            }
            self.ime_buffer
                .shape_until_scroll(&mut self.font_system, false);
        } else {
            self.ime_text.clear();
        }
        let focus_origin = panes.iter().find(|p| p.focused).map(|p| p.terminal);
        let ime_geometry = focus_origin
            .zip(overlay.ime_preedit.as_ref())
            .map(|(pane, preedit)| {
                let shaped_width = self
                    .ime_buffer
                    .layout_runs()
                    .map(|run| run.line_w)
                    .fold(0.0_f32, f32::max);
                let rect = ime_preedit_rect(
                    preedit,
                    pane_grid_origin(pane, (pad_x, pad_y)),
                    (cw, ch),
                    (shaped_width, metrics.line_height),
                );
                let bounds = push_ime_preedit_quads(
                    &mut ime_q,
                    rect,
                    pane,
                    theme.selection_background,
                    cfg.search_background.unwrap_or(theme.palette[3]),
                );
                (rect, bounds)
            });

        // Assemble text areas (panes + tab bar + search).
        self.viewport.update(
            &self.gpu.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        // Theme foreground — used by tab bar text and other chrome below
        // (where there's no specific pane to take an OSC 10 override from).
        let fg = theme.foreground;
        let mut areas: Vec<TextArea> = Vec::with_capacity(panes.len() + 2);
        // Menu text lives in its own areas vec, prepared by a dedicated
        // `menu_text_renderer.prepare(...)` after the main
        // `text_renderer.prepare(...)`. In one shared pass the menu's bg /
        // shadow / border / highlight would cover its text; the split gives
        // the menu its own bg→border→highlight→text pipeline at the end of the
        // render pass.
        // Pre-size for the menu / settings-overlay rows it collects.
        let mut menu_areas: Vec<TextArea> = Vec::with_capacity(48);
        let mut ime_areas: Vec<TextArea> = Vec::with_capacity(1);
        if let Some(toast) = &overlay.update_toast
            && let Some(geometry) = notifications.toast
        {
            let text_line_height = self.overlay_text_line_height();
            let (x, y, w, h) = geometry.rect;
            let alpha = toast.opacity.clamp(0.0, 1.0);
            let background = theme.palette[0];
            let foreground = self.chrome_colors.text(theme.foreground, background);
            menu_q.push(rect(x, y, w, h, background, alpha));
            let accent = self.ui_accent(cfg, theme);
            let accent = self.chrome_colors.icon(accent, background);
            menu_q.push(rect(x, y, w, 1.0, accent, alpha));
            if let Some(index) = toast.focused.filter(|i| *i == 1 || *i == 2) {
                let (bx, by, bw, bh) = if index == 1 {
                    geometry.details
                } else {
                    geometry.close
                };
                for (x, y, w, h) in [
                    (bx, by, bw, 2.0),
                    (bx, by + bh - 2.0, bw, 2.0),
                    (bx, by, 2.0, bh),
                    (bx + bw - 2.0, by, 2.0, bh),
                ] {
                    menu_q.push(rect(x, y, w, h, accent, alpha));
                }
            }

            for (i, area) in [geometry.message, geometry.details, geometry.close]
                .into_iter()
                .enumerate()
            {
                let (ax, ay, aw, ah) = area;
                menu_areas.push(TextArea {
                    buffer: &self.update_toast_buffers[i],
                    left: ax,
                    top: if i == 0 {
                        ay + (ah
                            - self.update_toast_buffers[i].layout_runs().count().max(1) as f32
                                * text_line_height)
                            .max(0.0)
                            * 0.5
                    } else {
                        ay + (ah - text_line_height).max(0.0) * 0.5
                    },
                    scale: 1.0,
                    bounds: TextBounds {
                        left: ax.ceil() as i32,
                        top: ay.ceil() as i32,
                        right: (ax + aw).floor() as i32,
                        bottom: (ay + ah).floor() as i32,
                    },
                    default_color: GColor::rgba(
                        foreground.r,
                        foreground.g,
                        foreground.b,
                        (alpha * 255.0).round() as u8,
                    ),
                    custom_glyphs: &[],
                });
            }
        }
        // Ghostty parity: the transient resize chip, centered and drawn in the
        // menu pass (last) so it reads over any pane content.
        if let Some((rcols, rrows)) = overlay.resize_overlay {
            let label_cells = format!("{rcols}×{rrows}").chars().count() as f32;
            let pad = 14.0_f32;
            let chip_w = label_cells * cw + pad * 2.0;
            let chip_h = ch + pad;
            let cx = (sw - chip_w) / 2.0;
            let cy = (sh - chip_h) / 2.0;
            menu_q.push(rect(cx, cy, chip_w, chip_h, theme.palette[0], 0.92));
            // 1px accent outline so the chip reads on same-color content.
            let acc = cfg.accent_color.unwrap_or(theme.palette[4]);
            menu_q.push(rect(cx, cy, chip_w, 1.0, acc, 1.0));
            menu_q.push(rect(cx, cy + chip_h - 1.0, chip_w, 1.0, acc, 1.0));
            menu_q.push(rect(cx, cy, 1.0, chip_h, acc, 1.0));
            menu_q.push(rect(cx + chip_w - 1.0, cy, 1.0, chip_h, acc, 1.0));
            menu_areas.push(TextArea {
                buffer: &self.resize_overlay_buffer,
                left: cx + pad,
                top: cy + pad / 2.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: cx as i32,
                    top: cy as i32,
                    right: (cx + chip_w) as i32,
                    bottom: (cy + chip_h) as i32,
                },
                default_color: GColor::rgb(
                    theme.foreground.r,
                    theme.foreground.g,
                    theme.foreground.b,
                ),
                custom_glyphs: &[],
            });
        }
        if overlay.confirm_dialog.is_none()
            && let Some(receipt) = &overlay.media_paste_receipt
            && let Some(geometry) = notifications.receipt
        {
            let accent = self.ui_accent(cfg, theme);
            let palette = self.chrome_colors.completion(theme, accent);
            let (x, y, width, height) = geometry.rect;
            for (offset, alpha) in [(1.0_f32, 0.28_f32), (2.0, 0.16), (3.0, 0.08)] {
                menu_q.push(rect(
                    x + offset,
                    y + offset,
                    width,
                    height,
                    Rgb::new(0, 0, 0),
                    alpha,
                ));
            }
            // The receipt is also captured as a standalone review artifact.
            // Keep its surface opaque so terminal text beneath the card cannot
            // bleed into that crop.
            menu_q.push(rect(x, y, width, height, palette.panel_bg, 1.0));
            let rail_color = if receipt.remote {
                self.chrome_colors.text(theme.palette[3], palette.panel_bg)
            } else {
                palette.emphasis
            };
            menu_q.push(rect(x + 1.0, y + 1.0, 3.0, height - 2.0, rail_color, 1.0));
            menu_q.push(rect(x, y, width, 1.0, palette.border, 1.0));
            menu_q.push(rect(x, y + height - 1.0, width, 1.0, palette.border, 1.0));
            menu_q.push(rect(x + width - 1.0, y, 1.0, height, palette.border, 1.0));

            let dismiss = geometry.dismiss_rect;
            menu_q.push(rect(
                dismiss.0,
                dismiss.1,
                dismiss.2,
                dismiss.3,
                palette.divider,
                1.0,
            ));
            menu_q.push(rect(
                dismiss.0,
                dismiss.1,
                dismiss.2,
                1.0,
                palette.border,
                1.0,
            ));

            if let Some(preview_rect) = geometry.preview_rect {
                // A quiet matte keeps transparent screenshots legible and
                // gives the generic video poster a deliberate frame.
                menu_q.push(rect(
                    preview_rect.0 - 1.0,
                    preview_rect.1 - 1.0,
                    preview_rect.2 + 2.0,
                    preview_rect.3 + 2.0,
                    theme.background,
                    1.0,
                ));
                if let Some(image) = receipt.image.as_ref() {
                    media_receipt_items.push(imgpipe::ImageItem::placement(
                        [
                            preview_rect.0,
                            preview_rect.1,
                            preview_rect.2,
                            preview_rect.3,
                        ],
                        image.clone(),
                        None,
                        None,
                        [x, y, width, height],
                    ));
                }
                if matches!(receipt.kind, MediaPasteReceiptKind::Video { .. }) {
                    let badge = 36.0_f32
                        .min(preview_rect.2 * 0.45)
                        .min(preview_rect.3 * 0.68);
                    let badge_rect = (
                        preview_rect.0 + (preview_rect.2 - badge) * 0.5,
                        preview_rect.1 + (preview_rect.3 - badge) * 0.5,
                        badge,
                        badge,
                    );
                    if receipt.image.is_none() {
                        menu_q.push(rect(
                            badge_rect.0,
                            badge_rect.1,
                            badge_rect.2,
                            badge_rect.3,
                            palette.divider,
                            1.0,
                        ));
                    }
                    let badge_left = badge_rect.0
                        + (badge_rect.2 - self.overlay_text_cell_width()).max(0.0) * 0.5;
                    let badge_top =
                        badge_rect.1 + (badge_rect.3 - self.metrics.line_height).max(0.0) * 0.5;
                    // Four dark copies form a one-pixel outline, keeping the
                    // play glyph legible over both bright and dark posters.
                    for (dx, dy) in [(-1.0_f32, 0.0_f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                        menu_areas.push(TextArea {
                            buffer: &self.media_receipt_badge_buffer,
                            left: badge_left + dx,
                            top: badge_top + dy,
                            scale: 1.0,
                            bounds: text_bounds_for_rect(badge_rect),
                            default_color: GColor::rgb(0, 0, 0),
                            custom_glyphs: &[],
                        });
                    }
                    menu_areas.push(TextArea {
                        buffer: &self.media_receipt_badge_buffer,
                        left: badge_left,
                        top: badge_top,
                        scale: 1.0,
                        bounds: text_bounds_for_rect(badge_rect),
                        default_color: GColor::rgb(255, 255, 255),
                        custom_glyphs: &[],
                    });
                }
            }

            let title = geometry.title_rect;
            menu_areas.push(TextArea {
                buffer: &self.media_receipt_title_buffer,
                left: title.0,
                top: title.1,
                scale: 1.0,
                bounds: text_bounds_for_rect(title),
                default_color: gc(palette.label),
                custom_glyphs: &[],
            });
            if let Some(detail) = geometry.detail_rect {
                menu_areas.push(TextArea {
                    buffer: &self.media_receipt_detail_buffer,
                    left: detail.0,
                    top: detail.1,
                    scale: 1.0,
                    bounds: text_bounds_for_rect(detail),
                    default_color: gc(palette.description),
                    custom_glyphs: &[],
                });
            }
            menu_areas.push(TextArea {
                buffer: &self.media_receipt_dismiss_buffer,
                left: dismiss.0 + (dismiss.2 - self.overlay_text_cell_width()).max(0.0) * 0.5,
                top: dismiss.1 + (dismiss.3 - self.metrics.line_height).max(0.0) * 0.5,
                scale: 1.0,
                bounds: text_bounds_for_rect(dismiss),
                default_color: gc(palette.label),
                custom_glyphs: &[],
            });
        }
        // The preview lanes: each panel fills its lane, opaque, with a rule
        // along the edge it shares with the terminal. Drawn in their own layer
        // above the terminal and below every overlay, so a menu or dialog
        // covers a lane's text as well as its frame.
        let mut transport_surfaces = Vec::new();
        let mut lane_areas: Vec<TextArea> = Vec::with_capacity(overlay.media_lanes.len() * 8);
        // Inside the pane's focus border, as the terminal's backdrop is.
        let lane_border = if cfg.handle_size < 0 {
            1.0
        } else {
            cfg.handle_size as f32
        };
        for lane in &overlay.media_lanes {
            let accent = self.ui_accent(cfg, theme);
            let palette = self.chrome_colors.completion(theme, accent);
            let (x, y, width, height) = lane.rect;
            let inner = if lane.pin_label.is_some() {
                lane.rect
            } else {
                (
                    x + lane_border,
                    y + lane_border,
                    (width - 2.0 * lane_border).max(0.0),
                    (height - 2.0 * lane_border).max(0.0),
                )
            };
            lane_q.push(rect(
                inner.0,
                inner.1,
                inner.2,
                inner.3,
                palette.panel_bg,
                1.0,
            ));
            lane_q.push(rect(inner.0, inner.1, inner.2, 1.0, palette.border, 1.0));
            let Some(geometry) = media_lane_geometry_scaled(
                lane,
                (cw, ch),
                (self.overlay_text_cell_width(), self.metrics.line_height),
                self.scale,
            ) else {
                continue;
            };
            let transport_paint = media_transport::paint(
                lane,
                &geometry,
                palette,
                overlay.media_pointer,
                overlay.media_scrubbing,
                self.scale,
            );
            if let Some(paint) = transport_paint {
                transport_surfaces.extend(paint.shapes);
            }
            for button in [
                geometry.play,
                geometry.sound,
                geometry.theater,
                geometry.fullscreen,
                geometry.previous,
                geometry.next,
                geometry.open_outside,
                geometry.mode,
                geometry.canvas,
                geometry.copy,
                geometry.reload,
                geometry.zoom_out,
                geometry.zoom_in,
                geometry.zoom_fit,
                geometry.page_previous,
                geometry.page_next,
                Some(geometry.toggle),
                Some(geometry.close),
            ]
            .into_iter()
            .flatten()
            {
                if geometry
                    .transport
                    .is_some_and(|transport| button.1 >= transport.1)
                {
                    continue;
                }
                lane_q.push(rect(
                    button.0,
                    button.1,
                    button.2,
                    button.3,
                    palette.divider,
                    1.0,
                ));
            }
            if let Some((fx, fy, fw, fh)) = lane.focused_control {
                let line = (2.0 * self.scale).max(1.0).min(fw / 2.0).min(fh / 2.0);
                for (x, y, w, h) in [
                    (fx, fy, fw, line),
                    (fx, fy + fh - line, fw, line),
                    (fx, fy, line, fh),
                    (fx + fw - line, fy, line, fh),
                ] {
                    lane_q.push(rect(x, y, w, h, palette.emphasis, 1.0));
                }
            }
            // A short grip in the middle of the edge that resizes the lane
            // says it can be dragged.
            if let Some((rx, ry, rw, rh)) = geometry.resize {
                let (long, thick) = (32.0 * self.scale, (2.0 * self.scale).max(1.0));
                let grip = if rw > rh {
                    (rx + (rw - long) / 2.0, ry + (rh - thick) / 2.0, long, thick)
                } else {
                    (rx + (rw - thick) / 2.0, ry + (rh - long) / 2.0, thick, long)
                };
                lane_q.push(rect(grip.0, grip.1, grip.2, grip.3, palette.divider, 1.0));
            }
            // A zoomed image reaches past the content, which clips it; its
            // canvas is drawn behind the part that shows. A preview's frame
            // is part of its sheet, in the item's place.
            let pixels = match &lane.frame {
                Some(frame) => Some((&frame.sheet, Some(frame.source))),
                None => lane.image.as_ref().map(|image| (image, None)),
            };
            if let (Some(image_rect), Some(shown), Some((image, source))) =
                (geometry.image, geometry.shown(), pixels)
            {
                let (ix, iy, iw, ih) = shown;
                match lane.canvas {
                    MediaCanvas::Theme => {
                        lane_q.push(rect(ix, iy, iw, ih, theme.background, 1.0));
                    }
                    MediaCanvas::White => {
                        lane_q.push(rect(ix, iy, iw, ih, Rgb::new(255, 255, 255), 1.0));
                    }
                    MediaCanvas::Checker => {
                        lane_q.push(rect(ix, iy, iw, ih, Rgb::new(0xee, 0xee, 0xee), 1.0));
                        let square = (8.0 * self.scale).round().max(4.0);
                        for (sx, sy, sw, sh) in media_lane::checker_squares(shown, square) {
                            lane_q.push(rect(sx, sy, sw, sh, Rgb::new(0xcc, 0xcc, 0xcc), 1.0));
                        }
                    }
                }
                // The sharper pixels when they cover what is in view, else
                // the lane's own: one image a lane either way.
                let (rect, pixels) = match (geometry.tile, lane.tile.as_ref()) {
                    (Some(rect), Some(tile)) => (rect, &tile.image),
                    _ => (image_rect, image),
                };
                let content = geometry.content;
                let source = source.map(|(x, y, width, height)| kettle_core::ImageSourceRect {
                    x: x as f32,
                    y: y as f32,
                    width: width as f32,
                    height: height as f32,
                });
                lane_items.push(imgpipe::ImageItem::placement(
                    [rect.0, rect.1, rect.2, rect.3],
                    pixels.clone(),
                    source,
                    None,
                    [content.0, content.1, content.2, content.3],
                ));
            }
            // A muted video's sound button: its glyph struck through.
            if let (Some(MediaLaneSound::Muted), Some(button)) = (lane.sound, geometry.sound) {
                let thick = (1.5 * self.scale).round().max(1.0);
                let glyph = self.overlay_text_cell_width();
                lane_q.push(rect(
                    button.0 + ((button.2 - glyph) / 2.0).max(0.0).round(),
                    button.1 + ((button.3 - thick) / 2.0).round(),
                    glyph.min(button.2).round(),
                    thick,
                    palette.description,
                    1.0,
                ));
            }
            // A playing video's bar: the track, the part played in the
            // accent, and a knob where the video is.
            if let (Some(bar), Some(track)) = (&lane.scrubber, geometry.scrubber)
                && geometry.transport.is_none()
            {
                let thick = (3.0 * self.scale).round().max(2.0);
                let top = track.1 + ((track.3 - thick) / 2.0).round();
                let along = if bar.duration_ms == 0 {
                    0.0
                } else {
                    (bar.position_ms.min(bar.duration_ms) as f64 / bar.duration_ms as f64) as f32
                };
                let played = (track.2 * along).round();
                lane_q.push(rect(track.0, top, track.2, thick, palette.divider, 1.0));
                lane_q.push(rect(track.0, top, played, thick, palette.emphasis, 1.0));
                let knob = (thick * 2.0).max(4.0);
                let knob_height = (track.3 * 0.6).round().max(thick);
                lane_q.push(rect(
                    media_lane::knob_left(track, played, knob),
                    track.1 + ((track.3 - knob_height) / 2.0).round(),
                    knob,
                    knob_height,
                    palette.emphasis,
                    1.0,
                ));
            }
            // Source rows sit on the pane's background, as a terminal's do.
            if lane.mode == MediaLaneMode::Source && geometry.full {
                let (sx, sy, sw, sh) = geometry.image_area;
                lane_q.push(rect(sx, sy, sw, sh, theme.background, 1.0));
            }
            if let Some(text) = self.lane_texts.get(&lane.pane) {
                lane_areas.extend(text.areas(
                    &geometry,
                    lane.collapsed || !geometry.full,
                    lane.mode,
                    lane.page,
                    lane.play,
                    lane.sound,
                    (
                        gc(palette.label),
                        gc(palette.description),
                        gc(palette.emphasis),
                    ),
                    self.overlay_text_cell_width(),
                    self.metrics.line_height,
                ));
            }
        }
        if let Some(completion) = &overlay.completion
            && let Some(geometry) = completion_panel_geometry(completion, (cw, ch))
        {
            let (x, y, width, height) = geometry.rect;
            let accent = self.ui_accent(cfg, theme);
            let palette = self.chrome_colors.completion(theme, accent);
            let border = COMPLETION_BORDER;
            // Three restrained steps of pure black. Enough depth to lift the
            // detached card off similar-colored terminal content without a
            // blurred halo, and cheap enough to stay in the quad list.
            for (offset, alpha) in [(1.0_f32, 0.30_f32), (2.0, 0.18), (3.0, 0.10)] {
                menu_q.push(rect(
                    x + offset,
                    y + offset,
                    width,
                    height,
                    Rgb::new(0, 0, 0),
                    alpha,
                ));
            }
            menu_q.push(rect(x, y, width, height, palette.panel_bg, 1.0));
            menu_q.push(rect(x, y, width, border, palette.border, 1.0));
            menu_q.push(rect(
                x,
                y + height - border,
                width,
                border,
                palette.border,
                1.0,
            ));
            menu_q.push(rect(x, y, border, height, palette.border, 1.0));
            menu_q.push(rect(
                x + width - border,
                y,
                border,
                height,
                palette.border,
                1.0,
            ));
            let (_, list_y, _, list_h) = geometry.list_rect();
            let scroll = completion_scroll_thumb(completion, &geometry);
            // The indicator stays continuously visible, so the selection
            // surface stops short of its lane instead of painting over it.
            let scroll_inset = if scroll.is_some() {
                COMPLETION_SCROLL_W
            } else {
                0.0
            };
            if let Some(divider_x) = geometry.divider_x {
                menu_q.push(rect(divider_x, list_y, 1.0, list_h, palette.divider, 1.0));
            }
            let text_dy = ((geometry.row_h - ch) * 0.5).round();
            let clip = |left: f32, right: f32| TextBounds {
                left: left.floor() as i32,
                top: y.floor() as i32,
                right: right.ceil() as i32,
                bottom: (y + height).ceil() as i32,
            };
            for row in 0..geometry.rows {
                let candidate_index = geometry.first + row;
                let row_y = geometry.list_top + row as f32 * geometry.row_h;
                let selected = completion.selected == Some(candidate_index);
                push_completion_selection_quads(
                    &mut menu_q,
                    selected,
                    &geometry,
                    row_y,
                    scroll_inset,
                    &palette,
                );
                menu_areas.push(TextArea {
                    buffer: &self.completion_buffers[row],
                    left: geometry.label_x,
                    top: row_y + text_dy,
                    scale: 1.0,
                    bounds: clip(geometry.label_x, geometry.label_x + geometry.label_w),
                    default_color: gc(if selected {
                        palette.selected_label
                    } else {
                        palette.label
                    }),
                    custom_glyphs: &[],
                });
                menu_areas.push(TextArea {
                    buffer: &self.completion_description_buffers[row],
                    left: geometry.description_x,
                    top: row_y + text_dy,
                    scale: 1.0,
                    bounds: clip(
                        geometry.description_x,
                        geometry.description_x + geometry.description_w,
                    ),
                    default_color: gc(if selected {
                        palette.selected_description
                    } else {
                        palette.description
                    }),
                    custom_glyphs: &[],
                });
            }
            if let Some((thumb_y, thumb_h)) = scroll {
                let track_x = (x + width - border - COMPLETION_SCROLL_W).round();
                menu_q.push(rect(
                    track_x,
                    list_y,
                    COMPLETION_SCROLL_W,
                    list_h,
                    palette.scroll_track,
                    1.0,
                ));
                menu_q.push(rect(
                    track_x,
                    thumb_y,
                    COMPLETION_SCROLL_W,
                    thumb_h,
                    palette.scroll_thumb,
                    1.0,
                ));
            }
            let (header_columns, count_columns) =
                completion_header_columns(&geometry, &completion_header_count(completion));
            let header_dy = ((geometry.header.3 - ch) * 0.5).round();
            let header_x = geometry.label_x;
            let count_x = x + width - (COMPLETION_PAD_COLUMNS + count_columns) as f32 * cw;
            menu_areas.push(TextArea {
                buffer: &self.completion_header_buffer,
                left: header_x,
                top: geometry.header.1 + header_dy,
                scale: 1.0,
                bounds: clip(header_x, header_x + header_columns as f32 * cw),
                default_color: gc(palette.header),
                custom_glyphs: &[],
            });
            menu_areas.push(TextArea {
                buffer: &self.completion_count_buffer,
                left: count_x,
                top: geometry.header.1 + header_dy,
                scale: 1.0,
                bounds: clip(count_x, count_x + count_columns as f32 * cw),
                default_color: gc(palette.header),
                custom_glyphs: &[],
            });
        }
        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.terminal;
            let grid_origin = pane_grid_origin(pv.terminal, (pad_x, pad_y));
            // Per-pane OSC 10 default-fg: glyphon's `default_color` is the
            // fallback when a span lacks an explicit color. Almost every
            // cell does carry an explicit color via `Attrs::color`, but
            // whitespace / IME composition / chrome strings ride the
            // default. Matches the OSC 11 chrome path —
            // engine override (Colors[256]) wins, theme is fallback.
            let pane_fg = pv.snap.colors[256]
                .map(|c| Rgb::new(c.r, c.g, c.b))
                .unwrap_or(theme.foreground);
            // Glyphon's color shader ignores foreground tint. Inactive legacy
            // panes use our pipeline at their original layout positions so
            // bitmap dimming and ordinary text contrast remain independent.
            if cfg.text_renderer == TextRendererMode::Legacy {
                if pane_inactive_amount(pv, panes.len(), cfg) > 0.0 {
                    continue;
                }
                areas.push(TextArea {
                    buffer: &self.pane_buffers[i],
                    left: grid_origin.0,
                    // The legacy and cell-locked paths share the exact same
                    // title-position-aware terminal-grid origin.
                    top: grid_origin.1,
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
        }
        // Render per-pane titlebar chrome for split panes. Push TextAreas for the
        // `pane_titlebar_buffers` shaped earlier in this pass.
        if pane_titlebar_h > 0.0 {
            for (i, pv) in panes.iter().enumerate() {
                let (rx, ry, rw, rh) = pv.rect;
                // Matching fg variant for the three states, derived from the
                // theme so the title text stays readable + on-theme. The
                // focused and broadcast bars default to the window accent (see
                // `pick_titlebar_bg`), so their text is the dark
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
                let background = pick_titlebar_bg(
                    cfg,
                    theme,
                    self.ui_accent(cfg, theme),
                    pv.focused,
                    tabbar.broadcast,
                );
                let fg = self.chrome_colors.text(fg, background);
                // Text-area position mirrors the
                // titlebar bar's y-position so the title text follows
                // the bar to the bottom when title_at_bottom is
                // true. 2px top padding matches the titlebar buffer population above.
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
                        // Clamp to ≥0 so a pane flush against the
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
                let (tx, ty_px, tw, th) = s.title_rect;
                let tab_background = if s.active {
                    default_bg
                } else {
                    chrome_strip_bg
                };
                let title_fg = self.chrome_colors.text(fg, tab_background);
                let label_cols = display_width(&self.tab_texts[bi]) as f32;
                let label_w = label_cols * cw.max(1.0);
                let centered_left = tx + ((tw - label_w) * 0.5).max(0.0);
                areas.push(TextArea {
                    buffer: &self.tab_buffers[bi],
                    left: centered_left,
                    top: ty_px + ((th - ch) * 0.5).max(0.0),
                    scale: 1.0,
                    bounds: TextBounds {
                        left: tx as i32,
                        top: ty_px as i32,
                        right: (tx + tw) as i32,
                        bottom: (ty_px + th) as i32,
                    },
                    default_color: gc(title_fg),
                    custom_glyphs: &[],
                });
                // `✕` close glyph — separate text area so we can color
                // it independently of the title. Bright on hover, dim
                // at rest (still readable, but visually subordinate to
                // the title text). Centered inside `seg.close`.
                //
                // Skipped when cfg.close_button_on_tab is
                // false (matches the quad branch above).
                if !cfg.close_button_on_tab {
                    continue;
                }
                let (cx, cy, ccw, cch) = s.close;
                let hovered = tabbar.hovered_close_idx == Some(s.idx);
                let close_fg = if hovered {
                    // Dark glyph (theme.cursor_text) on the theme-red
                    // close chip (palette[1]) — higher contrast than white on the
                    // Mocha pink-red, and tracks the theme instead of a literal.
                    theme.cursor_text
                } else {
                    // Rest: dim chrome — readable but secondary.
                    theme.palette[8]
                };
                let close_background = self.chrome_colors.close_background(
                    theme,
                    default_bg,
                    chrome_strip_bg,
                    (s.active, hovered),
                    self.config.format.is_srgb(),
                );
                let close_fg = self.chrome_colors.icon(close_fg, close_background);
                let (close_left, close_top) = centered_text_origin((cx, cy, ccw, cch), (cw, ch));
                areas.push(TextArea {
                    buffer: &self.tab_close_buffer,
                    left: close_left,
                    top: close_top,
                    scale: 1.0,
                    bounds: text_bounds_for_rect(s.close),
                    default_color: GColor::rgb(close_fg.r, close_fg.g, close_fg.b),
                    custom_glyphs: &[],
                });
            }
            let (nx, ny, nw, nh) = tabbar.new_tab;
            let toolbar_accent = if tabbar.broadcast {
                theme.palette[3]
            } else {
                self.ui_accent(cfg, theme)
            };
            let plus_fg = if tabbar.hovered_new_tab {
                toolbar_accent
            } else {
                fg
            };
            let toolbar_background = |colors: &mut chrome::Colors, hovered| {
                if hovered {
                    colors.surface(
                        chrome_strip_bg,
                        toolbar_accent,
                        0.14,
                        self.config.format.is_srgb(),
                    )
                } else {
                    chrome_strip_bg
                }
            };
            let background = toolbar_background(&mut self.chrome_colors, tabbar.hovered_new_tab);
            let plus_fg = self.chrome_colors.icon(plus_fg, background);
            let plus_w = self
                .tabbar_buffer
                .layout_runs()
                .next()
                .map_or(cw, |run| run.line_w);
            // The cap owns the outside two pixels. Center against the remaining
            // content box so it does not make the glyph read one pixel right.
            let plus_content = if matches!(cfg.tab_bar_pos, kettle_config::TabBarPos::Left) {
                (nx + 2.0, ny, (nw - 2.0).max(0.0), nh)
            } else {
                (nx, ny, (nw - 2.0).max(0.0), nh)
            };
            let (plus_left, plus_top) = centered_text_origin(plus_content, (plus_w, ch));
            areas.push(TextArea {
                buffer: &self.tabbar_buffer,
                left: plus_left,
                top: plus_top,
                scale: 1.0,
                bounds: text_bounds_for_rect(tabbar.new_tab),
                default_color: GColor::rgb(plus_fg.r, plus_fg.g, plus_fg.b),
                custom_glyphs: &[],
            });
            // The `▾` dropdown arrow glyph, at `new_tab_menu` (left
            // of `+`). Only present when the dropdown is enabled.
            if tabbar.new_tab_menu.2 > 0.0 {
                let (ax, ay, aw, ah) = tabbar.new_tab_menu;
                let arrow_fg = if tabbar.hovered_new_tab_menu {
                    toolbar_accent
                } else {
                    fg
                };
                let background =
                    toolbar_background(&mut self.chrome_colors, tabbar.hovered_new_tab_menu);
                let arrow_fg = self.chrome_colors.icon(arrow_fg, background);
                let arrow_w = self
                    .new_tab_arrow_buffer
                    .layout_runs()
                    .next()
                    .map_or(cw, |run| run.line_w);
                let (arrow_left, arrow_top) = centered_text_origin((ax, ay, aw, ah), (arrow_w, ch));
                areas.push(TextArea {
                    buffer: &self.new_tab_arrow_buffer,
                    left: arrow_left,
                    top: arrow_top,
                    scale: 1.0,
                    bounds: text_bounds_for_rect(tabbar.new_tab_menu),
                    default_color: GColor::rgb(arrow_fg.r, arrow_fg.g, arrow_fg.b),
                    custom_glyphs: &[],
                });
            }
            // Overflow scroll-arrow glyphs: `‹` / `›` at the ends of a top or
            // bottom strip, `⌃` / `⌄` centered in their rows on a side one.
            let vertical = cfg.tab_bar_pos.is_vertical();
            for (rect, buffer) in [
                (tabbar.scroll_left, &self.scroll_left_buffer),
                (tabbar.scroll_right, &self.scroll_right_buffer),
            ] {
                if rect.2 <= 0.0 {
                    continue;
                }
                let (ax, _, aw, _) = rect;
                let (left, top, bounds) = if vertical {
                    let glyph_w = buffer.layout_runs().next().map_or(cw, |run| run.line_w);
                    let (left, top) = centered_text_origin(rect, (glyph_w, ch));
                    (left, top, text_bounds_for_rect(rect))
                } else {
                    let bounds = TextBounds {
                        left: ax as i32,
                        top: ty,
                        right: (ax + aw) as i32,
                        bottom: tb,
                    };
                    (ax + 4.0, tabbar.y + 4.0, bounds)
                };
                areas.push(TextArea {
                    buffer,
                    left,
                    top,
                    scale: 1.0,
                    bounds,
                    default_color: gc(self.chrome_colors.icon(fg, chrome_strip_bg)),
                    custom_glyphs: &[],
                });
            }
        }
        for (index, left, top, bounds, color) in search_segment_places {
            let color = color.unwrap_or(fg);
            areas.push(TextArea {
                buffer: &self.search_segment_buffers[index],
                left,
                top,
                scale: 1.0,
                bounds,
                default_color: GColor::rgb(color.r, color.g, color.b),
                custom_glyphs: &[],
            });
        }
        if have_search {
            let text_h = if overlay.confirm_dialog.is_some() {
                metrics.line_height
            } else {
                ch
            };
            let area = TextArea {
                buffer: &self.search_buffer,
                left: search_rect.0,
                top: if overlay.confirm_dialog.is_some() {
                    search_rect.1
                } else {
                    search_rect.1 + ((search_rect.3 - text_h) * 0.5).max(0.0)
                },
                scale: 1.0,
                bounds: TextBounds {
                    left: search_rect.0 as i32,
                    top: search_rect.1 as i32,
                    right: (search_rect.0 + search_rect.2) as i32,
                    bottom: (search_rect.1 + search_rect.3) as i32,
                },
                default_color: GColor::rgb(fg.r, fg.g, fg.b),
                custom_glyphs: &[],
            };
            areas.push(area);
        }
        // Status-bar text area. Left-padded 8 px, baseline
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
                default_color: gc(self.chrome_colors.text(fg, chrome_strip_bg)),
                custom_glyphs: &[],
            });
        }
        // Hint labels over the focused pane, drawn with the menus, above
        // their chips and every card.
        if let Some((frx, fry, frw, frh)) = focus_origin {
            let focus_grid_origin = pane_grid_origin((frx, fry, frw, frh), (pad_x, pad_y));
            // Hint-label text follows the theme background (dark on
            // the theme-yellow chip) unless overridden.
            let lab = cfg.search_foreground.unwrap_or(theme.background);
            for (i, hint) in overlay.hint_labels.iter().enumerate() {
                menu_areas.push(TextArea {
                    buffer: &self.hint_buffers[i],
                    left: focus_grid_origin.0 + hint.col as f32 * cw,
                    top: focus_grid_origin.1 + hint.row as f32 * ch,
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
            if let Some((rect, bounds)) = ime_geometry {
                ime_areas.push(TextArea {
                    buffer: &self.ime_buffer,
                    left: rect.0,
                    top: rect.1,
                    scale: 1.0,
                    bounds,
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
            }
        }

        // Right-click context menu, drawn in its own final pass. Chrome quads
        // go to `menu_q` (drawn after `over` via `self.menu_quads.draw`); row
        // labels go to `menu_areas` (drawn by a dedicated
        // `self.menu_text_renderer.render` call after the menu quads), so the
        // opaque panel bg stays under the menu text.
        if let Some(menu) = &overlay.context_menu {
            let accent = self.ui_accent(cfg, theme);
            let chrome = menu_chrome_quads(
                menu,
                theme,
                accent,
                cw,
                ch,
                &mut self.chrome_colors,
                self.config.format.is_srgb(),
            );
            menu_q.extend(chrome);
            // Row labels — collected into `menu_areas` so the second
            // TextRenderer can prepare them as their own batch.
            let panel_w = context_menu_panel_width(menu, cw);
            let row_h = ch + 12.0;
            let sep_h = 8.0_f32;
            let (ax, ay) = menu.anchor;
            // Skip scrolled-off rows + stop drawing when
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
            let (inset, content_h) = menu::scroll_content(natural_h, panel_h_eff, row_h);
            let start = menu.scroll_offset.min(menu.rows.len());
            let mut row_y = ay + inset;
            for (i, row) in menu.rows.iter().enumerate().skip(start) {
                let h = if row.separator { sep_h } else { row_h };
                if row_y + h > ay + inset + content_h {
                    break;
                }
                if row.separator {
                    row_y += sep_h;
                    continue;
                }
                let background = if i == menu.highlight && row.enabled {
                    self.chrome_colors.surface(
                        theme.background,
                        self.ui_accent(cfg, theme),
                        0.18,
                        self.config.format.is_srgb(),
                    )
                } else {
                    theme.background
                };
                let fg = if row.enabled {
                    self.chrome_colors.text(theme.foreground, background)
                } else {
                    self.chrome_colors.muted(theme.foreground, background)
                };
                let bounds = TextBounds {
                    left: ax as i32,
                    top: row_y as i32,
                    right: (ax + panel_w) as i32,
                    bottom: (row_y + row_h) as i32,
                };
                menu_areas.push(TextArea {
                    buffer: &self.context_menu_buffers[i],
                    left: ax + 16.0,
                    top: row_y + 6.0,
                    scale: 1.0,
                    bounds,
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
                // Dropdown-parity: the right-aligned dimmed hint.
                if !row.hint.is_empty() {
                    let hint_fg = self.chrome_colors.muted(theme.foreground, background);
                    let hint_w =
                        unicode_width::UnicodeWidthStr::width(row.hint.as_str()) as f32 * cw;
                    menu_areas.push(TextArea {
                        buffer: &self.context_menu_hint_buffers[i],
                        left: (ax + panel_w - 16.0 - hint_w).max(ax + 16.0),
                        top: row_y + 6.0,
                        scale: 1.0,
                        bounds,
                        default_color: GColor::rgb(hint_fg.r, hint_fg.g, hint_fg.b),
                        custom_glyphs: &[],
                    });
                }
                row_y += row_h;
            }
        }

        // Settings overlay — a centered modal panel drawn on top via
        // the menu pipeline (dim backdrop + panel + accent border + focused-row
        // highlight as quads; one TextArea per display line).
        if let Some(set) = &overlay.settings {
            // Reuse the buffer-prep pass's memoized lines rather than running
            // `settings_display_lines` (one `format!()` per display line) a
            // SECOND time per frame. `self.settings_lines_cache` was just
            // (re)computed for this exact `set` above, in the same
            // `render_frame` call, so both passes stay in lockstep.
            let lines = &self.settings_lines_cache;
            let layout = settings_panel_layout(set, lines, cw, ch, sw, sh);
            let (row_h, panel_w, panel_h, px, py) = (
                layout.row_h,
                layout.panel_w,
                layout.panel_h,
                layout.px,
                layout.py,
            );
            // Multi-window: the settings overlay's accent follows
            // this WINDOW's chrome accent, so it matches the focus border +
            // active tab rather than always-blue.
            let acc = self.ui_accent(cfg, theme);
            // Dim backdrop over the whole window so the panel reads as modal.
            // On the Background page, dim LESS so the live wallpaper (the real
            // animated starfield / image) shows around the panel as a genuine
            // preview while you change `background-type`.
            let backdrop_a = set.backdrop_alpha();
            menu_q.push(rect(0.0, 0.0, sw, sh, theme.background, backdrop_a));
            // Panel background (near-opaque) + accent border.
            menu_q.push(rect(px, py, panel_w, panel_h, theme.background, 1.0));
            menu_q.push(rect(px, py, panel_w, 2.0, acc, 1.0));
            menu_q.push(rect(px, py + panel_h - 2.0, panel_w, 2.0, acc, 1.0));
            menu_q.push(rect(px, py, 2.0, panel_h, acc, 1.0));
            menu_q.push(rect(px + panel_w - 2.0, py, 2.0, panel_h, acc, 1.0));
            if layout.first_line <= 1 && 1 < layout.first_line + layout.visible_lines {
                let strip_cols = ((panel_w - 32.0).max(cw) / cw).floor().max(1.0) as usize;
                let visible_cols = if display_width(&lines[1]) > strip_cols {
                    strip_cols.saturating_sub(1)
                } else {
                    strip_cols
                };
                let (col, width) = settings_tab_span(set, set.active_category);
                let visible_width = (col + width).min(visible_cols).saturating_sub(col);
                if visible_width > 0 {
                    menu_q.push(rect(
                        px + 16.0 + col as f32 * cw,
                        py + 12.0 + (1 - layout.first_line) as f32 * row_h + row_h - 2.0,
                        visible_width as f32 * cw,
                        2.0,
                        acc,
                        1.0,
                    ));
                }
            }
            // Focused field-row highlight.
            let hi_line = SETTINGS_FIELD_START + set.focused_row;
            let hi_y = py + 12.0 + (hi_line - layout.first_line) as f32 * row_h;
            menu_q.push(rect(px + 6.0, hi_y, panel_w - 12.0, row_h, acc, 0.22));
            let sfg = theme.foreground;
            // Disabled fields and footer hints keep a quieter, readable color.
            for (visible_index, i) in settings_painted_lines(&layout) {
                if i >= self.settings_buffers.len() {
                    break;
                }
                let background = if i == hi_line {
                    self.chrome_colors.surface(
                        theme.background,
                        acc,
                        0.22,
                        self.config.format.is_srgb(),
                    )
                } else {
                    theme.background
                };
                let muted = i >= SETTINGS_FIELD_START + set.rows.len()
                    || (i >= SETTINGS_FIELD_START
                        && set
                            .rows
                            .get(i - SETTINGS_FIELD_START)
                            .is_some_and(|r| r.disabled));
                let row_color = if muted {
                    self.chrome_colors.muted(sfg, background)
                } else {
                    self.chrome_colors.text(sfg, background)
                };
                menu_areas.push(TextArea {
                    buffer: &self.settings_buffers[i],
                    left: px + 16.0,
                    top: py + 12.0 + visible_index as f32 * row_h + 3.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: px as i32,
                        top: py as i32,
                        right: (px + panel_w) as i32,
                        bottom: (py + panel_h) as i32,
                    },
                    default_color: GColor::rgb(row_color.r, row_color.g, row_color.b),
                    custom_glyphs: &[],
                });
            }
            let cue_w = 12.0;
            let cue_x = px + (panel_w - cue_w) * 0.5;
            if layout.first_line > 0 {
                menu_q.push(rect(cue_x, py + 2.0, cue_w, 3.0, acc, 0.85));
            }
            let content_end = SETTINGS_FIELD_START + set.rows.len() + 2;
            if layout.first_line + layout.visible_lines < content_end {
                let content_bottom = py + 12.0 + layout.visible_lines as f32 * row_h;
                menu_q.push(rect(cue_x, content_bottom - 3.0, cue_w, 3.0, acc, 0.85));
            }
        }

        let mut confirmation_surfaces = Vec::new();
        let mut confirmation_colors = Vec::new();
        if let (Some(dialog), Some(layout)) = (&overlay.confirm_dialog, &confirmation_layout) {
            self.confirmation_text
                .prepare(&mut self.font_system, &family, layout, dialog);
            let rgba = |color: Rgb, alpha: f32| {
                [
                    color.r as f32 / 255.0,
                    color.g as f32 / 255.0,
                    color.b as f32 / 255.0,
                    alpha,
                ]
            };
            let mut shape = |rect: Rect4,
                             fill: Rgb,
                             alpha: f32,
                             stroke: Rgb,
                             stroke_alpha: f32,
                             radius: f32,
                             border: f32| {
                if rect.2 > 0.0 && rect.3 > 0.0 {
                    confirmation_surfaces.push(rounded_surface::RoundedSurface {
                        pos: [rect.0, rect.1],
                        size: [rect.2, rect.3],
                        fill: rgba(fill, alpha),
                        stroke: rgba(stroke, stroke_alpha),
                        params: [radius, border, 0.0, 0.0],
                    });
                }
            };
            let black = Rgb::new(0, 0, 0);
            shape((0.0, 0.0, sw, sh), black, 0.45, black, 0.0, 0.0, 0.0);
            shape(
                layout.rect,
                theme.background,
                1.0,
                confirm_dialog_border_color(theme),
                1.0,
                6.0 * layout.unit,
                layout.unit,
            );
            let foreground = confirm_dialog_text_color(theme);
            confirmation_colors.extend([GColor::rgb(foreground.r, foreground.g, foreground.b); 2]);
            for (index, &rect) in layout.buttons.iter().enumerate() {
                let (fill, label) =
                    confirm::button_colors(theme, self.ui_accent(cfg, theme), dialog, index);
                let stroke = color::with_min_contrast(fill, theme.background, 3.0);
                shape(rect, fill, 1.0, stroke, 1.0, 6.0 * layout.unit, layout.unit);
                if index == dialog.focus_idx && rect.2 > 0.0 && rect.3 > 0.0 {
                    let focus = confirm::focus_color(theme, self.ui_accent(cfg, theme));
                    let ring = confirm::intersect(
                        (
                            rect.0 - 4.0 * layout.unit,
                            rect.1 - 4.0 * layout.unit,
                            rect.2 + 8.0 * layout.unit,
                            rect.3 + 8.0 * layout.unit,
                        ),
                        layout.rect,
                    );
                    shape(
                        ring,
                        black,
                        0.0,
                        focus,
                        1.0,
                        8.0 * layout.unit,
                        2.0 * layout.unit,
                    );
                }
                confirmation_colors.push(GColor::rgb(label.r, label.g, label.b));
            }
            if layout.max_scroll > 0 {
                let region = if layout.whole_scroll {
                    layout.rect
                } else {
                    layout.body_rect
                };
                let color = color::with_min_contrast(theme.foreground, theme.background, 3.0);
                if layout.scroll_offset > 0 {
                    shape(
                        confirm::intersect(
                            (
                                region.0 + region.2 - 20.0 * layout.unit,
                                region.1,
                                20.0 * layout.unit,
                                2.0 * layout.unit,
                            ),
                            region,
                        ),
                        color,
                        1.0,
                        black,
                        0.0,
                        0.0,
                        0.0,
                    );
                }
                if layout.scroll_offset < layout.max_scroll {
                    shape(
                        confirm::intersect(
                            (
                                region.0 + region.2 - 20.0 * layout.unit,
                                region.1 + region.3 - 2.0 * layout.unit,
                                20.0 * layout.unit,
                                2.0 * layout.unit,
                            ),
                            region,
                        ),
                        color,
                        1.0,
                        black,
                        0.0,
                        0.0,
                        0.0,
                    );
                }
            }
        }
        let confirmation_areas = if let Some(layout) = &confirmation_layout {
            self.confirmation_text.areas(layout, &confirmation_colors)
        } else {
            Vec::new()
        };

        let label_count = self.card_scene.labels.len();
        if self.card_label_buffers.len() < label_count {
            self.card_label_buffers.resize_with(label_count, || {
                TextBuffer::new(&mut self.font_system, metrics)
            });
        }
        if self.card_label_texts.len() < label_count {
            self.card_label_texts.resize_with(label_count, String::new);
        }
        for (index, label) in self.card_scene.labels.iter().enumerate() {
            let [_, _, width, height] = label.rect;
            let scale = label.scale;
            let columns = (width / (cw * scale).max(1.0)).floor().max(1.0) as usize;
            let text = fit_single_line_label(&label.text(), columns);
            let buffer = &mut self.card_label_buffers[index];
            buffer.set_metrics(metrics);
            buffer.set_wrap(Wrap::None);
            buffer.set_size(Some(width / scale), Some(height / scale));
            if self.card_label_texts[index] != text {
                buffer.set_text(
                    &text,
                    &Attrs::new().family(Family::Name(&family)),
                    Shaping::Advanced,
                    None,
                );
                self.card_label_texts[index] = text;
            }
            buffer.shape_until_scroll(&mut self.font_system, false);
        }
        let card_foreground = resolved_cell_foreground_cached(
            theme.foreground,
            theme.background,
            CellHighlight::None,
            (false, false),
            cfg,
            theme,
            &mut self.minimum_contrast_cache,
        );
        let card_areas: Vec<TextArea> = self
            .card_scene
            .labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let [x, y, width, height] = label.rect;
                let ink_width =
                    display_width(&self.card_label_texts[index]) as f32 * cw * label.scale;
                TextArea {
                    buffer: &self.card_label_buffers[index],
                    left: x + ((width - ink_width) * 0.5).max(0.0),
                    top: y + (height - metrics.line_height * label.scale).max(0.0) * 0.5,
                    scale: label.scale,
                    bounds: TextBounds {
                        left: x.ceil() as i32,
                        top: y.ceil() as i32,
                        right: (x + width).floor() as i32,
                        bottom: (y + height).floor() as i32,
                    },
                    default_color: GColor::rgb(
                        card_foreground.r,
                        card_foreground.g,
                        card_foreground.b,
                    ),
                    custom_glyphs: &[],
                }
            })
            .collect();

        // Skip the whole-viewport glyphon `prepare` when nothing that feeds the
        // text renderers changed this frame. `prepare` re-encodes EVERY visible
        // glyph's vertices + does atlas housekeeping; on an idle repaint (a
        // cursor blink, a bell-flash decay, a focus-dim toggle) the text is
        // byte-identical, so we re-render the cached vertex buffers as-is and
        // only rebuild/upload the cheap quad list. Skipping is conservative.
        // ANY pane row reshape in legacy mode (where pane text is a glyphon
        // area), ANY chrome label or damage-key change, or an open text
        // overlay without its own damage key forces the prepare, so a stale
        // frame is impossible. `atlas.trim()` (below) is gated the same
        // way, because trimming without a following prepare would clear the
        // in-use set and let a later prepare evict still-displayed glyphs out
        // from under the cached vertices.
        let non_context_text_overlay_open = text_overlay_requires_continuous_prepare(overlay);
        let overlay_open = non_context_text_overlay_open || overlay.context_menu.is_some();
        let chrome_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            self.card_label_texts[..label_count].hash(&mut h);
            prepared_text_areas_damage_key(&card_areas).hash(&mut h);
            self.pane_titlebar_texts.hash(&mut h);
            self.tab_texts.hash(&mut h);
            self.tabbar_text.hash(&mut h);
            self.status_bar_text.hash(&mut h);
            self.tab_close_text.hash(&mut h);
            self.new_tab_arrow_text.hash(&mut h);
            self.scroll_left_text.hash(&mut h);
            self.scroll_right_text.hash(&mut h);
            self.resize_overlay_text.hash(&mut h);
            self.update_toast_text.hash(&mut h);
            self.ime_text.hash(&mut h);
            prepared_text_areas_damage_key(&areas).hash(&mut h);
            prepared_text_areas_damage_key(&ime_areas).hash(&mut h);
            // Completion buffers are retained while the card is static. Hash
            // both their geometry and their shaped source strings: glyphon
            // keeps each buffer at the same address when a same-size candidate
            // list changes, so the text-area identity alone cannot detect new
            // labels or descriptions.
            prepared_text_areas_damage_key(&menu_areas).hash(&mut h);
            prepared_text_areas_damage_key(&confirmation_areas).hash(&mut h);
            self.confirmation_text.key().hash(&mut h);
            bytemuck::cast_slice::<rounded_surface::RoundedSurface, u8>(&confirmation_surfaces)
                .hash(&mut h);
            completion_text_damage_key(
                &self.completion_header_text,
                &self.completion_count_text,
                &self.completion_texts,
                &self.completion_description_texts,
                &self.completion_spans,
                &self.completion_selected,
                &self.completion_emphasis_colors,
            )
            .hash(&mut h);
            self.media_receipt_title_text.hash(&mut h);
            self.media_receipt_detail_text.hash(&mut h);
            prepared_text_areas_damage_key(&lane_areas).hash(&mut h);
            // Retained like the receipt's: a buffer keeps its address when
            // the viewer moves to another item, so hash what it shows.
            for lane in &overlay.media_lanes {
                lane.pane.hash(&mut h);
                if let Some(text) = self.lane_texts.get(&lane.pane) {
                    text.shaped().hash(&mut h);
                }
            }
            context_menu_text_damage_key(
                overlay.context_menu.as_ref(),
                theme.foreground,
                theme.background,
            )
            .hash(&mut h);
            h.finish()
        };
        let chrome_changed = chrome_hash != self.last_chrome_hash;
        self.last_chrome_hash = chrome_hash;
        let text_layout_key =
            text_layout_damage_key(panes, cfg, (sw, sh), (cw, ch), pane_titlebar_h);
        let text_layout_changed = self.last_text_layout_key != Some(text_layout_key);
        self.last_text_layout_key = Some(text_layout_key);
        // When the cursor moves to a DIFFERENT glyph, force the main prepare so
        // that glyph is freshly resident in the atlas before the cursor pass
        // reuses its bitmap (otherwise the 1-glyph cursor prepare could be the
        // one that grows/repacks the atlas, invalidating the cached pane
        // vertices we're about to re-render). A char change almost always
        // coincides with a content change (so the prepare runs anyway); this
        // only adds a prepare for the rare move-without-output case.
        // A cursor glyph that rasterizes anything can be the allocation that
        // grows or evicts the shared atlas, at any new character, emoji
        // presentation, colour or subpixel position, and grid output no longer
        // prepares the chrome on its own. So a visible cursor glyph never
        // prepares alone: whenever its key changes, the chrome and pane text
        // that share the atlas are prepared with it. Only an ordinary space
        // or an empty cell is known to rasterize nothing (other whitespace,
        // such as U+1680 OGHAM SPACE MARK, can draw a stroke), and a blink
        // leaves the key as it was.
        let cursor_metrics = pane_metrics(self.metrics.font_size, self.cell_h);
        let cursor_glyph_key =
            cursor_glyph_damage_key(self.pending_cursor_glyph.as_ref(), cursor_metrics, &family);
        let cursor_glyph_changed = cursor_glyph_key != self.last_cursor_glyph_key;
        let cursor_char_changed = cursor_glyph_changed
            && self
                .pending_cursor_glyph
                .as_ref()
                .is_some_and(|c| c.text != " " && c.text != "\0");
        // The frame an overlay CLOSES (`overlay_open` flips true→false) must
        // still prepare once, or the closed panel's cached text vertices keep
        // rendering until the next keystroke. Open overlays are covered above;
        // this covers the close edge.
        let overlay_changed = overlay_open != self.last_overlay_open;
        self.last_overlay_open = overlay_open;
        // In the default grid mode the glyph pipeline draws pane text from its
        // own atlas, behind its own gate below, so output leaves every glyphon
        // vertex as it was. Preparing anyway would rewrite the chrome's
        // unchanged vertices through the queue on every frame of output.
        let glyphon_pane_text_changed =
            any_pane_text_changed && cfg.text_renderer == TextRendererMode::Legacy;
        let need_prepare = self.text_prepare_dirty
            || glyphon_pane_text_changed
            || chrome_changed
            || text_layout_changed
            || non_context_text_overlay_open
            || cursor_char_changed
            || overlay_changed;
        let need_cursor_prepare =
            self.pending_cursor_glyph.is_some() && (need_prepare || cursor_glyph_changed);
        if cursor_glyph_key.is_none() {
            self.last_cursor_glyph_key = None;
        }
        let preparing_text = need_prepare || need_cursor_prepare;
        if preparing_text {
            // Every glyphon renderer shares the atlas. Arm the retry latch even
            // for a cursor-only prepare so a partial atlas mutation cannot
            // leave otherwise-retained pane or menu vertices looking current.
            self.text_prepare_dirty = true;
        }
        if need_prepare {
            self.chrome_prepares += 1;
            self.text_prepares += 1;
            // Buffer and damage caches above have already advanced to this
            // frame. Any `?` return leaves the retry latch set for the next
            // redraw.
            self.text_renderer.prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            )?;
            if !ime_areas.is_empty() || self.ime_prepared {
                self.chrome_prepares += 1;
                self.text_prepares += 1;
                let has_preedit = !ime_areas.is_empty();
                self.ime_text_renderer.prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    ime_areas,
                    &mut self.swash,
                )?;
                self.ime_prepared = has_preedit;
            }
            // Second TextRenderer prepare, for the menu pass (context menu,
            // settings, completion, and other top-layer text). Empty
            // `menu_areas` is fine; glyphon's prepare handles a zero-area
            // batch as a no-op.
            self.chrome_prepares += 1;
            self.text_prepares += 1;
            if !card_areas.is_empty() || self.card_labels_prepared {
                self.chrome_prepares += 1;
                self.text_prepares += 1;
                let has_labels = !card_areas.is_empty();
                self.card_text_renderer.prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    card_areas,
                    &mut self.swash,
                )?;
                self.card_labels_prepared = has_labels;
            }
            if !lane_areas.is_empty() || self.lane_labels_prepared {
                self.chrome_prepares += 1;
                self.text_prepares += 1;
                let has_labels = !lane_areas.is_empty();
                self.lane_text_renderer.prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    lane_areas,
                    &mut self.swash,
                )?;
                self.lane_labels_prepared = has_labels;
            }
            if !confirmation_areas.is_empty() || self.confirmation_labels_prepared {
                self.chrome_prepares += 1;
                self.text_prepares += 1;
                let has_labels = !confirmation_areas.is_empty();
                self.confirmation_text_renderer.prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    confirmation_areas,
                    &mut self.swash,
                )?;
                self.confirmation_labels_prepared = has_labels;
            }
            self.menu_text_renderer.prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                menu_areas,
                &mut self.swash,
            )?;
        }
        // A mapped instance buffer becomes writable again only through a poll
        // (or a submit) after the frames that drew from it have finished.
        if self.gpu.mapped_uploads() {
            let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        }
        // Cell-locked pane text has its OWN damage gate. A cursor blink
        // can force `need_prepare` via `cursor_char_changed` for the separate
        // cursor glyph pass, but it must not clear/re-upload pane glyph
        // instances. Only pane text changes and layout/style damage refresh the
        // glyph pipeline. Legacy panes use it only while dimmed, preserving
        // their continuous layout. The same gate clears unused instances.
        let grid_upload_needed =
            self.grid_glyphs_dirty || any_pane_text_changed || text_layout_changed;
        if grid_upload_needed {
            let mut gi = std::mem::take(&mut self.glyph_instances);
            let mut gc = std::mem::take(&mut self.glyph_clips);
            gi.clear();
            gc.clear();
            self.emit_pane_glyphs(panes, cfg, &mut gi, &mut gc);
            self.grid_glyphs_dirty =
                !self
                    .glyph_pipeline
                    .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &gi);
            self.glyph_instances = gi;
            self.glyph_clips = gc;
        }
        // Prepare the focused solid-block cursor's inverted glyph only when its
        // text/position/style changed. A main text prepare also forces this tiny
        // prepare because both renderers share an atlas that may have repacked.
        // Menu-highlight and other quad-only frames reuse retained vertices.
        if let Some((gx, gy, gcolor, gclip, gtext, gattrs, baseline)) = self
            .pending_cursor_glyph
            .as_ref()
            .map(|c| {
                (
                    c.x,
                    c.y,
                    c.color,
                    c.clip,
                    c.text.as_str(),
                    c.attrs.as_attrs(),
                    c.row_baseline,
                )
            })
            .filter(|_| need_cursor_prepare)
        {
            self.cursor_glyph_buffer.set_metrics(cursor_metrics);
            // The cursor draws its own glyph last, over the pane's, so it has
            // to reach the same answer the pane did. Both halves matter: skip
            // the override and it paints a colour square over a text circle;
            // ignore the cell's variation selector and it paints a text circle
            // over the colour square the user explicitly asked for.
            self.cursor_glyph_buffer
                .set_text(gtext, &gattrs, Shaping::Advanced, None);
            self.cursor_glyph_buffer
                .shape_until_scroll(&mut self.font_system, false);
            let area = TextArea {
                buffer: &self.cursor_glyph_buffer,
                left: gx,
                top: cursor_glyph_top(gy, baseline, &self.cursor_glyph_buffer),
                scale: 1.0,
                bounds: TextBounds {
                    left: gclip.0 as i32,
                    top: gclip.1 as i32,
                    right: (gclip.0 + gclip.2) as i32,
                    bottom: (gclip.1 + gclip.3) as i32,
                },
                default_color: GColor::rgb(gcolor.r, gcolor.g, gcolor.b),
                custom_glyphs: &[],
            };
            self.text_prepares += 1;
            self.cursor_glyph_renderer.prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [area],
                &mut self.swash,
            )?;
            // Commit the retained-vertex key only after prepare succeeds. A
            // transient atlas/device error must retry rather than treating
            // stale vertices as current on the next frame.
            self.last_cursor_glyph_key = cursor_glyph_key;
        }
        if preparing_text {
            self.text_prepare_dirty = false;
        }
        self.pane_bases
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &pane_bases);
        // `pane_bases` deliberately uses replace blending so overlapping pane
        // interiors do not compound their configured transparency. That pass
        // would also erase the unsupported-blur underlay floor, leaving Linux
        // at the user's raw opacity instead of the advertised 99% fallback.
        // Upload a live-only copy with its final alpha clamped; offscreen
        // screenshots continue to draw the unmodified buffer above.
        if let Some(floor) = self.live_background_opacity_floor {
            apply_quad_alpha_floor(&mut pane_bases, floor);
            self.live_pane_bases
                .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &pane_bases);
        }
        self.quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &quads);
        self.pane_dimming
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &pane_dim);
        self.text_decorations
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &decorations);
        self.ime_quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &ime_q);
        self.pane_outlines
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &pane_outlines);
        // Return the scratch to the pool (keeps its capacity for next
        // frame). Last use of `quads` is the upload just above.
        self.quad_scratch = quads;
        // Wallpaper goes into its own back pipeline; inline images into
        // `imgs`. Prepare all layers before uploading: protect their complete
        // draw lists, transfer only needed exact-size retired textures, and
        // release the rest before admitting new allocations.
        self.card_base.upload(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &self.card_scene.base,
        );
        self.card_cursors.upload(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &self.card_scene.cursors,
        );
        self.card_decoration.upload(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &self.card_scene.decoration,
        );
        self.bg_imgs.prepare_frame(&self.gpu.device, &bg_img_items);
        self.imgs.prepare_frame(&self.gpu.device, &img_items);
        self.media_receipt_img
            .prepare_frame(&self.gpu.device, &media_receipt_items);
        if self.lane_img.is_none() && !lane_items.is_empty() {
            // Preview pressure cannot prevent a frame: without room a lane
            // paints its frame and text, and tries again next frame.
            self.lane_img = self.card_image_shared.layer_with_instance_limit(
                &self.gpu.device,
                kettle_core::GraphicsBudget::previews(),
                MAX_LANE_IMAGES,
            );
        }
        if let Some(lanes) = &mut self.lane_img {
            lanes.prepare_frame(&self.gpu.device, &lane_items);
        }
        let wallpaper_upload_complete = self.bg_imgs.upload_retained(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &bg_img_items,
        );
        opaque_wallpaper_covers_surface &= wallpaper_upload_complete;
        // Refresh the procedural starfield's per-frame uniform (just
        // resolution + the continuous `time` clock; the look is baked into the
        // shader) when it's the active wallpaper.
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            self.starfield.upload(
                &self.gpu.queue,
                [sw, sh],
                starfield_time(cfg.background_animation, self.starfield_started.elapsed()),
            );
        }
        self.imgs
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &img_items);
        self.media_receipt_img.upload(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &media_receipt_items,
        );
        if let Some(lanes) = &mut self.lane_img {
            lanes.upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &lane_items);
        }
        self.lane_quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &lane_q);
        if let Some(viewer) = overlay.media_viewer_rect {
            quad::exclude_rect(&mut over, viewer);
        }
        self.overlay_quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &over);
        self.transport_surface_count = transport_surfaces.len() as u32;
        transport_surfaces.extend_from_slice(&confirmation_surfaces);
        self.rounded_surfaces.upload(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &transport_surfaces,
        );
        self.menu_quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &menu_q);

        let target_size = [self.config.width.max(1), self.config.height.max(1)];
        let scene_is_opaque = final_scene_is_uniformly_opaque(cfg, opaque_wallpaper_covers_surface);
        let needs_presentation =
            needs_postmultiplied_presentation(self.config.alpha_mode, scene_is_opaque);
        // What this frame's scene pass draws, kept once the frame is on screen
        // for a cursor-layer hand-off (`present_cursor_patch`).
        let scene_facts = cursor_patch::SceneFacts {
            target_size,
            live_window: true,
            cursor_on: overlay.cursor_visible,
            translucent: self.config.alpha_mode != wgpu::CompositeAlphaMode::Opaque
                && !scene_is_opaque,
            alpha_mode: self.config.alpha_mode,
            background: cfg.theme.background,
            background_type: cfg.background_type,
        };
        let screenshot_request = self.pending_screenshot.take().and_then(|request| {
            if request.is_cancelled() {
                Self::complete_screenshot_error(
                    request,
                    "screenshot request was cancelled".to_string(),
                );
                None
            } else {
                Some(request)
            }
        });
        if !needs_presentation {
            // A target retained from a previous translucent configuration or
            // animation frame should not keep a full-surface texture charged
            // while the completed scene is provably alpha 1 everywhere.
            self.presentation.discard_target();
        }

        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kettle-encoder"),
            });
        // Screenshot capture owns an offscreen render. Do this before touching
        // the swapchain: Metal intentionally refuses `nextDrawable` for an
        // occluded NSWindow, but a control-plane screenshot still has a complete
        // terminal scene to render. The extra pass exists only for a capture;
        // ordinary visible frames retain their previous single-pass path.
        let prepared_screenshot = if let Some(request) = screenshot_request {
            match self.create_screenshot_target(target_size) {
                Ok(capture) => {
                    if let Err(error) = self.encode_scene_pass(
                        &capture.view,
                        target_size,
                        cfg,
                        false,
                        overlay.cursor_visible,
                        None,
                        &mut encoder,
                    ) {
                        Self::complete_screenshot_error(
                            request,
                            format!("screenshot render failed: {error}"),
                        );
                        return Err(error);
                    }
                    Some(Ok(self.prepare_texture_screenshot(
                        capture,
                        request,
                        &mut encoder,
                        true,
                    )))
                }
                Err(error) => {
                    Self::complete_screenshot_error(request, error);
                    None
                }
            }
        } else {
            None
        };

        let Some(acquired) = self
            .surface
            .as_ref()
            .map(wgpu::Surface::get_current_texture)
        else {
            // Headless (tests): the offscreen capture is the whole frame.
            self.submit_offscreen_screenshot(encoder, prepared_screenshot);
            if need_prepare {
                self.atlas.trim();
            }
            // Test-only: a headless capture substitutes for a presented frame.
            // A production Occluded frame never grants a hand-off.
            self.last_scene = Some(cursor_patch::SceneFacts {
                live_window: false,
                ..scene_facts
            });
            self.commit_painted_cards();
            return Ok(FrameOutcome::Occluded);
        };
        let (frame, reconfigure_after_present) = match acquired {
            wgpu::CurrentSurfaceTexture::Success(t) => (t, false),
            // The frame is usable, but no longer matches the underlying
            // surface. Present it once, then refresh the swapchain before the
            // next acquire as required by wgpu 30's explicit outcome model.
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => (t, true),
            // Occluded and Timeout are benign transient states, not device
            // loss. On macOS the Metal backend returns `Occluded` on every
            // acquire while the window is minimized/occluded (gfx-rs/wgpu#8309;
            // an occluded `nextDrawable` hangs ~1s, so the HAL short-circuits
            // before it). Treating that as a failure would falsely latch
            // `gpu_lost` for a minimized window with active output. Skip the
            // frame without reconfiguring; reconfiguring an occluded surface
            // can itself hang.
            wgpu::CurrentSurfaceTexture::Occluded => {
                self.submit_offscreen_screenshot(encoder, prepared_screenshot);
                if need_prepare {
                    self.atlas.trim();
                }
                return Ok(FrameOutcome::Occluded);
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                self.submit_offscreen_screenshot(encoder, prepared_screenshot);
                if need_prepare {
                    self.atlas.trim();
                }
                return Ok(FrameOutcome::RetryLater);
            }
            // Outdated means the existing surface is still valid but its
            // configuration no longer matches the window. Reconfigure that
            // surface, retain damage, and let the UI's bounded retry scheduler
            // decide when to acquire again.
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configure_surface();
                self.submit_offscreen_screenshot(encoder, prepared_screenshot);
                if need_prepare {
                    self.atlas.trim();
                }
                return Ok(FrameOutcome::RetryLater);
            }
            // wgpu 30 explicitly requires a Lost surface to be recreated with
            // Instance::create_surface; configuring the old object cannot
            // recover it. This is a per-window failure, not evidence that the
            // process-wide device shared by every renderer is lost.
            wgpu::CurrentSurfaceTexture::Lost => {
                self.submit_offscreen_screenshot(encoder, prepared_screenshot);
                if need_prepare {
                    self.atlas.trim();
                }
                return Ok(FrameOutcome::SurfaceLost);
            }
            // The uncaptured-error callback already logs the validation
            // details. Return an ordinary render error so the UI rebuilds this
            // renderer's retained resources on a bounded schedule rather than
            // consuming damage or escalating a healthy shared device.
            wgpu::CurrentSurfaceTexture::Validation => {
                self.submit_offscreen_screenshot(encoder, prepared_screenshot);
                if need_prepare {
                    self.atlas.trim();
                }
                return Err(anyhow!("surface acquisition validation error"));
            }
        };
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // This texture is presentation-only. Allocate it only after the
        // compositor actually vends a drawable: an occluded screenshot has no
        // need for it, and a retained-image cap must not gate an independent
        // 6K transient capture. If visible presentation cannot allocate, still
        // submit the already encoded screenshot before scheduling recovery.
        if needs_presentation
            && !self
                .presentation
                .ensure_target(&self.gpu.device, target_size[0], target_size[1])
        {
            self.submit_offscreen_screenshot(encoder, prepared_screenshot);
            if need_prepare {
                self.atlas.trim();
            }
            return Err(anyhow!(
                "GPU graphics budget exhausted while creating the presentation target"
            ));
        }
        let scene_view = if needs_presentation {
            self.presentation
                .scene_view()
                .expect("presentation target ensured after surface acquisition")
        } else {
            &surface_view
        };
        self.encode_scene_pass(
            scene_view,
            target_size,
            cfg,
            true,
            overlay.cursor_visible,
            None,
            &mut encoder,
        )?;
        if needs_presentation {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-presentation-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
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
            self.presentation.draw(&mut pass);
        }
        let submission = self.gpu.queue.submit(std::iter::once(encoder.finish()));
        self.dispatch_prepared_screenshot(prepared_screenshot, submission);
        pre_present();
        self.gpu.queue.present(frame);
        if reconfigure_after_present {
            self.configure_surface();
        }
        // Only trim when we prepared this frame (see the `need_prepare` gate):
        // trimming clears the glyph in-use set, so a trim with no following
        // prepare would let the next prepare evict glyphs the cached vertices
        // still point at.
        if need_prepare {
            self.atlas.trim();
        }
        self.frames_presented += 1;
        self.last_scene = Some(scene_facts);
        self.commit_painted_cards();
        Ok(FrameOutcome::Presented)
    }
}

#[cfg(test)]
mod shaped_row_tests;

#[cfg(test)]
mod pane_window_corner_tests;

/// Underline styles must not render pixel-identical to a plain line. Pure
/// functions, no GPU needed.
#[cfg(test)]
mod underline_style_tests;

#[cfg(test)]
mod gpu_tests;

#[cfg(test)]
mod screenshot_demo_tests;

#[cfg(test)]
#[allow(
    clippy::field_reassign_with_default,
    reason = "stepwise field set reads more clearly than a 80-field struct literal here"
)]
mod pick_titlebar_bg_tests {
    use super::pick_titlebar_bg;
    use kettle_config::{Config, Rgb, Theme};

    /// Drift guard. The focused titlebar must NEVER fall
    /// through to the historic hardcoded `#c80003` Terminator red.
    ///
    /// Cascade order (`Config::resolved_accent` folds accent-color + the
    /// theme accent together):
    ///   1. explicit `title_transmit_bg_color = #hex`
    ///   2. `focused_split_color` (split-border override)
    ///   3. resolved accent = explicit `accent-color` → Peacock auto →
    ///      `theme.accent` (the theme's signature accent — Catppuccin Mocha's
    ///      mauve; `palette[4]` for themes without one)
    ///
    /// Unfocused panes use the theme-derived fallbacks pinned by the next two
    /// tests.
    #[test]
    fn focused_titlebar_uses_accent_cascade_when_unset() {
        let theme = Theme::by_name("Default"); // falls back to Catppuccin Mocha
        let mut cfg = Config::default();
        cfg.title_transmit_bg_color = None;
        cfg.focused_split_color = None;
        cfg.accent_color = None;
        // Default fallback is the theme's signature accent (Mocha mauve), not
        // the hardcoded `#c80003` red nor a bare `palette[4]`.
        let bg = pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), true, false);
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
        // 3. accent_color wins over the theme accent.
        let accent = Rgb::new(0x00, 0xaa, 0x00);
        cfg.accent_color = Some(accent);
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), true, false),
            accent
        );
        // 2. focused_split_color wins over accent_color.
        let split = Rgb::new(0xff, 0x88, 0x00);
        cfg.focused_split_color = Some(split);
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), true, false),
            split
        );
        // 1. explicit title_transmit_bg_color wins over all (preserves
        //    the Terminator-look pin for any user who set it).
        let pinned = Rgb::new(0xc8, 0x00, 0x03);
        cfg.title_transmit_bg_color = Some(pinned);
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), true, false),
            pinned
        );
    }

    /// Unfocused + non-broadcast derives from the theme's surface
    /// `palette[8]`, not a hardcoded `#c0bebf` grey that clashes with dark
    /// themes like the Catppuccin Mocha default. An explicit
    /// `title-inactive-bg-color` still wins.
    #[test]
    fn unfocused_titlebar_derives_from_theme_surface() {
        let theme = Theme::by_name("Default");
        let mut cfg = Config::default();
        cfg.title_inactive_bg_color = None;
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), false, false),
            theme.palette[8]
        );
        let pinned = Rgb::new(0x33, 0x33, 0x33);
        cfg.title_inactive_bg_color = Some(pinned);
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), false, false),
            pinned
        );
    }

    /// Unfocused + broadcast mirrors the focused cascade
    /// (`title-receive-bg-color → resolved accent`), not a hardcoded `#0076c9`
    /// Terminator blue. The resolved accent defaults to the theme's signature
    /// accent (Mocha mauve). An explicit value still wins.
    #[test]
    fn broadcast_titlebar_derives_from_theme_accent() {
        let theme = Theme::by_name("Default"); // Catppuccin Mocha
        let mut cfg = Config::default();
        cfg.title_receive_bg_color = None;
        cfg.accent_color = None;
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), false, true),
            theme.accent
        );
        // accent_color wins over the theme fallback.
        let accent = Rgb::new(0x12, 0x34, 0x56);
        cfg.accent_color = Some(accent);
        assert_eq!(
            pick_titlebar_bg(&cfg, &theme, cfg.resolved_accent(&theme), false, true),
            accent
        );
    }
}

#[cfg(test)]
mod live_surface_dimension_tests;

#[cfg(test)]
mod cap_axis_cells_tests;

#[cfg(test)]
mod clamp_font_size_tests;

#[cfg(test)]
mod renderer_recovery_state_tests;

#[cfg(test)]
mod hidpi_scale_tests;

#[cfg(test)]
mod titlebar_glyph_fallback_tests;

#[cfg(test)]
mod pane_buffer_lifecycle_tests;

#[cfg(test)]
mod search_bar_tests;

#[cfg(test)]
mod completion_panel_tests;

#[cfg(test)]
mod startup_fonts_tests;

#[cfg(test)]
mod title_fit_tests;

#[cfg(test)]
mod settings_hit_test_tests;

#[cfg(test)]
mod background_image_geometry_tests;

#[cfg(test)]
mod inline_placement_budget_tests;

#[cfg(test)]
mod attributed_foreground_tests;

#[cfg(test)]
mod background_darkness_tests;

#[cfg(test)]
mod selection_row_span_tests;

#[cfg(test)]
mod run_attrs_tests;

#[cfg(test)]
mod settings_panel_cols_tests;

#[cfg(test)]
mod text_layout_damage_tests;

#[cfg(test)]
mod glyph_cell_lock_tests;

#[cfg(test)]
mod paragraph_separator_shaping_tests;

#[cfg(test)]
mod update_toast_tests;
