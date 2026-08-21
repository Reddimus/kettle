//! GPU renderer for pane grids, chrome, text, images, and overlays.
//!
//! Draw order is part of the contract: backgrounds and chrome, inline images,
//! pane text, post-text dimming/scrollbars, then menu chrome and text. Changing
//! it can break transparency, clipping, or overlay readability. See
//! `docs/ARCHITECTURE.md` for the full pipeline.
//!
//! [`capture_png`] renders an offscreen representative frame;
//! [`offscreen_selftest`] validates shader and pipeline creation without a
//! window surface.

mod bg_image;
mod color;
mod cursor_policy;
mod glyphpipe;
mod imgpipe;
mod outline;
mod present;
mod quad;
mod snapshot;
mod starfield;

pub use bg_image::{
    BgFrame, BgImage, bg_current_frame, decode_bg_image, decode_bg_image_frames,
    decode_bg_image_frames_with_blur, decode_bg_image_with_blur,
};
pub use snapshot::{PaneSnapshot, SnapCell};

use std::sync::Arc;

use alacritty_terminal::term::cell::Flags;
use anyhow::{Result, anyhow};
use glyphon::cosmic_text::{
    AttrsList, BufferLine, FeatureTag, FontFeatures, LineEnding, Wrap, fontdb,
};
use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution,
    Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
// `kettle_config::TextRenderer` (the grid|legacy mode enum) is aliased so it
// doesn't collide with glyphon's `TextRenderer` (the renderer) imported above.
use kettle_config::{Config, Rgb, ScrollbarMode, TextRenderer as TextRendererMode};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle};

pub use color::{
    dim as dim_color, reply_for_query, reply_for_text_area_size, resolve, resolve_query,
};
use glyphpipe::{GlyphClip, GlyphInstance, GlyphPipeline, RasterGlyph};
use outline::{OutlineInstance, OutlinePipeline};
use quad::{QuadInstance, QuadPipeline};

fn load_bundled_font(font_system: &mut FontSystem, face: &'static [u8]) {
    font_system
        .db_mut()
        .load_font_source(fontdb::Source::Binary(Arc::new(face)));
}

/// A search match in a pane's viewport.
///
/// `row` is always a viewport row, never an Alacritty grid line. Grid lines are
/// signed because history lives above line zero; callers should use
/// [`HighlightRect::from_grid_span`] instead of casting a grid line to `usize`.
/// Keeping that projection at this boundary prevents historical matches from
/// disappearing while the viewport is scrolled back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightRect {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub active: bool,
}

impl HighlightRect {
    /// Project a signed, grid-absolute match into the current viewport.
    ///
    /// Returns `None` when the span is above or below the visible screen. The
    /// addition is performed in `i64`, so a malicious/buggy offset cannot wrap
    /// a negative history line into a huge positive row.
    pub fn from_grid_span(
        grid_line: i32,
        display_offset: usize,
        screen_lines: usize,
        col: usize,
        width: usize,
        active: bool,
    ) -> Option<Self> {
        let row = i64::from(grid_line) + i64::try_from(display_offset).ok()?;
        let row = usize::try_from(row).ok()?;
        (row < screen_lines).then_some(Self {
            col,
            row,
            width: width.max(1),
            active,
        })
    }
}

/// Streaming lookup for sorted, non-overlapping viewport spans. The snapshot
/// cell walk and this cursor both move forward, keeping foreground projection
/// O(visible cells + visible spans) instead of O(cells × matches).
fn search_highlight_at(
    highlights: &[HighlightRect],
    cursor: &mut usize,
    row: i32,
    col: usize,
) -> Option<bool> {
    let row = usize::try_from(row).ok()?;
    while let Some(span) = highlights.get(*cursor) {
        let end = span.col.saturating_add(span.width.max(1));
        if span.row < row || (span.row == row && end <= col) {
            *cursor += 1;
        } else {
            break;
        }
    }
    let span = highlights.get(*cursor)?;
    (span.row == row && col >= span.col && col < span.col.saturating_add(span.width.max(1)))
        .then_some(span.active)
}

/// A hyperlink underline in a pane's viewport (grid coords).
#[derive(Clone, Copy)]
pub struct LinkRect {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub hover: bool,
}

// Named constants for the right-click context-menu chrome. These
// magic numbers (12.0 row-pad, 8.0 sep-h, 40.0 horiz-pad, 180.0
// min-w, 80.0 surface-breathing) used to be duplicated across 16
// sites in `kettle-render/src/lib.rs` + `kettle-ui/src/app.rs`; the
// duplication turned earlier layout-math changes into a 16-line
// search-and-replace instead of a 1-line edit. Re-exported so
// `kettle-ui` can pull them in via `use kettle_render::menu;`
// instead of redeclaring.
pub mod menu {
    /// Vertical padding inside each context-menu row. Cell-height +
    /// MENU_ROW_PAD = total row height (~28-32 px on default cell
    /// metrics — a comfortable click target).
    pub const ROW_PAD: f32 = 12.0;
    /// Separator row height. Smaller than a regular row so the menu
    /// reads as grouped without wasting vertical space.
    pub const SEP_H: f32 = 8.0;
    /// Horizontal padding inside the panel: `max_columns * cw + H_PAD`.
    /// Gives the longest label breathing room and lets short labels
    /// (Copy) still feel like a real menu surface.
    pub const H_PAD: f32 = 40.0;
    /// Minimum panel width — overrides the chars-based math when the
    /// longest label is tiny.
    pub const MIN_W: f32 = 180.0;
    /// Top + bottom breathing room reserved when clamping the panel
    /// height to the surface (for scrollable submenus). Keeps
    /// the menu from kissing the window edge.
    pub const PANEL_BREATHING: f32 = 80.0;
}

// v2.40.0 (tear-off UX): tab-drag rendering constants, single home — same
// rationale as `menu` above (the ghost/marker/highlight numbers were inline
// literals scattered across two crates before this). Gesture *thresholds*
// (arm distance, tear hysteresis) stay in kettle-ui with the drag FSM; only
// paint geometry/opacity lives here. Re-exported for kettle-ui via
// `kettle_render::tab_drag`.
pub mod tab_drag {
    /// Drag-ghost drop-shadow offset at rest (no tear pending).
    pub const GHOST_SHADOW_OFFSET_PX: f32 = 3.0;
    /// Drag-ghost drop-shadow opacity at rest.
    pub const GHOST_SHADOW_ALPHA: f32 = 0.30;
    /// Drag-ghost body opacity at rest (`theme.background` copy).
    pub const GHOST_BG_ALPHA: f32 = 0.85;
    /// Accent strip width on the ghost's leading edge — matches the live
    /// active segment's strip.
    pub const GHOST_ACCENT_W_PX: f32 = 2.0;

    /// Extra shadow offset at full tear-lift (`TabBar::tear_lift = 1.0`,
    /// cursor at the tear threshold): the ghost visibly "lifts off" the
    /// bar as a release becomes a tear.
    pub const GHOST_SHADOW_OFFSET_LIFT_PX: f32 = 5.0;
    /// Extra shadow opacity at full tear-lift.
    pub const GHOST_SHADOW_ALPHA_LIFT: f32 = 0.25;
    /// Body opacity *reduction* at full tear-lift — the ghost fades as it
    /// leaves the strip, signalling "this is coming out".
    pub const GHOST_BG_ALPHA_LIFT: f32 = 0.30;

    /// Re-dock insertion-marker line thickness (was a 2.0 inline literal;
    /// 3px survives dark-on-dark themes at a glance).
    pub const INSERT_MARKER_PX: f32 = 3.0;
    /// Square end-caps on the insertion marker — the "bullet" idiom the
    /// per-tab activity dot already uses (this renderer has no curves).
    pub const INSERT_MARKER_CAP_PX: f32 = 5.0;

    /// Dock-target band wash opacity while a torn window hovers a strip.
    /// The only latch signal every platform shares — the Windows-only
    /// torn-window translucency is additive on top of this.
    pub const DOCK_HIGHLIGHT_WASH_ALPHA: f32 = 0.14;
    /// Accent border thickness on the band's pane-facing edge.
    pub const DOCK_HIGHLIGHT_BORDER_PX: f32 = 2.0;
    /// Accent border opacity.
    pub const DOCK_HIGHLIGHT_BORDER_ALPHA: f32 = 0.55;
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
    /// Dropdown-parity: a right-aligned, dimmed shortcut hint
    /// (e.g. `Ctrl+Shift+1`). Empty = no hint. The App computes it from the
    /// LIVE keybind map so user rebinds show their actual chord.
    pub hint: String,
}

/// Dropdown-parity: a menu row's display-column budget — the label plus
/// its right-aligned shortcut hint (2 spacer columns between them). One
/// formula shared by the renderer's shape + draw passes; the App's
/// anchor-clamp and hit-test twins mirror it. Display width, rather than scalar
/// count, keeps CJK, emoji, and combining-mark labels inside the panel.
pub fn menu_row_chars(row: &ContextMenuRow) -> usize {
    use unicode_width::UnicodeWidthStr as _;

    row.label.width()
        + if row.hint.is_empty() {
            0
        } else {
            row.hint.width() + 2
        }
}

/// The production source of this file, excluding test-only items.
#[cfg(test)]
fn production_source() -> String {
    let production = kettle_test_support::production_source(include_str!("lib.rs"));
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
mod context_menu_row_width_tests {
    use super::{
        ContextMenu, ContextMenuRow, context_menu_clip_indicators, context_menu_panel_width,
        menu_chrome_quads, menu_row_chars,
    };

    #[test]
    fn row_budget_uses_display_columns() {
        let row = ContextMenuRow {
            label: "主题 👩‍💻".to_string(),
            separator: false,
            enabled: true,
            hint: "Ctrl+界".to_string(),
        };
        assert_eq!(
            menu_row_chars(&row),
            unicode_width::UnicodeWidthStr::width(row.label.as_str())
                + unicode_width::UnicodeWidthStr::width(row.hint.as_str())
                + 2
        );
    }

    #[test]
    fn app_clamped_width_is_authoritative_for_every_render_pass() {
        let row = ContextMenuRow {
            label: "short".to_string(),
            separator: false,
            enabled: true,
            hint: String::new(),
        };
        let menu = ContextMenu {
            anchor: (0.0, 0.0),
            rows: vec![row],
            highlight: 0,
            scroll_offset: 0,
            panel_w_clamped: 237.5,
            panel_h_clamped: 0.0,
        };
        assert_eq!(context_menu_panel_width(&menu, 9.6), 237.5);
    }

    #[test]
    fn scroll_indicators_follow_the_remaining_suffix() {
        let rows = [false, false, true, false, false]
            .into_iter()
            .enumerate()
            .map(|(index, separator)| ContextMenuRow {
                label: format!("row {index}"),
                separator,
                enabled: !separator,
                hint: String::new(),
            })
            .collect::<Vec<_>>();
        let row_h = 24.0;
        let sep_h = 8.0;
        let panel_h = 48.0;

        assert_eq!(
            context_menu_clip_indicators(&rows, 0, panel_h, row_h, sep_h),
            (false, true),
            "the first window has content below but none above"
        );
        assert_eq!(
            context_menu_clip_indicators(&rows, 2, panel_h, row_h, sep_h),
            (true, true),
            "the middle window has content on both sides"
        );
        assert_eq!(
            context_menu_clip_indicators(&rows, 3, panel_h, row_h, sep_h),
            (true, false),
            "the final two rows fit exactly, so no down indicator remains"
        );
        assert_eq!(
            context_menu_clip_indicators(&rows, rows.len(), panel_h, row_h, sep_h),
            (true, false),
            "an out-of-range offset is clamped to the empty suffix"
        );
    }

    #[test]
    fn tiny_or_invalid_menu_never_emits_negative_or_nonfinite_quads() {
        let menu = ContextMenu {
            anchor: (0.0, 0.0),
            rows: vec![ContextMenuRow {
                label: String::new(),
                separator: true,
                enabled: false,
                hint: String::new(),
            }],
            highlight: 0,
            scroll_offset: 0,
            panel_w_clamped: 1.0,
            panel_h_clamped: 1.0,
        };
        let theme = kettle_config::Theme::default();
        let quads = menu_chrome_quads(&menu, &theme, kettle_config::Rgb::new(1, 2, 3), 8.0, 16.0);
        assert!(!quads.is_empty());
        assert!(quads.iter().all(|quad| {
            quad.pos.iter().all(|value| value.is_finite())
                && quad
                    .size
                    .iter()
                    .all(|value| value.is_finite() && *value >= 0.0)
        }));

        let invalid = ContextMenu {
            anchor: (f32::NAN, 0.0),
            ..menu
        };
        assert!(
            menu_chrome_quads(
                &invalid,
                &theme,
                kettle_config::Rgb::new(1, 2, 3),
                8.0,
                16.0
            )
            .is_empty()
        );
    }
}

/// Text/layout damage key for the context-menu renderer.
///
/// The highlighted row is deliberately excluded: moving the pointer changes
/// only a cheap quad. Text must be prepared again when its content, color,
/// position, or visible scroll window changes.
fn context_menu_text_damage_key(
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

fn prepared_text_areas_damage_key(areas: &[TextArea<'_>]) -> u64 {
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
fn completion_text_damage_key(
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
fn text_overlay_requires_continuous_prepare(overlay: &Overlay) -> bool {
    overlay.search.is_some()
        || overlay.search_query.is_some()
        || !overlay.hint_labels.is_empty()
        || overlay.ime_preedit.is_some()
        || overlay.ssh_query.is_some()
        || overlay.palette_query.is_some()
        || overlay.layout_picker_query.is_some()
        || overlay.edit_title.is_some()
        || overlay.confirm_dialog.is_some()
        || overlay.settings.is_some()
        || overlay.resize_overlay.is_some()
        || overlay.update_available.is_some()
}

/// Disabled / secondary menu text: blend the foreground toward the panel
/// background (~55% mute) without alpha-blending through to whatever lives
/// under the panel.
fn dim_blend(fg: Rgb, bg: Rgb) -> Rgb {
    Rgb::new(
        ((fg.r as u16 + bg.r as u16 * 5) / 6) as u8,
        ((fg.g as u16 + bg.g as u16 * 5) / 6) as u8,
        ((fg.b as u16 + bg.b as u16 * 5) / 6) as u8,
    )
}

/// Opaque color mixture used for UI surfaces that must remain legible over a
/// translucent terminal window. `source_percent` is clamped to 0..=100; the
/// result is pre-blended rather than alpha-composited with terminal text.
fn solid_blend(source: Rgb, target: Rgb, source_percent: u16) -> Rgb {
    let source_percent = source_percent.min(100);
    let target_percent = 100 - source_percent;
    let channel = |source: u8, target: u8| {
        ((u16::from(source) * source_percent + u16::from(target) * target_percent + 50) / 100) as u8
    };
    Rgb::new(
        channel(source.r, target.r),
        channel(source.g, target.g),
        channel(source.b, target.b),
    )
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
    /// Index of the first row the renderer should paint (Terminator
    /// menu UX parity). Rows `0..scroll_offset` are scrolled
    /// off-panel; the renderer also stops drawing when the
    /// accumulated row height exceeds `panel_h_clamped`. Zero means
    /// "show from the top" (the default before scrollable submenus).
    pub scroll_offset: usize,
    /// Panel width after the surface clamp. Zero means use natural width.
    pub panel_w_clamped: f32,
    /// Panel height after the surface clamp (App-side
    /// `context_menu_geometry` already applies the clamp); the
    /// renderer reuses it to decide which rows are visible + to
    /// position the ▲/▼ arrows. Zero means "no clamp", in which
    /// case the renderer falls back to the natural panel height.
    pub panel_h_clamped: f32,
}

/// Width shared by context-menu buffer preparation, text placement, chrome,
/// and headless capture. A nonzero App-supplied width is authoritative because
/// it already accounts for surface clamping and label ellipsis; recomputing a
/// smaller natural width would desynchronize paint bounds from hit testing.
fn context_menu_panel_width(menu: &ContextMenu, cell_width: f32) -> f32 {
    let width = if menu.panel_w_clamped > 0.0 {
        menu.panel_w_clamped
    } else {
        let max_columns = menu
            .rows
            .iter()
            .filter(|row| !row.separator)
            .map(menu_row_chars)
            .max()
            .unwrap_or(0) as f32;
        (max_columns * cell_width + menu::H_PAD).max(menu::MIN_W)
    };
    if width.is_finite() {
        width.max(1.0)
    } else {
        1.0
    }
}

fn context_menu_clip_indicators(
    rows: &[ContextMenuRow],
    scroll_offset: usize,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> (bool, bool) {
    let start = scroll_offset.min(rows.len());
    let remaining_h: f32 = rows[start..]
        .iter()
        .map(|row| if row.separator { sep_h } else { row_h })
        .sum();
    (start > 0, remaining_h > panel_h)
}

/// Title-edit overlay projected by the UI.
pub struct TitleEditOverlay {
    pub label: String,
    pub input: String,
    pub rect: Rect4,
}

/// Active input-method composition projected over the focused terminal cursor.
pub struct ImePreedit {
    pub text: String,
    pub row: usize,
    pub col: usize,
}

/// Case policy shown by the search bar and applied by the UI's search engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchCaseMode {
    /// Match case only when the query contains an uppercase character.
    #[default]
    Smart,
    /// Always match case.
    Match,
    /// Always ignore case.
    Ignore,
}

impl SearchCaseMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Smart => "Smart",
            Self::Match => "Match",
            Self::Ignore => "Ignore",
        }
    }
}

/// Bounded semantic search feedback. Deliberately carries no unbounded regex
/// error string: diagnostic details belong in logs, while chrome stays stable
/// and cannot expose the query through accessibility/automation snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchStatus {
    #[default]
    Typing,
    Searching,
    Match,
    Wrapped,
    Start,
    End,
    NoMatch,
    Limited,
    Invalid,
    TooComplex,
    TooLong,
}

impl SearchStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Typing => "Type to search",
            Self::Searching => "Searching…",
            Self::Match => "Match",
            Self::Wrapped => "Wrapped",
            Self::Start => "Start reached",
            Self::End => "End reached",
            Self::NoMatch => "No match",
            Self::Limited => "Results limited",
            Self::Invalid => "Invalid pattern",
            Self::TooComplex => "Pattern too complex",
            Self::TooLong => "Query too long",
        }
    }
}

/// Focusable elements in the search lane. This renderer-owned enum is shared
/// by paint geometry, pointer hit-testing, and the UI's AccessKit projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SearchControl {
    #[default]
    Editor,
    Previous,
    Next,
    Wrap,
    Case,
    Invert,
    Close,
}

impl SearchControl {
    pub const ALL: [Self; 7] = [
        Self::Editor,
        Self::Previous,
        Self::Next,
        Self::Wrap,
        Self::Case,
        Self::Invert,
        Self::Close,
    ];

    pub const fn accessible_label(self) -> &'static str {
        match self {
            Self::Editor => "Search expression",
            Self::Previous => "Previous match",
            Self::Next => "Next match",
            Self::Wrap => "Wrap search",
            Self::Case => "Search case mode",
            Self::Invert => "Invert default search direction",
            Self::Close => "Close search",
        }
    }
}

/// Full renderer projection for the in-window search lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOverlay {
    /// Stable pane identity owning the terminal-coordinate highlight spans.
    /// Chrome remains window-local, but a focus change must never project one
    /// pane's signed grid coordinates onto another pane.
    pub target_pane: Option<u64>,
    pub query: String,
    /// UTF-8 byte offset of the editor caret. Invalid offsets are clamped to the
    /// nearest preceding character boundary during paint.
    pub cursor_byte: usize,
    /// Optional UTF-8 byte range selected in the editor.
    pub selection: Option<(usize, usize)>,
    /// Number of display columns intentionally hidden on the left.
    pub horizontal_scroll: usize,
    pub wrap: bool,
    pub case_mode: SearchCaseMode,
    pub invert: bool,
    pub status: SearchStatus,
    pub focused: SearchControl,
}

/// A shell-owned completion list projected over the focused pane. Kettle only
/// presents these rows; accepting or executing a candidate remains the shell's
/// job.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionOverlay {
    pub pane_rect: (f32, f32, f32, f32),
    /// Exact terminal grid bounds inside `pane_rect`, excluding padding and a
    /// pane title bar. The completion card stays inside this grid whether it
    /// opens above or below the active command.
    pub grid_rect: (f32, f32, f32, f32),
    /// Inclusive screen-row span from OSC 133 prompt start through the shell
    /// cursor. Geometry must stay outside it, including multiline prompts and
    /// wrapped input.
    pub command_rows: (usize, usize),
    /// Stable start column of the editable command, captured before candidate
    /// insertion moved the cursor. Legacy publishers fall back to the grid
    /// edge.
    pub anchor_col: Option<usize>,
    pub kind: String,
    pub source: String,
    pub selected: Option<usize>,
    /// Total rows in the shell result; `candidates` is one bounded page.
    pub total: usize,
    /// Protocol v4 emphasis hint: the token the shell was completing. Used only
    /// to tint the first literal occurrence inside a label. Kettle never
    /// filters, ranks, quotes, or inserts anything from it.
    pub token: Option<String>,
    pub candidates: Vec<CompletionOverlayRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionOverlayRow {
    pub label: String,
    pub description: String,
    /// Zero-based position in the complete shell result.
    pub position: usize,
}

/// User-facing media details that are safe to project into the renderer.
/// Paths remain in the UI layer.
#[derive(Clone, Debug)]
pub enum MediaPasteReceiptKind {
    Image {
        original_width: u32,
        original_height: u32,
    },
    Video {
        extension: String,
        size: u64,
        count: usize,
        preview_pending: bool,
    },
}

/// A short-lived receipt for media paths accepted by the initiating pane.
/// The renderer receives bounded pixels and display metadata, never a path.
#[derive(Clone, Debug)]
pub struct MediaPasteReceiptOverlay {
    pub pane_rect: Rect4,
    pub grid_rect: Rect4,
    /// Physical pixels reserved for the focused pane's live scrollbar grab
    /// strip. The receipt stays left of this edge instead of painting an
    /// apparently clickable card over another control.
    pub right_gutter: f32,
    pub image: Option<kettle_core::ImageData>,
    pub kind: MediaPasteReceiptKind,
    /// Only Kettle's retained private image copy can be opened from the card.
    /// Video sources remain informational because native launch APIs take a
    /// path and cannot be bound to the file handle used for validation.
    pub openable: bool,
    pub remote: bool,
    pub expanded: bool,
    /// Keep the card away from the live prompt: top when the cursor is in the
    /// lower half of the grid, bottom when it is in the upper half.
    pub prefer_top: bool,
}

impl Default for SearchOverlay {
    fn default() -> Self {
        Self {
            target_pane: None,
            query: String::new(),
            cursor_byte: 0,
            selection: None,
            horizontal_scroll: 0,
            wrap: true,
            case_mode: SearchCaseMode::Smart,
            invert: false,
            status: SearchStatus::Typing,
            focused: SearchControl::Editor,
        }
    }
}

/// Shared paint/input/accessibility geometry for the reserved search lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchBarGeometry {
    pub rect: Rect4,
    pub rows: usize,
    pub reserved_height: f32,
    pub label: Rect4,
    pub editor: Rect4,
    pub previous: Rect4,
    pub next: Rect4,
    pub wrap: Rect4,
    pub case_mode: Rect4,
    pub invert: Rect4,
    pub status: Rect4,
    pub close: Rect4,
}

impl SearchBarGeometry {
    pub const fn control_rect(self, control: SearchControl) -> Rect4 {
        match control {
            SearchControl::Editor => self.editor,
            SearchControl::Previous => self.previous,
            SearchControl::Next => self.next,
            SearchControl::Wrap => self.wrap,
            SearchControl::Case => self.case_mode,
            SearchControl::Invert => self.invert,
            SearchControl::Close => self.close,
        }
    }

    /// Resolve a pointer position using the exact rectangles used for paint.
    pub fn hit_test(self, x: f32, y: f32) -> Option<SearchControl> {
        SearchControl::ALL.into_iter().find(|control| {
            let (rx, ry, rw, rh) = self.control_rect(*control);
            rw > 0.0 && rh > 0.0 && x >= rx && x < rx + rw && y >= ry && y < ry + rh
        })
    }

    /// Terminal content rectangle after reserving the lane. The x/width are
    /// unchanged; height saturates at zero for extremely short surfaces.
    pub fn content_rect(self, surface_width: f32, surface_height: f32) -> Rect4 {
        (
            0.0,
            0.0,
            surface_width.max(0.0),
            (surface_height - self.reserved_height).max(0.0),
        )
    }
}

/// Search-bar + hyperlink overlay state.
#[derive(Default)]
pub struct Overlay {
    /// Rich v2.38 search-lane projection. When present it takes precedence over
    /// the legacy `search_query` fields below.
    pub search: Option<SearchOverlay>,
    /// Compatibility shim for callers predating [`SearchOverlay`]. New callers
    /// should leave these three fields at their defaults.
    pub search_query: Option<String>,
    pub search_count: usize,
    pub search_index: usize,
    /// Visible, non-overlapping match spans in `(row, col)` order. Keeping this
    /// list viewport-bounded lets both quad paint and glyph recoloring stay
    /// linear in visible work.
    pub highlights: Vec<HighlightRect>,
    pub links: Vec<LinkRect>,
    /// Quick-select hint labels (drawn over the focused pane).
    pub hint_labels: Vec<HintLabel>,
    /// Input-method preedit text drawn at the focused terminal cursor.
    pub ime_preedit: Option<ImePreedit>,
    /// Shell completion card shown near the top of the focused pane.
    pub completion: Option<CompletionOverlay>,
    /// Visual receipt for the focused pane's most recent clipboard bitmap.
    pub media_paste_receipt: Option<MediaPasteReceiptOverlay>,
    /// `Some(typed)` while the SSH launcher is open.
    pub ssh_query: Option<String>,
    pub ssh_hint: String,
    /// `Some(typed)` while the command palette is open.
    pub palette_query: Option<String>,
    /// The ranked command labels (selected one marked) for the palette.
    pub palette_hint: String,
    /// Terminator parity (`layoutlauncher.py`): `Some(typed)` while
    /// the layout picker is open. Same UX surface as the command
    /// palette but the hint string lists layout names from
    /// `Session::list_layouts`.
    pub layout_picker_query: Option<String>,
    pub layout_picker_hint: String,
    /// Terminator parity (edit-title overlay UX): the in-progress
    /// title-edit text + a scope label and chrome rect.
    /// `None` when no edit is in progress.
    pub edit_title: Option<TitleEditOverlay>,
    /// Window has keyboard focus. Terminal cursors are suppressed entirely
    /// while false and resume from the unchanged DEC state when focus returns.
    pub window_focused: bool,
    /// v2.26.0: the focused pane's scrollbar should paint in its bright
    /// (interacting) state — the pointer is hovering the scrollbar gutter or the
    /// thumb is being dragged. At rest the bar is drawn dim; being scrolled back
    /// (`display_offset > 0`) also brightens it, decided per-pane in the renderer
    /// from the snapshot, so this only needs to carry the hover/drag signal.
    pub scrollbar_active: bool,
    /// Cursor is in its "on" blink phase.
    pub cursor_visible: bool,
    /// Visual-bell intensity, 0.0 (none) .. 1.0 (just rang).
    pub bell: f32,
    /// `Some` while the right-click context menu is open. Rendered on
    /// top of everything else so an overlapping pane border doesn't
    /// occlude the menu.
    pub context_menu: Option<ContextMenu>,
    /// Phase 3 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
    /// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md): when
    /// `Some`, render a centered modal dialog over a dimming
    /// backdrop. The renderer paints the prompt + button row;
    /// the button at `focus_idx` gets the accent-border treatment.
    pub confirm_dialog: Option<ConfirmDialogOverlay>,
    /// `Some` while the in-app settings overlay is open. Painted
    /// centered, above panes but below the confirm dialog.
    pub settings: Option<SettingsOverlay>,
    /// v2.20.0 (Ghostty `resize-overlay` parity): `Some((cols, rows))` while
    /// the transient size chip should paint (the app owns the timing; the
    /// renderer just draws a centered `cols×rows` chip above everything).
    pub resize_overlay: Option<(u16, u16)>,
    /// `Some((tag, url))` while the "a newer kettle release is
    /// available" banner is showing. Rendered as a passive, lowest-priority
    /// bottom bar — any real modal (search/palette/…) takes the bar instead,
    /// and it returns when they close. Dismissed with Esc, opened with Enter.
    pub update_available: Option<(String, String)>,
    /// Terminator parity (drag a terminal elsewhere in its tab): `Some(rect)`
    /// while a live pane drag has a drop target latched. The rect is the half
    /// of the target pane the dropped terminal would take, washed in accent
    /// with a border — the same UI-computes-geometry contract as
    /// [`TabBar::insert_marker`], so the renderer never has to know how splits
    /// are laid out.
    pub pane_drop_hint: Option<Rect4>,
}

#[inline]
fn cursor_focus_gate(window_focused: bool, terminal_requests_cursor: bool) -> bool {
    window_focused && terminal_requests_cursor
}

/// Renderer-side projection of `App::confirm_dialog`.
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

/// Paint-side button shape. `destructive: true` gets
/// the red-accent treatment (Close / Delete buttons).
#[derive(Debug, Clone)]
pub struct ConfirmDialogButton {
    pub label: String,
    pub destructive: bool,
}

/// Renderer-side projection of `App::settings_nav` + the resolved
/// field values. The UI computes labels/values (reading `Config`); the renderer
/// just paints a centered panel — a row of category tabs, then label/value
/// rows for the active category, with the focused row highlighted.
// `PartialEq` (audit P1b fix): lets the renderer memoize
// `settings_display_lines`'s output against the last `SettingsOverlay` it was
// computed from, instead of re-running a `format!()` per display line on
// every painted frame the Settings overlay stays open.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsOverlay {
    /// Category tab names, in order.
    pub categories: Vec<String>,
    /// Index of the active category (its tab is highlighted, its rows shown).
    pub active_category: usize,
    /// The active category's fields as (label, current-value) pairs.
    pub rows: Vec<SettingsRow>,
    /// Index into `rows` of the focused field (gets the accent highlight).
    pub focused_row: usize,
    /// v2.20.0: `cfg.vim_menu_nav` — the footer hint advertises the vim keys
    /// when the setting is on.
    pub vim_nav: bool,
    /// v2.23.0: an optional contextual note shown below the keybind footer —
    /// e.g. the Graphics category's "Active GPU: … • ⚠ restart to apply". `None`
    /// on categories that don't need it.
    pub footer_note: Option<String>,
}

/// One settings row — a human label and its current value string.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRow {
    pub label: String,
    pub value: String,
    /// v2.24.0: `true` when this row doesn't apply to the current state (e.g. the
    /// image path while `background-type != image`) — drawn dimmed and skipped by
    /// keyboard/mouse nav.
    pub disabled: bool,
}

/// Pixel rectangle `(x, y, w, h)`.
pub type Rect4 = (f32, f32, f32, f32);

/// Visible candidate rows in one completion card.
const MAX_COMPLETION_ROWS: usize = 10;
/// Overall card width in grid columns. The published page picks a content-fit
/// width inside this band so the card neither shrink-wraps to an unreadable
/// sliver nor spans a wide pane.
const COMPLETION_MIN_COLUMNS: usize = 20;
const COMPLETION_MAX_COLUMNS: usize = 96;
const COMPLETION_MIN_LABEL_COLUMNS: usize = 8;
const COMPLETION_MAX_LABEL_COLUMNS: usize = 40;
const COMPLETION_MAX_DESCRIPTION_COLUMNS: usize = 48;
/// One column of inner padding on each side of the card.
const COMPLETION_PAD_COLUMNS: usize = 1;
/// Two columns between the label and description lanes; the hairline divider
/// sits on the boundary between them.
const COMPLETION_DIVIDER_COLUMNS: usize = 2;
const COMPLETION_BORDER: f32 = 1.0;
/// Right-edge scroll track/thumb, drawn only when the shell result is longer
/// than the visible rows.
const COMPLETION_SCROLL_W: f32 = 2.0;
/// Leading accent rail on the selected row only.
const COMPLETION_RAIL_W: f32 = 2.0;
/// Minimum separation between the selected row's surface and the panel body
/// before Kettle stops trusting the theme's own selection color.
const COMPLETION_SELECTION_SEPARATION: f64 = 1.35;

#[derive(Debug, Clone, Copy, PartialEq)]
struct CompletionPanelGeometry {
    /// The whole card, header and padding included.
    rect: Rect4,
    /// Header band inside the card. Part of the list container, never a row.
    header: Rect4,
    /// Top of the first painted candidate row.
    list_top: f32,
    row_h: f32,
    first: usize,
    rows: usize,
    /// Card width minus its side padding, in columns. The header lanes and the
    /// candidate lanes are both cut out of this.
    inner_columns: usize,
    label_x: f32,
    label_w: f32,
    label_columns: usize,
    /// Divider hairline abscissa; `None` when the page has no descriptions.
    divider_x: Option<f32>,
    description_x: f32,
    description_w: f32,
    description_columns: usize,
    placement: CompletionPanelPlacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionPanelPlacement {
    Above,
    Below,
}

impl CompletionPanelGeometry {
    /// Band occupied by the painted candidate rows.
    fn list_rect(&self) -> Rect4 {
        (
            self.rect.0,
            self.list_top,
            self.rect.2,
            self.rows as f32 * self.row_h,
        )
    }
}

/// Left header caption, e.g. `Completions · fish`.
fn completion_header_label(overlay: &CompletionOverlay) -> String {
    format!("{} · {}", overlay.kind, overlay.source)
}

/// Right header caption. A selected candidate reports its absolute position in
/// the shell result; otherwise the result size is spelled out.
fn completion_header_count(overlay: &CompletionOverlay) -> String {
    if let Some(selected) = overlay
        .selected
        .and_then(|index| overlay.candidates.get(index))
    {
        return format!("{}/{}", selected.position.saturating_add(1), overlay.total);
    }
    if overlay.total == 1 {
        "1 match".to_string()
    } else {
        format!("{} matches", overlay.total)
    }
}

fn completion_panel_geometry(
    overlay: &CompletionOverlay,
    cell: (f32, f32),
) -> Option<CompletionPanelGeometry> {
    let count = overlay.candidates.len();
    if count == 0 {
        return None;
    }
    let (cw, ch) = cell;
    if !cw.is_finite() || !ch.is_finite() || cw <= 0.0 || ch <= 0.0 {
        return None;
    }
    let (px, py, pw, ph) = overlay.pane_rect;
    let (gx, gy, gw, gh) = overlay.grid_rect;
    let left = px.max(gx);
    let right = (px + pw).min(gx + gw);
    let top = py.max(gy);
    let bottom = (py + ph).min(gy + gh);
    let available_w = (right - left).max(0.0);
    let grid_columns = (available_w / cw).floor().max(0.0) as usize;
    if grid_columns < COMPLETION_MIN_COLUMNS {
        return None;
    }

    // Vertical rhythm. A half-cell gap separates the card from the prompt and
    // from the top of the grid; rows are a little taller than a terminal line
    // so the labels are not boxed in. Every step is rounded to whole pixels so
    // the card stays crisp at fractional DPI.
    let half_cell = (ch * 0.5).round().max(1.0);
    let gap = half_cell.max(4.0);
    let row_h = (ch + 4.0).max((ch * 1.35).round());
    let header_h = row_h;
    // Restrained padding above and below the candidate rows. Deriving it from
    // the row's own leading keeps every glyph baseline optically centered: the
    // header and the first row are separated by exactly the same slack that
    // centers text inside a row.
    let list_pad = ((row_h - ch) * 0.5).round().max(2.0);
    let chrome_h = 2.0 * COMPLETION_BORDER + header_h + 2.0 * list_pad;

    // Prefer the detached lane above the command. If that lane cannot show the
    // requested page and the lane below can show more, flip the whole card
    // below the final wrapped row. Both lanes stay inside the terminal grid,
    // so the card never crosses pane chrome or the window's tab bar.
    let grid_rows = ((bottom - top) / ch).floor().max(0.0) as usize;
    if grid_rows == 0 {
        return None;
    }
    let command_start = overlay
        .command_rows
        .0
        .min(overlay.command_rows.1)
        .min(grid_rows - 1);
    let command_end = overlay
        .command_rows
        .0
        .max(overlay.command_rows.1)
        .min(grid_rows - 1);
    let above_bottom = top + command_start as f32 * ch - gap;
    let below_top = top + (command_end + 1) as f32 * ch + gap;
    let above_h = above_bottom - (top + half_cell);
    let below_h = (bottom - half_cell) - below_top;
    let capacity = |lane_h: f32| ((lane_h - chrome_h) / row_h).floor().max(0.0) as usize;
    let above_capacity = capacity(above_h).min(MAX_COMPLETION_ROWS);
    let below_capacity = capacity(below_h).min(MAX_COMPLETION_ROWS);
    let wanted_rows = count.min(MAX_COMPLETION_ROWS);
    let placement = if above_capacity >= wanted_rows {
        CompletionPanelPlacement::Above
    } else if below_capacity > above_capacity {
        CompletionPanelPlacement::Below
    } else if above_capacity > 0 {
        CompletionPanelPlacement::Above
    } else if below_capacity > 0 {
        CompletionPanelPlacement::Below
    } else {
        return None;
    };
    let rows = wanted_rows.min(match placement {
        CompletionPanelPlacement::Above => above_capacity,
        CompletionPanelPlacement::Below => below_capacity,
    });
    if rows == 0 {
        return None;
    }

    // Keep one row of lookahead under the selection so the next candidate is
    // already on screen when the user presses Tab again.
    let selected = overlay.selected.unwrap_or(0).min(count - 1);
    let first = selected
        .saturating_add(2)
        .saturating_sub(rows)
        .min(count - rows)
        .min(selected);

    // Content fit is computed over the whole published page, not just the
    // visible rows, so cycling inside one page never resizes the card.
    let label_columns = overlay
        .candidates
        .iter()
        .map(|candidate| display_width(&candidate.label))
        .max()
        .unwrap_or(0)
        .clamp(COMPLETION_MIN_LABEL_COLUMNS, COMPLETION_MAX_LABEL_COLUMNS);
    let description_columns = overlay
        .candidates
        .iter()
        .map(|candidate| display_width(&candidate.description))
        .max()
        .unwrap_or(0)
        .min(COMPLETION_MAX_DESCRIPTION_COLUMNS);
    let divider_columns = if description_columns > 0 {
        COMPLETION_DIVIDER_COLUMNS
    } else {
        0
    };
    let padding_columns = 2 * COMPLETION_PAD_COLUMNS;
    let content_columns = padding_columns + label_columns + divider_columns + description_columns;
    // The header is part of the card, so it participates in the content fit.
    let header_columns = padding_columns
        + display_width(&completion_header_label(overlay))
        + 2
        + display_width(&completion_header_count(overlay));
    let columns = content_columns
        .max(header_columns)
        .clamp(COMPLETION_MIN_COLUMNS, COMPLETION_MAX_COLUMNS)
        .min(grid_columns);

    // Redistribute after the clamp so paint, text preparation, and hit testing
    // agree on the lane split even when the grid is narrower than the content.
    let inner_columns = columns - padding_columns;
    let mut divider_columns = divider_columns.min(inner_columns);
    let mut label_columns = label_columns.min(inner_columns - divider_columns);
    let mut description_columns = inner_columns - divider_columns - label_columns;
    if description_columns == 0 {
        // A clamped card with no description lane must not keep a detached
        // hairline and two dead columns at its right edge. Give those columns
        // back to the label and omit the divider entirely.
        divider_columns = 0;
        label_columns = label_columns
            .saturating_add(COMPLETION_DIVIDER_COLUMNS)
            .min(inner_columns);
        description_columns = inner_columns - label_columns;
    }

    let width = columns as f32 * cw;
    let height = chrome_h + rows as f32 * row_h;
    // IDE completion follows the editable command column, not the decorative
    // prompt. Clamp toward the left only when a right-edge command would push
    // the content-fitted card outside the grid.
    let preferred_x = left + overlay.anchor_col.unwrap_or(0).min(grid_columns) as f32 * cw;
    let x = preferred_x.min((right - width).max(left));
    let y = match placement {
        CompletionPanelPlacement::Above => above_bottom - height,
        CompletionPanelPlacement::Below => below_top,
    };
    let label_x = x + COMPLETION_PAD_COLUMNS as f32 * cw;
    let label_w = label_columns as f32 * cw;
    // One free column on each side of the hairline keeps the two lanes from
    // reading as a table rule.
    let divider_x = (divider_columns > 0).then(|| (label_x + label_w + cw).round());
    let description_x = x + (COMPLETION_PAD_COLUMNS + label_columns + divider_columns) as f32 * cw;
    Some(CompletionPanelGeometry {
        rect: (x, y, width, height),
        header: (x, y + COMPLETION_BORDER, width, header_h),
        list_top: y + COMPLETION_BORDER + header_h + list_pad,
        row_h,
        first,
        rows,
        inner_columns,
        label_x,
        label_w,
        label_columns,
        divider_x,
        description_x,
        description_w: description_columns as f32 * cw,
        description_columns,
        placement,
    })
}

/// Header lane split in columns: the right-aligned count is served first and
/// the caption takes what is left, minus one separating column.
fn completion_header_columns(geometry: &CompletionPanelGeometry, count: &str) -> (usize, usize) {
    let count_columns = display_width(count).min(geometry.inner_columns);
    (
        geometry.inner_columns.saturating_sub(count_columns + 1),
        count_columns,
    )
}

/// Every color the completion card paints. Buffer preparation shapes the
/// emphasis run with an explicit color, so both passes must derive them from
/// one place or a selected row would keep the unselected tint.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CompletionPalette {
    panel_bg: Rgb,
    border: Rgb,
    divider: Rgb,
    selection_bg: Rgb,
    rail: Rgb,
    header: Rgb,
    label: Rgb,
    description: Rgb,
    emphasis: Rgb,
    selected_label: Rgb,
    selected_description: Rgb,
    selected_emphasis: Rgb,
    scroll_track: Rgb,
    scroll_thumb: Rgb,
}

fn completion_palette(theme: &kettle_config::Theme, accent: Rgb) -> CompletionPalette {
    let panel_bg = solid_blend(theme.foreground, theme.background, 9);
    let selection_bg = completion_selection_surface(theme.selection_background, panel_bg, accent);
    // A blended surface is no longer the pair the theme designed its selection
    // foreground for, so fall back to the ordinary text color there.
    let selected_fg = if selection_bg == theme.selection_background {
        theme.selection_foreground
    } else {
        theme.foreground
    };
    CompletionPalette {
        panel_bg,
        border: solid_blend(theme.foreground, theme.background, 30),
        divider: solid_blend(theme.foreground, panel_bg, 16),
        selection_bg,
        rail: color::with_min_contrast(accent, selection_bg, 2.0),
        header: color::with_min_contrast(color::dim(theme.foreground, panel_bg), panel_bg, 4.5),
        label: color::with_min_contrast(theme.foreground, panel_bg, 4.5),
        description: color::with_min_contrast(
            color::dim(theme.foreground, panel_bg),
            panel_bg,
            4.0,
        ),
        emphasis: color::with_min_contrast(accent, panel_bg, 4.5),
        selected_label: color::with_min_contrast(selected_fg, selection_bg, 4.5),
        selected_description: color::with_min_contrast(
            color::dim(selected_fg, selection_bg),
            selection_bg,
            4.0,
        ),
        selected_emphasis: color::with_min_contrast(accent, selection_bg, 4.5),
        scroll_track: solid_blend(theme.foreground, panel_bg, 12),
        scroll_thumb: solid_blend(theme.foreground, panel_bg, 45),
    }
}

fn push_completion_selection_quads(
    quads: &mut Vec<QuadInstance>,
    selected: bool,
    geometry: &CompletionPanelGeometry,
    row_y: f32,
    scroll_inset: f32,
    palette: &CompletionPalette,
) {
    if !selected {
        return;
    }
    let (x, _, width, _) = geometry.rect;
    // Rows never touch the card border, so the active surface spans the full
    // row height with no notch to inset around.
    quads.push(rect(
        x + COMPLETION_BORDER,
        row_y,
        (width - COMPLETION_BORDER * 2.0 - scroll_inset).max(0.0),
        geometry.row_h,
        palette.selection_bg,
        1.0,
    ));
    quads.push(rect(
        x + COMPLETION_BORDER,
        row_y,
        COMPLETION_RAIL_W,
        geometry.row_h,
        palette.rail,
        1.0,
    ));
}

/// Top and height of the scroll thumb, or `None` when the whole shell result
/// already fits the visible rows.
fn completion_scroll_thumb(
    overlay: &CompletionOverlay,
    geometry: &CompletionPanelGeometry,
) -> Option<(f32, f32)> {
    let total = overlay.total;
    if total <= geometry.rows {
        return None;
    }
    let (_, track_y, _, track_h) = geometry.list_rect();
    if track_h <= 0.0 {
        return None;
    }
    let first_position = overlay
        .candidates
        .get(geometry.first)
        .map(|candidate| candidate.position)
        .unwrap_or(geometry.first)
        .min(total.saturating_sub(geometry.rows));
    let visible = geometry.rows as f32 / total as f32;
    // Keep a short thumb legible on a very long result.
    let thumb_h = (track_h * visible)
        .max(row_thumb_floor(geometry.row_h))
        .min(track_h);
    let travel = (track_h - thumb_h).max(0.0);
    let progress = if total > geometry.rows {
        first_position as f32 / (total - geometry.rows) as f32
    } else {
        0.0
    };
    let thumb_y = track_y + (travel * progress.clamp(0.0, 1.0)).round();
    Some((thumb_y, thumb_h.round().max(1.0)))
}

fn row_thumb_floor(row_h: f32) -> f32 {
    (row_h * 0.5).round().max(4.0)
}

/// Selected-row surface. The theme's own selection color is preferred; a theme
/// whose selection sits on top of the panel body would erase the active row, so
/// the window accent is blended in until the two surfaces separate.
fn completion_selection_surface(theme_selection: Rgb, panel_bg: Rgb, accent: Rgb) -> Rgb {
    if color::contrast_ratio(theme_selection, panel_bg) >= COMPLETION_SELECTION_SEPARATION {
        return theme_selection;
    }
    for percent in [30_u16, 45, 60, 80, 100] {
        let blended = solid_blend(accent, panel_bg, percent);
        if color::contrast_ratio(blended, panel_bg) >= COMPLETION_SELECTION_SEPARATION {
            return blended;
        }
    }
    // An accent that cannot separate on its own still has to yield a visible
    // row; drive it toward the reachable endpoint instead.
    color::with_min_contrast(accent, panel_bg, COMPLETION_SELECTION_SEPARATION)
}

/// Byte range of the emphasis token inside an already-fitted label.
///
/// Kettle never filters, ranks, quotes, or inserts from the token: this is the
/// only thing it is allowed to do with one. Pure ASCII compares
/// case-insensitively because shells complete case-insensitively in practice;
/// anything else needs an exact substring, since case folding a script Kettle
/// does not analyze can change byte lengths and split a grapheme.
fn completion_match_span(label: &str, token: &str) -> Option<(usize, usize)> {
    if token.is_empty() || token.len() > label.len() {
        return None;
    }
    if label.is_ascii() && token.is_ascii() {
        let haystack = label.as_bytes();
        let needle = token.as_bytes();
        return (0..=haystack.len() - needle.len())
            .find(|start| haystack[*start..*start + needle.len()].eq_ignore_ascii_case(needle))
            .map(|start| (start, start + needle.len()));
    }
    label.find(token).map(|start| (start, start + token.len()))
}

/// Bounds of the visible completion card. Shared with accessibility so the
/// semantic list occupies the same compact region users see on screen.
pub fn completion_overlay_rect(overlay: &CompletionOverlay, cell: (f32, f32)) -> Option<Rect4> {
    completion_panel_geometry(overlay, cell).map(|geometry| geometry.rect)
}

/// Exact paint/input geometry for the pasted-media receipt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaPasteReceiptGeometry {
    pub rect: Rect4,
    pub preview_rect: Option<Rect4>,
    pub title_rect: Rect4,
    pub detail_rect: Option<Rect4>,
    pub dismiss_rect: Rect4,
    pub openable: bool,
    pub compact: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaPasteReceiptHit {
    Open,
    Body,
    Dismiss,
}

impl MediaPasteReceiptGeometry {
    pub fn hit_test(self, x: f32, y: f32) -> Option<MediaPasteReceiptHit> {
        let contains = |rect: Rect4| {
            x >= rect.0 && x <= rect.0 + rect.2 && y >= rect.1 && y <= rect.1 + rect.3
        };
        if contains(self.dismiss_rect) {
            Some(MediaPasteReceiptHit::Dismiss)
        } else if contains(self.rect) {
            Some(if self.openable {
                MediaPasteReceiptHit::Open
            } else {
                MediaPasteReceiptHit::Body
            })
        } else {
            None
        }
    }
}

fn rects_overlap(a: Rect4, b: Rect4) -> bool {
    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1
}

/// Place the receipt inside its terminal grid without covering completion UI.
pub fn media_paste_receipt_geometry(
    receipt: &MediaPasteReceiptOverlay,
    completion: Option<&CompletionOverlay>,
    cell: (f32, f32),
    text_cell_width: f32,
    text_line_height: f32,
) -> Option<MediaPasteReceiptGeometry> {
    let (cw, ch) = cell;
    if !cw.is_finite()
        || !ch.is_finite()
        || !text_cell_width.is_finite()
        || !text_line_height.is_finite()
        || cw <= 0.0
        || ch <= 0.0
        || text_cell_width <= 0.0
        || text_line_height <= 0.0
    {
        return None;
    }
    let (px, py, pw, ph) = receipt.pane_rect;
    let (gx, gy, gw, gh) = receipt.grid_rect;
    let left = px.max(gx);
    let right = (px + pw - receipt.right_gutter.max(0.0)).min(gx + gw);
    let top = py.max(gy);
    let bottom = (py + ph).min(gy + gh);
    let width = (right - left).max(0.0);
    let height = (bottom - top).max(0.0);
    let inset_x = (cw * 0.5).round().max(4.0);
    let inset_y = (ch * 0.5).round().max(4.0);
    let completion_rect = completion.and_then(|card| completion_overlay_rect(card, cell));

    let rect_for_corner = |card_w: f32, card_h: f32, top_lane: bool, right_lane: bool| {
        let x = if right_lane {
            right - inset_x - card_w
        } else {
            left + inset_x
        };
        if top_lane {
            (x, top + inset_y, card_w, card_h)
        } else {
            (x, bottom - inset_y - card_h, card_w, card_h)
        }
    };
    let choose_corner = |card_w: f32, card_h: f32| {
        if card_w + inset_x * 2.0 > width || card_h + inset_y * 2.0 > height {
            return None;
        }
        let right_top = (rect_for_corner(card_w, card_h, true, true), true);
        let right_bottom = (rect_for_corner(card_w, card_h, false, true), false);
        let left_top = (rect_for_corner(card_w, card_h, true, false), true);
        let left_bottom = (rect_for_corner(card_w, card_h, false, false), false);
        let candidates = if receipt.prefer_top {
            [right_top, right_bottom, left_top, left_bottom]
        } else {
            [right_bottom, right_top, left_bottom, left_top]
        };
        candidates.into_iter().find(|(candidate, _)| {
            completion_rect.is_none_or(|other| !rects_overlap(*candidate, other))
        })
    };

    let expanded_w = (cw * 42.0).min(width - inset_x * 2.0);
    let pad = (cw * 0.75).round().max(6.0);
    let detail_lines = if receipt.remote { 4.0 } else { 2.0 };
    let text_height = pad * 2.0 + text_line_height * (1.8 + detail_lines);
    let expanded_h = (ch * if receipt.remote { 8.0 } else { 7.0 }).max(text_height);
    let image_box_w = (expanded_w * 0.40).max(text_cell_width * 8.0);
    let detail_w = expanded_w - pad * 3.0 - image_box_w;
    // The remote warning is safety information, not decorative copy. Admit
    // the full card only when its actual detail box can hold the longest line;
    // the six-pixel padding floor and terminal `cell-width` scaling mean a
    // budget based on terminal columns is not the width the chrome glyphs use.
    let min_expanded_columns = if receipt.remote { 28.0 } else { 22.0 };
    let min_detail_columns = if receipt.remote { 18.0 } else { 5.0 };
    let expanded_corner = (expanded_w >= text_cell_width * min_expanded_columns
        && detail_w >= text_cell_width * min_detail_columns
        && expanded_h + inset_y * 2.0 <= height)
        .then(|| choose_corner(expanded_w, expanded_h))
        .flatten();

    if receipt.expanded
        && let Some((rect, top_lane)) = expanded_corner
    {
        let preview_box_h = (expanded_h - pad * 2.0).max(ch * 3.0);
        let preview_aspect = receipt.image.as_ref().map_or(16.0 / 9.0, |image| {
            image.width as f32 / image.height.max(1) as f32
        });
        let box_aspect = image_box_w / preview_box_h;
        let (preview_w, preview_h) = if preview_aspect >= box_aspect {
            (image_box_w, image_box_w / preview_aspect)
        } else {
            (preview_box_h * preview_aspect, preview_box_h)
        };
        let preview_rect = (
            rect.0 + pad + (image_box_w - preview_w) * 0.5,
            rect.1 + pad + (preview_box_h - preview_h) * 0.5,
            preview_w,
            preview_h,
        );
        let text_x = rect.0 + pad + image_box_w + pad;
        let dismiss_size = 24.0_f32.max(text_line_height * 1.5);
        let dismiss_inset = 5.0;
        let dismiss_rect = (
            rect.0 + rect.2 - dismiss_inset - dismiss_size,
            if top_lane {
                rect.1 + dismiss_inset
            } else {
                rect.1 + rect.3 - dismiss_inset - dismiss_size
            },
            dismiss_size,
            dismiss_size,
        );
        // Reserve the dismiss column in both lanes. At ordinary sizes the
        // bottom-lane title sits well above the button, but the supported
        // minimum font and cell height can make those bands touch.
        let title_right = dismiss_rect.0 - text_cell_width * 0.5;
        let title_w = (title_right - text_x).max(0.0);
        let detail_right = if top_lane {
            rect.0 + rect.2 - pad
        } else {
            dismiss_rect.0 - text_cell_width * 0.5
        };
        let detail_box_w = (detail_right - text_x).max(0.0);
        return Some(MediaPasteReceiptGeometry {
            rect,
            preview_rect: Some(preview_rect),
            title_rect: (text_x, rect.1 + pad, title_w, text_line_height * 1.5),
            detail_rect: Some((
                text_x,
                rect.1 + pad + text_line_height * 1.8,
                detail_box_w,
                text_line_height * detail_lines,
            )),
            dismiss_rect,
            openable: receipt.openable,
            compact: false,
        });
    }

    let card_w = (cw * 28.0).min(width - inset_x * 2.0);
    let card_h = (ch * 2.1).max(text_line_height + inset_y).max(34.0);
    // Compact and expanded states share a lane whenever the full card can be
    // placed. Otherwise moving onto the chip can make the card jump to the
    // opposite corner, immediately lose hover, and oscillate forever.
    let (rect, top_lane) = if let Some((expanded_rect, top_lane)) = expanded_corner {
        (
            (
                expanded_rect.0 + expanded_rect.2 - card_w,
                if top_lane {
                    expanded_rect.1
                } else {
                    expanded_rect.1 + expanded_rect.3 - card_h
                },
                card_w,
                card_h,
            ),
            top_lane,
        )
    } else {
        choose_corner(card_w, card_h)?
    };
    let dismiss_size = 24.0;
    let dismiss_inset = 5.0;
    let dismiss_rect = (
        rect.0 + rect.2 - dismiss_inset - dismiss_size,
        if top_lane {
            rect.1 + dismiss_inset
        } else {
            rect.1 + card_h - dismiss_inset - dismiss_size
        },
        dismiss_size,
        dismiss_size,
    );
    (card_w >= cw * 12.0).then_some(MediaPasteReceiptGeometry {
        rect,
        preview_rect: None,
        title_rect: (
            rect.0 + cw,
            rect.1 + (card_h - text_line_height) * 0.5,
            (dismiss_rect.0 - cw * 0.5 - (rect.0 + cw)).max(cw),
            text_line_height,
        ),
        detail_rect: None,
        dismiss_rect,
        openable: receipt.openable,
        compact: true,
    })
}

fn media_paste_receipt_text(
    receipt: &MediaPasteReceiptOverlay,
    geometry: &MediaPasteReceiptGeometry,
    text_cell_width: f32,
) -> (String, String) {
    let title_columns = (geometry.title_rect.2 / text_cell_width).floor().max(0.0) as usize;
    let title = match (&receipt.kind, geometry.compact, receipt.remote) {
        (_, true, true) => "Remote · local path only".to_string(),
        (MediaPasteReceiptKind::Image { .. }, true, false) => "Image path pasted".to_string(),
        (MediaPasteReceiptKind::Video { count, .. }, true, false) if *count > 1 => {
            format!("{count} video paths pasted")
        }
        (MediaPasteReceiptKind::Video { .. }, true, false) => "Video path pasted".to_string(),
        (MediaPasteReceiptKind::Image { .. }, false, _) => "Image path pasted".to_string(),
        (MediaPasteReceiptKind::Video { .. }, false, _) => "Video path pasted".to_string(),
    };
    let title = fit_single_line_label(&title, title_columns);

    let detail = geometry.detail_rect.map_or_else(String::new, |rect| {
        let columns = (rect.2 / text_cell_width).floor().max(0.0) as usize;
        let mut lines = match &receipt.kind {
            MediaPasteReceiptKind::Image {
                original_width,
                original_height,
            } => vec![
                format!("{original_width} × {original_height}"),
                "Path on command line".to_string(),
            ],
            MediaPasteReceiptKind::Video {
                extension,
                size,
                count,
                preview_pending,
            } => {
                let mut lines = Vec::with_capacity(4);
                lines.push(if *count > 1 {
                    format!("1 of {count} · {extension} · {}", format_media_size(*size))
                } else {
                    format!("{extension} · {}", format_media_size(*size))
                });
                lines.push(if *preview_pending {
                    "Preparing poster".to_string()
                } else {
                    "Path on command line".to_string()
                });
                lines
            }
        };
        if receipt.remote {
            lines.push("Remote pane".to_string());
            lines.push("Local path only".to_string());
        }
        lines
            .into_iter()
            .map(|line| fit_single_line_label(&line, columns))
            .collect::<Vec<_>>()
            .join("\n")
    });
    (title, detail)
}

/// Format a media byte count for receipt chrome and accessibility labels.
pub fn format_media_size(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = KB * 1_000;
    const GB: u64 = MB * 1_000;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Candidate index and pixel bounds for each visible completion row. Paint
/// and accessibility share this geometry so assistive hit targets never point
/// at clipped or off-screen candidates, and so the header — which belongs to
/// the list container, not to any candidate — is never inside a row.
pub fn completion_overlay_row_rects(
    overlay: &CompletionOverlay,
    cell: (f32, f32),
) -> Vec<(usize, Rect4)> {
    let Some(geometry) = completion_panel_geometry(overlay, cell) else {
        return Vec::new();
    };
    (0..geometry.rows)
        .map(|row| {
            (
                geometry.first + row,
                (
                    geometry.rect.0,
                    geometry.list_top + row as f32 * geometry.row_h,
                    geometry.rect.2,
                    geometry.row_h,
                ),
            )
        })
        .collect()
}

/// Activity state of a tab — `Normal` draws no indicator, `Output`
/// draws a small cyan dot, `Bell` draws a yellow dot. Terminator-
/// parity affordance ("you've got new output in an inactive tab").
/// Renderer-side enum so the UI doesn't need to leak its
/// `kettle_ui::mux::TabActivity` type across crate boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabActivity {
    #[default]
    Normal,
    Output,
    Bell,
    /// Inactive tab had unseen output but went quiet for
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
    /// Text lane within the segment, excluding fixed controls such as the
    /// trailing close button.
    pub title_rect: Rect4,
    /// Close-button (✕) hit rect within the segment.
    pub close: Rect4,
    pub title: String,
    /// v2.26.0: home-abbreviated full cwd path when the label is directory-
    /// derived, enabling width-aware tiering (full path → leaf dir name → tail).
    /// `None` for explicit/override and shell-set (OSC 2) titles, which are
    /// fitted with the older `fit_tab_title` middle-ellipsis path.
    pub path: Option<String>,
    pub active: bool,
    /// Inactive-tab activity. Always `Normal` on the
    /// active segment so the focused-tab accent isn't doubled-up by
    /// a redundant dot.
    pub activity: TabActivity,
}

/// The tab bar geometry — computed once in the UI, used for both drawing
/// (here) and click hit-testing (app), so there is a single source of truth.
/// Thin status-bar strip at the top or bottom of the
/// surface. Disabled by default; when on, the App sets `height` > 0
/// and supplies a pre-formatted single-line string.
///
/// Content is a free-form `String` so the App can compose whatever
/// it wants (the default: "HH:MM:SS · theme · pane title").
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
    /// The `▾` dropdown-arrow rect, immediately LEFT of `new_tab`.
    /// Clicking it opens the new-tab shell chooser. Zero-area `(0,0,0,0)` when
    /// the dropdown is disabled (vertical tab bars) — the renderer then draws a
    /// plain `+` and the hit-test skips the arrow branch.
    pub new_tab_menu: Rect4,
    /// The pointer is over the `+` hit target. The UI computes this from the
    /// same rect used for click dispatch, so hover paint cannot drift from the
    /// action users actually trigger.
    pub hovered_new_tab: bool,
    /// The pointer is over the shell-dropdown hit target immediately left of
    /// `+`. Kept separate so each half has an unambiguous hover response.
    pub hovered_new_tab_menu: bool,
    /// Visual indicator that broadcast / group-input mode is
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
    /// Ghost-drag indicator: `Some(cursor_x)` while a
    /// left-button drag is in progress in the tab bar. The renderer
    /// draws a translucent overlay copy of the dragged (active) tab
    /// segment centered at `cursor_x`, so the user sees what's being
    /// moved while the underlying segments snap into place via
    /// `Mux::move_active_tab`. `None` while no drag is active.
    pub drag_cursor_x: Option<f32>,
    /// The same thing for a vertical (left/right) bar, where the strip runs
    /// down the window and the ghost follows the cursor's **y**. Exactly one of
    /// the two is ever `Some`, chosen by which bar was built. A single
    /// "main-axis" field would have been tidier, but this one is reported over
    /// the control plane under its own name, and agents already read it.
    pub drag_cursor_y: Option<f32>,
    /// v2.40.0 (tear-off UX): 0.0..=1.0 — how far the drag has moved from
    /// the tab band toward the tear threshold. 0.0 at/inside the band,
    /// 1.0 at (or past) the distance `tear_threshold_crossed` fires at.
    /// Escalates the ghost's shadow/opacity (`tab_drag::*_LIFT*`) so a
    /// release visibly reads as "this will tear off". Only non-zero while
    /// `drag_cursor_x` is live and the drag FSM is in a Dragging state;
    /// on non-Wayland the tear consumes the gesture at 1.0, so the full
    /// range is mostly visible on Wayland's at-release path.
    pub tear_lift: f32,
    /// v2.19.0 (tear-off UX, re-dock): `Some(rect)` while a torn-off
    /// window hovers this window's tab band — the accent-colored
    /// insertion marker showing where the dropped tab will land. The
    /// UI computes the rect (a `tab_drag::INSERT_MARKER_PX` line between
    /// segments, oriented per `tab-bar-pos`) so the renderer stays
    /// geometry-free, same contract as `hovered_close_idx`.
    pub insert_marker: Option<Rect4>,
    /// v2.40.0 (tear-off UX): the strip's full band rect in surface
    /// coords, both orientations — the canvas for the dock-target
    /// highlight (`tab_drag::DOCK_HIGHLIGHT_*`) drawn while
    /// `insert_marker` is latched. UI-computed, same geometry-free
    /// contract as `insert_marker`.
    pub band: Rect4,
    /// v2.26.0: `‹` scroll-left button rect, present (non-zero) only when the
    /// horizontal tab bar overflows (more tabs than fit at `tab_min_width`).
    /// Clicking it reveals tabs scrolled off the left. `(0,0,0,0)` when the bar
    /// fits or for vertical bars.
    pub scroll_left: Rect4,
    /// v2.26.0: `›` scroll-right button rect (see `scroll_left`).
    pub scroll_right: Rect4,
}

impl TabBar {
    pub fn hidden() -> Self {
        TabBar {
            height: 0.0,
            y: 0.0,
            segments: Vec::new(),
            new_tab: (0.0, 0.0, 0.0, 0.0),
            new_tab_menu: (0.0, 0.0, 0.0, 0.0),
            hovered_new_tab: false,
            hovered_new_tab_menu: false,
            broadcast: false,
            hovered_close_idx: None,
            drag_cursor_x: None,
            drag_cursor_y: None,
            tear_lift: 0.0,
            insert_marker: None,
            band: (0.0, 0.0, 0.0, 0.0),
            scroll_left: (0.0, 0.0, 0.0, 0.0),
            scroll_right: (0.0, 0.0, 0.0, 0.0),
        }
    }
}

/// One tiled pane to draw this frame.
pub struct PaneView<'a> {
    /// Process-global pane id. Used to keep renderer caches attached to the
    /// same terminal pane across split reorders and tab/window moves.
    pub id: u64,
    /// Pixel rect `(x, y, w, h)` within the surface.
    pub rect: (f32, f32, f32, f32),
    /// v2.20.0 P2 (perf): RAW terminal state captured under the Term lock by
    /// `redraw` (µs-scale flat copy, pooled per window), borrowed here so the
    /// whole GPU frame runs with the lock RELEASED — the PTY reader no longer
    /// stalls behind shaping/acquire/present. Replaces the former
    /// `&'a Term<EventProxy>` borrowed from a frame-held `MutexGuard`.
    pub snap: &'a PaneSnapshot,
    pub focused: bool,
    /// Decoded images placed in this pane (Sixel / kitty / iTerm2).
    ///
    /// Borrowed: the backing `Vec` lives in the per-frame
    /// `metas` collection for the whole frame — exactly like `snap` borrows
    /// the pooled snapshot — so the renderer reads it without a second
    /// per-pane clone.
    pub images: &'a [kettle_core::Placement],
    /// Terminator parity, per-pane-titlebar: the pane's title —
    /// rendered into the titlebar background quad (see
    /// `pick_titlebar_bg`) when cfg.show_titlebar = true. Borrowed
    /// from `metas`.
    pub title: &'a str,
    /// Prefix badges that belong immediately before the semantic title, such as
    /// `[RO] ` and the configured agent badge. Kept separate from `title` so
    /// cwd/path-aware fitting can replace only the title body.
    pub title_prefix: &'a str,
    /// Authoritative cwd-derived path for title fitting, when the app can
    /// prove the pane title is a placeholder or an already-truncated cwd
    /// suffix. Home-abbreviated by the caller.
    pub title_path: Option<&'a str>,
    /// Terminator parity, per-pane-titlebar: pane terminal size in
    /// cols × rows. Appended to the titlebar title text as `WxH`
    /// unless cfg.title_hide_sizetext is true.
    pub size_cols: u16,
    pub size_rows: u16,
    /// Terminator parity: bell-state indicator for the pane. When
    /// true and cfg.icon_bell is also true, a small dot renders in
    /// the titlebar.
    pub bell: bool,
    /// Terminator parity, titlebar: optional named broadcast group.
    /// When `Some(name)`, the titlebar prefixes `[name]` (group
    /// label in brackets) before the pane title. Borrowed from
    /// `metas`.
    pub group_name: Option<&'a str>,
}

/// C3 (multi-window): the process-wide GPU objects shared by every window's
/// Renderer. wgpu's Instance/Adapter/Device/Queue handles are internally
/// ref-counted — `Clone` is a refcount bump, and one device happily serves N
/// surfaces. Window 1 creates this inside `Renderer::new`; windows 2..N reuse
/// it via the synchronous `Renderer::new_with_gpu` (no adapter/device
/// request, no block_on, no GPU-init watchdog needed).
#[derive(Clone)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// v2.31.0: set `true` when wgpu reports a fatal uncaptured error or a
    /// device-loss (a GPU driver TDR/reset, VRAM exhaustion, or internal
    /// backend fault). Validation errors are logged but deliberately excluded.
    /// The handlers installed in `install_gpu_error_handlers` set this flag
    /// instead of letting wgpu's default handler panic — which, with the
    /// release profile's `panic = "abort"`, hard-killed kettle on a GPU reset
    /// with no crash log. The App checks this (a refcount-shared `Arc`, so every
    /// window's clone sees it) to stop rendering on a dead device and surface a
    /// "GPU device lost" state rather than spin or crash. Reset by rebuilding
    /// the renderer on a fresh context.
    pub gpu_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// First fatal wgpu error for this device. Error callbacks only latch this
    /// bounded in-memory value; the UI thread owns durable diagnostics so a
    /// driver callback never blocks on filesystem I/O.
    gpu_fault: std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    /// Application event-loop wake shared with driver callbacks. The context
    /// is created before the UI owns a live proxy, so the App installs this
    /// immediately after renderer construction.
    recovery_wake: std::sync::Arc<std::sync::Mutex<Option<ScreenshotRecoveryWake>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuFault {
    pub kind: String,
    pub message: String,
}

/// Result of attempting to paint one live surface frame.
///
/// A successful Rust return does not necessarily mean pixels reached the
/// compositor: wgpu reports timeout, occlusion, and swapchain-loss conditions
/// as normal acquisition outcomes.  Callers must only consume terminal/UI
/// damage after [`Presented`](Self::Presented).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum FrameOutcome {
    /// Queue submission completed and the surface texture was presented.
    Presented,
    /// Nothing was presented, but the configured surface remains usable.
    /// Retain damage and retry through the caller's deadline-driven backoff.
    RetryLater,
    /// Nothing was presented because the window is not currently visible.
    /// Retain damage and wait for an un-occlusion/visibility event.
    Occluded,
    /// The surface was lost and must be recreated through
    /// [`Instance::create_surface`](wgpu::Instance::create_surface) before
    /// another frame is attempted. The shared device can remain healthy.
    SurfaceLost,
}

impl GpuContext {
    /// v2.31.0: has the GPU device been lost (driver reset / TDR) or hit an
    /// uncaptured error (e.g. VRAM exhaustion)? Once `true`, no rendering will
    /// succeed against this context.
    pub fn is_lost(&self) -> bool {
        self.gpu_lost.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Return the first fatal error reported for this device. The value stays
    /// available throughout recovery and is reset with a new [`GpuContext`].
    pub fn fault(&self) -> Option<GpuFault> {
        self.gpu_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_recovery_wake(&self, wake: ScreenshotRecoveryWake) {
        *self
            .recovery_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }

    pub fn is_software(&self) -> bool {
        matches!(self.adapter.get_info().device_type, wgpu::DeviceType::Cpu)
    }

    pub fn adapter_name(&self) -> String {
        self.adapter.get_info().name
    }

    pub fn adapter_ids(&self) -> (u32, u32) {
        let info = self.adapter.get_info();
        (info.vendor, info.device)
    }

    pub fn adapter_key(&self) -> GpuAdapterKey {
        GpuAdapterKey::from_info(&self.adapter.get_info())
    }
}

/// v2.31.0: install wgpu's uncaptured-error + device-lost handlers so a GPU
/// fault becomes a LOGGED, observable event instead of wgpu's default panic —
/// which, under the release `panic = "abort"`, hard-aborted kettle (no unwind,
/// no log) on a driver TDR/reset or a VRAM allocation failure. After this, the
/// shared `gpu_lost` flag flips and the App degrades gracefully.
fn install_gpu_error_handlers(
    device: &wgpu::Device,
    gpu_lost: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    gpu_fault: &std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    recovery_wake: &std::sync::Arc<std::sync::Mutex<Option<ScreenshotRecoveryWake>>>,
) {
    let flag = gpu_lost.clone();
    let fault = gpu_fault.clone();
    let wake = recovery_wake.clone();
    // The uncaptured-error path catches errors NOT routed to an error scope.
    // NOTE (adversarial review): in wgpu 29 a genuine DEVICE-LOSS is delivered
    // via `set_device_lost_callback` below, NOT here — this handler only ever
    // sees `Validation` / `OutOfMemory` / `Internal`. So gate the latch by kind:
    // a `Validation` error is a kettle code/data bug (bad descriptor, over-limit
    // dim, stale bind group) on a HEALTHY device — log it loudly but do NOT set
    // `gpu_lost` (that would falsely brick the window with "GPU device lost").
    // Only `OutOfMemory` (VRAM exhaustion) / `Internal` are device-fatal. Without
    // ANY handler, wgpu's default panics → `panic=abort` hard-crash, so installing
    // this (even just to log) is what prevents the crash. `Fn`; must not panic.
    device.on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| match e {
        wgpu::Error::Validation { .. } => {
            log::error!("wgpu validation error (a kettle bug, NOT device loss): {e}");
        }
        wgpu::Error::OutOfMemory { .. } => {
            log::error!("wgpu fatal GPU error (out of memory): {e}");
            latch_gpu_fault(&flag, &fault, "out_of_memory", e.to_string());
            wake_gpu_recovery(&wake);
        }
        wgpu::Error::Internal { .. } => {
            log::error!("wgpu fatal GPU error (internal): {e}");
            latch_gpu_fault(&flag, &fault, "internal", e.to_string());
            wake_gpu_recovery(&wake);
        }
    }));
    let flag2 = gpu_lost.clone();
    let fault2 = gpu_fault.clone();
    let wake2 = recovery_wake.clone();
    device.set_device_lost_callback(move |reason, msg| {
        // `Destroyed` fires on our own clean shutdown (`device.destroy()` at drop)
        // — that is not a crash. Only an `Unknown` loss (driver TDR/reset) flags.
        if !matches!(reason, wgpu::DeviceLostReason::Destroyed) {
            log::error!("wgpu device lost ({reason:?}): {msg}");
            latch_gpu_fault(&flag2, &fault2, "device_lost", msg);
            wake_gpu_recovery(&wake2);
        }
    });
}

fn wake_gpu_recovery(
    recovery_wake: &std::sync::Arc<std::sync::Mutex<Option<ScreenshotRecoveryWake>>>,
) {
    let wake = recovery_wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(wake) = wake {
        wake.wake();
    }
}

fn latch_gpu_fault(
    gpu_lost: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    gpu_fault: &std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    kind: &str,
    message: String,
) {
    let mut fault = gpu_fault
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if fault.is_none() {
        *fault = Some(GpuFault {
            kind: kind.to_string(),
            message: bounded_gpu_message(&message),
        });
    }
    drop(fault);
    gpu_lost.store(true, std::sync::atomic::Ordering::Release);
}

fn bounded_gpu_message(message: &str) -> String {
    const MAX_CHARS: usize = 2048;
    let mut out = String::with_capacity(message.len().min(MAX_CHARS));
    for ch in message.chars().take(MAX_CHARS) {
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    out
}

impl GpuContext {
    /// v2.23.0: the live adapter's identity, in kettle's vocabulary — feeds the
    /// settings `Active now: <gpu> (<kind>, <backend>)` line so the user sees
    /// which GPU is actually in use (vs. the pinned/preferred one, which only
    /// takes effect on restart).
    pub fn adapter_info(&self) -> GpuAdapterInfo {
        let i = self.adapter.get_info();
        GpuAdapterInfo {
            name: i.name,
            vendor: i.vendor,
            device: i.device,
            kind: device_kind_str(i.device_type),
            backend: backend_str(i.backend),
        }
    }
}

/// v2.21.0 (idle perf): the foreground glyph drawn on top of a focused solid
/// block cursor this frame. The glyph is rendered in its OWN tiny renderer +
/// 1-line buffer rather than recolored INTO the pane text buffer, so a cursor
/// blink no longer mutates the pane buffer (which would force the expensive
/// whole-viewport `prepare`). The glyph bitmap is already in the atlas (it is
/// part of the visible pane text), so the 1-glyph prepare never grows it.
struct PendingCursorGlyph {
    /// Surface-pixel top-left of the cursor cell.
    x: f32,
    y: f32,
    /// The character under the cursor (drawn in `color`).
    ch: char,
    /// Cursor foreground (theme `cursor_text`, or the cell bg under an OSC 12
    /// runtime cursor color so the inverted glyph follows reverse-video).
    color: Rgb,
    /// Pane rect `(x, y, w, h)` used to clip the glyph to its pane.
    clip: (f32, f32, f32, f32),
}

fn cursor_glyph_damage_key(
    cursor: Option<&PendingCursorGlyph>,
    metrics: Metrics,
    family: &str,
) -> Option<u64> {
    use std::hash::{Hash, Hasher};

    let cursor = cursor?;
    let mut hash = std::hash::DefaultHasher::new();
    cursor.x.to_bits().hash(&mut hash);
    cursor.y.to_bits().hash(&mut hash);
    cursor.ch.hash(&mut hash);
    cursor.color.r.hash(&mut hash);
    cursor.color.g.hash(&mut hash);
    cursor.color.b.hash(&mut hash);
    cursor.clip.0.to_bits().hash(&mut hash);
    cursor.clip.1.to_bits().hash(&mut hash);
    cursor.clip.2.to_bits().hash(&mut hash);
    cursor.clip.3.to_bits().hash(&mut hash);
    metrics.font_size.to_bits().hash(&mut hash);
    metrics.line_height.to_bits().hash(&mut hash);
    family.hash(&mut hash);
    Some(hash.finish())
}

/// v2.21.x: the decoded background-image, animated. A still image is one frame;
/// an animated GIF / APNG / animated WebP is many. `frames.is_empty()` encodes a
/// FAILED decode (drives the retry throttle, like the old inner `Option::None`).
struct BgImageAnim {
    /// The configured path this was decoded from (cache key part 1).
    path: String,
    /// The blur radius this was decoded with (cache key part 2).
    blur: u32,
    /// GPU-ready frames. Each frame's `rgba` is `Arc`-shared, so the imgpipe
    /// texture cache (keyed by `Arc::as_ptr`) reuses one GPU texture per frame
    /// and only re-uploads when the displayed frame index actually changes.
    frames: Vec<kettle_core::ImageData>,
    /// Whether every source texel in each parallel frame has alpha 255.
    /// Computed once when the decoded result enters the cache, not by scanning
    /// a 4K wallpaper again on every redraw.
    opaque_frames: Vec<bool>,
    /// Per-frame dwell time (ms), parallel to `frames`.
    gaps: Vec<u32>,
    /// Wall-clock origin for the playback loop (`bg_current_frame`).
    started: std::time::Instant,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    /// Shared CPU/GPU retained-resource scope for this renderer window.
    graphics_budget: kettle_core::GraphicsBudget,
    config: wgpu::SurfaceConfiguration,
    supported_alpha_modes: Vec<wgpu::CompositeAlphaMode>,

    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
    /// v2.25.0: cell-locked pane-text renderer (the default `text-renderer=grid`
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
    /// Pooled per-run scratch: byte offset of each char in a row (char index ==
    /// grid column, since `build_pane` pushes exactly one char per cell). Reused
    /// across runs/frames to keep the glyph-emit walk allocation-free.
    glyph_char_starts: Vec<u32>,
    /// Cell-locked glyph instance buffer needs a forced upload after the GPU
    /// pipeline is cleared, even if pane text/layout hashes did not change.
    /// Without this, a font/scale/cache invalidation can drop the live draw
    /// count to zero and cursor-only frames keep presenting blank pane text.
    grid_glyphs_dirty: bool,
    /// v2.25.1: text-area / grid-glyph layout damage key. Cursor blink must not
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
    /// v2.20.0 P1 (perf): per-pane, per-row content keys for the line-level
    /// shaping cache. `build_pane` hashes each grid row's style runs (text,
    /// fg, bold, italic); a row whose key matches last frame is SKIPPED
    /// entirely — its `BufferLine` keeps its shaped+laid-out caches. The old
    /// whole-buffer `set_rich_text` reset every line's shaping every frame,
    /// so an idle blink repaint re-shaped 100% of all visible text. Grown /
    /// truncated in lockstep with `pane_buffers` (the keys describe what is
    /// IN the buffer at that index, so they must live and die with it).
    pane_line_keys: Vec<Vec<u64>>,
    /// v2.20.0 P1: per-pane key over the inputs that change how a row SHAPES
    /// without changing its run tuples — font-family variants, ligature
    /// toggle, font-features, shaping mode. On mismatch the pane's row keys
    /// are wiped so every row re-sets via `reset_new` (the only path that
    /// updates a `BufferLine`'s internal shaping mode).
    pane_style_keys: Vec<u64>,
    /// v2.20.0 P1: pooled scratch for assembling one row's text.
    line_text_scratch: String,
    /// v2.20.0 P1b: chrome-label caches (titlebar / tab / status / glyph
    /// buttons) gate their `Buffer::set_text` (which re-shapes
    /// unconditionally) on text equality. Text-only keys are sound while the
    /// font family is stable; this key invalidates them all when it changes.
    chrome_style_key: u64,
    /// v2.21.0 (idle perf): hash of the chrome label text shaped last frame
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
    /// v2.23.0 fix: whether ANY text overlay (settings, palette, search, menu,
    /// …) was open the previous frame. The `need_prepare` damage gate forces a
    /// glyphon prepare while an overlay is open, but the frame an overlay
    /// *closes* would otherwise see "no overlay + nothing changed" and SKIP the
    /// prepare — re-rendering the just-closed overlay's cached text vertices, so
    /// the panel lingered on screen until the next keystroke. Tracking the
    /// previous open-state lets the close transition force one clearing prepare.
    last_overlay_open: bool,
    /// v2.21.0 (idle perf): dedicated renderer + 1-line buffer for the focused
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
    /// The cursor-cell glyph shaped last frame. A change forces a `prepare` so
    /// the new glyph is guaranteed resident in the atlas before the cursor pass
    /// reuses its bitmap (the only way the 1-glyph cursor prepare could grow
    /// the atlas and invalidate the cached pane vertices).
    last_cursor_char: Option<char>,
    /// v2.20.0 P1b: last text shaped into each `pane_titlebar_buffers` slot.
    pane_titlebar_texts: Vec<String>,
    /// v2.20.0 P1b: last text shaped into each `tab_buffers` slot.
    tab_texts: Vec<String>,
    /// v2.38.2: last text shaped into each `hint_buffers` slot. Quick-select
    /// labels are byte-stable while the overlay is open, but the loop
    /// re-shaped all of them (up to ~100) on every blink/keystroke redraw —
    /// free-ish under no-fallback shaping, real work under Advanced.
    hint_texts: Vec<String>,
    /// v2.20.0 P1b: last text shaped into `tab_close_buffer` / `tabbar_buffer`
    /// / `new_tab_arrow_buffer` / `status_bar_buffer`. The first three are
    /// constant glyphs, so after frame 1 these gates always hold.
    tab_close_text: String,
    tabbar_text: String,
    new_tab_arrow_text: String,
    /// v2.26.0: last text shaped into the `‹` / `›` overflow scroll-arrow
    /// buffers (constant glyphs → the gate holds after frame 1).
    scroll_left_text: String,
    scroll_right_text: String,
    status_bar_text: String,
    /// v2.20.0 (Ghostty parity): the transient resize chip's text buffer +
    /// its P1b equality gate (re-shaped only when the grid size changes).
    resize_overlay_buffer: TextBuffer,
    resize_overlay_text: String,
    /// Input-method preedit buffer and equality gate.
    ime_buffer: TextBuffer,
    ime_text: String,
    /// Pooled scratch for the per-frame cell/UI quad list
    /// (`render_frame_with_status` filled a fresh `Vec` of `panes*16+256`
    /// `QuadInstance`s every frame). Taken + cleared at the top of the frame,
    /// returned after the GPU upload — same high-water pooling as `span_scratch`.
    quad_scratch: Vec<QuadInstance>,
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
    /// v2.38.2 P1b: last text shaped into each `context_menu_buffers` slot —
    /// same equality gate `tab_texts`/`hint_texts` use, so a static open menu
    /// doesn't re-shape every row on every blink/hover redraw.
    context_menu_texts: Vec<String>,
    /// Dropdown-parity: one buffer per row's right-aligned shortcut
    /// hint (empty-hint rows shape nothing). Pooled like its sibling.
    context_menu_hint_buffers: Vec<TextBuffer>,
    /// v2.38.2 P1b: last text shaped into each `context_menu_hint_buffers` slot.
    context_menu_hint_texts: Vec<String>,
    /// One text buffer per display line of the settings overlay
    /// (title, category tabs, field rows, footer). Grown + truncated like the
    /// context-menu pool.
    settings_buffers: Vec<TextBuffer>,
    /// v2.38.2 P1b: last text shaped into each `settings_buffers` slot. Moving
    /// the focused row only changes 2 of N lines (the old/new `▸` mark), so
    /// this catches what the whole-overlay `settings_lines_cache` gate below
    /// can't: it still recomputes `lines` on ANY overlay change, but the
    /// per-row reshape is skipped for every row whose text is unaffected.
    settings_texts: Vec<String>,
    /// v2.38.2 P1b: memoizes `settings_display_lines(set)` — a `format!()`
    /// per display line — keyed on the last `SettingsOverlay` it was computed
    /// from. `None`/mismatched source means `settings_lines_cache` is stale
    /// (or never populated) and must be recomputed.
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
    /// v2.26.0: `‹` / `›` tab-bar overflow scroll-arrow glyphs, each in its own
    /// buffer (constant glyph, shaped once). Drawn only when the horizontal tab
    /// bar overflows (more tabs than fit at `tab_min_width`).
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
    /// v2.38.2 P1b: last text shaped into `search_buffer`. Shared across the
    /// search / command-palette / layout-picker / ssh-launcher / edit-title /
    /// confirm-dialog / update-banner bars — only one of those `else if`
    /// branches paints per frame, so a single cache is enough (unlike the
    /// per-row pools above, there's no risk of comparing one overlay's label
    /// against a different overlay's stale cache: whichever branch runs next
    /// simply re-shapes once, the same one-time cost a fresh buffer already
    /// pays on the first frame it opens).
    search_buffer_text: String,
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
    /// Second quad pass drawn *after* text (pane dimming, scrollbar).
    overlay_quads: QuadPipeline,
    /// Third quad pass drawn after the overlay quads — reserved for
    /// the right-click context menu's shadow / panel / border /
    /// highlight quads. Lives in its own pass so the menu's text
    /// (rendered by `menu_text_renderer` below) lands *on top of* the
    /// panel bg rather than underneath it. This was split out
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
    /// Single-instance overlay pipeline drawn between menu chrome and menu
    /// text, so the receipt thumbnail cannot cover its own status labels.
    media_receipt_img: imgpipe::ImagePipeline,
    /// v2.23.0: dedicated pipeline for the **background image (wallpaper)**,
    /// drawn at the very back — between the surface clear and the cell/chrome
    /// `quads` pass — so cell backgrounds (selection, syntax, TUI panels),
    /// chrome (tab bar / status bar / per-pane titlebars), and pane borders all
    /// composite OPAQUELY on top of the wallpaper (the standard kitty / wezterm
    /// / alacritty layering). Inline kitty / sixel images stay in `imgs`, drawn
    /// *after* the quads so they sit over cell backgrounds. Pre-2.23.0 the
    /// wallpaper shared `imgs` and drew *after* every quad, so an opaque
    /// wallpaper hid all cell backgrounds AND let the animation bleed through
    /// the tab bar.
    bg_imgs: imgpipe::ImagePipeline,
    /// v2.24.0 procedural starfield wallpaper (`background-type = starfield`),
    /// drawn in the same back-most slot as `bg_imgs`. Stateless on the GPU side
    /// (just a per-frame uniform); the only state is `starfield_started`.
    starfield: starfield::StarfieldPipeline,
    presentation: present::PresentationPipeline,
    /// Playback clock for the starfield drift — `elapsed()` feeds the shader's
    /// continuous `time` so motion is smooth-valued even though we repaint at a
    /// low fps cap.
    starfield_started: std::time::Instant,
    /// Terminator parity, bg-image: decoded background-image cache.
    /// Tuple of (cfg.background_image path, decoded ImageData).
    /// Invalidated + re-decoded when the config path changes.
    // Key is `(path, blur_radius)` — keying on the path
    // alone meant toggling `background-blur` was ignored on reload unless
    // the image path *also* changed. The value is bounded to 64 MiB per frame
    // and 128 MiB per animation; it is freed (`= None`) when config moves away from
    // `background-type = image` so a large wallpaper doesn't sit resident
    // for the rest of the session after the user turns it off.
    //
    // A FAILED decode is cached as `frames.is_empty()` (was the
    // inner `Option::None`). Caching the failed key (a) stops rendering the
    // previous wallpaper after the path changes to a broken one, and (b) stops
    // re-attempting the failing decode every frame.
    // v2.21.x: holds ALL frames of an animated background (GIF/APNG/WebP) — one
    // for a still image — plus per-frame gaps + the playback clock origin, so
    // the render loop swaps the already-decoded frame per `bg_current_frame`.
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
    /// Phase 3 of
    /// [`TERMINATOR-TERMINALSHOT-DESIGN.md`](../../../docs/TERMINATOR-TERMINALSHOT-DESIGN.md):
    /// when `Some`, the next `render_frame` call renders the prepared scene to
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

/// CPU-owned renderer state that must survive a surface/device rebuild.
///
/// GPU resources and retained draw caches deliberately stay out of this
/// snapshot: they are tied to the failed device and are rebuilt lazily. The
/// values here are the live per-window overrides that may have diverged from
/// [`Config`] since launch, plus a screenshot request that has not yet been
/// submitted to the worker.
#[derive(Clone, Debug)]
pub struct RendererRecoveryState {
    font_family: Arc<str>,
    font_size: f32,
    cell_scale_w: f32,
    cell_scale_h: f32,
    accent_override: Option<Rgb>,
    pending_screenshot: Option<ScreenshotRequest>,
}

/// A queued screenshot request. Phase 4 of the terminalshot design
/// consumes this in `render_frame`.
#[derive(Debug, Clone)]
pub struct ScreenshotRequest {
    /// Where to save the PNG.
    pub out_path: std::path::PathBuf,
    /// Whether the destination belongs to Kettle's private state or was
    /// explicitly selected by the user.
    pub output_policy: ScreenshotOutputPolicy,
    /// If `Some`, crop the captured frame to this pixel rect
    /// (the focused pane's geometry). If `None`, capture the
    /// whole window.
    pub crop: Option<(f32, f32, f32, f32)>,
    /// Optional completion signal for programmatic callers (`kettle ctl
    /// screenshot` / MCP). The UI action leaves this `None` and keeps its
    /// optimistic notification behavior.
    pub completion: Option<std::sync::mpsc::Sender<Result<std::path::PathBuf, String>>>,
    /// Cooperative cancellation for bounded control-plane callers. A timed-out
    /// request must neither keep the renderer BUSY nor publish a file later
    /// when a minimized window is restored. UI-triggered captures leave this
    /// `None` because their lifecycle is owned entirely by the renderer.
    pub cancellation: Option<ScreenshotCancellation>,
    /// Event-loop wake used only when a bounded GPU wait classifies the shared
    /// device as wedged. The worker runs off-thread; without this wake an idle
    /// application can remain asleep with the destroyed device installed.
    pub recovery_wake: Option<ScreenshotRecoveryWake>,
}

/// Cloneable, application-owned event-loop wake for screenshot GPU recovery.
#[derive(Clone)]
pub struct ScreenshotRecoveryWake(
    std::sync::Arc<dyn Fn() + Send + Sync + std::panic::UnwindSafe + 'static>,
);

impl std::fmt::Debug for ScreenshotRecoveryWake {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ScreenshotRecoveryWake(..)")
    }
}

impl ScreenshotRecoveryWake {
    pub fn new(wake: impl Fn() + Send + Sync + std::panic::UnwindSafe + 'static) -> Self {
        Self(std::sync::Arc::new(wake))
    }

    fn wake(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.0)()));
    }
}

/// Cloneable publication state shared by a control request and the renderer.
///
/// The final transition is a race between the caller cancelling and the worker
/// committing the fully encoded, durably flushed file. Whichever wins is irreversible:
/// a timeout that successfully cancels cannot publish later, while a worker
/// that has already committed must report its real completion instead of a
/// contradictory timeout.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotCancellation(std::sync::Arc<std::sync::atomic::AtomicU8>);

const SCREENSHOT_ACTIVE: u8 = 0;
const SCREENSHOT_CANCELLED: u8 = 1;
const SCREENSHOT_COMMITTED: u8 = 2;

impl ScreenshotCancellation {
    /// Cancel publication. Returns `false` only when the worker already won the
    /// final commit race, in which case the caller must await its real result.
    pub fn cancel(&self) -> bool {
        loop {
            match self.0.load(std::sync::atomic::Ordering::Acquire) {
                SCREENSHOT_CANCELLED => return true,
                SCREENSHOT_COMMITTED => return false,
                SCREENSHOT_ACTIVE => {
                    if self
                        .0
                        .compare_exchange(
                            SCREENSHOT_ACTIVE,
                            SCREENSHOT_CANCELLED,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                }
                _ => unreachable!("invalid screenshot publication state"),
            }
        }
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire) == SCREENSHOT_CANCELLED
    }

    fn commit(&self) -> bool {
        self.0
            .compare_exchange(
                SCREENSHOT_ACTIVE,
                SCREENSHOT_COMMITTED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
}

impl ScreenshotRequest {
    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(ScreenshotCancellation::is_cancelled)
    }
}

/// Filesystem policy for a screenshot destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotOutputPolicy {
    /// Kettle chose the path under its cache/state tree. Ancestors are created
    /// and verified as private before the owner-only PNG is staged.
    PrivateState,
    /// The caller explicitly chose the path. Its parent must already exist,
    /// but ordinary user directories are accepted; an owner-only sibling is
    /// atomically linked or renamed into the requested leaf only after encoding succeeds.
    UserSelected,
}

const MAX_LIVE_SCREENSHOT_BYTES: u64 = 256 * 1024 * 1024;
const LIVE_SCREENSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const LIVE_SCREENSHOT_MAX_GPU_WAITS: u32 = 2;

fn screenshot_target_bytes(width: u32, height: u32) -> Option<usize> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    if bytes == 0 || bytes > MAX_LIVE_SCREENSHOT_BYTES {
        return None;
    }
    usize::try_from(bytes).ok()
}

struct ScreenshotCaptureTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Process-wide charge for this short-lived render target. It travels with
    /// the readback job so accounting remains live until the GPU submission and
    /// PNG worker are both finished, not merely until command encoding ends.
    gpu: kettle_core::GraphicsReservation,
    staging: wgpu::Buffer,
    staging_gpu: kettle_core::GraphicsReservation,
    width: u32,
    height: u32,
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

struct PreparedScreenshot {
    staging: wgpu::Buffer,
    /// Keep the copy source and its transient reservation alive through the
    /// worker's bounded GPU wait. wgpu also retains the texture internally,
    /// but Kettle's accounting must cover the same lifetime.
    _capture_texture: wgpu::Texture,
    _capture_gpu: kettle_core::GraphicsReservation,
    /// The MAP_READ staging buffer is a second allocation, not part of the
    /// capture texture. Charge it independently for the same in-flight
    /// lifetime so the process GPU limit bounds actual screenshot resources.
    _staging_gpu: kettle_core::GraphicsReservation,
    width: u32,
    height: u32,
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
    format: wgpu::TextureFormat,
    /// Whether the captured surface holds kettle's premultiplied scene.
    /// `PostMultiplied` is the sole exception because its final presentation
    /// pass has already converted the completed scene to straight alpha. PNG
    /// stores straight alpha, so every direct-scene readback is converted.
    premultiplied: bool,
    request: ScreenshotRequest,
}

struct ScreenshotJob {
    device: wgpu::Device,
    gpu_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    gpu_fault: std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    submission: wgpu::SubmissionIndex,
    prepared: PreparedScreenshot,
}

struct ScreenshotPersistenceJob {
    request: ScreenshotRequest,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    format: wgpu::TextureFormat,
    premultiplied: bool,
}

const MAX_SCREENSHOT_PERSISTENCE_JOBS: usize = 2;

struct ScreenshotPersistencePermit(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl ScreenshotPersistencePermit {
    fn try_acquire(outstanding: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Option<Self> {
        outstanding
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |current| (current < MAX_SCREENSHOT_PERSISTENCE_JOBS).then_some(current + 1),
            )
            .ok()
            .map(|_| Self(outstanding.clone()))
    }
}

impl Drop for ScreenshotPersistencePermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct ScreenshotPersistenceWork {
    job: ScreenshotPersistenceJob,
    _permit: ScreenshotPersistencePermit,
}

#[derive(Clone)]
struct ScreenshotPersistencePool {
    sender: std::sync::mpsc::SyncSender<ScreenshotPersistenceWork>,
    outstanding: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

static SCREENSHOT_PERSISTENCE_POOL: std::sync::OnceLock<
    Result<ScreenshotPersistencePool, (std::io::ErrorKind, String)>,
> = std::sync::OnceLock::new();

impl ScreenshotPersistencePool {
    /// Return the one process-wide persistence pool.
    ///
    /// Renderers are per window and are replaced during GPU recovery. Owning
    /// these threads from a renderer would let every replacement leave another
    /// pair blocked in an encoder or filesystem call. The global retains one
    /// sender for the process lifetime, so there are exactly two persistence
    /// threads and one shared admission counter across all windows and renderer
    /// generations.
    fn shared() -> std::io::Result<Self> {
        match SCREENSHOT_PERSISTENCE_POOL.get_or_init(|| {
            Self::start_process_pool().map_err(|error| (error.kind(), error.to_string()))
        }) {
            Ok(pool) => Ok(pool.clone()),
            Err((kind, message)) => Err(std::io::Error::new(*kind, message.clone())),
        }
    }

    fn start_process_pool() -> std::io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ScreenshotPersistenceWork>(
            MAX_SCREENSHOT_PERSISTENCE_JOBS,
        );
        let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
        let outstanding = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..MAX_SCREENSHOT_PERSISTENCE_JOBS {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("kettle-shot-save-{}", index + 1))
                .spawn(move || {
                    loop {
                        let job = receiver
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .recv();
                        let Ok(job) = job else {
                            break;
                        };
                        let ScreenshotPersistenceWork { job, _permit } = job;
                        let completion = job.request.completion.clone();
                        let result = finish_live_screenshot_persistence(job);
                        drop(_permit);
                        if let Some(tx) = completion {
                            let _ = tx.send(result.clone());
                        }
                        match result {
                            Ok(path) => log::info!("screenshot saved: {}", path.display()),
                            Err(error) => log::warn!("take_screenshot persistence failed: {error}"),
                        }
                    }
                })
                .map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("could not create screenshot persistence worker: {error}"),
                    )
                })?;
        }
        Ok(Self {
            sender,
            outstanding,
        })
    }

    fn try_submit(
        &self,
        job: ScreenshotPersistenceJob,
    ) -> Result<(), ScreenshotPersistenceSubmitError> {
        let Some(permit) = ScreenshotPersistencePermit::try_acquire(&self.outstanding) else {
            return Err(ScreenshotPersistenceSubmitError::Busy(Box::new(job)));
        };
        let work = ScreenshotPersistenceWork {
            job,
            _permit: permit,
        };
        match self.sender.try_send(work) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(work)) => {
                Err(ScreenshotPersistenceSubmitError::Busy(Box::new(work.job)))
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(work)) => Err(
                ScreenshotPersistenceSubmitError::Disconnected(Box::new(work.job)),
            ),
        }
    }
}

enum ScreenshotPersistenceSubmitError {
    Busy(Box<ScreenshotPersistenceJob>),
    Disconnected(Box<ScreenshotPersistenceJob>),
}

struct ScreenshotWorker {
    sender: std::sync::mpsc::SyncSender<ScreenshotJob>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ScreenshotWorker {
    fn start() -> std::io::Result<Self> {
        let persistence = ScreenshotPersistencePool::shared()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ScreenshotJob>(1);
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_busy = busy.clone();
        std::thread::Builder::new()
            .name("kettle-screenshot".to_string())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let completion = job.prepared.request.completion.clone();
                    let captured = finish_live_screenshot_capture(job);
                    // GPU capture admission ends as soon as the readback has
                    // moved into bounded CPU memory. Persistence has its own
                    // fixed-size pool, so a cancelled request stuck in an
                    // encoder or filesystem flush cannot retain GPU accounting
                    // or make every later capture BUSY indefinitely.
                    worker_busy.store(false, std::sync::atomic::Ordering::Release);
                    match captured {
                        Ok(job) => match persistence.try_submit(job) {
                            Ok(()) => {}
                            Err(ScreenshotPersistenceSubmitError::Busy(job)) => {
                                let error = "screenshot persistence workers are busy; retry after an earlier save finishes".to_string();
                                if let Some(tx) = job.request.completion {
                                    let _ = tx.send(Err(error.clone()));
                                }
                                log::warn!("take_screenshot persistence rejected: {error}");
                            }
                            Err(ScreenshotPersistenceSubmitError::Disconnected(job)) => {
                                let error = "screenshot persistence workers disconnected".to_string();
                                if let Some(tx) = job.request.completion {
                                    let _ = tx.send(Err(error.clone()));
                                }
                                log::warn!("take_screenshot persistence rejected: {error}");
                            }
                        },
                        Err(error) => {
                            if let Some(tx) = completion {
                                let _ = tx.send(Err(error.clone()));
                            }
                            log::warn!("take_screenshot capture failed: {error}");
                        }
                    }
                }
            })
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not create screenshot worker: {error}"),
                )
            })?;
        Ok(Self { sender, busy })
    }

    fn is_busy(&self) -> bool {
        self.busy.load(std::sync::atomic::Ordering::Acquire)
    }

    fn try_submit(&self, job: ScreenshotJob) -> Result<(), ScreenshotSubmitError> {
        if self
            .busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(ScreenshotSubmitError::Busy(Box::new(job)));
        }
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(job)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(ScreenshotSubmitError::Busy(Box::new(job)))
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(job)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(ScreenshotSubmitError::Disconnected(Box::new(job)))
            }
        }
    }
}

enum ScreenshotSubmitError {
    Busy(Box<ScreenshotJob>),
    Disconnected(Box<ScreenshotJob>),
}

/// One background-image decode request: the configured path plus the
/// resolved blur radius (0 when `background-blur` is off). Pure CPU input —
/// no GPU handles — so it's trivially `Send` across the worker thread
/// boundary, unlike `ScreenshotJob`.
struct BgImageJob {
    path: String,
    blur_radius: u32,
}

/// The decoded frames for a finished `BgImageJob`. Carries the request's key
/// back alongside the result so the render thread can tell a fresh result
/// from a stale one (the config may have moved on to a different image/blur
/// while this job was still decoding).
struct BgImageResult {
    path: String,
    blur_radius: u32,
    /// Empty when the decode failed (bad path, unsupported format, decode
    /// error) — mirrors the synchronous path's `None => Vec::new()` handling
    /// so `apply_bg_image_worker_result` can reuse the same
    /// failed-decode-caches-the-key self-heal behavior.
    frames: Vec<bg_image::BgFrame>,
}

/// Lazy, single-consumer background-image decode worker. Mirrors
/// `ScreenshotWorker`'s shape (a `busy` flag guarding a capacity-1 job
/// channel) but adds a result channel: unlike a screenshot save, a finished
/// decode has to feed data back into `render_frame` rather than just log a
/// side effect.
struct BgImageWorker {
    sender: std::sync::mpsc::SyncSender<BgImageJob>,
    receiver: std::sync::mpsc::Receiver<BgImageResult>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

enum BgImageSubmitError {
    /// A different job is still decoding on the worker thread. The caller
    /// should just wait — `apply_bg_image_worker_result` will pick up the
    /// in-flight job's result once it lands, and `request_bg_image_reload`
    /// will resubmit for the (still-current) desired key on a later frame.
    Busy,
    /// The worker thread exited (e.g. panicked mid-decode). The caller
    /// drops the worker so the next reload attempt spawns a fresh one.
    Disconnected,
}

impl BgImageWorker {
    fn start() -> std::io::Result<Self> {
        let (sender, job_rx) = std::sync::mpsc::sync_channel::<BgImageJob>(1);
        let (result_tx, receiver) = std::sync::mpsc::channel::<BgImageResult>();
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_busy = busy.clone();
        std::thread::Builder::new()
            .name("kettle-bg-image".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    // Same decode+blur helper the old synchronous path called
                    // inline in `render_frame` — only WHERE it runs changed.
                    let frames =
                        bg_image::decode_bg_image_frames_with_blur(&job.path, job.blur_radius)
                            .unwrap_or_default();
                    let delivered = result_tx.send(BgImageResult {
                        path: job.path,
                        blur_radius: job.blur_radius,
                        frames,
                    });
                    worker_busy.store(false, std::sync::atomic::Ordering::Release);
                    if delivered.is_err() {
                        // The Renderer (and its `BgImageWorker`) was dropped —
                        // no one is left to hand results to.
                        break;
                    }
                }
            })
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not create background-image decode worker: {error}"),
                )
            })?;
        Ok(Self {
            sender,
            receiver,
            busy,
        })
    }

    fn try_submit(&self, job: BgImageJob) -> Result<(), BgImageSubmitError> {
        if self
            .busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(BgImageSubmitError::Busy);
        }
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(BgImageSubmitError::Busy)
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(BgImageSubmitError::Disconnected)
            }
        }
    }

    /// Non-blocking: `None` when no decode has finished since the last poll.
    fn try_recv(&self) -> Option<BgImageResult> {
        self.receiver.try_recv().ok()
    }
}

fn wait_for_screenshot_submission_with(
    mut poll: impl FnMut() -> Result<(), wgpu::PollError>,
    device_lost: impl Fn() -> bool,
    mut device_loss_detected: impl FnMut(),
    max_timeouts: u32,
    mut submission_stalled: impl FnMut(),
) -> Result<(), String> {
    let mut timeouts = 0_u32;
    loop {
        let polled = poll();
        // wgpu may deliver the device-lost callback while `poll` itself still
        // reports success, or alongside a backend-specific error. The flag is
        // the authoritative result and must wake an occluded event loop on
        // every path, not only after `Timeout`.
        if device_lost() {
            device_loss_detected();
            return Err("screenshot GPU wait ended because the device was lost".to_string());
        }
        match polled {
            Ok(()) => return Ok(()),
            Err(wgpu::PollError::Timeout) => {
                timeouts = timeouts.saturating_add(1);
                if timeouts >= max_timeouts {
                    submission_stalled();
                    return Err(format!(
                        "screenshot GPU submission did not retire after {timeouts} bounded waits; resetting the GPU device"
                    ));
                }
                log::warn!(
                    "screenshot GPU submission is still pending after {timeouts} bounded wait(s); retaining its resources until completion or device loss"
                );
            }
            Err(error) => return Err(format!("screenshot GPU wait failed: {error:?}")),
        }
    }
}

fn finish_live_screenshot_capture(job: ScreenshotJob) -> Result<ScreenshotPersistenceJob, String> {
    let ScreenshotJob {
        device,
        gpu_lost,
        gpu_fault,
        submission,
        prepared,
    } = job;
    let PreparedScreenshot {
        staging,
        _capture_texture: capture_texture,
        _capture_gpu: capture_gpu,
        _staging_gpu: staging_gpu,
        width,
        height,
        unpadded_bytes_per_row,
        padded_bytes_per_row,
        format,
        premultiplied,
        request,
    } = prepared;
    let recovery_wake = request.recovery_wake.clone();
    let buffer_slice = staging.slice(..);
    let (map_tx, map_rx) = std::sync::mpsc::sync_channel(1);
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = map_tx.send(result.map_err(|error| format!("{error:?}")));
    });
    // A finite wait protects the worker from one uninterruptible driver call,
    // not the accounting lifetime. `Timeout` explicitly means the submission
    // may still own the texture and staging buffer, so retry while retaining
    // `prepared` and its reservations. A latched device loss is the only safe
    // early retirement boundary.
    wait_for_screenshot_submission_with(
        || {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission.clone()),
                    timeout: Some(LIVE_SCREENSHOT_TIMEOUT),
                })
                .map(|_| ())
        },
        || gpu_lost.load(std::sync::atomic::Ordering::Acquire),
        || {
            if let Some(wake) = recovery_wake.as_ref() {
                wake.wake();
            }
        },
        LIVE_SCREENSHOT_MAX_GPU_WAITS,
        || {
            latch_gpu_fault(
                &gpu_lost,
                &gpu_fault,
                "submission_stalled",
                "screenshot GPU submission did not retire within 10 seconds".to_string(),
            );
            // A submission that has not retired after two five-second waits is
            // no longer a screenshot problem: the shared device is wedged.
            // Destroying it is the only safe boundary at which the in-flight
            // texture and staging reservations may be released. The UI sees
            // the latched fault and rebuilds every renderer on a fresh device.
            device.destroy();
            if let Some(wake) = recovery_wake.as_ref() {
                wake.wake();
            }
        },
    )?;
    map_rx
        .recv_timeout(std::time::Duration::from_millis(100))
        .map_err(|_| "screenshot map callback timed out".to_string())?
        .map_err(|error| format!("screenshot map failed: {error}"))?;

    let mapped = buffer_slice
        .get_mapped_range()
        .map_err(|error| format!("screenshot mapped range failed: {error:?}"))?;
    let pixel_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "screenshot pixel size overflow".to_string())?;
    let capacity = usize::try_from(pixel_bytes)
        .map_err(|_| "screenshot pixel buffer does not fit this platform".to_string())?;
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    let mut rgba = Vec::with_capacity(capacity);
    for row in 0..height {
        let start = usize::try_from(u64::from(row) * u64::from(padded_bytes_per_row))
            .map_err(|_| "screenshot row offset overflow".to_string())?;
        let end = start
            .checked_add(unpadded_bytes_per_row as usize)
            .ok_or_else(|| "screenshot row end overflow".to_string())?;
        let row_pixels = mapped
            .get(start..end)
            .ok_or_else(|| "screenshot mapped buffer is shorter than expected".to_string())?;
        if bgra {
            for chunk in row_pixels.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
            }
        } else {
            rgba.extend_from_slice(row_pixels);
        }
    }
    drop(mapped);
    staging.unmap();
    // The GPU submission has retired and every byte now lives in `rgba`.
    // Drop both resources and both reservations before any encoder or
    // filesystem call can block. Kettle's GPU accounting therefore reflects
    // the actual device lifetime rather than the unrelated PNG lifetime.
    drop(staging);
    drop(capture_texture);
    drop(capture_gpu);
    drop(staging_gpu);

    if request.is_cancelled() {
        return Err("screenshot request was cancelled".to_string());
    }

    Ok(ScreenshotPersistenceJob {
        request,
        width,
        height,
        rgba,
        format,
        premultiplied,
    })
}

fn finish_live_screenshot_persistence(
    job: ScreenshotPersistenceJob,
) -> Result<std::path::PathBuf, String> {
    let ScreenshotPersistenceJob {
        request,
        width,
        height,
        rgba,
        format,
        premultiplied,
    } = job;
    if request.is_cancelled() {
        return Err("screenshot request was cancelled".to_string());
    }

    let (out_w, out_h, mut out_pixels) = crop_screenshot(width, height, rgba, request.crop)?;
    // A `PreMultiplied` surface holds premultiplied colour, and PNG stores
    // straight alpha. Converting after the crop touches only the pixels that
    // are actually saved.
    if premultiplied {
        unpremultiply_rgba8(&mut out_pixels, format.is_srgb());
    }
    use image::{ImageBuffer, Rgba};
    let image: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(out_w, out_h, out_pixels)
        .ok_or_else(|| "screenshot image buffer shape is invalid".to_string())?;
    // A terminal screenshot can contain the same class of transient
    // secrets a terminal session can (a password briefly echoed, an API
    // key, a private diff) that `kettle-core`'s `record.rs` already
    // protects by chmod'ing `.cast` files to `0o600`. Opening the PNG
    // ourselves with owner-only permissions (rather than letting
    // `ImageBuffer::save`'s internal `File::create` pick up the process
    // umask, typically `0o644` on a `022` umask) closes that gap for the
    // render crate's screenshot path too.
    if request.is_cancelled() {
        return Err("screenshot request was cancelled".to_string());
    }
    let file = create_screenshot_file(&request.out_path, request.output_policy)
        .map_err(|error| format!("screenshot output file could not be opened: {error}"))?;
    let cancellation = request.cancellation.clone();
    persist_screenshot_file_if(
        file,
        |writer| {
            image
                .write_to(writer, image::ImageFormat::Png)
                .map_err(|error| format!("PNG save failed: {error}"))
        },
        || {
            cancellation
                .as_ref()
                .is_none_or(ScreenshotCancellation::commit)
        },
    )?;
    Ok(request.out_path)
}

#[derive(Debug)]
struct CreatedScreenshotFile {
    file: Option<kettle_state::StagedUserSelectedFile>,
}

impl CreatedScreenshotFile {
    fn file(&self) -> &std::fs::File {
        self.file
            .as_ref()
            .expect("created screenshot file is present")
    }

    fn file_mut(&mut self) -> &mut std::fs::File {
        self.file
            .as_mut()
            .expect("created screenshot file is present")
    }

    fn sync_for_publish(&self) -> std::io::Result<()> {
        self.file
            .as_ref()
            .expect("created screenshot file is present")
            .sync_for_publish()
    }

    fn publish_synced(mut self) -> Result<(), kettle_state::StagedFilePublishError> {
        self.file
            .take()
            .expect("created screenshot file is present")
            .publish_synced()
    }

    fn discard(mut self) -> std::io::Result<()> {
        self.file
            .take()
            .expect("created screenshot file is present")
            .discard()
    }
}

impl std::ops::Deref for CreatedScreenshotFile {
    type Target = std::fs::File;

    fn deref(&self) -> &Self::Target {
        self.file()
    }
}

impl std::io::Write for CreatedScreenshotFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(self.file_mut(), buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.file_mut())
    }
}

impl std::io::Seek for CreatedScreenshotFile {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(self.file_mut(), position)
    }
}

#[cfg(test)]
fn persist_screenshot_file(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
) -> Result<(), String> {
    persist_screenshot_file_if(file, encode, || true)
}

fn persist_screenshot_file_if(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    publish: impl FnOnce() -> bool,
) -> Result<(), String> {
    persist_screenshot_file_with_flush_and_publish(file, encode, std::io::Write::flush, publish)
}

#[cfg(test)]
fn persist_screenshot_file_with_flush(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    flush: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> std::io::Result<()>,
) -> Result<(), String> {
    persist_screenshot_file_with_flush_and_publish(file, encode, flush, || true)
}

fn persist_screenshot_file_with_flush_and_publish(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    flush: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> std::io::Result<()>,
    begin_publication: impl FnOnce() -> bool,
) -> Result<(), String> {
    persist_screenshot_file_with_steps(
        file,
        encode,
        flush,
        CreatedScreenshotFile::sync_for_publish,
        begin_publication,
    )
}

fn persist_screenshot_file_with_steps(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    flush: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> std::io::Result<()>,
    sync: impl FnOnce(&CreatedScreenshotFile) -> std::io::Result<()>,
    begin_publication: impl FnOnce() -> bool,
) -> Result<(), String> {
    fn fail_file(file: CreatedScreenshotFile, error: String) -> Result<(), String> {
        match file.discard() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; partial-file cleanup failed: {cleanup}")),
        }
    }

    fn fail(
        writer: std::io::BufWriter<CreatedScreenshotFile>,
        error: String,
    ) -> Result<(), String> {
        let (file, _) = writer.into_parts();
        fail_file(file, error)
    }

    let mut writer = std::io::BufWriter::new(file);
    if let Err(error) = encode(&mut writer) {
        return fail(writer, error);
    }
    if let Err(error) = flush(&mut writer) {
        return fail(writer, format!("PNG save failed: {error}"));
    }
    let (file, buffered) = writer.into_parts();
    match buffered {
        Ok(bytes) if bytes.is_empty() => {}
        Ok(_) => {
            return fail_file(
                file,
                "PNG save failed: flush left buffered bytes".to_string(),
            );
        }
        Err(_) => {
            return fail_file(file, "PNG save failed: writer panicked".to_string());
        }
    }
    // Encoding, buffering, and the potentially slow inode flush deliberately
    // happen while publication remains cancellable. Commit immediately before
    // the atomic no-replace filesystem operation; if the caller won the
    // timeout race, the armed file guard removes the exact staging leaf instead
    // of allowing a late PNG to contradict the response.
    if let Err(error) = sync(&file) {
        return fail_file(file, format!("PNG durability flush failed: {error}"));
    }
    if !begin_publication() {
        return fail_file(file, "screenshot request was cancelled".to_string());
    }
    file.publish_synced()
        .map_err(|error| screenshot_publication_error(&error))?;
    Ok(())
}

fn screenshot_publication_error(error: &kettle_state::StagedFilePublishError) -> String {
    screenshot_publication_error_message(
        error.destination_may_exist(),
        error.kind(),
        &error.to_string(),
    )
}

fn screenshot_publication_error_message(
    kettle_may_have_published: bool,
    kind: std::io::ErrorKind,
    detail: &str,
) -> String {
    if kettle_may_have_published {
        format!(
            "PNG publication completed, but cleanup or durability verification failed; Kettle may have published the destination: {detail}"
        )
    } else if kind == std::io::ErrorKind::AlreadyExists {
        format!(
            "Kettle did not publish this screenshot because the destination already exists: {detail}"
        )
    } else {
        format!("Kettle did not publish this screenshot: {detail}")
    }
}

/// Stages an owner-only PNG sibling (`0600` on Unix, a protected current-user
/// DACL on Windows), refusing any existing destination at publication.
/// Private-state outputs also reject untrusted ancestors; an explicit
/// user-selected output pins and verifies its already-existing parent.
/// Screenshot PNGs may capture private on-screen content; see the call site in
/// `finish_live_screenshot`.
fn create_screenshot_file(
    path: &std::path::Path,
    policy: ScreenshotOutputPolicy,
) -> std::io::Result<CreatedScreenshotFile> {
    // The final no-replace link/rename, not the earlier
    // `validate_screenshot_path` metadata probe, is the security boundary. The
    // probe runs when the request arrives; GPU readback and encoding happen
    // later. A regular file or symlink planted during that interval therefore
    // wins with `AlreadyExists` rather than being followed or replaced. PNG
    // bytes stream only into an owner-only sibling, so the requested path is
    // absent until a complete, flushed inode is atomically published.
    if matches!(policy, ScreenshotOutputPolicy::PrivateState) {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("screenshot output has no parent: {}", path.display()),
            )
        })?;
        kettle_state::create_private_dirs(parent)?;
        kettle_state::validate_trusted_directory(parent)?;
    }
    let file = kettle_state::stage_user_selected_file_new(path)?;
    Ok(CreatedScreenshotFile { file: Some(file) })
}

fn crop_screenshot(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    crop: Option<(f32, f32, f32, f32)>,
) -> Result<(u32, u32, Vec<u8>), String> {
    let Some((cx, cy, cw, ch)) = crop else {
        return Ok((width, height, rgba));
    };
    let cx = cx.max(0.0) as u32;
    let cy = cy.max(0.0) as u32;
    let x_end = cx.saturating_add(cw.max(1.0) as u32).min(width);
    let y_end = cy.saturating_add(ch.max(1.0) as u32).min(height);
    let cropped_w = x_end.saturating_sub(cx);
    let cropped_h = y_end.saturating_sub(cy);
    if cropped_w == 0 || cropped_h == 0 {
        return Err("screenshot crop is outside the surface".to_string());
    }
    let capacity = u64::from(cropped_w)
        .checked_mul(u64::from(cropped_h))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| "screenshot crop size overflow".to_string())?;
    let source_stride = usize::try_from(u64::from(width) * 4)
        .map_err(|_| "screenshot source stride overflow".to_string())?;
    let col_start = usize::try_from(u64::from(cx) * 4)
        .map_err(|_| "screenshot crop column overflow".to_string())?;
    let col_end = usize::try_from(u64::from(x_end) * 4)
        .map_err(|_| "screenshot crop column overflow".to_string())?;
    let mut cropped = Vec::with_capacity(capacity);
    for y in cy..y_end {
        let row_start = usize::try_from(u64::from(y) * source_stride as u64)
            .map_err(|_| "screenshot crop row overflow".to_string())?;
        let row_end = row_start
            .checked_add(source_stride)
            .ok_or_else(|| "screenshot source row overflow".to_string())?;
        let row = rgba
            .get(row_start..row_end)
            .ok_or_else(|| "screenshot source buffer is shorter than expected".to_string())?;
        cropped.extend_from_slice(
            row.get(col_start..col_end)
                .ok_or_else(|| "screenshot crop columns are outside the source".to_string())?,
        );
    }
    Ok((cropped_w, cropped_h, cropped))
}

#[cfg(test)]
mod live_screenshot_tests {
    use super::{
        MAX_SCREENSHOT_PERSISTENCE_JOBS, ScreenshotCancellation, ScreenshotOutputPolicy,
        ScreenshotPersistencePermit, ScreenshotPersistencePool, create_screenshot_file,
        crop_screenshot, persist_screenshot_file, persist_screenshot_file_if,
        persist_screenshot_file_with_flush, persist_screenshot_file_with_steps, production_source,
        screenshot_publication_error_message, screenshot_target_bytes,
        wait_for_screenshot_submission_with,
    };

    fn test_tempdir() -> kettle_test_support::PrivateTempDir {
        kettle_test_support::private_tempdir("kettle-render-test-")
    }

    fn pixels(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|pixel| [pixel as u8, 0, 0, 255])
            .collect()
    }

    /// Metal returns `SurfaceError::Occluded` before it vends a drawable. The
    /// capture therefore has to render and copy Kettle's own target before
    /// swapchain acquisition, and every no-drawable outcome still has to submit
    /// that work. Moving any of those operations restores the control timeout
    /// even though visible screenshots continue to pass.
    #[test]
    fn live_capture_is_encoded_before_swapchain_acquisition() {
        let src = production_source();
        let body = src
            .split("pub fn render_frame_with_status_and_pre_present")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("render_frame_with_status_and_pre_present body");
        let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
        let target = normalized
            .find("self.create_screenshot_target(target_size)")
            .expect("live capture must allocate an independent offscreen target");
        let render = normalized
            .find("self.encode_scene_pass(&capture.view")
            .expect("live capture must render the scene into its target");
        let copy = normalized
            .find("self.prepare_texture_screenshot(")
            .expect("live capture must copy the rendered target");
        let acquire = normalized
            .find("self.surface.get_current_texture()")
            .expect("live rendering must acquire the swapchain");
        let presentation = normalized
            .find(".presentation .ensure_target(")
            .expect("translucent visible frames need a presentation target");
        assert!(
            target < render && render < copy && copy < acquire && acquire < presentation,
            "capture ordering must be target creation < scene render < readback copy < acquire < presentation allocation"
        );
        let acquire_tail = &normalized[acquire..];
        assert_eq!(
            acquire_tail
                .matches("self.submit_offscreen_screenshot(encoder, prepared_screenshot)")
                .count(),
            6,
            "Occluded, Timeout, Outdated, Lost, Validation, and presentation-allocation failure must all submit the offscreen capture"
        );
        assert!(
            src.contains(
                "usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC"
            ),
            "only Kettle's transient capture texture, not the compositor surface, needs COPY_SRC"
        );
        let creator = src
            .split("fn create_screenshot_target")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("screenshot target creator");
        let staging_reservation = creator
            .find("reserve_transient_gpu(staging_bytes)")
            .expect("staging memory must be reserved");
        let texture_creation = creator
            .find("device.create_texture")
            .expect("capture texture must be created");
        assert!(
            staging_reservation < texture_creation,
            "all screenshot allocations must be reserved before a texture can enter an encoder"
        );
    }

    #[test]
    fn live_capture_keeps_the_advertised_6k_and_256_mib_bounds() {
        let six_k = screenshot_target_bytes(6016, 3384).expect("6K target must fit");
        assert!(
            six_k > 64 * 1024 * 1024,
            "this discriminates the old image cap"
        );
        assert_eq!(screenshot_target_bytes(8192, 8192), Some(256 * 1024 * 1024));
        assert_eq!(screenshot_target_bytes(8193, 8192), None);
        assert_eq!(screenshot_target_bytes(0, 1), None);

        let src = production_source();
        let creator = src
            .split("fn create_screenshot_target")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("screenshot target creator");
        assert!(
            creator.contains("reserve_transient_gpu(bytes)"),
            "large captures must use the process-only transient budget, not the 64 MiB image cap"
        );
        assert!(
            src.contains("_capture_texture: wgpu::Texture")
                && src.contains("_capture_gpu: kettle_core::GraphicsReservation")
                && src.contains("_staging_gpu: kettle_core::GraphicsReservation"),
            "the copy source, staging buffer, and both accounting reservations must travel with the worker job"
        );
    }

    #[test]
    fn control_timeout_cancellation_is_shared_with_the_renderer() {
        let cancellation = ScreenshotCancellation::default();
        let renderer_side = cancellation.clone();
        assert!(!renderer_side.is_cancelled());
        assert!(cancellation.cancel());
        assert!(renderer_side.is_cancelled());
        assert!(!renderer_side.commit(), "cancelled work cannot publish");

        let committed = ScreenshotCancellation::default();
        assert!(committed.commit());
        assert!(
            !committed.cancel(),
            "a caller must await the real result once publication committed"
        );
    }

    #[test]
    fn cancellation_during_encoding_discards_instead_of_publishing() {
        use std::io::Write as _;

        let dir = test_tempdir();
        let path = dir.path().join("cancelled-during-encode.png");
        let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected).unwrap();
        assert!(
            !path.exists(),
            "the requested leaf must stay absent while encoding is cancellable"
        );
        let cancellation = ScreenshotCancellation::default();
        let worker_state = cancellation.clone();
        let (encoding_tx, encoding_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);

        let worker = std::thread::spawn(move || {
            persist_screenshot_file_if(
                file,
                |writer| {
                    writer.write_all(b"encoded bytes").unwrap();
                    encoding_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Ok(())
                },
                || worker_state.commit(),
            )
        });
        encoding_rx.recv().unwrap();
        assert!(
            !path.exists(),
            "a blocked encoder must not expose a changing destination leaf"
        );
        assert!(
            cancellation.cancel(),
            "timeout must win while the encoder is still active"
        );
        resume_tx.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            "screenshot request was cancelled"
        );
        assert!(!path.exists(), "cancelled output must never become visible");
    }

    #[test]
    fn cancellation_can_win_during_the_staged_inode_sync() {
        use std::io::Write as _;

        let dir = test_tempdir();
        let path = dir.path().join("cancelled-during-sync.png");
        let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected).unwrap();
        let cancellation = ScreenshotCancellation::default();
        let worker_state = cancellation.clone();
        let (syncing_tx, syncing_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);

        let worker = std::thread::spawn(move || {
            persist_screenshot_file_with_steps(
                file,
                |writer| {
                    writer.write_all(b"encoded bytes").unwrap();
                    Ok(())
                },
                std::io::Write::flush,
                |_| {
                    syncing_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Ok(())
                },
                || worker_state.commit(),
            )
        });
        syncing_rx.recv().unwrap();
        assert!(
            cancellation.cancel(),
            "a slow durability flush must remain on the reversible side of publication"
        );
        resume_tx.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            "screenshot request was cancelled"
        );
        assert!(!path.exists(), "cancelled output must never become visible");
    }

    #[test]
    fn a_gpu_wait_timeout_keeps_accounting_live_until_retirement() {
        struct Reservation(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Reservation {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            }
        }

        let usage = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let reservation = Reservation(usage.clone());
        let mut polls = 0;
        wait_for_screenshot_submission_with(
            || {
                polls += 1;
                assert_eq!(
                    usage.load(std::sync::atomic::Ordering::Acquire),
                    1,
                    "timeout must not retire the in-flight reservation"
                );
                if polls == 1 {
                    Err(wgpu::PollError::Timeout)
                } else {
                    Ok(())
                }
            },
            || false,
            || {},
            2,
            || panic!("one timeout followed by success must not reset the device"),
        )
        .unwrap();
        assert_eq!(usage.load(std::sync::atomic::Ordering::Acquire), 1);
        drop(reservation);
        assert_eq!(usage.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn repeated_gpu_wait_timeouts_reset_instead_of_stranding_admission() {
        let resets = std::cell::Cell::new(0);
        let error = wait_for_screenshot_submission_with(
            || Err(wgpu::PollError::Timeout),
            || false,
            || {},
            2,
            || resets.set(resets.get() + 1),
        )
        .unwrap_err();
        assert_eq!(resets.get(), 1);
        assert!(error.contains("resetting the GPU device"));

        let src = production_source();
        let wait = src
            .split("wait_for_screenshot_submission_with(")
            .nth(2)
            .and_then(|rest| rest.split("map_rx").next())
            .expect("live screenshot GPU wait call");
        let destroy = wait.find("device.destroy()").expect("device reset");
        let wake = wait
            .rfind("wake.wake()")
            .expect("event-loop recovery wake after reset");
        assert!(
            destroy < wake,
            "a wedged device must wake the event loop after it is destroyed"
        );
    }

    #[test]
    fn device_loss_wakes_recovery_even_when_poll_reports_success() {
        let lost = std::cell::Cell::new(false);
        let wakes = std::cell::Cell::new(0_u32);
        let error = wait_for_screenshot_submission_with(
            || {
                lost.set(true);
                Ok(())
            },
            || lost.get(),
            || wakes.set(wakes.get() + 1),
            2,
            || panic!("a real device loss is not a submission timeout"),
        )
        .unwrap_err();
        assert!(error.contains("device was lost"));
        assert_eq!(wakes.get(), 1, "device loss must wake an occluded loop");
    }

    #[test]
    fn capture_admission_clears_before_persistence_can_block() {
        let src = production_source();
        let worker = src
            .split("impl ScreenshotWorker")
            .nth(1)
            .and_then(|rest| rest.split("enum ScreenshotSubmitError").next())
            .expect("ScreenshotWorker implementation");
        let clear = worker
            .find("worker_busy.store(false")
            .expect("worker busy flag must be cleared");
        let persist = worker
            .find("persistence.try_submit(job)")
            .expect("captured pixels must move to the bounded persistence pool");
        assert!(
            clear < persist,
            "filesystem persistence must not retain GPU capture admission"
        );
    }

    #[test]
    fn persistence_admission_is_process_wide_across_renderer_generations() {
        let first_generation = ScreenshotPersistencePool::shared().unwrap();
        let replacement_generation = ScreenshotPersistencePool::shared().unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &first_generation.outstanding,
            &replacement_generation.outstanding
        ));

        let outstanding = &first_generation.outstanding;
        let first = ScreenshotPersistencePermit::try_acquire(outstanding).unwrap();
        let second =
            ScreenshotPersistencePermit::try_acquire(&replacement_generation.outstanding).unwrap();
        assert!(ScreenshotPersistencePermit::try_acquire(outstanding).is_none());
        assert_eq!(
            outstanding.load(std::sync::atomic::Ordering::Acquire),
            MAX_SCREENSHOT_PERSISTENCE_JOBS
        );
        drop(first);
        let replacement = ScreenshotPersistencePermit::try_acquire(outstanding)
            .expect("a finished persistence job must reopen exactly one slot");
        drop((second, replacement));
        assert_eq!(outstanding.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn publication_errors_distinguish_kettles_commit_from_path_existence() {
        assert_eq!(
            screenshot_publication_error_message(
                false,
                std::io::ErrorKind::AlreadyExists,
                "racing writer won",
            ),
            "Kettle did not publish this screenshot because the destination already exists: racing writer won"
        );
        assert_eq!(
            screenshot_publication_error_message(
                false,
                std::io::ErrorKind::PermissionDenied,
                "parent refused publication",
            ),
            "Kettle did not publish this screenshot: parent refused publication"
        );
        assert_eq!(
            screenshot_publication_error_message(
                true,
                std::io::ErrorKind::Other,
                "directory sync failed",
            ),
            "PNG publication completed, but cleanup or durability verification failed; Kettle may have published the destination: directory sync failed"
        );
    }

    #[test]
    fn no_crop_preserves_surface_pixels() {
        let input = pixels(2, 2);
        let (width, height, output) = crop_screenshot(2, 2, input.clone(), None).unwrap();
        assert_eq!((width, height), (2, 2));
        assert_eq!(output, input);
    }

    #[test]
    fn crop_extracts_requested_rows_and_columns() {
        let (width, height, output) =
            crop_screenshot(3, 2, pixels(3, 2), Some((1.0, 0.0, 2.0, 2.0))).unwrap();
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            output
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            [1, 2, 4, 5]
        );
    }

    #[test]
    fn crop_rejects_rectangles_outside_the_surface() {
        let error = crop_screenshot(2, 2, pixels(2, 2), Some((5.0, 5.0, 1.0, 1.0))).unwrap_err();
        assert_eq!(error, "screenshot crop is outside the surface");
    }

    #[test]
    fn crop_rejects_short_source_buffers() {
        let error = crop_screenshot(2, 2, vec![0; 4], Some((0.0, 0.0, 2.0, 2.0))).unwrap_err();
        assert_eq!(error, "screenshot source buffer is shorter than expected");
    }

    // Privacy hardening (audit): a screenshot can capture the same class of
    // transient secrets a `.cast` recording can (kettle-core/src/record.rs
    // chmods those to 0o600) — the PNG file must land with the same
    // owner-only permissions regardless of the process umask.
    #[cfg(unix)]
    #[test]
    fn private_screenshot_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = test_tempdir();
        let path = dir.path().join("shot.png");
        let file =
            create_screenshot_file(&path, ScreenshotOutputPolicy::PrivateState).expect("open");
        let mode = file.metadata().expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "screenshot file must be owner-read/write only");
    }

    /// The write is `create_new` (O_EXCL): anything already at the path — a
    /// leftover file or, in the threat model, a symlink planted into the
    /// check-then-use window to redirect the write at a sensitive file — must
    /// make the open fail with `AlreadyExists` rather than being followed or
    /// truncated. This is the atomic half of the screenshot path-traversal fix
    /// (the ctl-side `validate_screenshot_path` pre-check is only a fast-fail).
    #[test]
    fn private_screenshot_file_refuses_a_pre_existing_path() {
        let dir = test_tempdir();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"stale").expect("seed file");
        let err = create_screenshot_file(&path, ScreenshotOutputPolicy::PrivateState)
            .expect_err("must refuse an existing path");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&path).expect("original file untouched"),
            b"stale",
            "an existing file at the path must not be truncated or overwritten"
        );
    }

    #[test]
    fn failed_screenshot_encode_removes_the_exact_created_leaf() {
        use std::io::Write as _;

        let dir = test_tempdir();
        for (name, policy) in [
            ("private.png", ScreenshotOutputPolicy::PrivateState),
            ("selected.png", ScreenshotOutputPolicy::UserSelected),
        ] {
            let path = dir.path().join(name);
            let file = create_screenshot_file(&path, policy).expect("create screenshot leaf");
            let error = persist_screenshot_file(file, |writer| {
                writer.write_all(b"partial PNG bytes").unwrap();
                Err("injected encoder failure".to_string())
            })
            .expect_err("the injected encoder failure must propagate");
            assert_eq!(error, "injected encoder failure");
            assert!(
                !path.exists(),
                "an encode failure must not strand a path that blocks retry: {}",
                path.display()
            );
        }
    }

    #[test]
    fn failed_screenshot_flush_removes_an_on_disk_partial_leaf() {
        use std::io::Write as _;

        let dir = test_tempdir();
        let path = dir.path().join("flush-failure.png");
        let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected)
            .expect("create screenshot leaf");
        let error = persist_screenshot_file_with_flush(
            file,
            |writer| {
                writer
                    .write_all(&vec![0x5a; 32 * 1024])
                    .map_err(|error| error.to_string())
            },
            |_writer| Err(std::io::Error::other("injected flush failure")),
        )
        .expect_err("the injected flush failure must propagate");
        assert!(error.contains("injected flush failure"));
        assert!(
            !path.exists(),
            "a flush failure after underlying writes must remove the partial leaf"
        );
    }

    #[test]
    fn selected_screenshot_never_replaces_a_racing_leaf() {
        use std::io::Write as _;

        let dir = test_tempdir();
        let path = dir.path().join("selected.png");
        let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected)
            .expect("create screenshot leaf");
        std::fs::write(&path, b"replacement").expect("install replacement leaf");

        let error = persist_screenshot_file(file, |writer| {
            writer
                .write_all(b"complete png")
                .map_err(|error| error.to_string())
        })
        .expect_err("no-replace publication must lose to the racing writer");
        assert!(
            error.starts_with(
                "Kettle did not publish this screenshot because the destination already exists:"
            ),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }

    /// The same O_EXCL guarantee, exercised against a symlink: a planted
    /// symlink at the output path must not be followed to overwrite its target.
    #[cfg(unix)]
    #[test]
    fn private_screenshot_file_refuses_to_follow_a_symlink() {
        let dir = test_tempdir();
        let sensitive = dir.path().join("sensitive");
        std::fs::write(&sensitive, b"secret").expect("seed target");
        let link = dir.path().join("shot.png");
        std::os::unix::fs::symlink(&sensitive, &link).expect("plant symlink");
        let err = create_screenshot_file(&link, ScreenshotOutputPolicy::UserSelected)
            .expect_err("must refuse a symlink");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&sensitive).expect("symlink target untouched"),
            b"secret",
            "the symlink target must not be truncated through the followed link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_selected_screenshot_accepts_a_public_existing_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = test_tempdir();
        let exports = dir.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o775)).unwrap();

        let private_path = exports.join("private.png");
        let error = create_screenshot_file(&private_path, ScreenshotOutputPolicy::PrivateState)
            .expect_err("the private-state policy must retain ancestor checks");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

        let selected_path = exports.join("selected.png");
        let file = create_screenshot_file(&selected_path, ScreenshotOutputPolicy::UserSelected)
            .expect("an explicit path may live under an ordinary user directory");
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}

/// The async background-image decode worker (audit fix: `render_frame` must
/// never block the render thread on `decode_bg_image_frames_with_blur` — see
/// `Renderer::request_bg_image_reload` / `Renderer::apply_bg_image_worker_result`).
/// Pure CPU + `std::sync::mpsc`, so — unlike `ScreenshotWorker`, which needs a
/// real `wgpu::Device` — this is fully unit-testable without a GPU.
#[cfg(test)]
mod bg_image_worker_tests {
    use super::{BgImageJob, BgImageWorker};

    /// A nonexistent path decodes to an empty frame list (mirrors the old
    /// synchronous path's `None => Vec::new()` handling) rather than blocking
    /// or panicking — and the result must arrive on `try_recv` carrying back
    /// the same `(path, blur_radius)` key the job was submitted with, which is
    /// exactly what `apply_bg_image_worker_result` keys its stale-result check
    /// on.
    #[test]
    fn worker_delivers_a_keyed_result_for_a_failed_decode() {
        let worker = BgImageWorker::start().expect("worker thread should start");
        let path = "/definitely/does/not/exist/kettle-test-wallpaper.png".to_string();
        assert!(
            worker
                .try_submit(BgImageJob {
                    path: path.clone(),
                    blur_radius: 8,
                })
                .is_ok(),
            "first submit on an idle worker must succeed"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            if let Some(result) = worker.try_recv() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not deliver a result within the timeout"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        assert_eq!(result.path, path);
        assert_eq!(result.blur_radius, 8);
        assert!(
            result.frames.is_empty(),
            "a decode failure must deliver an empty frame list, not block or panic"
        );
    }

    /// `try_submit` while a job is still in flight must report `Busy` rather
    /// than silently dropping or blocking — `request_bg_image_reload` relies
    /// on this to decide whether to (re)submit.
    #[test]
    fn worker_reports_busy_for_a_second_submit_before_the_first_completes() {
        let worker = BgImageWorker::start().expect("worker thread should start");
        assert!(
            worker
                .try_submit(BgImageJob {
                    path: "/definitely/does/not/exist/kettle-test-wallpaper-a.png".to_string(),
                    blur_radius: 0,
                })
                .is_ok()
        );
        // Best-effort race: submit again immediately. Either the first job
        // already finished (busy flag cleared) and this succeeds, or it's
        // still in flight and this reports Busy — both are valid outcomes;
        // what must NOT happen is a panic or a silently dropped job.
        let _ = worker.try_submit(BgImageJob {
            path: "/definitely/does/not/exist/kettle-test-wallpaper-b.png".to_string(),
            blur_radius: 0,
        });
        // Drain whatever results land within the timeout so the test doesn't
        // leak an assertion about ordering — the key behavior under test is
        // "no panic, no hang", already exercised above.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = 0;
        while seen < 1 && std::time::Instant::now() < deadline {
            if worker.try_recv().is_some() {
                seen += 1;
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert!(seen >= 1, "at least the first submitted job must complete");
    }
}

fn live_device_limits(adapter_limits: wgpu::Limits) -> wgpu::Limits {
    // Start with Kettle's normal WebGPU defaults, keep the adapter's full
    // surface resolution, then clamp every remaining request to what the
    // adapter actually advertises. Virtual GLES adapters can legitimately
    // expose no compute pipeline at all (`max_compute_workgroups... == 0`).
    // Asking them for wgpu's default 65_535 workgroups fails device creation
    // even though Kettle renders entirely through graphics pipelines. This is
    // also safer than requesting every adapter maximum: the application keeps
    // its deliberately small resource envelope except where presentation
    // resolution requires more.
    wgpu::Limits {
        max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
        ..Default::default()
    }
    .or_worse_values_from(&adapter_limits)
}

/// Clamp a window size into the range `Surface::configure` will accept.
///
/// Zero panics, and anything above the device's announced
/// `max_texture_dimension_2d` fails validation and leaves a stale surface that
/// paints nothing. Raising the REQUESTED limit at device creation (see
/// [`live_device_limits`]) lifts this ceiling to whatever the hardware really
/// supports, so an 8K or multi-display window keeps its true size instead of
/// being clipped at wgpu's default 8192 -- but the clamp itself has to stay.
/// Dropping it would turn a gracefully clipped window into a broken surface on
/// any adapter whose genuine limit is smaller than the window.
fn live_surface_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    let max = max_dimension.max(1);
    (width.clamp(1, max), height.clamp(1, max))
}

impl Renderer {
    /// Compatibility constructor for embedders that only provide a window.
    ///
    /// Winit applications should prefer [`Renderer::new_with_display_handle`]
    /// and pass `ActiveEventLoop::owned_display_handle()`. The owned event-loop
    /// handle lets wgpu initialize the Wayland GLES presentation path without
    /// retaining the window for the lifetime of the shared GPU context.
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
        Self::new_with_escalation(
            window,
            width,
            height,
            scale,
            cfg,
            AdapterEscalation::Preferred,
            None,
        )
        .await
    }

    /// Create a renderer while retaining an owned display connection separately
    /// from the surface window.
    ///
    /// For winit, `display_handle` should be
    /// `ActiveEventLoop::owned_display_handle()`. It is cheap to clone and,
    /// unlike an `Arc<Window>`, does not keep a closed OS window alive.
    pub async fn new_with_display_handle<W, D>(
        window: Arc<W>,
        display_handle: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation_and_display_handle(
            window,
            display_handle,
            width,
            height,
            scale,
            cfg,
            AdapterSelection::new(AdapterEscalation::Preferred, None),
        )
        .await
    }

    pub async fn new_with_escalation<W>(
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        escalation: AdapterEscalation,
        avoid: Option<GpuAdapterKey>,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation_inner(
            window,
            None,
            width,
            height,
            scale,
            cfg,
            AdapterSelection::new(escalation, avoid),
        )
        .await
    }

    /// Recovery-capable variant of [`Renderer::new_with_display_handle`].
    pub async fn new_with_escalation_and_display_handle<W, D>(
        window: Arc<W>,
        display_handle: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        selection: AdapterSelection,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation_inner(
            window,
            Some(OwnedGpuDisplayHandle::new(display_handle)),
            width,
            height,
            scale,
            cfg,
            selection,
        )
        .await
    }

    async fn new_with_escalation_inner<W>(
        window: Arc<W>,
        display_handle: Option<OwnedGpuDisplayHandle>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        selection: AdapterSelection,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        // Default, unpinned Auto startup probes one native backend at a time.
        // On Windows a successful DX12 request therefore never loads the
        // Vulkan ICD. Pins and explicit low/high policy need a cross-backend
        // view, while recovery deliberately inspects alternate adapters.
        // Startup is measured, not guessed. This init blocks the event-loop
        // thread and the window stays hidden until the first paint, so every
        // millisecond here is time the user spends looking at nothing --
        // and a comparator benchmark can only report the total. Splitting it
        // three ways says WHICH part to attack.
        let t_start = std::time::Instant::now();
        let (instance, surface, adapter) = resolve_window_adapter(
            window,
            display_handle.as_ref(),
            cfg,
            selection.escalation,
            selection.avoid,
            "Renderer::new",
        )
        .await?;
        let adapter_ms = t_start.elapsed().as_secs_f64() * 1000.0;
        let t_device = std::time::Instant::now();
        // wgpu's default requested limit is 8192 even when the adapter can
        // present a larger surface. Request the adapter's real 2D limit up
        // front so later high-DPI or multi-display resizes can configure the
        // swapchain at the window's actual physical dimensions.
        let required_limits = live_device_limits(adapter.limits());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-device"),
                required_limits,
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("failed to create device: {e:?}"))?;
        // v2.31.0: turn a GPU driver reset (TDR) / VRAM exhaustion into a logged,
        // observable event instead of wgpu's default panic (which `panic=abort`
        // turned into a hard crash). Installed once on the shared device.
        let gpu_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gpu_fault = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recovery_wake = std::sync::Arc::new(std::sync::Mutex::new(None));
        install_gpu_error_handlers(&device, &gpu_lost, &gpu_fault, &recovery_wake);
        let gpu = GpuContext {
            instance,
            adapter,
            device,
            queue,
            gpu_lost,
            gpu_fault,
            recovery_wake,
        };
        let device_ms = t_device.elapsed().as_secs_f64() * 1000.0;
        let t_rest = std::time::Instant::now();
        let built = Self::with_gpu_and_surface(gpu, surface, width, height, scale, cfg);
        // Named for what it actually spans: everything after device creation.
        // That INCLUDES the font-system time logged separately just below, so
        // the two must not be added together -- the earlier `pipelines+atlas`
        // label invited exactly that double-count.
        log::info!(
            "renderer init: adapter {adapter_ms:.1}ms, device {device_ms:.1}ms,              surface+fonts+pipelines {:.1}ms (font init logged separately is              part of it), total {:.1}ms",
            t_rest.elapsed().as_secs_f64() * 1000.0,
            t_start.elapsed().as_secs_f64() * 1000.0
        );
        built
    }

    /// C3 (multi-window): synchronous constructor for windows 2..N — reuses
    /// the shared [`GpuContext`] instead of requesting an adapter/device, so
    /// it never blocks the event loop (the ~1.5s async init and its hung-
    /// driver watchdog are a window-1-only cost). Fails cleanly if the shared
    /// adapter can't present to the new window's surface (e.g. a window on a
    /// display driven by a different GPU) — the caller falls back to keeping
    /// the tab where it was.
    pub fn new_with_gpu<W>(
        gpu: &GpuContext,
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        let surface = gpu.instance.create_surface(window)?;
        if !gpu.adapter.is_surface_supported(&surface) {
            return Err(anyhow!(
                "the shared GPU adapter cannot present to the new window's surface"
            ));
        }
        Self::with_gpu_and_surface(gpu.clone(), surface, width, height, scale, cfg)
    }

    /// Shared constructor tail: everything after a surface + GPU exist
    /// (format/alpha selection, surface configure, font system, pipelines).
    fn with_gpu_and_surface(
        gpu: GpuContext,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
    ) -> Result<Renderer> {
        let GpuContext {
            adapter,
            device,
            queue,
            ..
        } = gpu.clone();

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let alpha_mode = desired_alpha_mode(cfg, &caps.alpha_modes);
        let (width, height) =
            live_surface_dimensions(width, height, device.limits().max_texture_dimension_2d);
        let config = wgpu::SurfaceConfiguration {
            // Screenshots read the offscreen scene target, not the swapchain.
            // Keeping presentation to its sole required usage also supports
            // RDP/virtual adapters that advertise no surface COPY_SRC.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let supported_alpha_modes = caps.alpha_modes;

        let t_font_system = std::time::Instant::now();
        let mut font_system = FontSystem::new();
        let font_system_ms = t_font_system.elapsed().as_secs_f64() * 1000.0;
        let t_bundled = std::time::Instant::now();
        load_bundled_font(&mut font_system, kettle_config::font::REGULAR);
        // Split, because `FontSystem::new()` is the one people suspect (it
        // enumerates system fonts) and a combined figure cannot exonerate it.
        log::info!(
            "renderer init: FontSystem::new {font_system_ms:.1}ms, bundled font {:.1}ms",
            t_bundled.elapsed().as_secs_f64() * 1000.0
        );

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
        // had this clamp; `Renderer::new` silently didn't,
        // so the bound was only enforced after a Ctrl+0 ResetFontSize
        // round-trip — same "downstream cache stale at startup" shape
        // as the `set_font_family` fix.
        let font_size = clamp_font_size(cfg.font_size);
        // Physical-pixel metrics — logical font size × DPI scale.
        let metrics = metrics_for(font_size, scale);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let tabbar_buffer = TextBuffer::new(&mut font_system, metrics);
        let new_tab_arrow_buffer = TextBuffer::new(&mut font_system, metrics);
        let scroll_left_buffer = TextBuffer::new(&mut font_system, metrics);
        let scroll_right_buffer = TextBuffer::new(&mut font_system, metrics);
        let tab_close_buffer = TextBuffer::new(&mut font_system, metrics);
        let mut search_buffer = TextBuffer::new(&mut font_system, metrics);
        search_buffer.set_wrap(Wrap::None);
        let status_bar_buffer = TextBuffer::new(&mut font_system, metrics);
        let resize_overlay_buffer = TextBuffer::new(&mut font_system, metrics);
        let mut completion_header_buffer = TextBuffer::new(&mut font_system, metrics);
        completion_header_buffer.set_wrap(Wrap::None);
        let mut completion_count_buffer = TextBuffer::new(&mut font_system, metrics);
        completion_count_buffer.set_wrap(Wrap::None);
        let mut media_receipt_title_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_title_buffer.set_wrap(Wrap::None);
        let mut media_receipt_detail_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_detail_buffer.set_wrap(Wrap::None);
        let mut media_receipt_dismiss_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_dismiss_buffer.set_wrap(Wrap::None);
        media_receipt_dismiss_buffer.set_text(
            "×",
            &Attrs::new().weight(Weight::BOLD),
            Shaping::Advanced,
            None,
        );
        let mut media_receipt_badge_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_badge_buffer.set_wrap(Wrap::None);
        media_receipt_badge_buffer.set_text(
            "▶",
            &Attrs::new().weight(Weight::BOLD),
            Shaping::Advanced,
            None,
        );
        let mut ime_buffer = TextBuffer::new(&mut font_system, metrics);
        ime_buffer.set_wrap(Wrap::None);
        let (cell_w, cell_h) =
            measure_cell(&mut font_system, &mut measure, &cfg.font_family, metrics);
        // Honor cfg.cell_width / cell_height multipliers
        // (Terminator parity). Values are pre-clamped to [0.5, 3.0]
        // at parse time so the cell can't degenerate to 0 here.
        let cell_scale_w = cfg.cell_width.max(0.01);
        let cell_scale_h = cfg.cell_height.max(0.01);
        let cell_w = cell_w * cell_scale_w;
        let cell_h = cell_h * cell_scale_h;

        let pane_bases = QuadPipeline::new_replace(&device, format);
        let live_pane_bases = QuadPipeline::new_replace(&device, format);
        let quads = QuadPipeline::new(&device, format);
        let pane_outlines = OutlinePipeline::new(&device, format);
        let overlay_quads = QuadPipeline::new(&device, format);
        let menu_quads = QuadPipeline::new(&device, format);
        let menu_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let cursor_glyph_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let cursor_glyph_buffer = TextBuffer::new(&mut font_system, metrics);
        let graphics_budget = kettle_core::GraphicsBudget::default();
        let presentation =
            present::PresentationPipeline::new(&device, format, graphics_budget.clone());
        let imgs =
            imgpipe::ImagePipeline::new_with_budget(&device, format, graphics_budget.clone())
                .ok_or_else(|| {
                    anyhow!("GPU graphics budget exhausted while creating image pipeline")
                })?;
        let media_receipt_img = imgpipe::ImagePipeline::new_with_budget_and_instance_limit(
            &device,
            format,
            graphics_budget.clone(),
            1,
        )
        .ok_or_else(|| {
            anyhow!("GPU graphics budget exhausted while creating image-receipt pipeline")
        })?;
        // v2.23.0: separate pipeline so the wallpaper draws behind cell/chrome
        // quads (see the `bg_imgs` field docs).
        let bg_imgs = imgpipe::ImagePipeline::new_with_budget_and_instance_limit(
            &device,
            format,
            graphics_budget.clone(),
            1,
        )
        .ok_or_else(|| {
            anyhow!("GPU graphics budget exhausted while creating background image pipeline")
        })?;
        // v2.24.0: procedural starfield wallpaper, same back-most slot.
        let starfield = starfield::StarfieldPipeline::new(&device, format);
        // v2.25.0: cell-locked pane-text pipeline (the `text-renderer=grid`
        // default). Always constructed; only emitted/drawn in grid mode.
        let glyph_pipeline =
            GlyphPipeline::new_with_budget(&device, format, graphics_budget.clone()).ok_or_else(
                || anyhow!("GPU graphics budget exhausted while creating glyph pipeline"),
            )?;

        Ok(Renderer {
            surface,
            gpu,
            graphics_budget,
            config,
            supported_alpha_modes,
            font_system,
            swash,
            atlas,
            viewport,
            text_renderer,
            glyph_pipeline,
            glyph_instances: Vec::new(),
            glyph_clips: Vec::new(),
            glyph_char_starts: Vec::new(),
            grid_glyphs_dirty: true,
            last_text_layout_key: None,
            bundled_style_faces_loaded: false,
            pane_buffer_ids: Vec::new(),
            pane_buffers: Vec::new(),
            span_scratch: Vec::new(),
            quad_scratch: Vec::new(),
            span_breaks_scratch: Vec::new(),
            pane_line_keys: Vec::new(),
            pane_style_keys: Vec::new(),
            line_text_scratch: String::new(),
            chrome_style_key: 0,
            last_chrome_hash: 0,
            text_prepare_dirty: true,
            last_overlay_open: false,
            cursor_glyph_renderer,
            cursor_glyph_buffer,
            pending_cursor_glyph: None,
            last_cursor_glyph_key: None,
            last_cursor_char: None,
            pane_titlebar_texts: Vec::new(),
            tab_texts: Vec::new(),
            hint_texts: Vec::new(),
            tab_close_text: String::new(),
            tabbar_text: String::new(),
            new_tab_arrow_text: String::new(),
            scroll_left_text: String::new(),
            scroll_right_text: String::new(),
            status_bar_text: String::new(),
            resize_overlay_buffer,
            resize_overlay_text: String::new(),
            ime_buffer,
            ime_text: String::new(),
            pane_titlebar_buffers: Vec::new(),
            tab_buffers: Vec::new(),
            hint_buffers: Vec::new(),
            context_menu_buffers: Vec::new(),
            context_menu_texts: Vec::new(),
            context_menu_hint_buffers: Vec::new(),
            context_menu_hint_texts: Vec::new(),
            settings_buffers: Vec::new(),
            settings_texts: Vec::new(),
            settings_lines_source: None,
            settings_lines_cache: Vec::new(),
            completion_buffers: Vec::new(),
            completion_texts: Vec::new(),
            completion_spans: Vec::new(),
            completion_selected: Vec::new(),
            completion_emphasis_colors: Vec::new(),
            completion_description_buffers: Vec::new(),
            completion_description_texts: Vec::new(),
            completion_header_buffer,
            completion_header_text: String::new(),
            completion_count_buffer,
            completion_count_text: String::new(),
            media_receipt_title_buffer,
            media_receipt_title_text: String::new(),
            media_receipt_detail_buffer,
            media_receipt_detail_text: String::new(),
            media_receipt_dismiss_buffer,
            media_receipt_badge_buffer,
            tabbar_buffer,
            new_tab_arrow_buffer,
            scroll_left_buffer,
            scroll_right_buffer,
            tab_close_buffer,
            search_buffer,
            search_buffer_text: String::new(),
            status_bar_buffer,
            pane_bases,
            live_pane_bases,
            quads,
            pane_outlines,
            overlay_quads,
            menu_quads,
            menu_text_renderer,
            imgs,
            media_receipt_img,
            bg_imgs,
            starfield,
            presentation,
            starfield_started: std::time::Instant::now(),
            bg_image_cache: None,
            bg_image_retry_at: None,
            bg_image_worker: None,
            bg_image_pending: None,
            font_family: cfg.font_family.as_str().into(),
            font_size,
            metrics,
            cell_w,
            cell_h,
            cell_scale_w,
            cell_scale_h,
            scale,
            accent_override: None,
            rounded_window_corners: false,
            live_background_opacity_floor: None,
            pending_screenshot: None,
            screenshot_worker: None,
        })
    }

    /// C3 (multi-window): the shared GPU handles, for spawning another
    /// window's Renderer via [`Renderer::new_with_gpu`]. Cloning the returned
    /// context is a refcount bump.
    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    /// Snapshot the CPU-owned live state needed to rebuild this renderer.
    ///
    /// A screenshot already submitted to `screenshot_worker` owns its request
    /// and completion sender on that worker thread, so it continues to finish
    /// independently when the renderer is dropped. Only a request still queued
    /// for the next frame belongs in the recovery snapshot.
    pub fn recovery_state(&self) -> RendererRecoveryState {
        RendererRecoveryState {
            font_family: self.font_family.clone(),
            font_size: self.font_size,
            cell_scale_w: self.cell_scale_w,
            cell_scale_h: self.cell_scale_h,
            accent_override: self.accent_override,
            pending_screenshot: self.pending_screenshot.clone(),
        }
    }

    /// Reapply a recovery snapshot to a freshly constructed renderer.
    ///
    /// The replacement keeps the scale and surface size selected by its
    /// constructor because those values may have changed while the GPU was
    /// unavailable (for example, after moving the window to another monitor).
    /// All font-derived metrics are recomputed once at that current scale.
    pub fn restore_recovery_state(&mut self, state: &RendererRecoveryState) {
        self.font_family = state.font_family.clone();
        self.font_size = clamp_font_size(state.font_size);
        self.cell_scale_w = state.cell_scale_w.max(0.01);
        self.cell_scale_h = state.cell_scale_h.max(0.01);
        self.accent_override = state.accent_override;
        self.pending_screenshot = state.pending_screenshot.clone();
        self.metrics = metrics_for(self.font_size, self.scale);
        self.remeasure_cell();
    }

    /// Multi-window (Peacock): set/clear this window's accent.
    pub fn set_accent_override(&mut self, accent: Option<Rgb>) {
        self.accent_override = accent;
    }

    /// Tell the renderer whether the native content surface currently ends in
    /// rounded bottom corners. The app owns decoration/fullscreen state; the
    /// renderer owns pane geometry and is therefore the only layer that can
    /// round exactly the focused/inactive pane corners that meet the window.
    pub fn set_rounded_window_corners(&mut self, rounded: bool) {
        self.rounded_window_corners = rounded;
    }

    pub fn set_live_background_opacity_floor(&mut self, floor: Option<f32>) {
        self.live_background_opacity_floor = floor.map(|value| value.clamp(0.0, 1.0));
    }

    /// The accent every LIVE chrome element uses (focused-pane border,
    /// active-tab strip, drag ghost, context-menu + settings highlights,
    /// pane titlebars): the per-window override when the App resolved one,
    /// else the static config cascade.
    fn ui_accent(&self, cfg: &Config, theme: &kettle_config::Theme) -> Rgb {
        self.accent_override
            .unwrap_or_else(|| cfg.resolved_accent(theme))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        // Terminator parity, bg-image: explicit resize handler for
        // the background-image render path. The `bg_image_cache`
        // stores the DECODED image (not a window-sized texture); the
        // background-image-mode dispatch recomputes the image rect
        // from the current surface dims every frame via build_frame.
        // So a resize implicitly takes effect on the next frame — no
        // manual texture re-upload needed.
        //
        // This comment closes phase 8 of
        // docs/TERMINATOR-BG-IMAGE-DESIGN.md with the "implicit
        // per-frame recompute" contract documented so a future
        // contributor sees that the per-frame recompute IS the impl.
        //
        // Floor at 1 (`surface.configure(0, ...)` panics) and ceiling at the
        // device's max-texture-dimension-2d. The device is now created with the
        // ADAPTER's full 2D limit rather than wgpu's default 8192, so a window
        // stretched across multiple 4K monitors keeps its real physical size on
        // hardware that can present it -- but the ceiling stays, because on an
        // adapter whose genuine limit is smaller than the window, configuring
        // past it fails validation and leaves a stale surface that paints
        // nothing at all. Clipping to the visible top-left region is the better
        // failure. Sibling to `cap_axis_cells` (same bug class on the
        // `--screenshot` path).
        let (width, height) = live_surface_dimensions(
            width,
            height,
            self.gpu.device.limits().max_texture_dimension_2d,
        );
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
    }

    pub fn set_background_compositing(&mut self, cfg: &Config) {
        let alpha_mode = desired_alpha_mode(cfg, &self.supported_alpha_modes);
        if self.config.alpha_mode == alpha_mode {
            return;
        }
        self.config.alpha_mode = alpha_mode;
        self.surface.configure(&self.gpu.device, &self.config);
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Line height used by chrome text buffers. Terminal rows may be shorter
    /// or taller via `cell-height`, so overlay geometry must not infer this
    /// value from `cell_h` when budgeting safety text.
    pub fn overlay_text_line_height(&self) -> f32 {
        self.metrics.line_height
    }

    /// Measured advance used by chrome text. Terminal columns may be narrowed
    /// or widened via `cell-width`, but overlay glyphs keep their natural
    /// advance, so safety-copy budgets must not divide by `cell_w` directly.
    pub fn overlay_text_cell_width(&self) -> f32 {
        self.cell_w / self.cell_scale_w.max(0.01)
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = clamp_font_size(size);
        // Re-derive physical metrics at the current DPI scale so a
        // font-size change (zoom, reload) keeps HiDPI scaling applied.
        self.metrics = metrics_for(self.font_size, self.scale);
        self.remeasure_cell();
    }

    /// The current *logical* font size (the user-facing pt value, before the
    /// DPI multiply). Zoom keybinds step this rather than
    /// back-deriving it from the now-physical `cell_h`, which would otherwise
    /// double-apply the scale factor.
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Update the device-pixel scale factor (DPI). Wired to winit's
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
    /// part of a reload was visible (silent partial-apply, the same
    /// "reload doesn't re-flow downstream caches" gap class).
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
        // Apply the configured cell_width/cell_height
        // multipliers AFTER measurement so a font-family or font-
        // size change preserves the user's chosen scale.
        self.cell_w = cw * self.cell_scale_w.max(0.01);
        self.cell_h = ch * self.cell_scale_h.max(0.01);
        // The cell-locked glyph cache keys bake in the physical font size; a
        // size / scale / family change makes every cached slot stale, so drop
        // them (the atlas textures keep their allocation; the packer resets).
        self.glyph_pipeline.clear();
        self.grid_glyphs_dirty = true;
    }

    /// Queue a screenshot request for the next frame. At most one capture may
    /// be pending or in flight; callers receive the original request back when
    /// the bounded worker is busy so they can report an explicit error.
    pub fn set_pending_screenshot(
        &mut self,
        req: ScreenshotRequest,
    ) -> Result<(), ScreenshotRequest> {
        if self
            .pending_screenshot
            .as_ref()
            .is_some_and(ScreenshotRequest::is_cancelled)
        {
            let cancelled = self
                .pending_screenshot
                .take()
                .expect("cancelled pending screenshot checked above");
            Self::complete_screenshot_error(cancelled, "screenshot request was cancelled".into());
        }
        if self.pending_screenshot.is_some()
            || self
                .screenshot_worker
                .as_ref()
                .is_some_and(ScreenshotWorker::is_busy)
        {
            return Err(req);
        }
        self.pending_screenshot = Some(req);
        Ok(())
    }

    /// Whether the next frame owes an explicit live-scene capture.
    ///
    /// The app uses this to distinguish ordinary background-window paints,
    /// which stay quiescent, from a requested screenshot that must render one
    /// frame even when the compositor reports the visible window as occluded.
    pub fn has_pending_screenshot(&self) -> bool {
        self.pending_screenshot
            .as_ref()
            .is_some_and(|request| !request.is_cancelled())
    }

    /// Peek and clear a queued request. Primarily retained for focused tests
    /// and callers that need to cancel before the next frame is encoded.
    pub fn take_pending_screenshot(&mut self) -> Option<ScreenshotRequest> {
        self.pending_screenshot.take()
    }

    /// Terminator parity (`cell_width` / `cell_height`):
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

    fn ensure_bundled_style_faces(&mut self) {
        if self.bundled_style_faces_loaded {
            return;
        }
        for face in [
            kettle_config::font::BOLD,
            kettle_config::font::ITALIC,
            kettle_config::font::BOLD_ITALIC,
        ] {
            load_bundled_font(&mut self.font_system, face);
        }
        self.bundled_style_faces_loaded = true;
        self.pane_style_keys.fill(0);
        self.pane_line_keys.iter_mut().for_each(Vec::clear);
        self.chrome_style_key = 0;
    }

    /// Repaint cap for the procedural starfield (`background-type = starfield`).
    /// The drift is slow, so a low rate keeps idle CPU near today's level while
    /// the steps stay imperceptible; the shader's `time` is continuous either
    /// way. A synthetic frame index (`elapsed / 100 ms`) lets the starfield
    /// reuse the GIF's edge-trigger + wake machinery unchanged.
    const STARFIELD_FPS: u64 = 10;

    /// Whether the background is ANIMATING and the event loop should proactively
    /// keep redrawing it (feeds the app's anim tick). True for a procedural
    /// starfield, OR a decoded MULTI-frame image (animated GIF / APNG / WebP) with
    /// `background-animation != off`. For `background-animation = when-focused` it
    /// is true only while the window is focused; the DEFAULT is `Always` (v2.24.0)
    /// — it animates even unfocused, but the event loop still FREEZES the wake when
    /// the window is minimized or occluded, so a hidden window costs zero idle (the
    /// battery behavior Ghostty's always-on custom shaders lack). The frame shown is
    /// time-correct on any other repaint (see the bg frame-select in
    /// `render_frame_with_status`); this only governs proactive waking.
    pub fn background_is_animating(&self, cfg: &Config, window_focused: bool) -> bool {
        use kettle_config::BackgroundType;
        // Shared focus/Off gate.
        let enabled = match cfg.background_animation {
            kettle_config::BackgroundAnimation::Off => false,
            kettle_config::BackgroundAnimation::Always => true,
            kettle_config::BackgroundAnimation::WhenFocused => window_focused,
        };
        if !enabled {
            return false;
        }
        match cfg.background_type {
            // Procedural — always advancing while enabled (no frame cache).
            BackgroundType::Starfield => true,
            // Decoded — only a genuinely multi-frame image animates.
            BackgroundType::Image => self
                .bg_image_cache
                .as_ref()
                .is_some_and(|c| c.frames.len() > 1),
            BackgroundType::Solid | BackgroundType::Transparent => false,
        }
    }

    /// Synthetic frame period (ms) for the starfield's fps cap.
    fn starfield_frame_ms() -> u128 {
        (1000 / Self::STARFIELD_FPS) as u128
    }

    /// v2.23.1: milliseconds until the animated background's displayed frame
    /// next changes — the wake interval the event loop should use for the
    /// bg-animation tick. Animating at a fixed 30 fps repaints the SAME frame
    /// ~22×/s for a typical 8 fps GIF (wasted full-surface `present()`s — the
    /// cause of the ~55% animated-idle CPU); waking at the actual frame boundary
    /// caps the repaint rate at the GIF's own fps. `None` when the background
    /// isn't animating. Floored at 16 ms so a degenerate fast GIF can't drive
    /// the loop past ~60 fps.
    pub fn bg_anim_interval_ms(&self, cfg: &Config, window_focused: bool) -> Option<u64> {
        if !self.background_is_animating(cfg, window_focused) {
            return None;
        }
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            // Wake at the next fps-cap boundary.
            let frame = Self::starfield_frame_ms();
            let into = self.starfield_started.elapsed().as_millis() % frame;
            return Some(((frame - into) as u64).max(16));
        }
        let c = self.bg_image_cache.as_ref()?;
        bg_image::bg_next_frame_ms(&c.gaps, c.started.elapsed().as_millis())
    }

    /// v2.23.1: the animated background's currently-displayed frame index, or
    /// `None` when it isn't animating. The event loop compares this against the
    /// last-painted index and requests a redraw ONLY when it changes (an
    /// edge-trigger, like the cursor blink) — without that, `request_redraw` is
    /// called every `about_to_wait` while the bg animates, so winit redraws
    /// continuously (vsync-bound) instead of at the GIF's fps. That continuous
    /// repaint was the real cause of the high animated-idle CPU.
    pub fn bg_current_frame_index(&self, cfg: &Config, window_focused: bool) -> Option<usize> {
        if !self.background_is_animating(cfg, window_focused) {
            return None;
        }
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            // Quantize continuous time to the fps cap so the edge-trigger fires
            // once per displayed step.
            let frame = Self::starfield_frame_ms();
            return Some((self.starfield_started.elapsed().as_millis() / frame) as usize);
        }
        let c = self.bg_image_cache.as_ref()?;
        if c.frames.len() <= 1 {
            return None;
        }
        Some(bg_image::bg_current_frame(
            &c.gaps,
            c.started.elapsed().as_millis(),
        ))
    }

    pub fn render_frame(
        &mut self,
        panes: &[PaneView<'_>],
        tabbar: &TabBar,
        cfg: &Config,
        overlay: &Overlay,
    ) -> Result<FrameOutcome> {
        self.render_frame_with_status(panes, tabbar, cfg, overlay, &StatusBar::hidden())
    }

    /// Extended `render_frame` variant that also draws the
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
    ) -> Result<FrameOutcome> {
        self.render_frame_with_status_and_pre_present(panes, tabbar, cfg, overlay, status, || {})
    }

    /// (Re)submit a background-image decode to `bg_image_worker`, starting
    /// the worker thread on first use. A no-op when a job for this exact
    /// `(path, blur_radius)` key is already in flight, so a `need_reload ==
    /// true` frame that fires every frame while the decode is still running
    /// (a large/animated wallpaper can take well over one frame at 60 fps)
    /// doesn't flood the worker's capacity-1 job channel with redundant work.
    fn request_bg_image_reload(&mut self, path: &str, blur_radius: u32) {
        if self
            .bg_image_pending
            .as_ref()
            .is_some_and(|(p, b)| p == path && *b == blur_radius)
        {
            return;
        }
        if self.bg_image_worker.is_none() {
            match BgImageWorker::start() {
                Ok(worker) => self.bg_image_worker = Some(worker),
                Err(error) => {
                    log::warn!("could not start background-image decode worker: {error}");
                    return;
                }
            }
        }
        let Some(worker) = self.bg_image_worker.as_ref() else {
            return;
        };
        match worker.try_submit(BgImageJob {
            path: path.to_string(),
            blur_radius,
        }) {
            Ok(()) => self.bg_image_pending = Some((path.to_string(), blur_radius)),
            Err(BgImageSubmitError::Busy) => {
                // A different job is still decoding (shouldn't normally
                // happen given the key check above, but config could churn
                // faster than the worker drains) — this frame keeps showing
                // the previous wallpaper (or none); retried next frame.
            }
            Err(BgImageSubmitError::Disconnected) => {
                // Worker thread exited — drop it so the NEXT reload attempt
                // spawns a fresh one instead of submitting into a dead
                // channel every frame.
                self.bg_image_worker = None;
            }
        }
    }

    /// Drain any finished background-image decode(s) and, for one that
    /// matches the currently-pending `(path, blur_radius)` key, install it
    /// into `bg_image_cache` — the same failure-throttle / success-clears-
    /// throttle bookkeeping the old synchronous path did inline. A result
    /// whose key no longer matches `bg_image_pending` (the config moved on
    /// to a different image/blur — or off background images entirely —
    /// before this decode finished) is silently discarded: a fresh request
    /// for the current key has already been queued.
    fn apply_bg_image_worker_result(&mut self) {
        let Some(worker) = self.bg_image_worker.as_ref() else {
            return;
        };
        while let Some(result) = worker.try_recv() {
            let is_current = self
                .bg_image_pending
                .as_ref()
                .is_some_and(|(p, b)| *p == result.path && *b == result.blur_radius);
            if !is_current {
                continue;
            }
            self.bg_image_pending = None;
            let mut frames = Vec::with_capacity(result.frames.len());
            let mut opaque_frames = Vec::with_capacity(result.frames.len());
            let mut gaps = Vec::with_capacity(result.frames.len());
            for frame in result.frames {
                let opaque = frame
                    .image
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255);
                if let Some(image) = kettle_core::ImageData::new_with_budget(
                    frame.image.width,
                    frame.image.height,
                    frame.image.rgba,
                    &self.graphics_budget,
                ) {
                    frames.push(image);
                    opaque_frames.push(opaque);
                    gaps.push(frame.gap_ms);
                }
            }
            // On failure, throttle the next retry; on success, clear it so
            // the loaded wallpaper never re-decodes.
            self.bg_image_retry_at = if frames.is_empty() {
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3))
            } else {
                None
            };
            self.bg_image_cache = Some(BgImageAnim {
                path: result.path,
                blur: result.blur_radius,
                frames,
                opaque_frames,
                gaps,
                started: std::time::Instant::now(),
            });
        }
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
        // v2.20.0 P1b: the chrome-label caches below compare TEXT only, which
        // is sound while the font family is stable — invalidate them all once
        // when it changes (config reload with a new `font-family`).
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            family.hash(&mut h);
            let k = h.finish();
            if self.chrome_style_key != k {
                self.chrome_style_key = k;
                self.pane_titlebar_texts.clear();
                self.tab_texts.clear();
                self.tab_close_text.clear();
                self.tabbar_text.clear();
                self.new_tab_arrow_text.clear();
                self.status_bar_text.clear();
                self.resize_overlay_text.clear();
                // v2.38.2 P1b: the context-menu/settings/search-family caches
                // added alongside the equality gates below have the exact
                // same font-staleness hazard — unlike `hint_texts` (whose
                // pool truncates to 0 whenever `hint_labels` empties, so it
                // self-invalidates on next open), these overlays' buffer
                // pools are only touched while the overlay is OPEN, so a
                // font-family reload that lands while one is closed (or that
                // doesn't change the label text) would otherwise leave a
                // stale cache pointing at glyphs shaped in the old family.
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
                self.search_buffer_text.clear();
            }
        }
        // Ensure one text buffer per pane.
        while self.pane_buffers.len() < panes.len() {
            let b = TextBuffer::new(&mut self.font_system, metrics);
            self.pane_buffers.push(b);
        }
        // v2.20.0 P1: the line-key / style-key pools live and die with
        // `pane_buffers` — a key must always describe the content actually
        // shaped into the buffer at the same index.
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
        // Release buffers for panes that have closed. The grow
        // loops above only ever extend, so without this the two vecs sat at
        // the session's high-water pane count — a 6-way split that you close
        // back to one pane left 5 idle TextBuffers (with their shaped glyph
        // runs) allocated for the rest of the session. Truncation is safe:
        // every later loop indexes by enumerate position `< panes.len()`.
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
                // Titlebar text = "  TITLE [WxH] [●]"
                // where:
                //   - [WxH] is shown unless cfg.title_hide_sizetext
                //   - [●] is shown when cfg.icon_bell && pv.bell
                // Named groups: when
                //   `pane.group_name = Some("fleet")`, prepend
                //   the group pill: "  [fleet] TITLE …".
                //   The render-side bracket gives it a visual
                //   weight without needing a separate quad
                //   shape (a future pass could promote it to a real
                //   colored chip).
                // v2.24.0: fit the label to the pane width, shedding the size
                // text then the group tag then middle-ellipsizing the title
                // (keeping the program/leaf name) — was a hard glyphon clip with
                // no ellipsis, so a narrow split cut "C:\Program…" to "C:\Program".
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
                // v2.20.0 P1b: `Buffer::set_text` re-shapes unconditionally —
                // gate it on text change so a steady title costs nothing.
                if self.pane_titlebar_texts[i] != label {
                    // Advanced shaping, like every other chrome buffer: the
                    // title is shell-controlled OSC 0/2 text (agents such as
                    // Claude Code lead with a status glyph) and the label can
                    // carry the 🔔 bell — Basic skips cosmic-text's platform
                    // font fallback (Segoe UI Emoji/Symbol, Noto), so any
                    // glyph outside the bundled Nerd Font tofu-boxed here
                    // while the tab bar rendered the same string fine.
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

        // Pre-size the per-frame quad/image vectors so the render
        // hot path doesn't repeatedly reallocate as they grow (borders +
        // per-pane chrome + cell-background quads dominate `quads`). Capacities
        // are rough upper-of-typical estimates; growth still happens for
        // outliers but the common 60fps path avoids the realloc churn.
        // Reuse the pooled quad scratch (cleared, capacity
        // retained from the prior frame) instead of allocating a fresh Vec every
        // frame. Returned to `self.quad_scratch` after the GPU upload below.
        let mut quads: Vec<QuadInstance> = std::mem::take(&mut self.quad_scratch);
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
        // Third quad pass — drawn after `over` so the right-click
        // context menu's bg/shadow/border/highlight sit on top of
        // every other UI element. The menu's text is rendered by
        // `menu_text_renderer` after this pass so the labels land on
        // top of the panel bg.
        //
        // The four per-frame buffers below (menu_q / over /
        // img_items / image-live sets) are intentionally allocated fresh each frame, unlike
        // the pooled `quad_scratch` / `span_scratch`. They are small and usually
        // near-empty (no open context menu, a handful of panes, no cell images),
        // so the allocation is trivial; high-water pooling is reserved for the
        // large per-cell `quads` / `spans` buffers where it actually pays off.
        // The asymmetry is deliberate, not an oversight.
        use kettle_config::BackgroundType;
        let mut menu_q: Vec<QuadInstance> = Vec::with_capacity(64);
        // Drawn *after* text: unfocused-pane dimming + scrollbar thumbs.
        let mut over: Vec<QuadInstance> = Vec::with_capacity(panes.len() * 4 + 8);
        let mut img_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(16);
        let mut media_receipt_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(1);
        // The wallpaper is always one retained item in its own back-most pass;
        // tile mode repeats UVs in the sampler instead of rebuilding a quad per
        // tile on every frame.
        let mut bg_img_items: Vec<imgpipe::ImageItem> = Vec::with_capacity(1);
        // v2.23.0: when `chrome-background = auto`, the average color of the
        // currently-displayed wallpaper frame, used to tint the chrome strips.
        // Computed once from the displayed frame below (only when auto is set).
        let mut bg_frame_avg: Option<Rgb> = None;
        // Starfield's fragment shader writes alpha 1 over the whole surface.
        // Image backgrounds prove this separately from the selected frame's
        // cached alpha scan and its destination geometry below.
        let mut opaque_wallpaper_covers_surface =
            matches!(cfg.background_type, BackgroundType::Starfield);
        let mut bg_live: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut inline_live: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut media_receipt_live: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        // Terminator parity, bg-image: when cfg.background_type = Image +
        // cfg.background_image is set, decode-once + cache + prepend a
        // fullscreen image item BEFORE any cell-images so the wallpaper
        // renders at the back. The `decode_bg_image_frames_with_blur`
        // helper handles the file-not-found / decode-error paths
        // gracefully.
        if matches!(cfg.background_type, BackgroundType::Image) && !cfg.background_image.is_empty()
        {
            let want = cfg.background_image.clone();
            // Route through decode_bg_image_with_blur
            // so cfg.background_blur takes effect at load time.
            // Radius 8 is a reasonable default for the on/off
            // toggle Terminator's bool config exposes; a follow-up
            // could expose a `background_blur_radius`
            // numeric for finer control.
            let blur_radius: u32 = if cfg.background_blur { 8 } else { 0 };
            // Reload when the path OR the blur radius
            // changes. Before, blur lived outside the cache key, so toggling
            // `background-blur` on a still-loaded image was silently ignored.
            let need_reload = match self.bg_image_cache.as_ref() {
                None => true,
                // Reload when the (path, blur) key changed, OR when
                // the cached entry is a FAILED decode (`frames` empty)
                // and the throttle has elapsed: a transient read error / an
                // in-place file fix self-heals, but THROTTLED (≥3s between
                // attempts) so a broken or corrupt path is NOT re-decoded every
                // frame (avoiding the per-frame thrash this replaced). A successful
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
                // animated GIF/APNG/WebP) — but off the render thread: an
                // animated + blurred wallpaper's decode+blur can run tens of
                // ms PER FRAME (up to 128 frames), which used to run inline
                // here and stall the winit event loop + every window's render
                // pass for the whole duration. `request_bg_image_reload`
                // just (re)submits the job to `bg_image_worker`; the result
                // lands on a LATER frame via `apply_bg_image_worker_result`
                // below, same key-caching / failure-throttle semantics as
                // the old synchronous path.
                self.request_bg_image_reload(&want, blur_radius);
            }
            // Pick up a finished decode (if any) before selecting the frame
            // to display. A no-op (cheap `try_recv`) on every frame where no
            // background image is configured or nothing has finished yet.
            self.apply_bg_image_worker_result();
            // v2.21.x: select the frame to display now. A still image (1 frame)
            // or `background-animation = off` shows frame 0; otherwise the
            // playback clock loops through frames at their own gaps. Focus does
            // NOT gate the index (so an output-driven repaint while unfocused
            // still shows the time-correct frame, no jump) — focus only gates
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
                // v2.23.0: sample the displayed frame's average color for
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
                bg_live.insert(data.allocation_key());
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
        } else if self.bg_image_cache.is_some() {
            // Config no longer requests an image background
            // (type switched away, or path cleared) — drop the decoded RGBA so
            // a large wallpaper isn't pinned for the rest of the
            // session. Re-enabling re-decodes via the need_reload path above.
            self.bg_image_cache = None;
            self.bg_image_retry_at = None; // reset the self-heal throttle
            // A still-in-flight decode for the old path/blur is now
            // irrelevant — its result (once it lands) is discarded by
            // `apply_bg_image_worker_result`'s key check rather than
            // resurrecting a wallpaper the config no longer wants.
            self.bg_image_pending = None;
        }

        // v2.23.0: the opaque fill color for the window chrome strips (tab bar,
        // status bar, new-tab button). Only differs from the theme when a
        // wallpaper is in use AND `chrome-background` asks for it; otherwise
        // it's `palette[8]` exactly as before. See `resolve_chrome_bg`.
        let chrome_strip_bg = resolve_chrome_bg(cfg, theme, bg_frame_avg);

        // Status-bar background. The text is uploaded
        // alongside `tabbar_buffer.set_text` further down so the same
        // text-renderer pass handles both. Just a chrome-dim panel
        // here (1 quad).
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
                // v2.40.0 (tear-off UX): pre-tear escalation — the ghost
                // "lifts off" as the cursor approaches the tear threshold
                // (bigger/darker shadow, fading body), so a release reads
                // as "this will tear" instead of surprising the user with
                // a new window. `tear_lift` is 0 for a plain reorder.
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
                // Accent strip on the left edge, same color the live
                // active segment uses (palette[3] yellow under
                // broadcast, accent-color → palette[4]
                // otherwise — keeps the ghost visually identical to
                // the source segment).
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
            // v2.19.0 (tear-off UX, re-dock): the insertion marker — an
            // accent line between segments showing where a torn-off
            // window's tab will dock. Pushed to `over` so it sits above
            // segment backgrounds AND text (a thin line under text would
            // vanish behind a long title). Rect comes oriented from the
            // UI (vertical line for horizontal bars, horizontal line for
            // vertical bars).
            if let Some((ix, iy, iw, ih)) = tabbar.insert_marker {
                let accent = self.ui_accent(cfg, theme);
                // v2.40.0 (tear-off UX): dock-target highlight — wash the
                // whole latched band in translucent accent plus a border on
                // the pane-facing edge. Before this, the marker line was the
                // ONLY latch signal off-Windows (the torn-window alpha trick
                // is Windows-only) and a session recording showed it reads
                // as "nothing happened" on Linux. Presence is derived from
                // `insert_marker` so the two signals cannot drift apart.
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
        // v2.21.0 (idle perf): true if ANY pane reshaped a row this frame.
        let mut any_pane_text_changed = false;
        // Reset the focused-cursor glyph; the focused pane's `build_pane` re-sets
        // it this frame if a solid block cursor is visible.
        self.pending_cursor_glyph = None;
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
        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.rect;
            // Pane separators / focus border. Both colors are config-
            // overridable: `split-divider-color` for inactive panes
            // (defaults to theme `palette[8]`, the dim color) and
            // `focused-split-color` for the focused pane (defaults to
            // theme `palette[4]`, the accent blue).
            //
            // When broadcast / group-input mode is on, the
            // focused-pane border flips to theme palette[3] (yellow,
            // the same warning slot the tab-bar accent uses). The tab-bar indicator alone wasn't enough: with
            // `tab-bar = auto` and only one tab open (the default
            // single-window case), the tab bar is hidden and the
            // user has no visual cue that broadcast is active.
            // Per-pane border-color shift works regardless of tab-bar
            // state. Inactive panes keep their normal divider color
            // — broadcast is scoped to the active tab (a
            // broadcast-scope invariant) and the focused-pane border is the single
            // most-visible chrome element on every layout.
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
            // "use theme default" (1.0 here); positive values 0-20 are
            // honored directly. Clamping was already done at parse
            // time.
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
                // See `pick_titlebar_bg`.
                let bar_bg = pick_titlebar_bg(
                    cfg,
                    theme,
                    self.ui_accent(cfg, theme),
                    pv.focused,
                    tabbar.broadcast,
                );
                // Terminator parity, titlebar Bucket-D phase 9 of
                // TERMINATOR-PANE-TITLEBAR-DESIGN.md: title_at_bottom flips
                // the bar from the top of the pane to the bottom. Terminal
                // content uses `pane_grid_origin`, so the reserved strip moves
                // with the title instead of leaving a phantom top inset.
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
            let grid_origin = pane_grid_origin(
                pv.rect,
                (pad_x, pad_y),
                pane_titlebar_h,
                cfg.title_at_bottom,
            );
            any_pane_text_changed |= self.build_pane(
                i,
                pv,
                cfg,
                &family,
                overlay.window_focused,
                overlay.cursor_visible,
                if pane_has_search {
                    &overlay.highlights
                } else {
                    &[]
                },
                &mut quads,
                &mut pane_bases,
                pane_titlebar_h,
            );

            // Image placements, anchored history-aware so they scroll.
            {
                let quota = placement_quotas[i];
                let image_clip =
                    pane_backdrop_rect(pv.rect, bw, pane_titlebar_h, cfg.title_at_bottom).and_then(
                        |pane_body| {
                            inline_image_clip(
                                pane_body,
                                grid_origin,
                                (pv.snap.columns, pv.snap.screen_lines),
                                (cw, ch),
                            )
                        },
                    );
                let mut draw = |p: &kettle_core::Placement| {
                    let Some(image_clip) = image_clip else {
                        return;
                    };
                    let Some(row) = placement_viewport_row(pv.snap, p) else {
                        return;
                    };
                    let (image_x, image_y, image_width, image_height) =
                        inline_placement_rect(grid_origin.0, grid_origin.1, row, cw, ch, p);
                    inline_live.insert(p.img.allocation_key());
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
                            // The active match follows the theme's
                            // yellow (Mocha #f9e2af) unless overridden, so it
                            // matches the inactive highlight's theme.selection_bg
                            // instead of a hardcoded TokyoNight amber.
                            cfg.search_background.unwrap_or(theme.palette[3])
                        } else {
                            theme.selection_background
                        },
                        1.0,
                    ));
                }
                // Quick-select hint label chips.
                for hint in &overlay.hint_labels {
                    let n = hint.label.chars().count().max(1) as f32;
                    quads.push(rect(
                        grid_origin.0 + hint.col as f32 * cw,
                        grid_origin.1 + hint.row as f32 * ch,
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
                if let Some(preedit) = &overlay.ime_preedit {
                    let cells = unicode_width::UnicodeWidthStr::width(preedit.text.as_str()).max(1);
                    let x = grid_origin.0 + preedit.col as f32 * cw;
                    let y = grid_origin.1 + preedit.row as f32 * ch;
                    quads.push(rect(
                        x,
                        y,
                        cells as f32 * cw,
                        ch,
                        theme.selection_background,
                        0.96,
                    ));
                    // A persistent underline distinguishes composition from
                    // selected terminal text and survives high-contrast themes.
                    quads.push(rect(
                        x,
                        y + ch - 2.0,
                        cells as f32 * cw,
                        2.0,
                        cfg.search_background.unwrap_or(theme.palette[3]),
                        1.0,
                    ));
                }
            }

            // Post-text overlay: dim unfocused panes; per-pane scrollbar.
            //
            // Terminator parity, terminatorlib/config.py:84-85
            // `inactive_color_offset` + `inactive_bg_color_offset`: when EITHER
            // offset is < 1.0, layer a dim over the unfocused pane.
            //
            // The FG offset used to be READ NOWHERE — only the BG offset and
            // the split opacity composed the alpha. That made Terminator's own
            // default pair (`inactive_color_offset = 0.8`,
            // `inactive_bg_color_offset = 1.0`) produce no visible change at
            // all: the BG term was 1.0, so the dim was zero and the setting
            // the user actually reached for did nothing.
            //
            // Both offsets now contribute. This is not Terminator's exact
            // model — it scales the foreground palette per glyph
            // (terminal.py:809-823) rather than compositing — but an overlay
            // reproduces the visible intent ("unfocused panes recede") without
            // re-running the shaper for every unfocused pane. Taking the max
            // rather than summing keeps a config that sets both from dimming
            // twice as hard as either alone.
            let inactive_fg_dim = (1.0 - cfg.inactive_color_offset).clamp(0.0, 0.95);
            let inactive_bg_dim = (1.0 - cfg.inactive_bg_color_offset).clamp(0.0, 0.95);
            let split_opacity_dim = (1.0 - cfg.unfocused_split_opacity).clamp(0.0, 0.95);
            let composed_dim = inactive_fg_dim.max(inactive_bg_dim).max(split_opacity_dim);
            if !pv.focused && panes.len() > 1 && composed_dim > 0.0 {
                over.push(rect(rx, ry, rw, rh, theme.background, composed_dim));
            }
            if cfg.scrollbar != ScrollbarMode::Never {
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

        // Visual bell: a brief full-surface flash (replaces an audible beep).
        // `overlay.bell` is the 300 ms decay ramp; `bell-flash-intensity` is
        // its peak alpha. The peak used to be a hard-coded 0.18, which is a
        // lot of theme foreground across the whole surface for what is usually
        // an empty Tab completion — the most frequent bell there is, and a
        // non-event. `0.0` opts out of the flash while leaving the rest of the
        // bell (window attention) alone.
        let bell_alpha = overlay.bell * cfg.bell_flash_intensity;
        if bell_alpha > 0.0 {
            quads.push(rect(0.0, 0.0, sw, sh, theme.foreground, bell_alpha));
        }

        // Search uses a responsive RESERVED lane. The app subtracts the public
        // `search_bar_geometry(...).reserved_height` from pane layout; paint and
        // hit-testing then consume these exact same rectangles. Other legacy
        // modals remain single-line overlays.
        let mut have_search = false;
        let mut search_rect = (0.0, sh - (ch + 10.0), sw, ch + 10.0);
        let mut search_text_top = None;
        // Text color for the shared bottom-bar buffer. `None` means "theme
        // foreground", which is right for every arm that paints itself on the
        // ordinary chrome background. The confirm bar is the exception: it
        // paints a saturated `palette[1]` and has to raise its own contrast.
        let mut search_text_color = None;
        if let Some(search) = &overlay.search {
            have_search = true;
            let geometry = search_bar_geometry(sw, sh, cw, ch);
            search_rect = geometry.rect;
            let row_h = geometry.reserved_height / geometry.rows.max(1) as f32;
            search_text_top = Some(geometry.rect.1 + ((row_h - ch) * 0.5).max(0.0));
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
            // accent as the active terminal match.
            quads.push(rect(
                geometry.editor.0,
                geometry.editor.1 + 2.0,
                geometry.editor.2,
                (geometry.editor.3 - 4.0).max(1.0),
                theme.background,
                0.72,
            ));
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
                            0.92
                        } else {
                            0.28
                        },
                    ));
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
            }
            if let Some((a, b)) = search.selection {
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                if start != end {
                    let start_col = search_query_column(&search.query, start)
                        .saturating_sub(search.horizontal_scroll);
                    let end_col = search_query_column(&search.query, end)
                        .saturating_sub(search.horizontal_scroll);
                    let inner_cols = (geometry.editor.2 / cw).floor().max(2.0) as usize - 2;
                    let start_col = start_col.min(inner_cols);
                    let end_col = end_col.min(inner_cols);
                    if end_col > start_col {
                        quads.push(rect(
                            geometry.editor.0 + (start_col + 1) as f32 * cw,
                            geometry.editor.1 + 2.0,
                            (end_col - start_col) as f32 * cw,
                            (geometry.editor.3 - 4.0).max(1.0),
                            theme.selection_background,
                            1.0,
                        ));
                    }
                }
            }

            let label = search_bar_text(search, geometry, cw);
            self.search_buffer.set_metrics(Metrics::new(
                metrics.font_size,
                row_h.max(metrics.font_size),
            ));
            self.search_buffer
                .set_size(Some(sw), Some(geometry.reserved_height));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some(q) = &overlay.search_query {
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[8], 0.96));
            // v2.20.0: advertise the Ctrl+j/k match stepping when
            // `vim-menu-nav` is on (the keys themselves live app-side).
            // Review fix: ^j/^k are LITERAL directions while `invert-search`
            // flips Enter's default — the hint pairs them accordingly so it
            // never claims an equivalence the keys don't have.
            let nav_hint = match (cfg.vim_menu_nav, cfg.invert_search) {
                (true, false) => "(Enter/^j next · Shift+Enter/^k prev · Esc close)",
                (true, true) => "(Shift+Enter/^j next · Enter/^k prev · Esc close)",
                (false, false) => "(Enter next · Shift+Enter prev · Esc close)",
                (false, true) => "(Enter prev · Shift+Enter next · Esc close)",
            };
            let status = if overlay.search_count == 0 {
                SearchStatus::NoMatch.label()
            } else {
                SearchStatus::Match.label()
            };
            let label = format!("  search: {q}_    {status}   {nav_hint}");
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some(q) = &overlay.palette_query {
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[5], 0.96));
            let label = format!(
                "  ⌘ {q}_   ▸ {}   (Enter run · Tab/↑↓ select · Esc cancel)",
                overlay.palette_hint
            );
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some(q) = &overlay.layout_picker_query {
            // Terminator parity, layoutlauncher.py:
            // layout picker overlay. Same bar shape as the
            // palette but the hint string lists layouts.
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[6], 0.96));
            let label = format!(
                "  ▤ layout: {q}_   ▸ {}   (Enter spawn · Tab/↑↓ select · Esc cancel)",
                overlay.layout_picker_hint
            );
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some(q) = &overlay.ssh_query {
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            quads.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[4], 0.96));
            let label = format!(
                "  ssh ❯ {q}_    {}   (Enter connect · Tab complete · Esc cancel)",
                overlay.ssh_hint
            );
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some(edit) = &overlay.edit_title {
            // Terminator parity, edit-title overlay UX:
            // a thin bar in app chrome mirroring the shape of the
            // palette + ssh-input overlays without covering terminal rows.
            // Uses palette[3] (yellow) so it's visually distinct
            // from the palette (5) and ssh (4) bars.
            have_search = true;
            search_rect = edit.rect;
            quads.push(rect(
                edit.rect.0,
                edit.rect.1,
                edit.rect.2,
                edit.rect.3,
                theme.palette[3],
                0.96,
            ));
            let label = format!(
                "  ✎ {} {}_   (Enter apply · Esc cancel)",
                edit.label, edit.input
            );
            let label = fit_single_line_label(&label, overlay_label_cols(edit.rect.2, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer
                .set_size(Some(edit.rect.2), Some(edit.rect.3));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some(dlg) = &overlay.confirm_dialog {
            // Phase 3 of TERMINATOR-CONFIRM-DIALOG-DESIGN.md:
            // a bottom-bar projection of the modal. v1 of the
            // renderer skips the fancy centered-panel + backdrop
            // The bottom bar gives immediate modal feedback with prompt,
            // visible button labels, and a focus indicator. The UI hit-test
            // path mirrors this text layout so keyboard and mouse activation
            // dispatch through the same confirmation state machine.
            have_search = true;
            let bar_h = ch + 10.0;
            search_rect = (0.0, sh - bar_h, sw, bar_h);
            // Red-ish accent (palette[1]) to signal "destructive
            // confirmation pending" vs the palette/ssh/
            // edit-title yellows/blues/cyans.
            //
            // Queued with the menu chrome rather than the base overlay quads:
            // the settings panel washes the whole surface in a dim backdrop
            // from `menu_q`, which is drawn last, so a bar pushed to `quads`
            // came out greyed under it. A modal question has to be the most
            // legible thing on screen, and the one raised by rebinding onto an
            // already-bound chord is raised from inside that very panel.
            // Opaque, and that is load-bearing rather than cosmetic:
            // `confirm_bar_text_color` guarantees AA against `palette[1]`
            // itself. At the previous 0.96 the painted background was
            // `palette[1]` composited over whatever terminal content happened
            // to sit underneath, so the real ratio drifted with the scrollback
            // and a valid custom theme could land under the floor the helper
            // advertises. A destructive question is the one overlay that has
            // no business being translucent.
            menu_q.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[1], 1.0));
            let mut buttons_label = String::new();
            for (i, btn) in dlg.buttons.iter().enumerate() {
                if !buttons_label.is_empty() {
                    buttons_label.push_str("  ");
                }
                let marker = if i == dlg.focus_idx { "▶" } else { " " };
                buttons_label.push('[');
                buttons_label.push_str(marker);
                buttons_label.push(' ');
                buttons_label.push_str(&btn.label);
                buttons_label.push(']');
            }
            // The bar is `palette[1]`, not the chrome background, so the theme
            // foreground is not guaranteed to be readable on it — on the
            // shipped TokyoNight Night default it is light lavender (#c0caf5)
            // on light red (#f7768e), about 1.6:1, which is how a close
            // confirmation ended up unanswerable. Lift it to WCAG AA the same
            // way the completion panel does (`confirm_bar_text_color`). The
            // 0.96 alpha leaves the effective background within a few percent
            // of `palette[1]`, so measuring against the flat color is right.
            let bar_fg = confirm_bar_text_color(theme);
            search_text_color = Some(GColor::rgb(bar_fg.r, bar_fg.g, bar_fg.b));
            let prompt = format!("  ⚠ {}", dlg.prompt);
            // `y`/`n` answer the question directly regardless of which button
            // has focus, while Enter fires the FOCUSED button (which is the
            // safe `Cancel` on every close prompt). A user who cannot tell
            // which button is focused needs the unambiguous pair advertised —
            // but only when it actually works: the App gates `y`/`n` on
            // `vim-menu-nav`, so advertising them unconditionally would name a
            // key that does nothing for anyone who turned that off.
            let help = if cfg.vim_menu_nav {
                "  Tab/←→ · Enter · Esc · y/n"
            } else {
                "  Tab/←→ · Enter · Esc"
            };
            let label = compose_confirm_bar_label(
                &prompt,
                help,
                &buttons_label,
                confirm_bar_columns(sw, cw),
            );
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
        } else if let Some((tag, url)) = &overlay.update_available {
            // Passive "newer release available" banner — lowest
            // priority, so any real modal above takes the bar and this returns
            // when they close. The full strip uses readable chrome; a small
            // green accent carries the update cue without making foreground text
            // fight a bright background.
            have_search = true;
            let bar_h = ch + 10.0;
            // Stack above a bottom-anchored tab / status bar
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
            let bottom_search_h = overlay
                .search
                .as_ref()
                .map(|_| search_bar_geometry(sw, sh, cw, ch).reserved_height)
                .unwrap_or(0.0);
            let bar_y = update_banner_top_with_reserved(
                sh,
                bar_h,
                bottom_tabbar_h,
                bottom_status_h,
                bottom_search_h,
            );
            search_rect = (0.0, bar_y, sw, bar_h);
            let (banner_bg, banner_accent) = update_banner_chrome_colors(theme);
            quads.push(rect(0.0, bar_y, sw, bar_h, banner_bg, 0.96));
            quads.push(rect(0.0, bar_y, sw, 2.0, banner_accent, 1.0));
            quads.push(rect(0.0, bar_y, 4.0, bar_h, banner_accent, 1.0));
            let label = format!(
                "  ⬆ kettle {tag} available — {url}    (click: open · right-click: dismiss)"
            );
            let label = fit_single_line_label(&label, overlay_label_cols(sw, cw));
            self.search_buffer.set_metrics(metrics);
            self.search_buffer.set_size(Some(sw), Some(bar_h));
            // v2.38.2 P1b: same equality gate as the other chrome buffers —
            // only one of this `if`/`else if` chain's arms runs per frame, so
            // a single cache is enough (see `search_buffer_text`'s doc comment).
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
            // v2.20.0 P1b: label cache lives and dies with `tab_buffers`.
            while self.tab_texts.len() < tabbar.segments.len() {
                self.tab_texts.push(String::new());
            }
            self.tab_texts.truncate(tabbar.segments.len());
            // Shrink the pool when tabs close, matching
            // `pane_buffers`/`settings_buffers` — otherwise it stuck at the
            // peak tab count for the whole session (open 50, close to 5 → 50
            // shaped-text buffers retained).
            self.tab_buffers.truncate(tabbar.segments.len());
            for (bi, s) in tabbar.segments.iter().enumerate() {
                let (_, _, title_w, title_h) = s.title_rect;
                // chars that fit: explicit title lane, ~cell_w each. The lane
                // excludes fixed tab chrome such as the close button, so
                // fitting and visual centering share the same coordinate space.
                //
                // The budget tracks the *actual* segment width
                // instead of a hard 24-char cap, so a wide tab shows its full
                // title (and only ellipsizes when the title genuinely doesn't
                // fit). We reserve `fixed_w` for the non-title part of the
                // format (the leading space + e.g. "{n}: ") so the title
                // ellipsizes to keep the WHOLE label inside the segment rather
                // than letting the prefix push it past the right edge.
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
                // v2.20.0 P1b: re-shape only when the label actually changed.
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
            // v2.20.0 P1b: constant glyph — shaped once per font family.
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
            // v2.20.0 P1b: constant glyph — shaped once per font family.
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
                // v2.20.0 P1b: constant glyph — shaped once per font family.
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
            // v2.26.0: overflow scroll-arrow glyphs `‹` / `›`, each shaped in its
            // own buffer and sized to its button rect. Present only when the
            // horizontal tab bar overflows.
            if tabbar.scroll_left.2 > 0.0 {
                self.scroll_left_buffer.set_metrics(metrics);
                self.scroll_left_buffer
                    .set_size(Some(tabbar.scroll_left.2), Some(tabbar.height));
                if self.scroll_left_text != " ‹" {
                    self.scroll_left_buffer.set_text(
                        " ‹",
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.scroll_left_text = " ‹".into();
                }
                self.scroll_left_buffer
                    .shape_until_scroll(&mut self.font_system, false);
            }
            if tabbar.scroll_right.2 > 0.0 {
                self.scroll_right_buffer.set_metrics(metrics);
                self.scroll_right_buffer
                    .set_size(Some(tabbar.scroll_right.2), Some(tabbar.height));
                if self.scroll_right_text != " ›" {
                    self.scroll_right_buffer.set_text(
                        " ›",
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.scroll_right_text = " ›".into();
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
            // v2.20.0 P1b: the status line changes at most once a second (the
            // HH:MM:SS clock) — don't re-shape it on every painted frame.
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

        // v2.20.0 (Ghostty `resize-overlay` parity): shape the transient
        // size chip's text ("120×40"). Drawn later in the menu pass so it
        // sits above pane content; the P1b equality gate means a live
        // resize only re-shapes when the GRID size actually changed.
        if let Some((rcols, rrows)) = overlay.resize_overlay {
            let label = format!("{rcols}×{rrows}");
            // Metrics/size stay OUTSIDE the text gate (review fix): a DPI
            // change can re-show the chip with an UNCHANGED label, and the
            // gated form left the glyphs shaped at the old monitor's scale.
            // Both calls early-out when unchanged, like the other chrome
            // buffers.
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

        // Context-menu row labels (one buffer per row, separators skipped)
        // + right-aligned shortcut hints (dropdown-parity).
        if let Some(menu) = &overlay.context_menu {
            while self.context_menu_buffers.len() < menu.rows.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.context_menu_buffers.push(b);
            }
            // v2.38.2 P1b: label cache lives and dies with `context_menu_buffers`
            // but grows off ITS OWN length (like `tab_texts`/`tab_buffers`), not
            // the buffer pool's — so the font-family invalidation above (which
            // clears only the `_texts` caches, not the heavier buffer pools) can't
            // desync the two into different lengths and panic on indexing below.
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
            // Shrink to the current row count so a small
            // menu after a large one (common with dynamic Lua menus) doesn't
            // keep the peak's worth of shaped-glyph buffers. The field doc
            // promised this trim; the code never did it until now.
            self.context_menu_buffers.truncate(menu.rows.len());
            self.context_menu_texts.truncate(menu.rows.len());
            self.context_menu_hint_buffers.truncate(menu.rows.len());
            self.context_menu_hint_texts.truncate(menu.rows.len());
            // The App's clamped width is authoritative when present; all
            // render passes share this helper so text, chrome, and hit testing
            // cannot drift by a fractional cell after ellipsis.
            let panel_w = context_menu_panel_width(menu, cw);
            // Row height matches a comfortable click target (~28-32 px
            // on default cell metrics) — was 6 px of pad which gave a
            // cramped 18-19 px row.
            let row_h = ch + 12.0;
            for (i, row) in menu.rows.iter().enumerate() {
                if row.separator {
                    continue;
                }
                let buf = &mut self.context_menu_buffers[i];
                buf.set_metrics(metrics);
                buf.set_size(Some(panel_w), Some(row_h));
                // v2.38.2 P1b: re-shape only when the row's label actually
                // changed — an open menu previously re-shaped every row on
                // every blink/hover-driven redraw even though its rows are
                // byte-stable while it stays open.
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
            // v2.38.2 P1b: `settings_display_lines` runs a `format!()` per
            // display line — memoize its output against the last
            // `SettingsOverlay` it was computed from instead of rebuilding
            // every painted frame (the settings panel, like the context
            // menu, sits open across blink/hover-driven redraws with nothing
            // actually changing).
            if self.settings_lines_source.as_ref() != Some(set) {
                self.settings_lines_cache = settings_display_lines(set);
                self.settings_lines_source = Some(set.clone());
            }
            let lines = &self.settings_lines_cache;
            while self.settings_buffers.len() < lines.len() {
                let b = TextBuffer::new(&mut self.font_system, metrics);
                self.settings_buffers.push(b);
            }
            // v2.38.2 P1b: grows off ITS OWN length, not `settings_buffers`' —
            // same reasoning as `context_menu_texts` above (the font-family
            // invalidation clears only the `_texts` cache, so the two pools
            // must each regrow independently or indexing below could panic).
            while self.settings_texts.len() < lines.len() {
                self.settings_texts.push(String::new());
            }
            self.settings_buffers.truncate(lines.len());
            self.settings_texts.truncate(lines.len());
            // Panel width fits the content but never exceeds the surface
            // (so it stays usable in a small window); see the matching clamp
            // in the quad/area pass below.
            let panel_w = (settings_panel_cols(lines) * cw + 48.0).min((sw - 40.0).max(120.0));
            let row_h = ch + 6.0;
            for (i, line) in lines.iter().enumerate() {
                let buf = &mut self.settings_buffers[i];
                buf.set_metrics(metrics);
                buf.set_size(Some(panel_w), Some(row_h));
                // v2.38.2 P1b: moving the focused row only changes 2 of N
                // lines (the old/new `▸` mark) — this per-row gate spares the
                // other N-2 rows a reshape even on a frame where the overlay
                // memoization above DID recompute `lines`.
                if self.settings_texts[i] != *line {
                    buf.set_text(
                        line,
                        &Attrs::new().family(Family::Name(&family)),
                        Shaping::Advanced,
                        None,
                    );
                    self.settings_texts[i].clone_from(line);
                }
                buf.shape_until_scroll(&mut self.font_system, false);
            }
        }

        if let Some(completion) = &overlay.completion
            && let Some(geometry) = completion_panel_geometry(completion, (cw, ch))
        {
            let palette = completion_palette(theme, self.ui_accent(cfg, theme));
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

        if let Some(receipt) = &overlay.media_paste_receipt
            && let Some(geometry) = media_paste_receipt_geometry(
                receipt,
                overlay.completion.as_ref(),
                (cw, ch),
                self.overlay_text_cell_width(),
                self.metrics.line_height,
            )
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
                // unchanged. The gate spares the ~100-label reshape the
                // blink-driven redraw paid while the overlay sat open.
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
            let cells = unicode_width::UnicodeWidthStr::width(preedit.text.as_str()).max(1);
            self.ime_buffer.set_metrics(metrics);
            self.ime_buffer
                .set_size(Some(cells as f32 * cw + 2.0), Some(ch));
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
        let focus_origin = panes.iter().find(|p| p.focused).map(|p| p.rect);

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
        // Menu text lives in its own areas vec so we can hand it to a
        // dedicated `menu_text_renderer.prepare(...)` call after the
        // main `text_renderer.prepare(...)` — drawing the
        // menu's bg / shadow / border / highlight before the menu's
        // text in the same pass painted text right under bg; this
        // split fixes that by giving the menu its own
        // bg→border→highlight→text pipeline at the end of the render
        // pass.
        // Pre-size for the menu / settings-overlay rows it collects.
        let mut menu_areas: Vec<TextArea> = Vec::with_capacity(48);
        // v2.20.0 (Ghostty parity): the transient resize chip — centered,
        // drawn in the menu pass (last) so it reads over any pane content.
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
        if let Some(receipt) = &overlay.media_paste_receipt
            && let Some(geometry) = media_paste_receipt_geometry(
                receipt,
                overlay.completion.as_ref(),
                (cw, ch),
                self.overlay_text_cell_width(),
                self.metrics.line_height,
            )
        {
            let palette = completion_palette(theme, self.ui_accent(cfg, theme));
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
                color::with_min_contrast(theme.palette[3], palette.panel_bg, 4.5)
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
                    media_receipt_live.insert(image.allocation_key());
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
        if let Some(completion) = &overlay.completion
            && let Some(geometry) = completion_panel_geometry(completion, (cw, ch))
        {
            let (x, y, width, height) = geometry.rect;
            let palette = completion_palette(theme, self.ui_accent(cfg, theme));
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
            let (rx, ry, rw, rh) = pv.rect;
            let grid_origin = pane_grid_origin(
                pv.rect,
                (pad_x, pad_y),
                pane_titlebar_h,
                cfg.title_at_bottom,
            );
            // Per-pane OSC 10 default-fg: glyphon's `default_color` is the
            // fallback when a span lacks an explicit color. Almost every
            // cell does carry an explicit color via `Attrs::color`, but
            // whitespace / IME composition / chrome strings ride the
            // default. Matches the OSC 11 chrome path —
            // engine override (Colors[256]) wins, theme is fallback.
            let pane_fg = pv.snap.colors[256]
                .map(|c| Rgb::new(c.r, c.g, c.b))
                .unwrap_or(theme.foreground);
            // v2.25.0: in the default grid mode, pane cell text is drawn by the
            // cell-locked `glyph_pipeline` (emitted below), NOT glyphon — so don't
            // push a pane TextArea here. Legacy mode keeps the old glyphon path.
            if cfg.text_renderer == TextRendererMode::Legacy {
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
        // Terminator parity, per-pane-titlebar Bucket-D,
        // phase 3 of TERMINATOR-PANE-TITLEBAR-DESIGN.md: per-pane title text. Push the TextAreas
        // referencing the `pane_titlebar_buffers` (already populated
        // earlier in this pass — see
        // build_pane_titlebar_text).
        if pane_titlebar_h > 0.0 {
            for (i, pv) in panes.iter().enumerate() {
                let (rx, ry, rw, rh) = pv.rect;
                // Matching fg variant for the three states,
                // derived from the theme so the title text stays
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
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
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
            let plus_fg = if tabbar.hovered_new_tab {
                self.ui_accent(cfg, theme)
            } else {
                fg
            };
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
                    self.ui_accent(cfg, theme)
                } else {
                    fg
                };
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
            // v2.26.0: overflow scroll-arrow glyphs `‹` / `›` at the strip edges.
            if tabbar.scroll_left.2 > 0.0 {
                let (ax, _, aw, _) = tabbar.scroll_left;
                areas.push(TextArea {
                    buffer: &self.scroll_left_buffer,
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
            if tabbar.scroll_right.2 > 0.0 {
                let (ax, _, aw, _) = tabbar.scroll_right;
                areas.push(TextArea {
                    buffer: &self.scroll_right_buffer,
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
            areas.push(TextArea {
                buffer: &self.search_buffer,
                left: search_rect.0,
                top: search_text_top
                    .unwrap_or(search_rect.1 + ((search_rect.3 - ch) * 0.5).max(0.0)),
                scale: 1.0,
                bounds: TextBounds {
                    left: search_rect.0 as i32,
                    top: search_rect.1 as i32,
                    right: (search_rect.0 + search_rect.2) as i32,
                    bottom: (search_rect.1 + search_rect.3) as i32,
                },
                default_color: search_text_color.unwrap_or(GColor::rgb(fg.r, fg.g, fg.b)),
                custom_glyphs: &[],
            });
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
                default_color: GColor::rgb(fg.r, fg.g, fg.b),
                custom_glyphs: &[],
            });
        }
        // Hint labels over the focused pane (chips drawn above as quads).
        if let Some((frx, fry, frw, frh)) = focus_origin {
            let focus_grid_origin = pane_grid_origin(
                (frx, fry, frw, frh),
                (pad_x, pad_y),
                pane_titlebar_h,
                cfg.title_at_bottom,
            );
            // Hint-label text follows the theme background (dark on
            // the theme-yellow chip) unless overridden.
            let lab = cfg.search_foreground.unwrap_or(theme.background);
            for (i, hint) in overlay.hint_labels.iter().enumerate() {
                areas.push(TextArea {
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
            if let Some(preedit) = &overlay.ime_preedit {
                areas.push(TextArea {
                    buffer: &self.ime_buffer,
                    left: focus_grid_origin.0 + preedit.col as f32 * cw,
                    top: focus_grid_origin.1 + preedit.row as f32 * ch,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: frx as i32,
                        top: fry as i32,
                        right: (frx + frw) as i32,
                        bottom: (fry + frh) as i32,
                    },
                    default_color: GColor::rgb(fg.r, fg.g, fg.b),
                    custom_glyphs: &[],
                });
            }
        }

        // Right-click context menu — drawn in its own final pass.
        // v1.3.0/v1.3.1 put the menu's panel-bg quad in
        // `over` (drawn AFTER text), with the opaque bg covering the
        // menu text underneath. Now: chrome quads go to `menu_q`
        // (drawn after `over` via `self.menu_quads.draw`); row labels
        // go to `menu_areas` (drawn via a dedicated
        // `self.menu_text_renderer.render` call after the menu
        // quads). The bg-under-text order finally matches reality.
        if let Some(menu) = &overlay.context_menu {
            let chrome = menu_chrome_quads(menu, theme, self.ui_accent(cfg, theme), cw, ch);
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
                    dim_blend(theme.foreground, theme.background)
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
                    let hint_fg = dim_blend(theme.foreground, theme.background);
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
            // v2.38.2 P1b: reuse the buffer-prep pass's memoized lines rather
            // than running `settings_display_lines` (one `format!()` per
            // display line) a SECOND time per frame — the two passes already
            // shared a comment promising they "call this off the same
            // `settings_display_lines` output, keeping them in lockstep";
            // `self.settings_lines_cache` was just (re)computed for this
            // exact `set` above, in the same `render_frame` call.
            let lines = &self.settings_lines_cache;
            let row_h = ch + 6.0;
            let panel_w = (settings_panel_cols(lines) * cw + 48.0).min((sw - 40.0).max(120.0));
            let panel_h = (lines.len() as f32 * row_h + 24.0).min((sh - 40.0).max(80.0));
            let px = ((sw - panel_w) * 0.5).max(0.0);
            let py = ((sh - panel_h) * 0.5).max(0.0);
            // Multi-window: the settings overlay's accent follows
            // this WINDOW's chrome accent, so it matches the focus border +
            // active tab rather than always-blue.
            let acc = self.ui_accent(cfg, theme);
            // Dim backdrop over the whole window so the panel reads as modal.
            // v2.24.0: on the Background page, dim LESS so the live wallpaper
            // (the real animated starfield / image) shows around the panel as a
            // genuine preview while you change `background-type`.
            let on_bg_page = set
                .categories
                .get(set.active_category)
                .map(|c| c == "Background")
                .unwrap_or(false);
            let backdrop_a = if on_bg_page { 0.30 } else { 0.55 };
            // Stop the dim above a confirm bar when one is showing. The bar is
            // pushed to this same list earlier in the frame, so a full-height
            // backdrop lands on top of it and greys out the one thing that has
            // to stay legible — and the dialog raised by rebinding onto an
            // already-bound chord comes from inside this very panel.
            let dim_h = if overlay.confirm_dialog.is_some() {
                (sh - (ch + 10.0)).max(0.0)
            } else {
                sh
            };
            menu_q.push(rect(0.0, 0.0, sw, dim_h, theme.background, backdrop_a));
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
            // v2.24.0: a disabled field row (inapplicable to the current state)
            // renders dimmed — blended halfway toward the panel background.
            let dim = color::dim(sfg, theme.background);
            for (i, _line) in lines.iter().enumerate() {
                if i >= self.settings_buffers.len() {
                    break;
                }
                let row_color = if i >= SETTINGS_FIELD_START
                    && set
                        .rows
                        .get(i - SETTINGS_FIELD_START)
                        .is_some_and(|r| r.disabled)
                {
                    dim
                } else {
                    sfg
                };
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
                    default_color: GColor::rgb(row_color.r, row_color.g, row_color.b),
                    custom_glyphs: &[],
                });
            }
        }

        // v2.21.0 (idle perf): skip the whole-viewport glyphon `prepare` when
        // nothing that feeds the text renderers changed this frame. `prepare`
        // re-encodes EVERY visible glyph's vertices + does atlas housekeeping;
        // on an idle repaint (a cursor blink, a bell-flash decay, a focus-dim
        // toggle) the text is byte-identical, so we re-render the cached vertex
        // buffers as-is and only rebuild/upload the cheap quad list. Skipping
        // is conservative — ANY pane row reshape, ANY chrome label change, or
        // ANY open text overlay forces the prepare, so a stale frame is
        // impossible. `atlas.trim()` (below) is likewise gated: trimming
        // without a following prepare would clear the in-use set and let a
        // later prepare evict still-displayed glyphs out from under the cached
        // vertices.
        let non_context_text_overlay_open = text_overlay_requires_continuous_prepare(overlay);
        let overlay_open = non_context_text_overlay_open || overlay.context_menu.is_some();
        let chrome_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            self.pane_titlebar_texts.hash(&mut h);
            self.tab_texts.hash(&mut h);
            self.tabbar_text.hash(&mut h);
            self.status_bar_text.hash(&mut h);
            self.tab_close_text.hash(&mut h);
            self.new_tab_arrow_text.hash(&mut h);
            self.resize_overlay_text.hash(&mut h);
            self.ime_text.hash(&mut h);
            prepared_text_areas_damage_key(&areas).hash(&mut h);
            // Completion buffers are retained while the card is static. Hash
            // both their geometry and their shaped source strings: glyphon
            // keeps each buffer at the same address when a same-size candidate
            // list changes, so the text-area identity alone cannot detect new
            // labels or descriptions.
            prepared_text_areas_damage_key(&menu_areas).hash(&mut h);
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
        let cursor_char = self.pending_cursor_glyph.as_ref().map(|c| c.ch);
        let cursor_char_changed = cursor_char != self.last_cursor_char;
        self.last_cursor_char = cursor_char;
        // v2.23.0 fix: the frame an overlay CLOSES (`overlay_open` flips
        // true→false) must still prepare once, or the closed panel's cached text
        // vertices keep rendering until the next keystroke. `overlay_open` alone
        // covers the open state; this covers the close edge.
        let overlay_changed = overlay_open != self.last_overlay_open;
        self.last_overlay_open = overlay_open;
        let need_prepare = self.text_prepare_dirty
            || any_pane_text_changed
            || chrome_changed
            || text_layout_changed
            || non_context_text_overlay_open
            || cursor_char_changed
            || overlay_changed;
        let cursor_glyph_key =
            cursor_glyph_damage_key(self.pending_cursor_glyph.as_ref(), metrics, &family);
        let cursor_glyph_changed = cursor_glyph_key != self.last_cursor_glyph_key;
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
            // Second TextRenderer prepare — context-menu rows. Empty
            // `menu_areas` is fine; glyphon's prepare handles a zero-area
            // batch as a no-op.
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
        // v2.25.1: cell-locked pane text has its OWN damage gate. A cursor blink
        // can force `need_prepare` via `cursor_char_changed` for the separate
        // cursor glyph pass, but it must not clear/re-upload pane glyph
        // instances. Only pane text changes and layout/style damage refresh the
        // grid pipeline. Legacy mode uploads an empty instance set on the same
        // gate so switching grid→legacy cannot leave stale grid glyphs behind.
        let grid_upload_needed =
            self.grid_glyphs_dirty || any_pane_text_changed || text_layout_changed;
        if grid_upload_needed {
            let mut gi = std::mem::take(&mut self.glyph_instances);
            let mut gc = std::mem::take(&mut self.glyph_clips);
            gi.clear();
            gc.clear();
            if cfg.text_renderer == TextRendererMode::Grid {
                self.emit_pane_glyphs(panes, cfg, pane_titlebar_h, &mut gi, &mut gc);
            }
            self.glyph_pipeline
                .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &gi);
            self.glyph_instances = gi;
            self.glyph_clips = gc;
            self.grid_glyphs_dirty = false;
        }
        // Prepare the focused solid-block cursor's inverted glyph only when its
        // text/position/style changed. A main text prepare also forces this tiny
        // prepare because both renderers share an atlas that may have repacked.
        // Menu-highlight and other quad-only frames reuse retained vertices.
        if let Some((gx, gy, gch, gcolor, gclip)) = self
            .pending_cursor_glyph
            .as_ref()
            .map(|c| (c.x, c.y, c.ch, c.color, c.clip))
            .filter(|_| need_cursor_prepare)
        {
            let mut enc = [0u8; 4];
            self.cursor_glyph_buffer.set_metrics(metrics);
            self.cursor_glyph_buffer.set_text(
                gch.encode_utf8(&mut enc),
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            self.cursor_glyph_buffer
                .shape_until_scroll(&mut self.font_system, false);
            let area = TextArea {
                buffer: &self.cursor_glyph_buffer,
                left: gx,
                top: gy,
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
        // interiors do not compound their configured transparency. That same
        // replace pass used to erase the unsupported-blur underlay floor,
        // leaving Linux at the user's raw opacity despite the advertised 99%
        // fallback. Upload a live-only copy with its final alpha clamped;
        // offscreen screenshots continue to draw the unmodified buffer above.
        if let Some(floor) = self.live_background_opacity_floor {
            apply_quad_alpha_floor(&mut pane_bases, floor);
            self.live_pane_bases
                .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &pane_bases);
        }
        self.quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &quads);
        self.pane_outlines
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &pane_outlines);
        // Return the scratch to the pool (keeps its capacity for next
        // frame). Last use of `quads` is the upload just above.
        self.quad_scratch = quads;
        // v2.23.0: wallpaper into its own back pipeline; inline images into
        // `imgs`. Each cache gets its own exact live set so an image used in one
        // role cannot accidentally pin a stale texture in the other pipeline.
        // Release textures not referenced by this frame before admitting new
        // ones. This prevents an old+new transient cache peak from breaching
        // the per-window/process GPU budgets; visible entries remain pinned.
        self.bg_imgs.gc(&bg_live);
        self.imgs.gc(&inline_live);
        self.media_receipt_img.gc(&media_receipt_live);
        let wallpaper_upload_complete = self.bg_imgs.upload_retained(
            &self.gpu.device,
            &self.gpu.queue,
            [sw, sh],
            &bg_img_items,
        );
        opaque_wallpaper_covers_surface &= wallpaper_upload_complete;
        // v2.24.0: refresh the procedural starfield's per-frame uniform (just
        // resolution + the continuous `time` clock; the look is baked into the
        // shader as of v2.24.1) when it's the active wallpaper.
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            self.starfield.upload(
                &self.gpu.queue,
                [sw, sh],
                self.starfield_started.elapsed().as_secs_f32(),
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
        self.overlay_quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &over);
        self.menu_quads
            .upload(&self.gpu.device, &self.gpu.queue, [sw, sh], &menu_q);

        let target_size = [self.config.width.max(1), self.config.height.max(1)];
        let scene_is_opaque = final_scene_is_uniformly_opaque(cfg, opaque_wallpaper_covers_surface);
        let needs_presentation =
            needs_postmultiplied_presentation(self.config.alpha_mode, scene_is_opaque);
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
                    if let Err(error) =
                        self.encode_scene_pass(&capture.view, target_size, cfg, false, &mut encoder)
                    {
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

        let (frame, reconfigure_after_present) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => (t, false),
            // The frame is usable, but no longer matches the underlying
            // surface. Present it once, then refresh the swapchain before the
            // next acquire as required by wgpu 30's explicit outcome model.
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => (t, true),
            // v2.31.0 (adversarial review): Occluded and Timeout are BENIGN
            // transient states, NOT device loss. CRITICAL: on macOS the Metal
            // backend returns `Occluded` on EVERY acquire while the window is
            // minimized/occluded (gfx-rs/wgpu#8309 — occluded `nextDrawable`
            // hangs ~1s, so the HAL short-circuits before it), so a minimized
            // window with any active output would otherwise rack up the streak
            // and FALSELY latch `gpu_lost` on a perfectly healthy device. Pure
            // skip-frame: do NOT reconfigure (reconfiguring an occluded surface
            // can itself hang).
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
                self.surface.configure(&self.gpu.device, &self.config);
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
        self.encode_scene_pass(scene_view, target_size, cfg, true, &mut encoder)?;
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
            self.surface.configure(&self.gpu.device, &self.config);
        }
        // Only trim when we prepared this frame (see the `need_prepare` gate):
        // trimming clears the glyph in-use set, so a trim with no following
        // prepare would let the next prepare evict glyphs the cached vertices
        // still point at.
        if need_prepare {
            self.atlas.trim();
        }
        Ok(FrameOutcome::Presented)
    }

    fn submit_offscreen_screenshot(
        &mut self,
        encoder: wgpu::CommandEncoder,
        prepared: Option<Result<PreparedScreenshot, (ScreenshotRequest, String)>>,
    ) {
        let Some(prepared) = prepared else {
            return;
        };
        let submission = self.gpu.queue.submit(std::iter::once(encoder.finish()));
        self.dispatch_prepared_screenshot(Some(prepared), submission);
    }

    fn dispatch_prepared_screenshot(
        &mut self,
        prepared: Option<Result<PreparedScreenshot, (ScreenshotRequest, String)>>,
        submission: wgpu::SubmissionIndex,
    ) {
        let Some(prepared) = prepared else {
            return;
        };
        match prepared {
            Ok(prepared) => {
                let job = ScreenshotJob {
                    device: self.gpu.device.clone(),
                    gpu_lost: self.gpu.gpu_lost.clone(),
                    gpu_fault: self.gpu.gpu_fault.clone(),
                    submission,
                    prepared,
                };
                self.submit_screenshot_job(job);
            }
            Err((request, error)) => Self::complete_screenshot_error(request, error),
        }
    }

    fn encode_scene_pass(
        &self,
        target: &wgpu::TextureView,
        target_size: [u32; 2],
        cfg: &Config,
        live_window: bool,
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
        // v2.23.0 layering: wallpaper at the very back → cell + chrome +
        // border quads opaquely on top → inline kitty/sixel images over the
        // cell backgrounds → text. Pre-2.23.0 the wallpaper drew *after*
        // `quads`, hiding all cell backgrounds and bleeding the animation
        // through the chrome.
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
        self.quads.draw(&mut pass);
        self.pane_outlines.draw(&mut pass);
        self.imgs.draw(&mut pass);
        // v2.25.0: cell-locked pane text sits above cell backgrounds + inline
        // images and below chrome text (titlebars / menus) and the cursor
        // glyph. A no-op (count 0) in legacy mode, where pane text rides the
        // glyphon `text_renderer` below.
        self.glyph_pipeline
            .draw(&mut pass, &self.glyph_clips, target_size);
        self.text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        // Dimming + scrollbar sit on top of glyphs.
        self.overlay_quads.draw(&mut pass);
        // Menu chrome sits above terminal content. The receipt thumbnail is
        // then composited below the shared chrome-text pass, so its own labels
        // and the context-menu labels both remain readable above images.
        self.menu_quads.draw(&mut pass);
        self.media_receipt_img.draw(&mut pass);
        self.menu_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        // v2.21.0 (idle perf): the focused solid-block cursor's inverted glyph,
        // drawn last so it sits on top of the block quad and normal glyph.
        if self.pending_cursor_glyph.is_some() {
            self.cursor_glyph_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
        }
        Ok(())
    }

    fn create_screenshot_target(&self, size: [u32; 2]) -> Result<ScreenshotCaptureTarget, String> {
        let bytes = screenshot_target_bytes(size[0], size[1]).ok_or_else(|| {
            "screenshot target size overflow or exceeds the 256 MiB capture limit".to_string()
        })?;
        let gpu = self
            .graphics_budget
            .reserve_transient_gpu(bytes)
            .ok_or_else(|| {
                format!(
                    "GPU graphics budget exhausted while creating a {bytes}-byte screenshot target"
                )
            })?;
        let unpadded_bytes_per_row = size[0]
            .checked_mul(4)
            .ok_or_else(|| "screenshot row size overflow".to_string())?;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(align - 1)
            .map(|row| row / align * align)
            .ok_or_else(|| "screenshot aligned row size overflow".to_string())?;
        let buffer_size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(size[1]))
            .ok_or_else(|| "screenshot readback size overflow".to_string())?;
        if buffer_size > MAX_LIVE_SCREENSHOT_BYTES {
            return Err(format!(
                "screenshot readback requires {buffer_size} bytes; limit is \
                 {MAX_LIVE_SCREENSHOT_BYTES} bytes"
            ));
        }
        let staging_bytes = usize::try_from(buffer_size)
            .map_err(|_| "screenshot staging buffer does not fit this platform".to_string())?;
        // Reserve every allocation before encoding any commands. If the second
        // reservation fails, `gpu` drops while no command buffer can yet refer
        // to the capture texture; accounting can therefore never end before a
        // submitted use of the resource retires.
        let staging_gpu = self
            .graphics_budget
            .reserve_transient_gpu(staging_bytes)
            .ok_or_else(|| {
                format!(
                    "GPU graphics budget exhausted while creating a {staging_bytes}-byte screenshot staging buffer"
                )
            })?;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-screenshot-scene"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-screenshot-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(ScreenshotCaptureTarget {
            texture,
            view,
            gpu,
            staging,
            staging_gpu,
            width: size[0],
            height: size[1],
            unpadded_bytes_per_row,
            padded_bytes_per_row,
        })
    }

    fn prepare_texture_screenshot(
        &self,
        capture: ScreenshotCaptureTarget,
        request: ScreenshotRequest,
        encoder: &mut wgpu::CommandEncoder,
        premultiplied: bool,
    ) -> PreparedScreenshot {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &capture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &capture.staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(capture.padded_bytes_per_row),
                    rows_per_image: Some(capture.height),
                },
            },
            wgpu::Extent3d {
                width: capture.width,
                height: capture.height,
                depth_or_array_layers: 1,
            },
        );
        let format = capture.texture.format();
        PreparedScreenshot {
            staging: capture.staging,
            _capture_texture: capture.texture,
            _capture_gpu: capture.gpu,
            _staging_gpu: capture.staging_gpu,
            width: capture.width,
            height: capture.height,
            unpadded_bytes_per_row: capture.unpadded_bytes_per_row,
            padded_bytes_per_row: capture.padded_bytes_per_row,
            format,
            premultiplied,
            request,
        }
    }

    fn complete_screenshot_error(request: ScreenshotRequest, error: String) {
        if let Some(tx) = request.completion {
            let _ = tx.send(Err(error.clone()));
        }
        log::warn!("take_screenshot capture failed: {error}");
    }

    fn submit_screenshot_job(&mut self, mut job: ScreenshotJob) {
        for attempt in 0..2 {
            if self.screenshot_worker.is_none() {
                match ScreenshotWorker::start() {
                    Ok(worker) => self.screenshot_worker = Some(worker),
                    Err(error) => {
                        Self::complete_screenshot_error(
                            job.prepared.request,
                            format!("screenshot worker unavailable: {error}"),
                        );
                        return;
                    }
                }
            }
            let Some(worker) = self.screenshot_worker.as_ref() else {
                unreachable!("screenshot worker was initialized above");
            };
            match worker.try_submit(job) {
                Ok(()) => return,
                Err(ScreenshotSubmitError::Busy(returned)) => {
                    Self::complete_screenshot_error(
                        returned.prepared.request,
                        "another live screenshot is already in progress".to_string(),
                    );
                    return;
                }
                Err(ScreenshotSubmitError::Disconnected(returned)) => {
                    self.screenshot_worker = None;
                    job = *returned;
                    if attempt == 1 {
                        Self::complete_screenshot_error(
                            job.prepared.request,
                            "screenshot worker disconnected".to_string(),
                        );
                        return;
                    }
                }
            }
        }
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
        search_highlights: &[HighlightRect],
        quads: &mut Vec<QuadInstance>,
        pane_bases: &mut Vec<QuadInstance>,
        // Terminator parity, per-pane-titlebar Bucket-D,
        // phase 2 of TERMINATOR-PANE-TITLEBAR-DESIGN.md: extra top offset for cell content
        // so it doesn't overlap the per-pane titlebar bar. When
        // titlebar is off this is 0.0 (zero overhead).
        pane_titlebar_h: f32,
    ) -> bool {
        // v2.21.0 (idle perf): becomes true iff this pane mutated its text
        // buffer this frame (a row reshaped, or the line count changed). When
        // NO pane changed — and chrome text is identical, no overlay is open —
        // `render_frame_with_status` skips the whole-viewport glyphon
        // `prepare`, re-rendering the cached glyph vertices instead. A cursor
        // blink that doesn't touch text (bar/underline/hollow, or any steady
        // cursor) therefore costs no reshape AND no glyph re-encode.
        let mut text_changed = false;
        let theme = &cfg.theme;
        let (_, _, rw, rh) = pv.rect;
        let (ox, oy) = pane_grid_origin(
            pv.rect,
            (cfg.padding_x, cfg.padding_y),
            pane_titlebar_h,
            cfg.title_at_bottom,
        );
        let cw = self.cell_w;
        let ch = self.cell_h;
        // v2.20.0 P2: everything below reads the lock-free snapshot captured
        // by `redraw` — same data `renderable_content()` used to yield, the
        // Term mutex is just no longer held while we process it.
        let snap = pv.snap;
        let term_colors = &snap.colors;
        let cols = snap.columns;
        // Cells inside the selection range get their fg swapped to
        // `theme.selection_foreground` so dark-on-dark themes stay readable
        // under the highlight. Without this, the configured
        // `selection-foreground` color was parsed and stored but the
        // renderer ignored it.
        let selection_range = snap.selection;
        // Snapshot cells + selection carry
        // GRID-ABSOLUTE lines (negative when scrolled into history); the per-cell
        // bg/underline/strikeout quads and the selection-bg quad position by
        // VIEWPORT row, so convert with `viewport_row = grid_line + display_offset`
        // (alacritty's `point_to_viewport`). The text itself flows correctly off
        // relative line-break deltas, so only the quad Y needs this. No-op when
        // not scrolled (display_offset == 0).
        let display_off = snap.display_offset as i32;
        let screen_rows = snap.screen_lines as i32;
        let default_bg = term_colors[257]
            .map(|c| Rgb::new(c.r, c.g, c.b))
            .unwrap_or(theme.background);

        let bw = if cfg.handle_size < 0 {
            1.0
        } else {
            cfg.handle_size as f32
        };
        if let Some((bx, by, bwid, bhgt)) =
            pane_backdrop_rect(pv.rect, bw, pane_titlebar_h, cfg.title_at_bottom)
        {
            let backdrop = rect(
                bx,
                by,
                bwid,
                bhgt,
                default_bg,
                composed_bg_alpha(cfg) as f32,
            );
            if background_has_wallpaper(cfg) {
                quads.push(backdrop);
            } else {
                pane_bases.push(backdrop);
            }
        }

        // Take the pooled scratch (with last frame's String
        // buffers) instead of allocating fresh. `n` is the LOGICAL run count;
        // `spans` may hold extra slots from a busier prior frame, which we reuse
        // (clear + refill) before falling back to a push. Stored back to `self`
        // at the end of this method so the capacity recycles next frame.
        let mut spans = std::mem::take(&mut self.span_scratch);
        let mut span_line_breaks = std::mem::take(&mut self.span_breaks_scratch);
        span_line_breaks.clear();
        let mut n = 0usize;
        let mut cur_row = 0i32;
        let mut saw_styled_text = false;
        // The style of the run currently being appended to (`spans[n - 1]`), or
        // `None` when the next char must open a new run.
        let mut cur: Option<(Rgb, bool, bool)> = None;
        let mut search_highlight_cursor = 0usize;

        // Terminator parity, cursor_fg_color / cursor_bg_color: a
        // focused SOLID block cursor renders the block in `theme.cursor`
        // (cursor-color / cursor-bg-color) with the glyph UNDER it recolored to
        // `theme.cursor_text` (cursor-fg-color) — the standard "inverted cursor"
        // model. Identify that grid-absolute cell so the span builder recolors
        // exactly its glyph. Only the full Block shape covers the glyph (beam /
        // underline leave it visible, so they aren't recolored).
        use alacritty_terminal::vte::ansi::CursorShape as EShape;
        let cp = snap.cursor.point;
        // The terminal engine owns the scrollback-aware vi cursor point. Give
        // it a stable hollow shape regardless of the application's current
        // DECSCUSR cursor style so vi mode remains visually distinct without a
        // second, viewport-relative cursor model in the UI.
        let shape = if snap.vi_mode {
            EShape::HollowBlock
        } else {
            snap.cursor.shape
        };
        let cvrow = cp.line.0 + display_off;
        let base_draw_cursor = cursor_focus_gate(
            window_focused,
            shape != EShape::Hidden
                && (0..screen_rows).contains(&cvrow)
                && pv.focused
                && (snap.vi_mode || cursor_visible),
        );
        // Codex's native-Windows cursor compatibility shim applies to the
        // application's writing cursor, never to the user-controlled vi
        // cursor.
        let draw_cursor = if snap.vi_mode {
            base_draw_cursor
        } else {
            cursor_policy::cursor_draw_allowed(
                snap,
                cvrow,
                base_draw_cursor,
                cfg!(target_os = "windows"),
            )
        };
        let recolor_cursor_cell: Option<(i32, usize)> = {
            if draw_cursor && !snap.vi_mode && window_focused && shape == EShape::Block {
                Some((cp.line.0, cp.column.0))
            } else {
                None
            }
        };
        // A wide (CJK/emoji) glyph under the cursor needs
        // a TWO-cell block — recoloring the glyph to cursor_text while the
        // 1-cell block covered only its left half left the right half drawn
        // in cursor_text on the default bg (invisible on Mocha, where
        // cursor_text == background). And a cursor parked on the SPACER half
        // re-anchors to the lead glyph one cell left. Discovered during the
        // cell walk (the display iterator is single-pass); `None` = narrow
        // cell, draw as before.
        let mut cursor_wide_quad: Option<(usize, f32)> = None;
        // v2.21.0 (idle perf): instead of recoloring the glyph UNDER a focused
        // solid block cursor INTO the pane text buffer (which dirtied the
        // cursor row's shaping cache every blink and forced a whole-viewport
        // re-prepare), capture (glyph, color) here and draw it in the dedicated
        // cursor-glyph pass on top of the block. The pane buffer then stays
        // byte-identical across a blink, so the prepare is skipped.
        let mut cursor_glyph_capture: Option<(char, Rgb)> = None;

        for sc in &snap.cells {
            let row = sc.line;
            let col = sc.col;
            // Viewport row for quad placement; `row` (grid-absolute,
            // negative when scrolled) stays for the relative line-break deltas.
            let vrow = row + display_off;
            if row != cur_row {
                cur = None; // runs never span a line break
                for _ in cur_row..row {
                    span_line_breaks.push(n);
                }
                cur_row = row;
            }

            let flags = sc.flags;
            let mut fg = color::resolve(sc.fg, theme, term_colors);
            let mut bg = color::resolve(sc.bg, theme, term_colors);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let selected = selection_range.is_some_and(|r| r.contains(sc.point()));
            // Terminator parity, terminatorlib/config.py:111
            // `allow_bold`: when false, suppress bold attr entirely.
            // Useful on fonts without a bold companion.
            let bold = cfg.allow_bold && flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            saw_styled_text |= bold || italic;
            let hidden = flags.contains(Flags::HIDDEN);
            // The same search pair drives the match background quads and the
            // terminal glyphs above them. Active matches use the configured
            // search fg/bg; inactive matches reuse the theme selection pair.
            // `search_highlight_at` advances once through sorted visible spans,
            // so this adds no match-count multiplier to the hot cell walk.
            let search =
                search_highlight_at(search_highlights, &mut search_highlight_cursor, vrow, col);
            let highlight = match search {
                Some(active) => CellHighlight::Search(active),
                None if selected => CellHighlight::Selection,
                None => CellHighlight::None,
            };
            fg = resolved_cell_foreground(
                fg,
                bg,
                highlight,
                flags.contains(Flags::DIM),
                bold,
                cfg,
                theme,
            );
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
                // The glyph keeps its NORMAL `fg` in the pane buffer; the
                // cursor pass draws this recolored copy on top of the block.
                // See `color::cursor_glyph_color` for which colour and why.
                let cursor_fg = color::cursor_glyph_color(theme, term_colors, bg);
                cursor_glyph_capture = Some((sc.c, cursor_fg));
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
            // tracked (see the `sgr_underline_dim_strike` test); render
            // support followed later.
            //
            // Underline color: SGR 58 (`\e[58;2;r;g;bm` / `[58;5;Nm`) sets
            // a per-cell `underline_color`, used by neovim spell-check to
            // draw red squiggles on otherwise-normal text. Resolve it via
            // the same path as fg/bg; fall back to `fg` when unset so
            // every existing usage keeps working.
            //
            // Underline style: alacritty exposes five style bits —
            // UNDERLINE, DOUBLE_UNDERLINE, UNDERCURL, DOTTED_UNDERLINE,
            // DASHED_UNDERLINE — all reached via `Flags::ALL_UNDERLINES`, and
            // mutually exclusive (alacritty_terminal clears the others
            // whenever SGR 4 sets a new sub-style; see `term/mod.rs`'s
            // `Attr::Underline*` arms). `push_underline_quads` now gives
            // each style a visually distinct shape (audit fix — see its doc
            // comment for the undercurl approximation's limits) instead of
            // collapsing all five to the same 1px line; DOUBLE_UNDERLINE's
            // extra stacked line is still added separately below.
            if flags.intersects(Flags::ALL_UNDERLINES) {
                let line_color = sc
                    .underline_color
                    .map(|c| color::resolve(c, theme, term_colors))
                    .unwrap_or(fg);
                let x = ox + col as f32 * cw;
                let y = oy + vrow as f32 * ch;
                push_underline_quads(quads, x, y, cw, ch, col, flags, line_color);
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
            let dc = if hidden { ' ' } else { sc.c };
            // Combining (zero-width) marks layered on this cell — a decomposed
            // accent (`e`+U+0301), an emoji ZWJ sequence, a variation selector.
            // Append them right after the base char so the shaper composes the
            // full grapheme; skip on a HIDDEN cell (the base became a space, so
            // the marks have nothing to attach to). audit v2.32.0.
            let marks: &[char] = if hidden { &[] } else { sc.zerowidth() };
            match cur {
                Some((f, cb, ci)) if f == fg && cb == bold && ci == italic => {
                    // Same style — extend the current run (the last live span).
                    spans[n - 1].0.push(dc);
                    spans[n - 1].0.extend(marks.iter().copied());
                }
                _ => {
                    // New run: reuse the pooled slot's String if one exists
                    // (clearing keeps its capacity), else push a fresh entry.
                    if n < spans.len() {
                        let slot = &mut spans[n];
                        slot.0.clear();
                        slot.0.push(dc);
                        slot.0.extend(marks.iter().copied());
                        slot.1 = fg;
                        slot.2 = bold;
                        slot.3 = italic;
                    } else {
                        let mut s = String::new();
                        s.push(dc);
                        s.extend(marks.iter().copied());
                        spans.push((s, fg, bold, italic));
                    }
                    n += 1;
                    cur = Some((fg, bold, italic));
                }
            }
        }
        if saw_styled_text {
            self.ensure_bundled_style_faces();
        }

        // Selection. A block (Alt+drag) selection draws a per-row COLUMN
        // rectangle so the highlight matches the rectangular text that's copied;
        // a linear selection wraps full lines. `selection_row_span` encodes both.
        if let Some(sel) = snap.selection {
            let (s, e) = (sel.start, sel.end);
            for r in visible_selection_rows(s.line.0, e.line.0, display_off, screen_rows) {
                // Selection lines are grid-absolute; map to the viewport row.
                // The old `r < 0` guard DROPPED any selection scrolled up into
                // history, and a positive `r` was drawn at the wrong
                // (un-offset) viewport y.
                let vrow = r + display_off;
                debug_assert!((0..screen_rows).contains(&vrow));
                let (c0, c1) = selection_row_span(
                    r,
                    (s.line.0, s.column.0),
                    (e.line.0, e.column.0),
                    cols,
                    sel.is_block,
                );
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

        // Cursor: hidden with the window unfocused, blink-aware otherwise.
        // Shape comes from the engine's live `RenderableContent.cursor.shape`
        // which DECSCUSR (`CSI Ps SP q`) updates per-pane — vim/neovim/fish
        // use this to flip between block/underline/beam for normal/insert/
        // replace modes. The engine is seeded from `cfg.cursor_style` at pane
        // creation so the default still matches the user's config.
        // Also require cursor_visible. The old check fell
        // through to draw the hollow-outline branch on an unfocused
        // window even when DEC ?25l had hidden the cursor. So a
        // program that called `printf '\e[?25l'` (vim, less, fzf…)
        // and the user clicked away — the unfocused-pane outline
        // still showed. cursor_visible now gates everything; the
        // hollow-outline-for-HollowBlock-shape case stays inside the
        // visible branch since DECSCUSR shapes and DEC ?25 hide are
        // independent (a program can use HollowBlock to mean "I'm
        // not in this pane" while still wanting the cursor visible).
        // The cursor point is grid-absolute, including alacritty's native vi
        // cursor. When scrolled back (`display_offset > 0`) it must convert to
        // a viewport row like the cells and selection already do above — else
        // a phantom cursor block paints over unrelated scrollback.
        if draw_cursor {
            // A wide glyph under a solid block cursor widens the
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
            let cursor_color = if snap.vi_mode {
                // Keep vi navigation distinct from both the application
                // cursor and broadcast-mode yellow.
                theme.palette[5]
            } else {
                color::resolve_query(258, theme, term_colors).unwrap_or(theme.cursor)
            };
            // Hollow outline only when the running program requests
            // `HollowBlock` through DECSCUSR. Window focus is a renderer gate
            // above, so losing focus does not mutate or substitute DEC state.
            if shape == EShape::HollowBlock {
                quads.push(rect(bx, by, cw, 1.0, cursor_color, 1.0));
                quads.push(rect(bx, by + ch - 1.0, cw, 1.0, cursor_color, 1.0));
                quads.push(rect(bx, by, 1.0, ch, cursor_color, 1.0));
                quads.push(rect(bx + cw - 1.0, by, 1.0, ch, cursor_color, 1.0));
            } else {
                let (cwidth, alpha, cheight, yoff) = match shape {
                    EShape::Beam => (cw * 0.15, 1.0, ch, 0.0),
                    EShape::Underline => (cw, 1.0, 2.0, ch - 2.0),
                    // A focused block cursor is SOLID (was a 0.55
                    // translucent tint). v2.21.0: the inverted glyph under it is
                    // drawn in the dedicated cursor-glyph pass (see below), not
                    // recolored into the pane buffer, so a blink no longer
                    // reshapes the row. `bcells` widens it over a
                    // wide (CJK/emoji) glyph so the right half isn't uncovered.
                    EShape::Block | EShape::HollowBlock | EShape::Hidden => {
                        (cw * bcells, 1.0, ch, 0.0)
                    }
                };
                quads.push(rect(bx, by + yoff, cwidth, cheight, cursor_color, alpha));
                // v2.21.0 (idle perf): queue the inverted foreground glyph to be
                // drawn ON TOP of the solid block in its own pass. Only the
                // full Block shape covers the glyph; beam/underline leave it
                // visible in its normal color, so they need no overdraw.
                if matches!(shape, EShape::Block)
                    && let Some((gch, gcolor)) = cursor_glyph_capture
                {
                    self.pending_cursor_glyph = Some(PendingCursorGlyph {
                        x: bx,
                        y: by,
                        ch: gch,
                        color: gcolor,
                        clip: pv.rect,
                    });
                }
            }
        }

        // Lay out the text buffer. Advance lines by the grid's
        // `cell_h` (which includes the cfg.cell_height multiplier) so the text
        // rows stay locked to the cursor/quad row step — see `pane_metrics`.
        let pm = pane_metrics(self.metrics.font_size, self.cell_h);
        let buf = &mut self.pane_buffers[idx];
        // Both calls below are no-ops when the values are unchanged
        // (cosmic-text's `set_metrics_and_size` early-outs on equality), so
        // steady-state frames don't relayout; a zoom / pane-resize relayouts
        // internally while PRESERVING each line's shaping cache.
        buf.set_metrics(pm);
        buf.set_size(
            Some((rw - cfg.padding_x * 2.0).max(1.0)),
            Some((rh - cfg.padding_y * 2.0 - pane_titlebar_h).max(1.0)),
        );
        // Terminal rows are hard-wrapped by the VT engine at `cols`; the renderer
        // must NEVER soft-wrap. Wrap::None keeps exactly one layout run per buffer
        // line — both so a too-wide fallback glyph can't push the line onto a
        // phantom second visual row, AND so the cell-locked emit can rely on
        // `char index == grid column`. cosmic-text no-ops this when unchanged.
        buf.set_wrap(Wrap::None);
        let ff = font_features(cfg);
        let default_attrs = Attrs::new()
            .family(Family::Name(family))
            .font_features(ff.clone());
        // Always Advanced: it is the only shaping mode that walks
        // cosmic-text's platform font-fallback cascade (CJK, emoji, symbols).
        // The ligature toggle is fully expressed as OpenType features
        // (`font_features()` emits liga/clig/calt/dlig=0 when off), so the
        // old ligatures-off drop to Basic shaping bought nothing but a
        // narrower fast path — and silently tofu-boxed every fallback glyph
        // for those users. Cost is bounded by the per-line shaping cache
        // below: only rows whose content changed re-shape.
        let shaping = Shaping::Advanced;

        // Keep buffer lines row-aligned and reshape only when a row key changes.
        // The key includes rendered runs; the style key below covers shaping
        // inputs such as font variants, ligatures, and features. `reset_new`
        // is required for a new row or style because `set_text` does not change
        // a line's shaping mode.
        use std::hash::{Hash, Hasher};
        let style_key = {
            let mut h = std::hash::DefaultHasher::new();
            for (b, i) in [(false, false), (true, false), (false, true), (true, true)] {
                cfg.family_for(b, i).hash(&mut h);
            }
            cfg.font_ligatures.hash(&mut h);
            for f in &cfg.font_features {
                f.tag.hash(&mut h);
                f.value.hash(&mut h);
            }
            h.finish()
        };
        if self.pane_style_keys[idx] != style_key {
            self.pane_style_keys[idx] = style_key;
            // Wipe the row keys: every row below re-sets via `reset_new`,
            // which is what propagates a changed font stack / feature set.
            self.pane_line_keys[idx].clear();
        }
        let rows = screen_rows.max(0) as usize;
        let old_lines = buf.lines.len();
        while buf.lines.len() < rows {
            buf.lines.push(BufferLine::new(
                String::new(),
                LineEnding::Lf,
                AttrsList::new(&default_attrs),
                shaping,
            ));
        }
        buf.lines.truncate(rows);
        // A grow/shrink changes the prepared area set, so the cached glyph
        // vertices can no longer be reused.
        text_changed |= buf.lines.len() != old_lines;
        let keys = &mut self.pane_line_keys[idx];
        keys.truncate(rows);
        let mut row_text = std::mem::take(&mut self.line_text_scratch);
        // Row r's runs are `spans[breaks[r-1]..breaks[r]]` — `span_line_breaks`
        // records the live run count at each row transition (one entry per
        // crossed row, `rows - 1` total), exactly the structure the old
        // `build_rich_spans` consumed to interleave its `"\n"` markers.
        let mut start = 0usize;
        for row in 0..rows {
            let end = span_line_breaks.get(row).copied().unwrap_or(n).min(n);
            let runs = &spans[start.min(end)..end];
            start = end;
            let key = {
                let mut h = std::hash::DefaultHasher::new();
                for (text, fg, bold, italic) in runs {
                    text.hash(&mut h);
                    (fg.r, fg.g, fg.b).hash(&mut h);
                    (bold, italic).hash(&mut h);
                }
                h.finish()
            };
            let prev = keys.get(row).copied();
            if prev == Some(key) {
                continue;
            }
            // This row reshapes — the buffer's glyph vertices will differ.
            text_changed = true;
            row_text.clear();
            let mut attrs_list = AttrsList::new(&default_attrs);
            for (text, fg, bold, italic) in runs {
                let s = row_text.len();
                row_text.push_str(text);
                let a = run_attrs(cfg, &ff, *fg, *bold, *italic);
                // Mirror `set_rich_text`: only record a span when it differs
                // from the row defaults (fewer spans = cheaper compares).
                if a != attrs_list.defaults() {
                    attrs_list.add_span(s..row_text.len(), &a);
                }
            }
            if prev.is_some() {
                buf.lines[row].set_text(&row_text, LineEnding::Lf, attrs_list);
            } else {
                buf.lines[row].reset_new(row_text.as_str(), LineEnding::Lf, attrs_list, shaping);
            }
            if keys.len() <= row {
                keys.push(key);
            } else {
                keys[row] = key;
            }
        }
        self.line_text_scratch = row_text;
        // Shapes whatever the loop dirtied; cached rows walk their warm
        // layout caches. (The buffer's scroll provably stays at the default
        // (0, 0.0) on this path: `shape_until_scroll` only moves it when
        // `scroll.vertical` is already non-zero or `scroll.line > 0`, and
        // nothing here sets either.)
        buf.shape_until_scroll(&mut self.font_system, false);
        // Return the scratch (with its grown String buffers) to the pool for the
        // next frame/pane.
        self.span_scratch = spans;
        self.span_breaks_scratch = span_line_breaks;
        text_changed
    }

    /// v2.25.0 (cell-locked rendering): walk every visible pane's freshly-shaped
    /// `Buffer` and emit ONE pinned glyph instance per laid-out glyph, positioned
    /// at its grid cell (`pane_origin + col*cell_w`) instead of cosmic-text's
    /// continuous advance. Rasterization + the cache key + the vertical / bearing
    /// math are byte-identical to glyphon (see `glyphpipe.rs`); only the X is
    /// substituted, and only that differs from the legacy path — a primary-face
    /// monospace glyph already has advance == cell_w, so its position is
    /// unchanged. The drift cases (fallback punctuation, Nerd icons, color emoji,
    /// CJK, ligatures, mismatched-width bold/italic) are what get pinned.
    fn emit_pane_glyphs(
        &mut self,
        panes: &[PaneView<'_>],
        cfg: &Config,
        pane_titlebar_h: f32,
        out: &mut Vec<GlyphInstance>,
        clips: &mut Vec<GlyphClip>,
    ) {
        let pad_x = cfg.padding_x;
        let pad_y = cfg.padding_y;
        let default_fg = cfg.theme.foreground;
        // Split `*self` into disjoint field borrows so the glyph pipeline (mut),
        // the swash cache + font system (mut, for rasterization), the shaped pane
        // buffers (shared) and the scratch all coexist during the walk.
        let Self {
            glyph_pipeline,
            swash,
            font_system,
            pane_buffers,
            glyph_char_starts,
            gpu,
            cell_w,
            ..
        } = self;
        let cw = *cell_w;
        let device = &gpu.device;
        let queue = &gpu.queue;

        for (i, pv) in panes.iter().enumerate() {
            let (rx, ry, rw, rh) = pv.rect;
            let (ox, oy) = pane_grid_origin(
                pv.rect,
                (pad_x, pad_y),
                pane_titlebar_h,
                cfg.title_at_bottom,
            );
            // This pane's glyphs form one contiguous instance range; record it
            // with the pane rect so `draw` can scissor-clip text to the pane.
            let clip_start = out.len() as u32;
            // Per-pane default fg (OSC 10 / theme), the fallback for a glyph with
            // no explicit color span. Mirrors the glyphon pane TextArea's
            // `default_color`.
            let pane_fg = pv.snap.colors[256]
                .map(|c| Rgb::new(c.r, c.g, c.b))
                .unwrap_or(default_fg);
            let default_color = GColor::rgb(pane_fg.r, pane_fg.g, pane_fg.b);

            let buf = &pane_buffers[i];
            // `build_pane` pushes exactly one char per cell with `Wrap::None`, so
            // the shared cell-lock emit pins each glyph to its grid column.
            emit_cell_locked_glyphs(
                out,
                buf,
                (ox, oy),
                cw,
                default_color,
                glyph_pipeline,
                swash,
                font_system,
                device,
                queue,
                glyph_char_starts,
            );
            clips.push(GlyphClip {
                rect: [rx, ry, rw, rh],
                start: clip_start,
                count: out.len() as u32 - clip_start,
            });
        }
    }
}

/// OpenType features to shape pane text with: the coarse ligature toggle
/// expressed as `liga/clig/calt/dlig = 0` when off, then the user's explicit
/// `font-feature` overrides applied on top (so they can re-enable or tune
/// individual features). Cited: Ghostty `font-feature`, kitty `font_features`.
/// Upper bound on tiles a `tile` background may emit per frame before falling
/// back to a single stretched quad. ~60-px tiles on a 4K surface (3840×2160 →
/// 64×34 ≈ 2176) stay under it; only pathologically small source images
/// (≤ ~30 px) trip the cap.
/// Divide a bounded frame resource across independent panes in deterministic
/// round-robin order. Saturated or empty panes donate their unused share, so
/// the result consumes `min(sum(counts), limit)` slots without allowing the
/// first busy pane to starve every pane after it.
fn fair_placement_quotas(counts: &[usize], limit: usize) -> Vec<usize> {
    let mut quotas = vec![0; counts.len()];
    let mut remaining = limit;
    while remaining > 0 {
        let before = remaining;
        for (quota, count) in quotas.iter_mut().zip(counts.iter().copied()) {
            if *quota < count {
                *quota += 1;
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
        }
        if remaining == before {
            break;
        }
    }
    quotas
}

fn placement_is_visible(snap: &PaneSnapshot, placement: &kettle_core::Placement) -> bool {
    let top = u128::from(snapshot_viewport_top(snap));
    let start = u128::from(placement.abs_line);
    let end = start + placement.cell_rows as u128;
    let viewport_end = top + snap.screen_lines as u128;
    snap.screen_lines != 0 && placement.cell_rows != 0 && start < viewport_end && top < end
}

fn snapshot_viewport_top(snap: &PaneSnapshot) -> u64 {
    snap.history_origin
        .saturating_add(snap.history_size as u64)
        .saturating_sub(snap.display_offset.min(snap.history_size) as u64)
}

fn placement_viewport_row(snap: &PaneSnapshot, placement: &kettle_core::Placement) -> Option<i64> {
    let delta = i128::from(placement.abs_line) - i128::from(snapshot_viewport_top(snap));
    i64::try_from(delta).ok()
}

fn inline_placement_rect(
    base_x: f32,
    base_y: f32,
    row: i64,
    cell_width: f32,
    cell_height: f32,
    placement: &kettle_core::Placement,
) -> (f32, f32, f32, f32) {
    (
        base_x + (placement.col as f32 + placement.x_offset_cells) * cell_width,
        base_y + (row as f32 + placement.y_offset_cells) * cell_height,
        placement.display_cols * cell_width,
        placement.display_rows * cell_height,
    )
}

/// Intersect the owning pane's drawable body with its terminal grid viewport.
///
/// Inline images are cell-anchored terminal content: they must not paint the
/// pane border/titlebar, padding, sibling panes, or window chrome even when a
/// placement starts above the viewport or declares an oversized destination.
fn inline_image_clip(
    pane_body: (f32, f32, f32, f32),
    grid_origin: (f32, f32),
    grid_size: (usize, usize),
    cell_size: (f32, f32),
) -> Option<[f32; 4]> {
    let body = [pane_body.0, pane_body.1, pane_body.2, pane_body.3];
    let grid = [
        grid_origin.0,
        grid_origin.1,
        grid_size.0 as f32 * cell_size.0,
        grid_size.1 as f32 * cell_size.1,
    ];
    if !body.into_iter().chain(grid).all(f32::is_finite)
        || body[2] <= 0.0
        || body[3] <= 0.0
        || grid[2] <= 0.0
        || grid[3] <= 0.0
    {
        return None;
    }

    let x0 = body[0].max(grid[0]);
    let y0 = body[1].max(grid[1]);
    let x1 = (body[0] + body[2]).min(grid[0] + grid[2]);
    let y1 = (body[1] + body[3]).min(grid[1] + grid[3]);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

fn background_image_rect(
    mode: &str,
    align_horiz: &str,
    align_vert: &str,
    surface: [f32; 2],
    image: [f32; 2],
) -> [f32; 4] {
    let [sw, sh] = surface;
    let [img_w, img_h] = image;
    let (w, h) = match mode {
        "center" => (img_w, img_h),
        "scale" => {
            let scale = (sw / img_w).min(sh / img_h);
            (img_w * scale, img_h * scale)
        }
        _ => return [0.0, 0.0, sw, sh],
    };
    let x = match align_horiz {
        "left" => 0.0,
        "right" => sw - w,
        _ => (sw - w) * 0.5,
    };
    let y = match align_vert {
        "top" => 0.0,
        "bottom" => sh - h,
        _ => (sh - h) * 0.5,
    };
    [x, y, w, h]
}

fn rect_covers_surface(rect: [f32; 4], surface: [f32; 2]) -> bool {
    let [x, y, width, height] = rect;
    let [surface_width, surface_height] = surface;
    rect.into_iter().chain(surface).all(f32::is_finite)
        && surface_width > 0.0
        && surface_height > 0.0
        && width > 0.0
        && height > 0.0
        && x <= 0.0
        && y <= 0.0
        && x + width >= surface_width
        && y + height >= surface_height
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

/// The grid rows of a selection that are actually on screen.
///
/// Selection endpoints are grid-absolute and a selection can cover the entire
/// scrollback: `Ctrl+A` in a pane holding a million lines of build output is one
/// gesture. Walking `start..=end` and skipping the offscreen rows inside the
/// loop cost a million iterations on EVERY repaint — every blink, every
/// keystroke — to draw at most `screen_lines` quads. Clamping first makes the
/// work proportional to what is drawn.
///
/// Returns an empty range when the selection is entirely off screen, which is
/// the ordinary case while scrolled away from it.
fn visible_selection_rows(
    start_line: i32,
    end_line: i32,
    display_offset: i32,
    screen_rows: i32,
) -> std::ops::RangeInclusive<i32> {
    // `vrow = r + display_offset` must land in `0..screen_rows`.
    let first = start_line.max(-display_offset);
    let last = end_line.min(screen_rows - display_offset - 1);
    first..=last
}

/// The inclusive `(first_col, last_col)` the mouse selection highlights on grid
/// row `r`. A **block** (Alt+drag) selection is a column rectangle: every row
/// spans the same `min(start_col, end_col)..=max(start_col, end_col)`, matching
/// the rectangular text that's copied. A **linear** selection wraps full lines:
/// the start row begins at the anchor and runs to `cols-1`, interior rows span
/// the whole width, and the end row runs from column 0 to the cursor.
///
/// Splitting this out of the inline `build_pane` loop is what makes the
/// block/linear distinction unit-testable: the highlight quad must match the
/// copied text, and a block selection drawn with the linear (full-line) spans
/// highlighted cells the copy never includes.
fn selection_row_span(
    r: i32,
    start: (i32, usize),
    end: (i32, usize),
    cols: usize,
    is_block: bool,
) -> (usize, usize) {
    if is_block {
        return (start.1.min(end.1), start.1.max(end.1));
    }
    let last_col = cols.saturating_sub(1);
    if start.0 == end.0 {
        (start.1, end.1)
    } else if r == start.0 {
        (start.1, last_col)
    } else if r == end.0 {
        (0, end.1)
    } else {
        (0, last_col)
    }
}

/// Attrs for one style run: the family picks the bold/italic variant
/// (`cfg.family_for`), the color is the run's resolved fg, weight/style
/// mirror the SGR bold/italic bits. Split out of the retired whole-buffer
/// `build_rich_spans` so the v2.20.0 P1 per-line shaping cache
/// can build a single row's `AttrsList` at a time — runs that didn't change
/// never construct an `Attrs` at all.
fn run_attrs<'a>(
    cfg: &'a Config,
    ff: &FontFeatures,
    fg: Rgb,
    bold: bool,
    italic: bool,
) -> Attrs<'a> {
    let mut a = Attrs::new()
        .family(Family::Name(cfg.family_for(bold, italic)))
        .font_features(ff.clone())
        .color(GColor::rgb(fg.r, fg.g, fg.b));
    if bold {
        a = a.weight(Weight::BOLD);
    }
    if italic {
        a = a.style(Style::Italic);
    }
    a
}

/// Truncate `s` to at most `n` **display columns** (not chars), adding `…`
/// when something was cut. CJK characters and emoji are wide (2 cells
/// each), so a char-count truncation overflows the tab segment / title
/// when these are present; this honors the cell width that the renderer
/// Pick the per-pane titlebar background
/// color from the focus / broadcast state.
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
/// Receive (broadcast) and inactive now ALSO derive from the theme
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
    // Multi-window: the already-resolved per-window accent (the live
    // renderer passes its `ui_accent`; tests and any cfg-only caller pass
    // `cfg.resolved_accent(theme)`).
    accent: Rgb,
    focused: bool,
    broadcast: bool,
) -> Rgb {
    if focused {
        cfg.title_transmit_bg_color
            .or(cfg.focused_split_color)
            .unwrap_or(accent)
    } else if broadcast {
        cfg.title_receive_bg_color.unwrap_or(accent)
    } else {
        cfg.title_inactive_bg_color.unwrap_or(theme.palette[8])
    }
}

/// v2.23.0: resolve the opaque fill color for the window chrome strips (tab
/// bar, status bar, new-tab button). Without a wallpaper, or with
/// `chrome-background = theme`, this is the theme's chrome color (`palette[8]`)
/// — identical to the pre-2.23.0 look. With a wallpaper, the other modes let
/// the chrome read deliberately against the moving background:
///   - `black` / `white`: a fixed neutral panel.
///   - `auto`: the wallpaper's average color, nudged toward black/white only as
///     far as needed to keep the (theme-colored) tab text readable on it
///     (`with_min_contrast` against `theme.foreground`, 3:1). Falls back to the
///     theme color if no frame has been sampled yet.
///
/// `bg_avg` is `Some` only when a wallpaper frame was sampled this frame for
/// `auto`. Pure so the mapping is unit-tested without a GPU.
pub(crate) fn resolve_chrome_bg(
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
    bg_avg: Option<Rgb>,
) -> Rgb {
    use kettle_config::{BackgroundType, ChromeBackground};
    // Only a wallpaper (image or starfield) changes the chrome color; otherwise
    // theme as before. (Over a pure-black starfield, `auto` keeps the chrome
    // black — black already clears the 3:1 contrast bar against a light fg.)
    if !matches!(
        cfg.background_type,
        BackgroundType::Image | BackgroundType::Starfield
    ) {
        return theme.palette[8];
    }
    match cfg.chrome_background {
        ChromeBackground::Theme => theme.palette[8],
        ChromeBackground::Black => Rgb::new(0, 0, 0),
        ChromeBackground::White => Rgb::new(255, 255, 255),
        ChromeBackground::Auto => {
            // The starfield has no decoded frame to sample; it's a black sky, so
            // treat its average as black (→ a seamless dark bar after the
            // contrast nudge) rather than falling back to the theme color.
            let avg = bg_avg.or_else(|| {
                matches!(cfg.background_type, BackgroundType::Starfield)
                    .then_some(Rgb::new(0, 0, 0))
            });
            match avg {
                Some(avg) => color::with_min_contrast(avg, theme.foreground, 3.0),
                None => theme.palette[8],
            }
        }
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

/// Build glyphon [`Metrics`] for a *logical* `font_size` at a given
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

/// Metrics for a terminal PANE's text buffer. The glyph size stays
/// `font_size` (the DPI-scaled px from `metrics_for`), but the LINE HEIGHT is
/// the grid's actual row height `cell_h` — which already folds in the 1.25
/// line-height ratio AND any `cfg.cell_height` multiplier. The
/// cursor and selection/vi quads step by `cell_h` per row (`by = oy + line *
/// ch`), so the text must advance by the same `cell_h`; laying it out at the
/// unscaled `metrics.line_height` instead drifts a fraction of a row per line —
/// a full row off near the bottom of a tall window whenever `cell_height != 1`.
pub fn pane_metrics(font_size: f32, cell_h: f32) -> Metrics {
    Metrics::new(font_size, cell_h)
}

/// Map the user-facing `gpu-power-preference` onto wgpu's adapter selector.
fn power_preference_of(pref: kettle_config::GpuPowerPreference) -> wgpu::PowerPreference {
    match pref {
        kettle_config::GpuPowerPreference::Low => wgpu::PowerPreference::LowPower,
        kettle_config::GpuPowerPreference::High => wgpu::PowerPreference::HighPerformance,
        kettle_config::GpuPowerPreference::Auto => wgpu::PowerPreference::None,
    }
}

/// Headless all-backend instance used by detection and cross-backend policy.
fn gpu_instance() -> wgpu::Instance {
    gpu_instance_for_backends(wgpu::Backends::all())
}

fn gpu_instance_for_backends(backends: wgpu::Backends) -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    wgpu::Instance::new(descriptor)
}

trait GpuDisplayHandleOwner: HasDisplayHandle + Send + Sync + 'static {}

impl<T> GpuDisplayHandleOwner for T where T: HasDisplayHandle + Send + Sync + 'static {}

/// Cloneable, type-erased owner of the platform display connection.
///
/// Kettle supplies winit's `OwnedDisplayHandle`, which owns only the event-loop
/// display connection. It deliberately does not own a window: the process-wide
/// [`GpuContext`] can therefore outlive window 1 without preventing that
/// window's native resources from being destroyed.
#[derive(Clone)]
struct OwnedGpuDisplayHandle(Arc<dyn GpuDisplayHandleOwner>);

impl OwnedGpuDisplayHandle {
    fn new<D>(display_handle: D) -> Self
    where
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self(Arc::new(display_handle))
    }
}

impl std::fmt::Debug for OwnedGpuDisplayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedGpuDisplayHandle")
    }
}

impl HasDisplayHandle for OwnedGpuDisplayHandle {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

/// Kettle's live path retains the display handle required by wgpu's GLES
/// backend, notably for Wayland presentation. Compatibility constructors that
/// were given only a window pass `None`; headless diagnostics intentionally use
/// [`gpu_instance_for_backends`] instead.
fn gpu_instance_for_live_display(
    display_handle: Option<&OwnedGpuDisplayHandle>,
    backends: wgpu::Backends,
) -> wgpu::Instance {
    let mut descriptor = match display_handle {
        Some(display_handle) => {
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display_handle.clone()))
        }
        None => wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    descriptor.backends = backends;
    wgpu::Instance::new(descriptor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterEscalation {
    Preferred,
    AlternateBackend,
    SurfacePreferred,
    AnyHardware,
    ForceSoftware,
}

/// Adapter-selection state grouped for display-handle-aware renderer rebuilds.
///
/// The existing [`Renderer::new_with_escalation`] signature remains available
/// for compatibility; new winit integrations pass this value alongside an
/// event-loop-owned display handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterSelection {
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
}

impl AdapterSelection {
    pub const fn new(escalation: AdapterEscalation, avoid: Option<GpuAdapterKey>) -> Self {
        Self { escalation, avoid }
    }
}

pub fn escalation_for_attempt(attempt: u32) -> AdapterEscalation {
    match attempt {
        0 => AdapterEscalation::AlternateBackend,
        1 => AdapterEscalation::SurfacePreferred,
        2 => AdapterEscalation::AnyHardware,
        _ => AdapterEscalation::ForceSoftware,
    }
}

/// v2.23.0: a detected GPU adapter, described in kettle's own vocabulary so
/// kettle-ui (the settings GPU picker) never has to name a `wgpu` type. Carries
/// the PCI `(vendor, device)` pair the config pins on, the human display name,
/// and string-ized `kind` / `backend` for the settings list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuAdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    /// "Discrete" | "Integrated" | "Virtual" | "Software" | "Other".
    pub kind: &'static str,
    /// "DX12" | "Vulkan" | "Metal" | "GL" | "Other".
    pub backend: &'static str,
}

/// Backend-aware identity of a live adapter. Recovery must distinguish two
/// backends exposing the same physical GPU: after a driver failure, retrying
/// the exact `(vendor, device, backend)` is different from rebuilding that GPU
/// through an alternate backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuAdapterKey {
    vendor: u32,
    device: u32,
    backend: wgpu::Backend,
}

impl GpuAdapterKey {
    fn from_info(info: &wgpu::AdapterInfo) -> Self {
        Self {
            vendor: info.vendor,
            device: info.device,
            backend: info.backend,
        }
    }

    fn same_physical(self, info: &wgpu::AdapterInfo) -> bool {
        self.vendor == info.vendor && self.device == info.device
    }
}

fn device_kind_str(t: wgpu::DeviceType) -> &'static str {
    match t {
        wgpu::DeviceType::DiscreteGpu => "Discrete",
        wgpu::DeviceType::IntegratedGpu => "Integrated",
        wgpu::DeviceType::VirtualGpu => "Virtual",
        wgpu::DeviceType::Cpu => "Software",
        _ => "Other",
    }
}

fn backend_str(b: wgpu::Backend) -> &'static str {
    match b {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DX12",
        wgpu::Backend::Gl => "GL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        _ => "Other",
    }
}

fn configured_backend(b: kettle_config::GpuBackend) -> Option<wgpu::Backend> {
    use kettle_config::GpuBackend;
    match b {
        GpuBackend::Auto => None,
        GpuBackend::Dx12 => Some(wgpu::Backend::Dx12),
        GpuBackend::Vulkan => Some(wgpu::Backend::Vulkan),
        GpuBackend::Metal => Some(wgpu::Backend::Metal),
        GpuBackend::Gl => Some(wgpu::Backend::Gl),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendPlatform {
    Windows,
    MacOs,
    Other,
}

const fn host_backend_platform() -> BackendPlatform {
    if cfg!(target_os = "windows") {
        BackendPlatform::Windows
    } else if cfg!(target_os = "macos") {
        BackendPlatform::MacOs
    } else {
        BackendPlatform::Other
    }
}

const WINDOWS_BACKEND_ORDER: &[wgpu::Backend] = &[
    wgpu::Backend::Dx12,
    wgpu::Backend::Vulkan,
    wgpu::Backend::Gl,
];
const MACOS_BACKEND_ORDER: &[wgpu::Backend] = &[
    wgpu::Backend::Metal,
    wgpu::Backend::Vulkan,
    wgpu::Backend::Gl,
];
const OTHER_BACKEND_ORDER: &[wgpu::Backend] = &[wgpu::Backend::Vulkan, wgpu::Backend::Gl];

const fn native_backend_order_for(platform: BackendPlatform) -> &'static [wgpu::Backend] {
    match platform {
        BackendPlatform::Windows => WINDOWS_BACKEND_ORDER,
        BackendPlatform::MacOs => MACOS_BACKEND_ORDER,
        BackendPlatform::Other => OTHER_BACKEND_ORDER,
    }
}

fn backend_attempt_order_for(
    platform: BackendPlatform,
    requested: Option<wgpu::Backend>,
) -> Vec<wgpu::Backend> {
    let mut order = Vec::with_capacity(native_backend_order_for(platform).len() + 1);
    if let Some(requested) = requested {
        order.push(requested);
    }
    order.extend(
        native_backend_order_for(platform)
            .iter()
            .copied()
            .filter(|backend| Some(*backend) != requested),
    );
    order
}

fn backend_attempt_order(cfg: &Config) -> Vec<wgpu::Backend> {
    backend_attempt_order_for(host_backend_platform(), configured_backend(cfg.gpu_backend))
}

fn backend_mask(backend: wgpu::Backend) -> wgpu::Backends {
    match backend {
        wgpu::Backend::Vulkan => wgpu::Backends::VULKAN,
        wgpu::Backend::Metal => wgpu::Backends::METAL,
        wgpu::Backend::Dx12 => wgpu::Backends::DX12,
        wgpu::Backend::Gl => wgpu::Backends::GL,
        wgpu::Backend::BrowserWebGpu => wgpu::Backends::BROWSER_WEBGPU,
        _ => wgpu::Backends::empty(),
    }
}

/// Stable native-first backend order. In particular, Windows Auto deliberately
/// chooses DX12 before Vulkan: this avoids depending on wgpu enumeration order
/// and avoids a cross-vendor Vulkan ICD becoming the accidental default.
const fn backend_rank_for(platform: BackendPlatform, backend: wgpu::Backend) -> u8 {
    match platform {
        BackendPlatform::Windows => match backend {
            wgpu::Backend::Dx12 => 0,
            wgpu::Backend::Vulkan => 1,
            wgpu::Backend::Gl => 2,
            _ => 3,
        },
        BackendPlatform::MacOs => match backend {
            wgpu::Backend::Metal => 0,
            wgpu::Backend::Vulkan => 1,
            wgpu::Backend::Gl => 2,
            _ => 3,
        },
        BackendPlatform::Other => match backend {
            wgpu::Backend::Vulkan => 0,
            wgpu::Backend::Gl => 1,
            _ => 2,
        },
    }
}

fn backend_rank(backend: wgpu::Backend) -> u8 {
    backend_rank_for(host_backend_platform(), backend)
}

fn device_rank(device_type: wgpu::DeviceType, preference: kettle_config::GpuPowerPreference) -> u8 {
    use kettle_config::GpuPowerPreference;
    match preference {
        GpuPowerPreference::High => match device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
            _ => 4,
        },
        GpuPowerPreference::Low => match device_type {
            wgpu::DeviceType::IntegratedGpu => 0,
            wgpu::DeviceType::DiscreteGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
            _ => 4,
        },
        GpuPowerPreference::Auto => match device_type {
            wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::VirtualGpu => 1,
            wgpu::DeviceType::Cpu => 2,
            _ => 3,
        },
    }
}

fn adapter_priority_for(
    platform: BackendPlatform,
    backend: wgpu::Backend,
    device_type: wgpu::DeviceType,
    preference: kettle_config::GpuPowerPreference,
    preferred: bool,
) -> (u8, u8, u8) {
    use kettle_config::GpuPowerPreference;
    let backend = backend_rank_for(platform, backend);
    let device = device_rank(device_type, preference);
    let not_preferred = u8::from(!preferred);
    match preference {
        // Auto is the native-backend policy. Within one backend, wgpu's
        // surface-preferred physical adapter breaks otherwise equal hardware.
        GpuPowerPreference::Auto => (backend, device, not_preferred),
        // Explicit low/high is a physical-GPU request and must win even when
        // that GPU is available only through a lower-ranked backend. When
        // several adapters have the requested class, preserve wgpu/the
        // platform's physical-adapter preference before choosing a backend.
        GpuPowerPreference::Low | GpuPowerPreference::High => (device, not_preferred, backend),
    }
}

fn has_gpu_pin(cfg: &Config) -> bool {
    (cfg.gpu_vendor_id != 0 && cfg.gpu_device_id != 0) || !cfg.gpu_name.trim().is_empty()
}

fn can_probe_native_backend_directly(cfg: &Config) -> bool {
    !has_gpu_pin(cfg) && cfg.gpu_power_preference == kettle_config::GpuPowerPreference::Auto
}

fn should_query_preferred_adapter(escalation: AdapterEscalation, force_software: bool) -> bool {
    !force_software
        && matches!(
            escalation,
            AdapterEscalation::Preferred
                | AdapterEscalation::SurfacePreferred
                | AdapterEscalation::AnyHardware
        )
}

fn same_physical(a: &wgpu::AdapterInfo, b: GpuAdapterKey) -> bool {
    b.same_physical(a)
}

fn stable_adapter_cmp(a: &wgpu::AdapterInfo, b: &wgpu::AdapterInfo) -> std::cmp::Ordering {
    (
        backend_rank(a.backend),
        a.vendor,
        a.device,
        a.name.to_ascii_lowercase(),
    )
        .cmp(&(
            backend_rank(b.backend),
            b.vendor,
            b.device,
            b.name.to_ascii_lowercase(),
        ))
}

/// Enumerate the machine's GPU adapters across every backend, de-duplicated by
/// `(vendor, device, name)` so the same physical GPU exposed under multiple
/// backends (e.g. DX12 *and* Vulkan on Windows) shows once in the picker. Sort
/// first so the picker records the deterministic native backend, not whichever
/// backend the driver happened to enumerate first.
pub async fn enumerate_adapter_infos(instance: &wgpu::Instance) -> Vec<GpuAdapterInfo> {
    let mut seen: std::collections::HashSet<(u32, u32, String)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut adapters = instance.enumerate_adapters(wgpu::Backends::all()).await;
    adapters.sort_by(|a, b| stable_adapter_cmp(&a.get_info(), &b.get_info()));
    for a in adapters {
        let info = a.get_info();
        if !seen.insert((info.vendor, info.device, info.name.clone())) {
            continue;
        }
        out.push(GpuAdapterInfo {
            name: info.name,
            vendor: info.vendor,
            device: info.device,
            kind: device_kind_str(info.device_type),
            backend: backend_str(info.backend),
        });
    }
    out
}

/// Convenience wrapper for kettle-ui: spin up a throwaway `wgpu::Instance` and
/// return the detected GPUs. Keeps `wgpu` out of the UI crate's vocabulary and
/// stays synchronous for the settings code (blocks on the async enumeration —
/// a one-shot, off the render hot path).
pub fn detect_gpus() -> Vec<GpuAdapterInfo> {
    pollster::block_on(enumerate_adapter_infos(&gpu_instance()))
}

fn pin_match_rank_fields(vendor: u32, device: u32, name: &str, cfg: &Config) -> u8 {
    if cfg.gpu_vendor_id != 0
        && cfg.gpu_device_id != 0
        && vendor == cfg.gpu_vendor_id
        && device == cfg.gpu_device_id
    {
        return 0;
    }
    let wanted = cfg.gpu_name.trim().to_ascii_lowercase();
    if !wanted.is_empty() {
        let actual = name.to_ascii_lowercase();
        if actual == wanted {
            return 1;
        }
        if actual.contains(&wanted) {
            return 2;
        }
    }
    u8::MAX
}

fn pin_match_rank(info: &wgpu::AdapterInfo, cfg: &Config) -> u8 {
    pin_match_rank_fields(info.vendor, info.device, &info.name, cfg)
}

fn take_best(
    mut adapters: Vec<wgpu::Adapter>,
    cfg: &Config,
    preferred: Option<GpuAdapterKey>,
) -> Option<wgpu::Adapter> {
    adapters.sort_by(|a, b| {
        let a = a.get_info();
        let b = b.get_info();
        let a_preferred = preferred.is_some_and(|p| same_physical(&a, p));
        let b_preferred = preferred.is_some_and(|p| same_physical(&b, p));
        adapter_priority_for(
            host_backend_platform(),
            a.backend,
            a.device_type,
            cfg.gpu_power_preference,
            a_preferred,
        )
        .cmp(&adapter_priority_for(
            host_backend_platform(),
            b.backend,
            b.device_type,
            cfg.gpu_power_preference,
            b_preferred,
        ))
        .then_with(|| stable_adapter_cmp(&a, &b))
    });
    adapters.into_iter().next()
}

fn effective_backend(
    requested: Option<wgpu::Backend>,
    available: &[wgpu::Backend],
) -> Option<wgpu::Backend> {
    requested.filter(|backend| available.contains(backend))
}

fn apply_backend_policy(
    adapters: Vec<wgpu::Adapter>,
    requested: Option<wgpu::Backend>,
    context: &str,
) -> Vec<wgpu::Adapter> {
    let Some(requested) = requested else {
        return adapters;
    };
    let available: Vec<_> = adapters
        .iter()
        .map(|adapter| adapter.get_info().backend)
        .collect();
    if effective_backend(Some(requested), &available).is_some() {
        return adapters
            .into_iter()
            .filter(|adapter| adapter.get_info().backend == requested)
            .collect();
    }
    log::warn!(
        "{context}: requested GPU backend {} is unavailable; falling back to the native backend order",
        backend_str(requested)
    );
    adapters
}

fn take_pinned(
    adapters: &[wgpu::Adapter],
    cfg: &Config,
    requested: Option<wgpu::Backend>,
    preferred: Option<GpuAdapterKey>,
    context: &str,
) -> Option<wgpu::Adapter> {
    if !has_gpu_pin(cfg) {
        return None;
    }
    let pin_rank = adapters
        .iter()
        .map(|adapter| pin_match_rank(&adapter.get_info(), cfg))
        .min()
        .unwrap_or(u8::MAX);
    if pin_rank == u8::MAX {
        return None;
    }
    let pinned: Vec<_> = adapters
        .iter()
        .filter(|adapter| pin_match_rank(&adapter.get_info(), cfg) == pin_rank)
        .cloned()
        .collect();
    take_best(
        apply_backend_policy(pinned, requested, context),
        cfg,
        preferred,
    )
}

async fn select_software_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    adapters: Vec<wgpu::Adapter>,
    cfg: &Config,
    preferred: Option<GpuAdapterKey>,
    requested: Option<wgpu::Backend>,
    context: &str,
) -> Result<wgpu::Adapter> {
    let software = apply_backend_policy(adapters, requested, context);
    let chosen = if let Some(chosen) = take_pinned(&software, cfg, None, preferred, context) {
        chosen
    } else if let Some(chosen) = take_best(software, cfg, preferred) {
        chosen
    } else {
        let chosen = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_preference_of(cfg.gpu_power_preference),
                compatible_surface: surface,
                force_fallback_adapter: true,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|error| anyhow!("{context}: software adapter unavailable: {error:?}"))?;
        let info = chosen.get_info();
        if requested.is_some_and(|backend| backend != info.backend) {
            log::warn!(
                "{context}: requested software backend {} is unavailable; using {}",
                requested.map(backend_str).unwrap_or("Auto"),
                backend_str(info.backend)
            );
        }
        chosen
    };
    let info = chosen.get_info();
    log::warn!(
        "{context}: using software adapter {} ({})",
        info.name,
        backend_str(info.backend)
    );
    Ok(chosen)
}

async fn preferred_adapter_key(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
) -> Option<GpuAdapterKey> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: power_preference_of(cfg.gpu_power_preference),
            compatible_surface: surface,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })
        .await
        .ok()
        .map(|adapter| GpuAdapterKey::from_info(&adapter.get_info()))
}

fn log_direct_adapter_choice(context: &str, cfg: &Config, info: &wgpu::AdapterInfo) {
    if let Some(requested) = configured_backend(cfg.gpu_backend)
        && requested != info.backend
    {
        log::warn!(
            "{context}: requested GPU backend {} is unavailable; using {}",
            backend_str(requested),
            backend_str(info.backend)
        );
    }
    let software = info.device_type == wgpu::DeviceType::Cpu;
    if software {
        log::warn!(
            "{context}: using software adapter {} ({})",
            info.name,
            backend_str(info.backend)
        );
    } else {
        log::info!(
            "{context}: selected GPU {} ({}, {})",
            info.name,
            device_kind_str(info.device_type),
            backend_str(info.backend)
        );
    }
}

async fn request_direct_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
    force_fallback_adapter: bool,
) -> Result<wgpu::Adapter> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: power_preference_of(cfg.gpu_power_preference),
            compatible_surface: surface,
            force_fallback_adapter,
            apply_limit_buckets: false,
        })
        .await
        .map_err(|error| anyhow!("{error:?}"))
}

fn direct_probe_passes(force_software: bool) -> &'static [bool] {
    if force_software {
        &[true]
    } else {
        &[false, true]
    }
}

/// Fast startup for the common unpinned Auto policy. Each instance enables one
/// backend, so a successful native request is one adapter discovery and does
/// not initialize lower-priority ICDs. Hardware across every backend is tried
/// before software fallback.
async fn resolve_window_adapter<W>(
    window: Arc<W>,
    display_handle: Option<&OwnedGpuDisplayHandle>,
    cfg: &Config,
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    context: &str,
) -> Result<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter)>
where
    W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
{
    if escalation == AdapterEscalation::Preferred && can_probe_native_backend_directly(cfg) {
        let mut failures = Vec::new();
        for &software_pass in direct_probe_passes(cfg.gpu_force_software) {
            for backend in backend_attempt_order(cfg) {
                let instance = gpu_instance_for_live_display(display_handle, backend_mask(backend));
                let surface = match instance.create_surface(window.clone()) {
                    Ok(surface) => surface,
                    Err(error) => {
                        failures.push(format!("{} surface: {error}", backend_str(backend)));
                        continue;
                    }
                };
                match request_direct_adapter(&instance, Some(&surface), cfg, software_pass).await {
                    Ok(adapter)
                        if !software_pass
                            && adapter.get_info().device_type == wgpu::DeviceType::Cpu =>
                    {
                        failures.push(format!(
                            "{} hardware pass returned a software adapter",
                            backend_str(backend)
                        ));
                    }
                    Ok(adapter) => {
                        log_direct_adapter_choice(context, cfg, &adapter.get_info());
                        return Ok((instance, surface, adapter));
                    }
                    Err(error) => {
                        failures.push(format!("{}: {error}", backend_str(backend)));
                    }
                }
            }
            if !software_pass {
                log::warn!(
                    "{context}: no hardware GPU adapter in native backend order; \
                     trying software fallback"
                );
            }
        }
        return Err(anyhow!(
            "{context}: no usable GPU adapter in native backend order: {}",
            failures.join("; ")
        ));
    }

    // Pins and low/high preference need a cross-backend view. Recovery also
    // inspects alternate backends and physical GPUs deliberately.
    let instance = gpu_instance_for_live_display(display_handle, wgpu::Backends::all());
    let surface = instance.create_surface(window)?;
    let adapter =
        resolve_adapter(&instance, Some(&surface), cfg, escalation, avoid, context).await?;
    Ok((instance, surface, adapter))
}

async fn resolve_headless_adapter(
    cfg: &Config,
    context: &str,
) -> Result<(wgpu::Instance, wgpu::Adapter)> {
    if can_probe_native_backend_directly(cfg) {
        let mut failures = Vec::new();
        for &software_pass in direct_probe_passes(cfg.gpu_force_software) {
            for backend in backend_attempt_order(cfg) {
                let instance = gpu_instance_for_backends(backend_mask(backend));
                match request_direct_adapter(&instance, None, cfg, software_pass).await {
                    Ok(adapter)
                        if !software_pass
                            && adapter.get_info().device_type == wgpu::DeviceType::Cpu =>
                    {
                        failures.push(format!(
                            "{} hardware pass returned a software adapter",
                            backend_str(backend)
                        ));
                    }
                    Ok(adapter) => {
                        log_direct_adapter_choice(context, cfg, &adapter.get_info());
                        return Ok((instance, adapter));
                    }
                    Err(error) => {
                        failures.push(format!("{}: {error}", backend_str(backend)));
                    }
                }
            }
            if !software_pass {
                log::warn!(
                    "{context}: no hardware GPU adapter in native backend order; \
                     trying software fallback"
                );
            }
        }
        return Err(anyhow!(
            "{context}: no usable GPU adapter in native backend order: {}",
            failures.join("; ")
        ));
    }

    let instance = gpu_instance();
    let adapter = resolve_adapter(
        &instance,
        None,
        cfg,
        AdapterEscalation::Preferred,
        None,
        context,
    )
    .await?;
    Ok((instance, adapter))
}

/// Resolve every live, diagnostic, screenshot, self-test and recovery adapter
/// through the same deterministic policy. Explicit backend selection works
/// with or without a physical GPU pin. An unavailable explicit backend is
/// observable and falls back to native order rather than making Kettle fail to
/// start. Initial startup can fall back to software; recovery advances through
/// explicit stages so a failed backend is not silently retried forever.
async fn resolve_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    context: &str,
) -> Result<wgpu::Adapter> {
    let requested = configured_backend(cfg.gpu_backend);
    let force_software = cfg.gpu_force_software || escalation == AdapterEscalation::ForceSoftware;
    let preferred = if should_query_preferred_adapter(escalation, force_software) {
        preferred_adapter_key(instance, surface, cfg).await
    } else {
        None
    };
    let mut candidates: Vec<wgpu::Adapter> = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .filter(|adapter| surface.is_none_or(|s| adapter.is_surface_supported(s)))
        .collect();

    // Resolve a visible settings/config pin before separating hardware from
    // software. CPU adapters such as llvmpipe/lavapipe are valid explicit
    // choices and were historically selectable in the GPU device picker.
    if escalation == AdapterEscalation::Preferred && !force_software && has_gpu_pin(cfg) {
        if let Some(chosen) = take_pinned(&candidates, cfg, requested, preferred, context) {
            let info = chosen.get_info();
            log::info!(
                "{context}: using pinned GPU {} ({}, {})",
                info.name,
                device_kind_str(info.device_type),
                backend_str(info.backend)
            );
            return Ok(chosen);
        }
        log::warn!(
            "{context}: pinned GPU (vendor={:#06x} device={:#06x} name={:?}) not found among \
             {} surface-capable adapter(s); falling back to gpu-power-preference",
            cfg.gpu_vendor_id,
            cfg.gpu_device_id,
            cfg.gpu_name,
            candidates.len()
        );
    }

    let software_candidates: Vec<_> = candidates
        .iter()
        .filter(|adapter| adapter.get_info().device_type == wgpu::DeviceType::Cpu)
        .cloned()
        .collect();

    if force_software {
        return select_software_adapter(
            instance,
            surface,
            software_candidates,
            cfg,
            preferred,
            requested,
            context,
        )
        .await;
    }

    candidates.retain(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu);
    if candidates.is_empty() {
        if escalation != AdapterEscalation::Preferred {
            return Err(anyhow!("{context}: no hardware GPU adapter available"));
        }
        log::warn!(
            "{context}: no hardware GPU adapter; retrying with software fallback \
             (llvmpipe / lavapipe / WARP)"
        );
        return select_software_adapter(
            instance,
            surface,
            software_candidates,
            cfg,
            preferred,
            requested,
            context,
        )
        .await;
    }

    if escalation == AdapterEscalation::AlternateBackend {
        let Some(avoid) = avoid else {
            return Err(anyhow!(
                "{context}: alternate-backend recovery has no failed adapter identity"
            ));
        };
        let alternate: Vec<_> = candidates
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();
                avoid.same_physical(&info) && info.backend != avoid.backend
            })
            .collect();
        let chosen = take_best(alternate, cfg, preferred).ok_or_else(|| {
            anyhow!("{context}: no alternate backend is available for the failed physical GPU")
        })?;
        let info = chosen.get_info();
        log::warn!(
            "{context}: recovery selected alternate backend {} on {}",
            backend_str(info.backend),
            info.name
        );
        return Ok(chosen);
    }

    if escalation == AdapterEscalation::SurfacePreferred {
        let Some(preferred) = preferred else {
            return Err(anyhow!(
                "{context}: platform did not report a surface-preferred hardware adapter"
            ));
        };
        let surface_preferred: Vec<_> = candidates
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();
                preferred.same_physical(&info)
                    && avoid.is_none_or(|failed| !failed.same_physical(&info))
            })
            .collect();
        let chosen = take_best(surface_preferred, cfg, Some(preferred)).ok_or_else(|| {
            anyhow!("{context}: no non-failed backend is available on the surface-preferred GPU")
        })?;
        let info = chosen.get_info();
        log::warn!(
            "{context}: recovery selected surface-preferred GPU {} ({})",
            info.name,
            backend_str(info.backend)
        );
        return Ok(chosen);
    }

    if escalation == AdapterEscalation::AnyHardware {
        let other: Vec<_> = candidates
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();
                avoid.is_none_or(|failed| !failed.same_physical(&info))
            })
            .collect();
        let chosen = take_best(other, cfg, preferred)
            .ok_or_else(|| anyhow!("{context}: no alternate physical hardware GPU is available"))?;
        let info = chosen.get_info();
        log::warn!(
            "{context}: recovery selected alternate physical GPU {} ({})",
            info.name,
            backend_str(info.backend)
        );
        return Ok(chosen);
    }

    let candidates = apply_backend_policy(candidates, requested, context);
    if let Some(chosen) = take_best(candidates, cfg, preferred) {
        let info = chosen.get_info();
        log::info!(
            "{context}: selected GPU {} ({}, {})",
            info.name,
            device_kind_str(info.device_type),
            backend_str(info.backend)
        );
        return Ok(chosen);
    }

    log::warn!(
        "{context}: no hardware GPU adapter; retrying with software fallback \
         (llvmpipe / lavapipe / WARP)"
    );
    select_software_adapter(
        instance,
        surface,
        software_candidates,
        cfg,
        preferred,
        requested,
        context,
    )
    .await
}

/// Number of header display-lines before the field rows in the
/// settings panel (title, category tabs, blank). The focused-row highlight
/// quad and the per-line text areas both index off this.
const SETTINGS_FIELD_START: usize = 3;

/// Build the settings panel's display lines from its renderer-side
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
    // v2.20.0: advertise the vim keys when `vim-menu-nav` is on.
    lines.push(if set.vim_nav {
        "↑↓/jk field    ←→/hl change    g/G ends    Tab category    Esc close".to_string()
    } else {
        "↑↓ field    ←→ change    Tab category    Esc close".to_string()
    });
    // v2.23.0: contextual note (e.g. the Graphics "Active GPU … • restart to
    // apply" line). Appended last so it never shifts the focused-row highlight.
    if let Some(note) = &set.footer_note {
        lines.push(note.clone());
    }
    lines
}

/// The settings panel's width in character cells — the widest
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

/// What a click on the settings overlay landed on (v2.24.0 mouse control). The
/// geometry is recomputed here from the SAME inputs the draw uses
/// (`settings_display_lines` + the panel math in `render_frame_with_status`),
/// so the hit-test can't drift from what's painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    /// Outside the panel entirely — dismiss the overlay.
    Outside,
    /// Inside the panel but not on an actionable row (title / blank / footer).
    Inert,
    /// The category-tab strip — switch to this category index.
    Category(usize),
    /// A field row — index into the active category's fields.
    Field(usize),
}

/// Map a cursor position to a [`SettingsHit`]. Pure (no GPU) so the row/tab
/// mapping is unit-tested. `cell_w`/`cell_h` are the renderer's cell metrics and
/// `surface_w`/`surface_h` the surface size — exactly the values the draw uses.
pub fn settings_hit_test(
    set: &SettingsOverlay,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    cursor_x: f32,
    cursor_y: f32,
) -> SettingsHit {
    let (cw, ch, sw, sh) = (cell_w, cell_h, surface_w, surface_h);
    let lines = settings_display_lines(set);
    let row_h = ch + 6.0;
    let panel_w = (settings_panel_cols(&lines) * cw + 48.0).min((sw - 40.0).max(120.0));
    let panel_h = (lines.len() as f32 * row_h + 24.0).min((sh - 40.0).max(80.0));
    let px = ((sw - panel_w) * 0.5).max(0.0);
    let py = ((sh - panel_h) * 0.5).max(0.0);
    if cursor_x < px || cursor_x >= px + panel_w || cursor_y < py || cursor_y >= py + panel_h {
        return SettingsHit::Outside;
    }
    // Rows are laid out from `py + 12.0`, each `row_h` tall (mirrors the draw).
    let rel = cursor_y - (py + 12.0);
    if rel < 0.0 {
        return SettingsHit::Inert;
    }
    let line = (rel / row_h) as usize;
    if line >= lines.len() {
        return SettingsHit::Inert;
    }
    // Line 1 is the category-tab strip.
    if line == 1 {
        let text_left = px + 16.0;
        let mut col = 0usize;
        for (i, c) in set.categories.iter().enumerate() {
            let seg = if i == set.active_category {
                format!("[ {c} ]")
            } else {
                format!("  {c}  ")
            };
            let w = display_width(&seg);
            let start = text_left + col as f32 * cw;
            let end = text_left + (col + w) as f32 * cw;
            if cursor_x >= start && cursor_x < end {
                return SettingsHit::Category(i);
            }
            col += w + 1; // + the joining space
        }
        return SettingsHit::Inert;
    }
    // Field rows: SETTINGS_FIELD_START .. SETTINGS_FIELD_START + rows.len().
    if line >= SETTINGS_FIELD_START && line < SETTINGS_FIELD_START + set.rows.len() {
        return SettingsHit::Field(line - SETTINGS_FIELD_START);
    }
    SettingsHit::Inert
}

/// Compute the responsive, bottom-reserved search lane.
///
/// Wide surfaces use one row. Narrow surfaces keep the editor and Close on the
/// first row and wrap every other control onto as many rows as needed. Nothing
/// is silently omitted, which gives keyboard, pointer, and accessibility users
/// the same control set at every supported window size.
pub fn search_bar_geometry(
    surface_width: f32,
    surface_height: f32,
    cell_width: f32,
    cell_height: f32,
) -> SearchBarGeometry {
    let zero = (0.0, 0.0, 0.0, 0.0);
    if !surface_width.is_finite()
        || !surface_height.is_finite()
        || !cell_width.is_finite()
        || !cell_height.is_finite()
        || surface_width <= 0.0
        || surface_height <= 0.0
        || cell_width <= 0.0
        || cell_height <= 0.0
    {
        return SearchBarGeometry {
            rect: zero,
            rows: 1,
            reserved_height: 0.0,
            label: zero,
            editor: zero,
            previous: zero,
            next: zero,
            wrap: zero,
            case_mode: zero,
            invert: zero,
            status: zero,
            close: zero,
        };
    }

    const LABEL: usize = 7;
    const PREVIOUS: usize = 8;
    const NEXT: usize = 8;
    const WRAP: usize = 10;
    const CASE: usize = 14;
    const INVERT: usize = 11;
    // Wide mode must fit every bounded status label without ellipsis. Narrow mode may shrink and
    // wrap the status lane along with the other secondary controls.
    const STATUS: usize = 19;
    const CLOSE: usize = 7;
    const EDITOR_MIN: usize = 12;
    const WIDE_MIN: usize =
        2 + LABEL + PREVIOUS + NEXT + WRAP + CASE + INVERT + STATUS + CLOSE + EDITOR_MIN + 8;

    let columns = (surface_width / cell_width).floor().max(1.0) as usize;
    let pad = usize::from(columns >= 4);
    let right = columns.saturating_sub(pad);
    let (
        label_pos,
        editor_pos,
        previous_pos,
        next_pos,
        wrap_pos,
        case_pos,
        invert_pos,
        status_pos,
        close_pos,
        rows,
    ) = if columns >= WIDE_MIN {
        let mut col = pad;
        let mut place = |width: usize| {
            let here = col;
            col += width + 1;
            (here, 0, width)
        };
        let label_pos = place(LABEL);
        // All fixed controls plus their inter-control gaps have already been
        // budgeted by WIDE_MIN; the editor gets every surplus column.
        let editor_width = EDITOR_MIN + (columns - WIDE_MIN);
        let editor_pos = place(editor_width);
        let previous_pos = place(PREVIOUS);
        let next_pos = place(NEXT);
        let wrap_pos = place(WRAP);
        let case_pos = place(CASE);
        let invert_pos = place(INVERT);
        let status_pos = place(STATUS);
        let close_pos = (col, 0, CLOSE.min(right.saturating_sub(col)).max(1));
        (
            label_pos,
            editor_pos,
            previous_pos,
            next_pos,
            wrap_pos,
            case_pos,
            invert_pos,
            status_pos,
            close_pos,
            1,
        )
    } else {
        // The editor and Close are invariant. The label collapses before either
        // interactive element; at a physically one-column width Close gets its
        // own row so neither hit target overlaps or disappears.
        let inner = right.saturating_sub(pad).max(1);
        let close_on_own_row = inner == 1;
        let close_width = CLOSE.min((inner / 3).max(1));
        let label_width = if inner >= 24 {
            LABEL
        } else if inner >= 14 {
            2 // compact "? " label, populated by the text builder
        } else {
            0
        };
        let gaps = usize::from(label_width > 0) + usize::from(!close_on_own_row);
        let editor_width = inner
            .saturating_sub(label_width + usize::from(!close_on_own_row) * close_width + gaps)
            .max(1);
        let label_pos = (pad, 0, label_width);
        let editor_col = pad + label_width + usize::from(label_width > 0);
        let editor_pos = (editor_col, 0, editor_width);
        let close_pos = if close_on_own_row {
            (pad, 1, 1)
        } else {
            (
                (editor_col + editor_width + 1).min(right.saturating_sub(1)),
                0,
                close_width.min(right.max(1)),
            )
        };

        // Pack the secondary row(s) with deterministic source order. Each
        // component may shrink on exceptionally narrow windows, but none is
        // hidden. Status is geometry, not a focusable control.
        let desired = [PREVIOUS, NEXT, WRAP, CASE, INVERT, STATUS];
        let usable = inner.max(1);
        let mut packed = [(0usize, 0usize, 1usize); 6];
        let mut row = if close_on_own_row { 2 } else { 1 };
        let mut col = pad;
        for (idx, wanted) in desired.into_iter().enumerate() {
            let width = wanted.min(usable).max(1);
            if col > pad && col + width > right {
                row += 1;
                col = pad;
            }
            packed[idx] = (col, row, width);
            col = col.saturating_add(width + 1);
        }
        let [
            previous_pos,
            next_pos,
            wrap_pos,
            case_pos,
            invert_pos,
            status_pos,
        ] = packed;
        (
            label_pos,
            editor_pos,
            previous_pos,
            next_pos,
            wrap_pos,
            case_pos,
            invert_pos,
            status_pos,
            close_pos,
            row + 1,
        )
    };

    let natural_row_height = cell_height + 10.0;
    let reserved_height = (natural_row_height * rows as f32).min(surface_height);
    let row_height = reserved_height / rows as f32;
    let top = surface_height - reserved_height;
    let to_rect = |(col, row, width): (usize, usize, usize)| -> Rect4 {
        let x = (col as f32 * cell_width).min(surface_width);
        let max_width = (surface_width - x).max(0.0);
        (
            x,
            top + row as f32 * row_height,
            (width as f32 * cell_width).min(max_width),
            row_height,
        )
    };

    SearchBarGeometry {
        rect: (0.0, top, surface_width, reserved_height),
        rows,
        reserved_height,
        label: to_rect(label_pos),
        editor: to_rect(editor_pos),
        previous: to_rect(previous_pos),
        next: to_rect(next_pos),
        wrap: to_rect(wrap_pos),
        case_mode: to_rect(case_pos),
        invert: to_rect(invert_pos),
        status: to_rect(status_pos),
        close: to_rect(close_pos),
    }
}

fn search_bar_text(search: &SearchOverlay, geometry: SearchBarGeometry, cell_width: f32) -> String {
    let row_height = geometry.reserved_height / geometry.rows.max(1) as f32;
    let mut rows = vec![Vec::<(usize, usize, String)>::new(); geometry.rows];
    let mut add = |rect: Rect4, text: String| {
        if rect.2 <= 0.0 || rect.3 <= 0.0 {
            return;
        }
        let row = if row_height > 0.0 {
            ((rect.1 - geometry.rect.1) / row_height).round().max(0.0) as usize
        } else {
            0
        }
        .min(rows.len().saturating_sub(1));
        let col = (rect.0 / cell_width).round().max(0.0) as usize;
        let width = (rect.2 / cell_width).floor().max(1.0) as usize;
        rows[row].push((col, width, fit_single_line_label(&text, width)));
    };

    let label_cols = (geometry.label.2 / cell_width).floor() as usize;
    add(
        geometry.label,
        if label_cols >= 6 { "Search" } else { "?" }.to_string(),
    );
    let editor_cols = (geometry.editor.2 / cell_width).floor().max(1.0) as usize;
    add(geometry.editor, search_editor_label(search, editor_cols));
    add(geometry.previous, "‹ Prev".to_string());
    add(geometry.next, "Next ›".to_string());
    add(
        geometry.wrap,
        format!("[{}] Wrap", if search.wrap { 'x' } else { ' ' }),
    );
    add(
        geometry.case_mode,
        format!("Case: {}", search.case_mode.label()),
    );
    add(
        geometry.invert,
        format!("[{}] Invert", if search.invert { 'x' } else { ' ' }),
    );
    add(geometry.status, search.status.label().to_string());
    add(geometry.close, "× Close".to_string());

    rows.iter_mut()
        .map(|segments| {
            segments.sort_by_key(|segment| segment.0);
            let mut line = String::new();
            let mut column = 0usize;
            for (start, width, text) in segments {
                if *start > column {
                    line.extend(std::iter::repeat_n(' ', *start - column));
                    column = *start;
                }
                if *start < column {
                    continue;
                }
                let fitted = fit_single_line_label(text, *width);
                column += display_width(&fitted);
                line.push_str(&fitted);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn search_editor_label(search: &SearchOverlay, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if max_cols == 1 {
        return "│".to_string();
    }

    // Do not let a pasted line break create extra chrome rows. The UI also
    // normalizes paste, but paint is a trust boundary for callers of this crate.
    let mut query = String::with_capacity(search.query.len().min(4096));
    for ch in search.query.chars() {
        if query.len() + ch.len_utf8() > 4096 {
            break;
        }
        query.push(if ch.is_control() { ' ' } else { ch });
    }
    let mut cursor = search.cursor_byte.min(query.len());
    while cursor > 0 && !query.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let caret = if search.focused == SearchControl::Editor {
        "│"
    } else {
        ""
    };
    query.insert_str(cursor, caret);

    let inner = max_cols.saturating_sub(2);
    let mut body = drop_cols_front(&query, search.horizontal_scroll);
    if display_width(&body) > inner {
        body = fit_single_line_label(&body, inner);
    }
    let label = format!("[{body}]");
    fit_single_line_label(&label, max_cols)
}

fn drop_cols_front(s: &str, cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    let mut skipped = 0usize;
    let mut byte = 0usize;
    for (idx, grapheme) in s.grapheme_indices(true) {
        let width = grapheme.width();
        if skipped + width > cols {
            byte = idx;
            break;
        }
        skipped += width;
        byte = idx + grapheme.len();
        if skipped == cols {
            break;
        }
    }
    s[byte..].to_string()
}

fn search_query_column(query: &str, byte: usize) -> usize {
    let mut byte = byte.min(query.len());
    while byte > 0 && !query.is_char_boundary(byte) {
        byte -= 1;
    }
    display_width(&query[..byte])
}

/// Minimum contrast the confirm bar's text holds against its own background.
/// WCAG AA for body text. A destructive question the user cannot read is worse
/// than no question at all.
pub const CONFIRM_BAR_MIN_CONTRAST: f64 = 4.5;

/// Text color for the `palette[1]` confirm bar.
///
/// The bar was painted with the theme's ordinary foreground, which is chosen to
/// contrast with the theme BACKGROUND, not with a saturated red. On the shipped
/// TokyoNight Night default that is `#c0caf5` on `#f7768e` — roughly 1.6:1, far
/// under any legibility floor, so the close confirmation could not be read and
/// therefore could not be answered.
///
/// Start from the theme's own cursor-text color, which is the same choice the
/// tab close chip already makes on this same `palette[1]` background, then lift
/// it to AA. Starting from a theme color rather than flat black keeps the bar
/// looking like part of the theme; `with_min_contrast` preserves as much of
/// that tint as the ratio allows. Pure, so the whole bundled theme set can be
/// checked without a GPU.
///
/// This holds only because the bar is painted OPAQUE. A translucent bar
/// composites `palette[1]` over live terminal content, and the real ratio then
/// depends on the scrollback underneath — which is exactly the guarantee this
/// function exists to remove.
pub fn confirm_bar_text_color(theme: &kettle_config::Theme) -> Rgb {
    color::with_min_contrast(
        theme.cursor_text,
        theme.palette[1],
        CONFIRM_BAR_MIN_CONTRAST,
    )
}

/// Maximum monospace columns available to a single-line overlay buffer.
/// Reserve one column for glyph overhang and fractional cell metrics.
/// Lay out the confirm bar: prompt (plus help text when it fits) on the left,
/// the button row flush right, within exactly `max_cols` columns.
///
/// The width this composes to and the width the caller then fits to MUST be the
/// same number. They were not: composition targeted `floor(sw/cw)` while
/// `fit_single_line_label` was handed `overlay_label_cols(sw, cw)`, which is one
/// column less. The bar therefore overflowed its budget by exactly one column at
/// every window size, and `fit_single_line_label` clipped two columns and
/// appended `…` — so the rightmost button rendered as `[  Clos…` rather than
/// `[  Close]` in every close-confirm, quit-confirm and reassign prompt ever
/// shown. Taking one budget as a parameter is what makes that class of mismatch
/// impossible to reintroduce silently.
fn compose_confirm_bar_label(
    prompt: &str,
    help: &str,
    buttons_label: &str,
    max_cols: usize,
) -> String {
    let buttons_cols = display_width(buttons_label);
    if buttons_cols > max_cols {
        return String::new();
    }

    let min_gap = 2usize;
    let full_left = format!("{prompt}{help}");
    let max_left = max_cols.saturating_sub(buttons_cols + min_gap);
    let left = if display_width(&full_left) <= max_left {
        full_left
    } else if display_width(prompt) <= max_left {
        prompt.to_string()
    } else if max_left > 3 {
        let mut truncated = take_cols_front(prompt, max_left - 3);
        truncated.push_str("...");
        truncated
    } else {
        String::new()
    };
    let gap = max_cols - buttons_cols - display_width(&left);
    format!("{left}{}{buttons_label}", " ".repeat(gap))
}

/// Columns in which the confirm bar is painted. The App uses this same budget
/// for mouse hit-testing so the live row cannot extend past visible glyphs.
pub fn confirm_bar_columns(width: f32, cell_width: f32) -> usize {
    overlay_label_cols(width, cell_width)
}

fn overlay_label_cols(width: f32, cell_width: f32) -> usize {
    if !width.is_finite() || !cell_width.is_finite() || width <= 0.0 || cell_width <= 0.0 {
        return 0;
    }

    ((width / cell_width).floor() as usize).saturating_sub(1)
}

/// Fit a bottom-bar label without allowing Glyphon to create clipped rows.
fn fit_single_line_label(label: &str, max_cols: usize) -> String {
    if display_width(label) <= max_cols {
        return label.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    if max_cols == 1 {
        return "…".to_string();
    }

    let mut fitted = take_cols_front(label, max_cols - 1);
    fitted.push('…');
    fitted
}

/// Display width (terminal columns) of a string, via `unicode-width`.
fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Take whole chars from the FRONT of `s` up to `cols` display columns.
fn take_cols_front(s: &str, cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut acc = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if acc + w > cols {
            break;
        }
        out.push(c);
        acc += w;
    }
    out
}

/// Take whole chars from the BACK of `s` up to `cols` display columns.
fn take_cols_back(s: &str, cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut acc = 0usize;
    let mut rev: Vec<char> = Vec::new();
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if acc + w > cols {
            break;
        }
        rev.push(c);
        acc += w;
    }
    rev.iter().rev().collect()
}

/// Shorten `s` to at most `n` display columns with a MIDDLE ellipsis, preserving
/// both ends — the right choice for paths and `user@host:dir` titles where the
/// tail (program / leaf dir) is the most identifying part. When `s` looks like a
/// path, the trailing segment after the last `/` or `\` is kept whole if it fits
/// (`C:\…\pwsh.exe`); otherwise it falls back to a symmetric split. Measured in
/// columns, not chars, so CJK/wide glyphs don't overflow.
fn middle_ellipsis(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if display_width(s) <= n {
        return s.to_string();
    }
    if n == 1 {
        return "…".to_string();
    }
    let budget = n - 1; // reserve 1 col for the `…`
    // Prefer keeping the trailing path segment (program / leaf) intact.
    let leaf_separator = s.rfind(['/', '\\']);
    let leaf_start = leaf_separator.map(|i| i + 1).unwrap_or(0);
    if let Some(separator_start) = leaf_separator {
        let leaf = &s[leaf_start..];
        let separator = &s[separator_start..leaf_start];
        let tail_w = display_width(separator) + display_width(leaf);
        if tail_w <= budget && !leaf.is_empty() {
            let head = take_cols_front(&s[..separator_start], budget - tail_w);
            return format!("{head}…{separator}{leaf}");
        }
    }
    // Fallback: symmetric middle split.
    let front_cols = budget.div_ceil(2);
    let back_cols = budget - front_cols;
    let head = take_cols_front(s, front_cols);
    let tail = take_cols_back(s, back_cols);
    format!("{head}…{tail}")
}

/// Build a tab title that fits `n` display columns. Unlike pane titles, tabs
/// favor the *right* side of path-like strings when they must truncate: in a
/// path, the tail is normally the project/leaf (`...flight-event-line-server-go`)
/// and is more useful than the home or drive prefix. Non-path titles keep the
/// older middle-ellipsis behavior.
fn fit_tab_title(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if display_width(s) <= n {
        return s.to_string();
    }
    if !s.contains(['/', '\\']) {
        return middle_ellipsis(s, n);
    }
    if n < 3 {
        return take_cols_back(s, n);
    }
    format!("...{}", take_cols_back(s, n - 3))
}

/// v2.26.0: fit a directory-derived tab label into `n` columns by progressively
/// shedding detail (the user-requested tiering): tier 1 the full (home-
/// abbreviated) path; tier 2 the leaf directory name alone; tier 3 the tail of
/// the leaf with a leading `…` once even the name doesn't fit. The rightmost
/// part (the project / current directory) is the most identifying, so it is kept
/// to the end — mirroring `fit_pane_title`'s progressive-shed shape.
fn fit_tab_path(full: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    // Tier 1: the whole path.
    if display_width(full) <= n {
        return full.to_string();
    }
    // Tier 2: the leaf directory name (segment after the last separator).
    let leaf_start = full.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let leaf = &full[leaf_start..];
    if !leaf.is_empty() && display_width(leaf) <= n {
        return leaf.to_string();
    }
    // Tier 3: the tail of the leaf, with a leading `…` marking the cut.
    let tail_src = if leaf.is_empty() { full } else { leaf };
    if n == 1 {
        return "…".to_string();
    }
    format!("…{}", take_cols_back(tail_src, n - 1))
}

/// Fit only the `{title}` value for a tab segment, using the same budget math as
/// the renderer's tab text path. `title_w` is the title lane in pixels, `cell_w`
/// is the measured monospace cell width, and `tab_format` contributes the fixed
/// non-title prefix/suffix such as `{n}: `.
pub fn fit_tab_segment_title(
    title: &str,
    path: Option<&str>,
    idx: usize,
    tab_format: &str,
    title_w: f32,
    cell_w: f32,
) -> String {
    let n = (idx + 1).to_string();
    let avail = (title_w.max(0.0) / cell_w.max(1.0)) as usize;
    let fixed_label = kettle_config::template::fill(tab_format, &[("n", &n), ("title", "")]);
    let fixed_w = display_width(&fixed_label);
    let maxc = avail.saturating_sub(fixed_w);
    match path {
        Some(p) => fit_tab_path(p, maxc),
        None => fit_tab_title(title, maxc),
    }
}

/// Build a per-pane titlebar label that fits `budget` display columns, shedding
/// the least-useful parts first (the v2.24.0 progressive-shed UX): full →
/// drop the `WxH` size text → drop the `[group]` tag → middle-ellipsize the
/// title (keeping the program/leaf name) → at the floor, the leaf alone. The
/// bell indicator (if any) is kept throughout — it's small and important.
/// Mirrors the label format built inline pre-2.24.0 (`"  [g]  title  WxH  🔔"`).
pub fn fit_pane_titlebar_title(
    group: Option<&str>,
    title_prefix: &str,
    title: &str,
    title_path: Option<&str>,
    size_text: Option<&str>,
    bell: Option<&str>,
    budget: usize,
) -> String {
    let bell_part = bell.map(|b| format!("  {b}")).unwrap_or_default();
    let display_title = title_path.unwrap_or(title);
    let assemble = |g: Option<&str>, t: &str, sz: Option<&str>| -> String {
        let mut s = String::new();
        if let Some(g) = g {
            s.push_str("  [");
            s.push_str(g);
            s.push(']');
        }
        s.push_str("  ");
        s.push_str(title_prefix);
        s.push_str(t);
        if let Some(sz) = sz {
            s.push_str("  ");
            s.push_str(sz);
        }
        s.push_str(&bell_part);
        s
    };
    // 1. Everything.
    let full = assemble(group, display_title, size_text);
    if display_width(&full) <= budget {
        return full;
    }
    // 2. Drop the size text.
    let no_size = assemble(group, display_title, None);
    if display_width(&no_size) <= budget {
        return no_size;
    }
    // 3. Drop the group tag.
    let no_group = assemble(None, display_title, None);
    if display_width(&no_group) <= budget {
        return no_group;
    }

    // 4. With metadata shed, prefer the authoritative cwd path when available:
    // full path if it fits, then leaf, then ellipsized leaf tail. This mirrors
    // tab fitting and avoids rendering a shell's already-truncated cwd suffix
    // when Kettle also has OSC 7 cwd truth.
    let fixed = 2 + display_width(title_prefix) + display_width(&bell_part);
    let title_budget = budget.saturating_sub(fixed).max(1);
    let fitted_title = match title_path {
        Some(path) => fit_tab_path(path, title_budget),
        None => middle_ellipsis(title, title_budget),
    };
    format!("  {title_prefix}{fitted_title}{bell_part}")
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

const NEW_TAB_MENU_GLYPH: &str = "▾";
const NEW_TAB_PLUS_GLYPH: &str = "+";

/// Center one shaped, single-line chrome glyph inside its interactive rect.
/// Keeping this independent of the font's advance and the tab-bar height is
/// what makes visually different symbols such as `▾` and `+` share one optical
/// center without hard-coded spaces or padding that drift across fonts/scales.
fn centered_text_origin(rect: Rect4, text_size: (f32, f32)) -> (f32, f32) {
    let (x, y, w, h) = rect;
    let (text_w, text_h) = text_size;
    (
        x + ((w - text_w) * 0.5).max(0.0),
        y + ((h - text_h) * 0.5).max(0.0),
    )
}

/// Clip chrome text to the interactive rect that positioned it. Vertical tab
/// rows do not share the strip's first-row y range, so reusing a bar-wide clip
/// can make a correctly positioned glyph disappear outside its bounds.
fn text_bounds_for_rect((x, y, width, height): Rect4) -> TextBounds {
    TextBounds {
        left: x as i32,
        top: y as i32,
        right: (x + width) as i32,
        bottom: (y + height) as i32,
    }
}

/// AppKit's decorated-window bottom radius in logical points. The renderer
/// multiplies by the live scale factor, so the visible curve stays concentric
/// with the native mask on both Retina and non-Retina displays.
const MACOS_WINDOW_CORNER_RADIUS_POINTS: f32 = 16.0;
const PANE_EDGE_EPSILON: f32 = 1.0;
const OUTLINE_BOTTOM_RIGHT: u32 = 1 << 2;
const OUTLINE_BOTTOM_LEFT: u32 = 1 << 3;

fn pane_bottom_window_corner_mask(
    rect: (f32, f32, f32, f32),
    surface: (f32, f32),
    rounded_window_corners: bool,
) -> u32 {
    if !rounded_window_corners {
        return 0;
    }
    let (x, y, width, height) = rect;
    let (surface_width, surface_height) = surface;
    if (y + height - surface_height).abs() > PANE_EDGE_EPSILON {
        return 0;
    }
    let mut mask = 0;
    if x.abs() <= PANE_EDGE_EPSILON {
        mask |= OUTLINE_BOTTOM_LEFT;
    }
    if (x + width - surface_width).abs() <= PANE_EDGE_EPSILON {
        mask |= OUTLINE_BOTTOM_RIGHT;
    }
    mask
}

fn pane_outline(
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

#[cfg(test)]
mod pane_window_corner_tests {
    use super::{
        NEW_TAB_MENU_GLYPH, NEW_TAB_PLUS_GLYPH, OUTLINE_BOTTOM_LEFT, OUTLINE_BOTTOM_RIGHT,
        centered_text_origin, pane_bottom_window_corner_mask, text_bounds_for_rect,
    };

    #[test]
    fn new_tab_glyphs_are_unpadded_and_centered_in_their_own_hit_rects() {
        assert_eq!(NEW_TAB_MENU_GLYPH, NEW_TAB_MENU_GLYPH.trim());
        assert_eq!(NEW_TAB_PLUS_GLYPH, NEW_TAB_PLUS_GLYPH.trim());
        assert_eq!(
            centered_text_origin((10.0, 20.0, 26.0, 26.0), (8.0, 18.0)),
            (19.0, 24.0)
        );
        assert_eq!(
            centered_text_origin((10.0, 20.0, 12.0, 10.0), (20.0, 20.0)),
            (10.0, 20.0)
        );
        let lower_row_clip = text_bounds_for_rect((10.0, 72.0, 26.0, 26.0));
        assert_eq!(lower_row_clip.left, 10);
        assert_eq!(lower_row_clip.top, 72);
        assert_eq!(lower_row_clip.right, 36);
        assert_eq!(lower_row_clip.bottom, 98);
    }

    #[test]
    fn only_panes_touching_a_rounded_window_bottom_receive_corner_masks() {
        let surface = (1200.0, 800.0);
        assert_eq!(
            pane_bottom_window_corner_mask((0.0, 100.0, 1200.0, 700.0), surface, true),
            OUTLINE_BOTTOM_LEFT | OUTLINE_BOTTOM_RIGHT
        );
        assert_eq!(
            pane_bottom_window_corner_mask((0.0, 100.0, 600.0, 700.0), surface, true),
            OUTLINE_BOTTOM_LEFT
        );
        assert_eq!(
            pane_bottom_window_corner_mask((600.0, 100.0, 600.0, 700.0), surface, true),
            OUTLINE_BOTTOM_RIGHT
        );
        assert_eq!(
            pane_bottom_window_corner_mask((0.0, 100.0, 1200.0, 350.0), surface, true),
            0,
            "an internal split edge must stay square"
        );
        assert_eq!(
            pane_bottom_window_corner_mask((0.0, 100.0, 1200.0, 700.0), surface, false),
            0,
            "borderless, fullscreen, Linux and Windows windows stay unchanged"
        );
    }

    #[test]
    fn fractional_layout_rounding_still_matches_the_surface_edge() {
        assert_eq!(
            pane_bottom_window_corner_mask((0.25, 64.0, 799.5, 535.25), (800.0, 600.0), true,),
            OUTLINE_BOTTOM_LEFT | OUTLINE_BOTTOM_RIGHT
        );
    }
}

/// Compatibility fix (audit): draws one cell's underline segment(s),
/// differentiating the `Flags::ALL_UNDERLINES` style bits instead of
/// collapsing UNDERLINE / UNDERCURL / DOTTED_UNDERLINE / DASHED_UNDERLINE to
/// the same straight 1px line. This renderer's only chrome/cell primitive is
/// the axis-aligned quad (`QuadInstance`/`rect`), so every style here is built
/// from small rects rather than a real path/curve:
///
/// - UNDERCURL is approximated as a stepped zigzag (a triangle wave built
///   from 1px-tall quads) spanning a 2-cell period, using `col` (the run's
///   ABSOLUTE grid column, not cell-local) to phase each cell's 4 segments
///   into the right quarter of that period — so a run of undercurl cells
///   shows one continuous wave rather than every cell independently
///   restarting the same little tent shape. It is NOT a smooth sine curve —
///   that needs a dedicated shader/SDF path in `quad.rs` (out of scope for a
///   pure-quad fix) — but it IS visually distinct from a straight line,
///   which is the actual gap this fixes: docs/RESEARCH.md + docs/TESTING.md
///   hold undercurl to a "must work" bar for Neovim/AstroNvim LSP
///   diagnostics (`DiagnosticUnderlineError`/`SpellBad` etc. bind
///   `gui=undercurl`), and those diagnostics were rendering pixel-identical
///   to a plain hyperlink underline before this fix.
/// - DOTTED_UNDERLINE / DASHED_UNDERLINE tile short marks with gaps across
///   the cell (sparser + shorter marks for dotted, longer + denser for
///   dashed), phased on the absolute `x` pixel position rather than `col` (a
///   mark width in px doesn't generally divide `cw` evenly, unlike
///   undercurl's cw-sized segments) — genuinely just gapped lines, so these
///   need no curve approximation.
#[allow(clippy::too_many_arguments)]
fn push_underline_quads(
    quads: &mut Vec<QuadInstance>,
    x: f32,
    y: f32,
    cw: f32,
    ch: f32,
    col: usize,
    flags: Flags,
    color: Rgb,
) {
    let base_y = y + ch - 2.0;
    if flags.contains(Flags::UNDERCURL) {
        const STEPS_PER_CELL: usize = 4;
        // A symmetric 8-step (2-cell) triangle wave, amplitude 2px around
        // the baseline: rises 0→1→2, falls 2→1→0→-1→-2, rises back to 0.
        // Amplitude is capped at 2px because the underline band itself is
        // only ~2-4px tall on typical cell sizes — a larger amplitude would
        // collide with the glyph above or the row below.
        const OFFSETS: [f32; 8] = [0.0, 1.0, 2.0, 1.0, 0.0, -1.0, -2.0, -1.0];
        let seg_w = (cw / STEPS_PER_CELL as f32).max(1.0);
        for i in 0..STEPS_PER_CELL {
            // `col` phases each cell into its quarter of the shared 8-step
            // period, so cell N+1 continues cell N's wave instead of every
            // cell restarting at step 0.
            let phase = (col * STEPS_PER_CELL + i) % OFFSETS.len();
            let seg_x = x + i as f32 * seg_w;
            // The last segment eats any rounding remainder so the segments
            // tile the FULL cell width with no sub-pixel gap at the right
            // edge (cw isn't necessarily an exact multiple of STEPS_PER_CELL).
            let this_w = if i + 1 == STEPS_PER_CELL {
                (x + cw - seg_x).max(1.0)
            } else {
                seg_w
            };
            quads.push(rect(
                seg_x,
                base_y + OFFSETS[phase],
                this_w,
                1.0,
                color,
                1.0,
            ));
        }
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        // Short marks (1px) with a 2px gap.
        push_gapped_line(quads, x, base_y, cw, 3.0, 1.0, color);
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        // Longer marks (3.5px) with a denser 6px pitch — reads as "dashed"
        // rather than "dotted" at a glance.
        push_gapped_line(quads, x, base_y, cw, 6.0, 3.5, color);
    } else {
        // Plain UNDERLINE (and DOUBLE_UNDERLINE's primary line, below) —
        // unchanged solid line.
        quads.push(rect(x, base_y, cw, 1.0, color, 1.0));
    }
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        quads.push(rect(x, y + ch - 4.0, cw, 1.0, color, 1.0));
    }
}

/// Tiles `mark_w`-wide, 1px-tall quads at `pitch` intervals across
/// `[x, x + cw)`, phased on the absolute `x` pixel position (not the cell's
/// local origin) so a run of same-styled cells shows one continuous gapped
/// line rather than each cell restarting the pattern at its own left edge.
fn push_gapped_line(
    quads: &mut Vec<QuadInstance>,
    x: f32,
    y: f32,
    cw: f32,
    pitch: f32,
    mark_w: f32,
    color: Rgb,
) {
    if pitch <= 0.0 || cw <= 0.0 {
        return;
    }
    let end = x + cw;
    let mut sx = (x / pitch).floor() * pitch;
    while sx < end {
        let seg_x = sx.max(x);
        let seg_end = (sx + mark_w).min(end);
        if seg_end > seg_x {
            quads.push(rect(seg_x, y, seg_end - seg_x, 1.0, color, 1.0));
        }
        sx += pitch;
    }
}

/// Compatibility fix (audit): underline styles must no longer render
/// pixel-identical to a plain line. Pure functions, no GPU needed.
#[cfg(test)]
mod underline_style_tests {
    use super::{Flags, push_underline_quads};
    use kettle_config::Rgb;

    const COLOR: Rgb = Rgb::new(200, 60, 60);
    const X: f32 = 100.0;
    const Y: f32 = 0.0;
    const CW: f32 = 12.0;
    const CH: f32 = 20.0;

    fn quads_for(flags: Flags, col: usize) -> Vec<super::QuadInstance> {
        let mut quads = Vec::new();
        push_underline_quads(&mut quads, X, Y, CW, CH, col, flags, COLOR);
        quads
    }

    #[test]
    fn plain_underline_is_a_single_full_width_line() {
        let quads = quads_for(Flags::UNDERLINE, 0);
        assert_eq!(quads.len(), 1, "plain underline is exactly one quad");
        assert_eq!(quads[0].pos, [X, Y + CH - 2.0]);
        assert_eq!(quads[0].size, [CW, 1.0]);
    }

    #[test]
    fn double_underline_adds_a_second_stacked_line() {
        let quads = quads_for(Flags::DOUBLE_UNDERLINE, 0);
        assert_eq!(
            quads.len(),
            2,
            "double underline draws the primary line plus one extra"
        );
        assert_eq!(quads[1].pos, [X, Y + CH - 4.0]);
    }

    #[test]
    fn undercurl_is_a_multi_segment_zigzag_not_a_straight_line() {
        let quads = quads_for(Flags::UNDERCURL, 0);
        assert!(
            quads.len() > 1,
            "undercurl must be built from more than one segment"
        );
        // At least two distinct y positions — a straight line (the pre-fix
        // behavior) has only one, which is exactly the bug this fixes.
        let ys: std::collections::BTreeSet<i32> =
            quads.iter().map(|q| (q.pos[1] * 1000.0) as i32).collect();
        assert!(
            ys.len() > 1,
            "undercurl segments must vary in y (a zigzag), not sit on one line"
        );
        // Segments must tile the full cell width with no gap (unlike dotted/
        // dashed, a curl is a continuous — if stepped — line).
        let total_w: f32 = quads.iter().map(|q| q.size[0]).sum();
        assert!(
            (total_w - CW).abs() < 0.01,
            "undercurl segments must cover the full cell width: got {total_w}, want {CW}"
        );
    }

    #[test]
    fn undercurl_phase_spans_a_two_cell_period_instead_of_resetting_every_cell() {
        // The wave's period is 2 cells: an even column rises (0, 1, 2, 1) and
        // the next odd column falls (0, -1, -2, -1) — continuing the SAME
        // wave — rather than every cell independently redrawing the
        // identical little tent shape (which would show a seam at every
        // cell edge instead of one continuous squiggle).
        let ys_at = |col: usize| -> Vec<f32> {
            quads_for(Flags::UNDERCURL, col)
                .iter()
                .map(|q| q.pos[1])
                .collect()
        };
        let col0 = ys_at(0);
        let col1 = ys_at(1);
        let col2 = ys_at(2);
        assert_ne!(
            col0, col1,
            "adjacent columns must continue the wave, not repeat the same shape"
        );
        assert_eq!(
            col0, col2,
            "the wave period is 2 cells, so column N and N+2 must match"
        );
        // Both halves of the period still start exactly on the baseline (a
        // proper wave crosses zero at its period boundaries) — verifies the
        // OFFSETS table is a real symmetric wave, not an arbitrary jump.
        assert_eq!(col0[0], base_y_for_test());
        assert_eq!(col1[0], base_y_for_test());
    }

    fn base_y_for_test() -> f32 {
        Y + CH - 2.0
    }

    #[test]
    fn dotted_and_dashed_both_leave_gaps_and_differ_from_each_other() {
        let dotted = quads_for(Flags::DOTTED_UNDERLINE, 0);
        let dashed = quads_for(Flags::DASHED_UNDERLINE, 0);
        assert!(dotted.len() > 1, "dotted must be more than one mark");
        assert!(dashed.len() > 1, "dashed must be more than one mark");
        let dotted_w: f32 = dotted.iter().map(|q| q.size[0]).sum();
        let dashed_w: f32 = dashed.iter().map(|q| q.size[0]).sum();
        assert!(
            dotted_w < CW,
            "dotted marks must leave visible gaps (not cover the full width)"
        );
        assert!(
            dashed_w < CW,
            "dashed marks must leave visible gaps (not cover the full width)"
        );
        // Dashed marks are individually longer than dotted marks — the two
        // styles must be visually distinguishable from each other, not just
        // from a plain/undercurl line.
        let dotted_mark_w = dotted[0].size[0];
        let dashed_mark_w = dashed[0].size[0];
        assert!(
            dashed_mark_w > dotted_mark_w,
            "dashed marks ({dashed_mark_w}) must be longer than dotted marks ({dotted_mark_w})"
        );
    }

    /// `QuadInstance` only derives `Pod`/`Zeroable` (needed for the GPU
    /// upload), not `PartialEq`/`Debug` — extract the two fields this test
    /// cares about into a comparable/printable shape instead.
    fn shape_of(quads: &[super::QuadInstance]) -> Vec<([f32; 2], [f32; 2])> {
        quads.iter().map(|q| (q.pos, q.size)).collect()
    }

    #[test]
    fn dotted_dashed_and_undercurl_are_all_pairwise_distinct_shapes() {
        // The actual regression this whole fix targets: before it, these
        // four styles (plus DOUBLE) all produced the exact same single
        // full-width quad. None of the "distinctive" styles may do that now.
        let plain = shape_of(&quads_for(Flags::UNDERLINE, 0));
        for (name, flags) in [
            ("undercurl", Flags::UNDERCURL),
            ("dotted", Flags::DOTTED_UNDERLINE),
            ("dashed", Flags::DASHED_UNDERLINE),
        ] {
            let styled = shape_of(&quads_for(flags, 0));
            assert_ne!(
                styled, plain,
                "{name} must render differently from a plain underline"
            );
        }
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
/// produce identical pixels.
fn menu_chrome_quads(
    menu: &ContextMenu,
    theme: &kettle_config::Theme,
    accent: Rgb,
    cw: f32,
    ch: f32,
) -> Vec<QuadInstance> {
    let mut out: Vec<QuadInstance> = Vec::new();
    let panel_w = context_menu_panel_width(menu, cw);
    let row_h = ch + 12.0;
    let sep_h = 8.0_f32;
    // Terminator menu UX: natural panel height
    // (sum of every row) may exceed the surface. App-side
    // `context_menu_geometry` already computed the clamped
    // height; if non-zero we honor it, otherwise fall back to
    // the natural sum (the original behavior — no clamp).
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
    let (ax, ay) = menu.anchor;
    if !panel_w.is_finite()
        || !panel_h.is_finite()
        || !ax.is_finite()
        || !ay.is_finite()
        || panel_w <= 0.0
        || panel_h <= 0.0
    {
        return out;
    }
    let (clipped_top, clipped_bottom) =
        context_menu_clip_indicators(&menu.rows, menu.scroll_offset, panel_h, row_h, sep_h);

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

    // Per-row highlight + separators. Skip scrolled-off
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
                (panel_w - 24.0).max(0.0),
                1.0,
                theme.palette[8],
                0.55,
            ));
            row_y += sep_h;
            continue;
        }
        if i == menu.highlight && row.enabled {
            // Soft accent tint across the row.
            out.push(rect(
                ax + 1.0,
                row_y,
                (panel_w - 2.0).max(0.0),
                row_h,
                accent,
                0.18,
            ));
            // 2-px accent strip on the left of the highlighted row —
            // same pattern as the active-tab accent strip and
            // the focused-pane border.
            out.push(rect(ax + 1.0, row_y, 2.0, row_h, accent, 1.0));
        }
        row_y += row_h;
    }
    // Terminator menu UX: top/bottom overflow cues when the natural list is
    // clipped. Drawn as small accent-colored bars rather than glyphs so they
    // do not need a separate text-buffer path.
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

/// Inverse of [`srgb`]: encode a linear channel back to an 8-bit sRGB byte.
fn srgb_encode(linear: f64) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The clear colour for a pass whose pipelines composite as if the destination
/// were premultiplied.
///
/// Every draw layered over this clear treats the existing attachment contents
/// as premultiplied: `quad` and `imgpipe` use `PREMULTIPLIED_ALPHA_BLENDING`
/// (`src_factor: One`), and `glyphpipe`'s `ALPHA_BLENDING` pairs
/// `src_factor: SrcAlpha` with `dst_factor: OneMinusSrcAlpha`, which is the
/// premultiplied "over" operator with the source premultiplied on the fly. The
/// clear is the one write in the chain that does not pass through a blend, so
/// it is the only one that has to premultiply itself — and it did not, leaving
/// a translucent background too bright by exactly the factor it skipped.
///
/// The multiply belongs in linear space: the attachment is an sRGB format, so
/// the hardware decodes before blending and re-encodes on write, and the whole
/// pass is linear. [`srgb`] already returns linear, so scaling its output is
/// the correct place.
///
/// Only `PreMultiplied` gets scaled. The live `PostMultiplied` path never draws
/// the scene directly into the surface: it renders this premultiplied
/// representation offscreen and explicitly unpremultiplies in the final
/// presentation pass. The other modes receive a straight clear here.
fn surface_clear_color(bg: Rgb, alpha: f64, mode: wgpu::CompositeAlphaMode) -> wgpu::Color {
    let alpha = alpha.clamp(0.0, 1.0);
    let scale = if matches!(mode, wgpu::CompositeAlphaMode::PreMultiplied) {
        alpha
    } else {
        1.0
    };
    wgpu::Color {
        r: srgb(bg.r) * scale,
        g: srgb(bg.g) * scale,
        b: srgb(bg.b) * scale,
        a: alpha,
    }
}

fn live_underlay_clear_color(
    bg: Rgb,
    floor: Option<f32>,
    mode: wgpu::CompositeAlphaMode,
    live_window: bool,
) -> wgpu::Color {
    if !live_window {
        return wgpu::Color::TRANSPARENT;
    }
    floor.map_or(wgpu::Color::TRANSPARENT, |alpha| {
        surface_clear_color(bg, f64::from(alpha), mode)
    })
}

fn apply_quad_alpha_floor(quads: &mut [QuadInstance], floor: f32) {
    let floor = floor.clamp(0.0, 1.0);
    for quad in quads {
        quad.color[3] = quad.color[3].max(floor);
    }
}

fn use_live_pane_bases(live_window: bool, floor: Option<f32>) -> bool {
    live_window && floor.is_some()
}

/// Undo premultiplication on an 8-bit RGBA/BGRA readback, in the space the GPU
/// applied it in.
///
/// PNG stores straight (non-premultiplied) alpha, so a capture taken off a
/// premultiplied target has to be converted before it is saved.
///
/// `srgb_encoded` selects the space, and it is not cosmetic. On an sRGB
/// attachment the hardware decodes to linear before blending and re-encodes on
/// write, so the stored texel is an sRGB encoding of a *linear* premultiplied
/// value and the inverse has to decode, divide, and re-encode. On a plain
/// `Unorm` attachment there is no decode, the multiply happened on the stored
/// values themselves, and dividing the bytes directly is exactly right.
/// Applying either reciprocal in the other's space leaves every translucent
/// capture visibly off.
///
/// The alpha channel sits at index 3 in both RGBA and BGRA, and the operation
/// is per-channel, so this runs correctly before or after a BGRA swizzle.
///
/// A fully transparent texel carries no colour to recover, so it stays zeroed;
/// a fully opaque one is already straight and is left untouched.
fn unpremultiply_rgba8(pixels: &mut [u8], srgb_encoded: bool) {
    for texel in pixels.as_chunks_mut::<4>().0 {
        match texel[3] {
            0 => texel[..3].fill(0),
            u8::MAX => {}
            a => {
                let a = a as f64 / 255.0;
                for channel in &mut texel[..3] {
                    *channel = if srgb_encoded {
                        srgb_encode(srgb(*channel) / a)
                    } else {
                        (*channel as f64 / a).round().clamp(0.0, 255.0) as u8
                    };
                }
            }
        }
    }
}

/// Terminator parity, terminatorlib/config.py:106 + 117:
/// compose the kettle background-opacity with Terminator's
/// `background_darkness` + `background_type`. Logic:
///
///   bg-type = solid (default):  alpha = background_opacity
///   bg-type = transparent:      alpha = background_opacity * background_darkness
///   bg-type = image/starfield:  same as transparent — darkness lets the
///                               backdrop show through the terminal area, not
///                               only behind the chrome
///
/// `background_darkness` runs SEE-THROUGH (`0.0`) to FULLY-COVERED (`1.0`),
/// because Terminator assigns it straight to the background colour's alpha and
/// its users lower it for more transparency. `docs/CONFIG.md` and the field's
/// own doc comment both used to describe that backwards, which sent anyone
/// following the documentation to the wrong end of the scale;
/// `darkness_scales_the_backdrop_toward_see_through` pins the direction so
/// prose and behaviour cannot drift apart again.
///
/// All inputs already clamped at parse time so no defensive math needed.
/// Surface-pixel origin of a pane's terminal grid.
///
/// A top titlebar consumes space before row zero; a bottom titlebar consumes
/// the same vertical space after the final row and therefore must not move the
/// origin. Renderer content and UI pointer/IME projection share this helper so
/// changing the title position cannot shift their coordinate systems apart.
pub fn pane_grid_origin(
    pane: (f32, f32, f32, f32),
    padding: (f32, f32),
    pane_titlebar_h: f32,
    title_at_bottom: bool,
) -> (f32, f32) {
    let title_top = if title_at_bottom {
        0.0
    } else {
        pane_titlebar_h
    };
    (pane.0 + padding.0, pane.1 + padding.1 + title_top)
}

/// The interior rectangle of a pane to paint with its
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

/// The foreground a cell's glyph is drawn in, after the attributes that modify
/// colour have all had their turn.
///
/// Order is the whole content of this function, which is why it is a function.
///
/// 1. **SGR 2 dim/faint** blends toward the background. The renderer ignored
///    `Flags::DIM` entirely at one point, so `\e[2m` looked like normal weight;
///    fish prompt themers, `less` status lines and `mc` all use it.
/// 2. **`bold-is-bright`** (Terminator `bold_is_bright`) remaps a bold
///    foreground from `palette[0..8]` to its `palette[8..16]` bright variant.
/// 3. **`minimum-contrast`** lifts toward whichever extreme is reachable, if
///    the result still falls below the configured WCAG ratio.
///
/// The lift has to be LAST, because it is the only step that reasons about the
/// colour against the background rather than transforming it. It used to run
/// second, before `bold_is_bright` — which then replaced the foreground
/// outright with a palette entry, discarding the lift. `minimum-contrast`
/// therefore did nothing at all for bold text whenever `bold-is-bright` was on,
/// which is the common configuration; and since the bright variant is the
/// lighter one, the case it discarded is precisely the one that needed it, pale
/// bold text on a pale background.
///
/// Callers apply the search-highlight and cursor-block overrides after this,
/// deliberately: each substitutes a colour paired with its OWN background, so
/// lifting them against the cell's background would measure the ratio against a
/// surface that is not behind them.
fn attributed_foreground(
    fg: Rgb,
    bg: Rgb,
    dim: bool,
    bold: bool,
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
) -> Rgb {
    let mut fg = fg;
    if dim {
        fg = color::dim(fg, bg);
    }
    if bold && cfg.bold_is_bright {
        fg = color::bright_for_bold(fg, theme);
    }
    if cfg.minimum_contrast > 1.0 {
        fg = color::with_min_contrast(fg, bg, cfg.minimum_contrast as f64);
    }
    fg
}

#[derive(Clone, Copy)]
enum CellHighlight {
    None,
    Selection,
    Search(bool),
}

fn resolved_cell_foreground(
    fg: Rgb,
    bg: Rgb,
    highlight: CellHighlight,
    dim: bool,
    bold: bool,
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
) -> Rgb {
    match highlight {
        CellHighlight::Search(true) => attributed_foreground(
            cfg.search_foreground.unwrap_or(theme.background),
            cfg.search_background.unwrap_or(theme.palette[3]),
            false,
            false,
            cfg,
            theme,
        ),
        CellHighlight::Search(false) | CellHighlight::Selection => attributed_foreground(
            theme.selection_foreground,
            theme.selection_background,
            false,
            false,
            cfg,
            theme,
        ),
        CellHighlight::None => attributed_foreground(fg, bg, dim, bold, cfg, theme),
    }
}

fn composed_bg_alpha(cfg: &kettle_config::Config) -> f64 {
    use kettle_config::BackgroundType;
    match cfg.background_type {
        BackgroundType::Solid => cfg.background_opacity as f64,
        // Starfield is an opaque kettle-drawn wallpaper, so the cell backgrounds
        // dim the same way as an image (darkness lets the field show through the
        // terminal area, not just behind the chrome).
        BackgroundType::Transparent | BackgroundType::Image | BackgroundType::Starfield => {
            (cfg.background_opacity as f64) * (cfg.background_darkness as f64)
        }
    }
}

/// Whether the window itself must support non-opaque presentation for the
/// configured background. Kept shared with window creation so wgpu never
/// selects an alpha mode for pixels the OS window was created unable to show.
pub fn window_requires_alpha_surface(cfg: &kettle_config::Config) -> bool {
    use kettle_config::BackgroundType;

    match cfg.background_type {
        BackgroundType::Solid | BackgroundType::Transparent => composed_bg_alpha(cfg) < 1.0,
        // Image alpha is unknown until decode. Starfield is different: its
        // back-most shader writes alpha 1 across the full surface, regardless
        // of the tint darkness used for terminal cells above it.
        BackgroundType::Image => true,
        BackgroundType::Starfield => false,
    }
}

fn background_has_wallpaper(cfg: &kettle_config::Config) -> bool {
    matches!(
        cfg.background_type,
        kettle_config::BackgroundType::Image | kettle_config::BackgroundType::Starfield
    )
}

/// Whether the back-most content proves alpha 1 at every surface pixel.
/// Once an opaque base covers the surface, source-over compositing cannot make
/// any later pixel translucent, regardless of the alpha of panes, glyphs,
/// inline images, or overlays.
fn final_scene_is_uniformly_opaque(
    cfg: &kettle_config::Config,
    opaque_wallpaper_covers_surface: bool,
) -> bool {
    match cfg.background_type {
        // The procedural wallpaper is a fullscreen oversized triangle whose
        // fragment shader always writes alpha 1. Unlike an image, there is no
        // decoded source alpha or cover-mode geometry to prove at runtime.
        kettle_config::BackgroundType::Starfield => true,
        kettle_config::BackgroundType::Image => opaque_wallpaper_covers_surface,
        kettle_config::BackgroundType::Solid | kettle_config::BackgroundType::Transparent => {
            composed_bg_alpha(cfg) >= 1.0
        }
    }
}

/// A premultiplied scene needs the fullscreen unpremultiply pass only for a
/// PostMultiplied surface and only while alpha can differ from one. At alpha 1,
/// straight and premultiplied RGB are identical.
fn needs_postmultiplied_presentation(
    alpha_mode: wgpu::CompositeAlphaMode,
    scene_is_uniformly_opaque: bool,
) -> bool {
    matches!(alpha_mode, wgpu::CompositeAlphaMode::PostMultiplied) && !scene_is_uniformly_opaque
}

fn desired_alpha_mode(
    cfg: &kettle_config::Config,
    supported: &[wgpu::CompositeAlphaMode],
) -> wgpu::CompositeAlphaMode {
    let preferred = if window_requires_alpha_surface(cfg) {
        [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Inherit,
            wgpu::CompositeAlphaMode::Opaque,
        ]
    } else {
        [
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Inherit,
        ]
    };
    preferred
        .into_iter()
        .find(|mode| supported.contains(mode))
        .unwrap_or_else(|| {
            supported
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Opaque)
        })
}

/// Grid column of the cluster a laid-out glyph belongs to (v2.25.0 cell-locked
/// rendering). `char_starts[k]` is the byte offset of the k-th char in the row
/// text, and because `build_pane` writes exactly ONE char per grid cell (the
/// wide-char spacer included), `k` IS the grid column. A glyph's `cluster_start`
/// byte indexes into that same row text, so its column is the char whose byte
/// range contains the cluster start — the last `char_starts` entry `<= start`.
fn glyph_grid_col(char_starts: &[u32], cluster_start: usize) -> usize {
    char_starts
        .partition_point(|&bs| (bs as usize) <= cluster_start)
        .saturating_sub(1)
}

/// Cell-locked logical pen X (physical px), snapped to an integer pixel so the
/// glyph is crisp and shares one subpixel-bin (x_bin = 0) cache slot regardless
/// of cell: the grid cell's left edge plus any intra-cluster `x_offset` (kept so
/// a combining mark still stacks on its base). Substituting this for cosmic-text's
/// advance-accumulated `glyph.x` IS the fix — for a primary-face monospace glyph
/// (advance == cell_w) it equals the glyph's old position, so ordinary text is
/// unchanged; only advance-mismatched glyphs (fallback / CJK / ligature) move.
fn cell_locked_pen_x(cell_left: f32, x_offset_px: f32) -> f32 {
    (cell_left + x_offset_px).round()
}

/// Emit cell-locked glyph instances for ONE shaped `TextBuffer`, appending to
/// `out`. This is the single source of truth for the Grid (cell-locked)
/// renderer's per-glyph emit: `emit_pane_glyphs` (live panes), the offscreen
/// `capture_png_with_annotation` screenshot path, and the `grid_prompt_blink`
/// test fixture all call it so they can never drift apart.
///
/// `origin` is the buffer's top-left in physical pixels (the same coordinate a
/// glyphon `TextArea`'s `left`/`top` would use); every glyph is pinned to its
/// grid cell `origin.0 + col * cell_w`, snapped to an integer pixel, while any
/// intra-cluster `x_offset` (combining marks) is preserved. `default_color` is
/// the fallback for a glyph with no explicit color span (mirrors a `TextArea`'s
/// `default_color`). `char_starts` is a scratch buffer reused across runs to
/// avoid per-line allocation. Glyphs are appended in buffer order, so the
/// caller can wrap the appended range in one `GlyphClip` for scissor clipping.
#[allow(clippy::too_many_arguments)]
fn emit_cell_locked_glyphs(
    out: &mut Vec<GlyphInstance>,
    buf: &TextBuffer,
    origin: (f32, f32),
    cell_w: f32,
    default_color: GColor,
    glyph_pipeline: &mut GlyphPipeline,
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    char_starts: &mut Vec<u32>,
) {
    for run in buf.layout_runs() {
        // Map a cluster's byte offset → grid column. `Wrap::None` guarantees one
        // run per buffer line and one char per cell, so the Nth char IS grid
        // column N.
        char_starts.clear();
        for (b, _) in run.text.char_indices() {
            char_starts.push(b as u32);
        }
        let line_y = run.line_y;
        for glyph in run.glyphs.iter() {
            let col = glyph_grid_col(char_starts, glyph.start);
            let fs = glyph.font_size;
            // Pin the pen to the cell, snapped to an integer physical pixel
            // (crisp + cache-friendly: x_bin = 0), while keeping any intra-
            // cluster x_offset. Feeding cosmic-text's `physical()` an offset that
            // lands the logical X exactly there reuses its exact cache-key +
            // vertical + subpixel math, so the rasterized bitmap is byte-
            // identical to glyphon's.
            let cell_left = origin.0 + col as f32 * cell_w;
            let x_off_px = fs * glyph.x_offset;
            let off_x = cell_locked_pen_x(cell_left, x_off_px) - glyph.x - x_off_px;
            let phys = glyph.physical((off_x, origin.1), 1.0);
            let key = phys.cache_key;
            let slot = match glyph_pipeline.ensure_glyph(device, queue, key, || {
                RasterGlyph::from_swash(swash.get_image(font_system, key).as_ref()?)
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

fn measure_cell(
    fs: &mut FontSystem,
    buf: &mut TextBuffer,
    family: &str,
    metrics: Metrics,
) -> (f32, f32) {
    buf.set_metrics(metrics);
    // Size the measure box relative to the (physical)
    // metrics, not a fixed 1000×100. At a large font on a high-DPI display the
    // physical font size can be ~200px, so the 10-glyph probe is ~1300px wide
    // and wrapped against the old 1000px box — `line_w` then reflected only the
    // first wrapped line and `cell_w` came out too narrow, mis-gridding the
    // terminal. A monospace `M` is ~0.6em, so 10 fit in ~6em; 20em + slack is
    // ample headroom that can never wrap regardless of size/scale.
    let box_w = metrics.font_size * 20.0 + 100.0;
    let box_h = metrics.line_height * 2.0 + 100.0;
    buf.set_size(Some(box_w), Some(box_h));
    buf.set_text(
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

fn text_layout_damage_key(
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
        for v in [pv.rect.0, pv.rect.1, pv.rect.2, pv.rect.3] {
            hf(&mut h, v);
        }
        pv.snap.columns.hash(&mut h);
        pv.snap.screen_lines.hash(&mut h);
        pv.snap.display_offset.hash(&mut h);
    }
    h.finish()
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
/// Which synthetic scene to render in [`capture_png_with`].
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
/// a correct screenshot with zero code churn.
pub(crate) const SCREENSHOT_DEMO_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugScene {
    /// Existing `--screenshot` behavior.
    #[default]
    Default,
    /// Render with a synthetic right-click context menu open over the
    /// pane. The menu carries the eight items kettle ships (Copy,
    /// Paste, sep, Split Right, Split Down, Close Pane, sep, New
    /// Tab) with the first enabled row highlighted, anchored at a
    /// fixed position so the resulting PNG is byte-deterministic
    /// across runs.
    ContextMenu,
    /// Render an active, partially-scrolled compact overlay scrollbar. Used by
    /// visual regression coverage; it is not exposed as a public CLI mode.
    Scrollbar,
}

/// Top edge (px from the surface top) of the passive "update available"
/// banner, given the surface height, the banner's own height, and the heights
/// of any **bottom-anchored** tab / status bars it must stack above.
///
/// The banner is a non-modal bottom strip. When the user
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
    update_banner_top_with_reserved(surface_h, banner_h, bottom_tabbar_h, bottom_status_h, 0.0)
}

/// Search-aware form of [`update_banner_top`]. `bottom_reserved_h` is the rich
/// search lane's `SearchBarGeometry::reserved_height` (or zero while closed).
/// Keeping the original four-argument helper as a wrapper preserves external
/// callers while new draw/hit-test paths can keep Search as the bottommost
/// strip and stack the passive update banner above it.
pub fn update_banner_top_with_reserved(
    surface_h: f32,
    banner_h: f32,
    bottom_tabbar_h: f32,
    bottom_status_h: f32,
    bottom_reserved_h: f32,
) -> f32 {
    surface_h - banner_h - bottom_tabbar_h - bottom_status_h - bottom_reserved_h
}

fn update_banner_chrome_colors(theme: &kettle_config::Theme) -> (Rgb, Rgb) {
    let bg = color::with_min_contrast(theme.palette[8], theme.foreground, 4.5);
    let accent = color::with_min_contrast(theme.palette[2], bg, 3.0);
    (bg, accent)
}

/// Back-compat wrapper for `capture_png` callers (the CLI smoke test
/// and the `--screenshot` end-to-end CI step). Always renders
/// [`DebugScene::Default`].
pub fn capture_png(
    cfg: &Config,
    cols: u32,
    rows: u32,
    out: &std::path::Path,
) -> Result<(u32, u32)> {
    capture_png_with(cfg, cols, rows, out, DebugScene::Default)
}

/// Resolve the wgpu adapter kettle would use on this machine and
/// return a human-readable diagnostic string. It uses the live renderer's
/// configured adapter policy, except no presentation surface is required
/// because this path does not create a window.
///
/// Used by `kettle --gpu-info` so a user filing a "blank window" /
/// "no GPU adapter" bug report can attach the adapter / backend /
/// driver / texture-limit details without a windowed run. The same
/// answer would otherwise require launching the binary, hitting the
/// failure mode, and digging through `RUST_LOG=info` output.
pub fn gpu_info(cfg: &Config) -> Result<String> {
    pollster::block_on(async {
        let (_instance, adapter) = resolve_headless_adapter(cfg, "gpu_info").await?;
        let info = adapter.get_info();
        let limits = adapter.limits();
        let requested_backend = match configured_backend(cfg.gpu_backend) {
            Some(backend) => backend_str(backend).to_string(),
            None if cfg!(target_os = "windows") => "Auto (DX12 preferred)".to_string(),
            None if cfg!(target_os = "macos") => "Auto (Metal preferred)".to_string(),
            None => "Auto (Vulkan preferred)".to_string(),
        };
        let backend_fallback =
            configured_backend(cfg.gpu_backend).is_some_and(|requested| requested != info.backend);
        Ok(format!(
            "Backend policy: {requested_backend}\n\
             Backend:        {:?}\n\
             Backend fallback: {}\n\
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
            if backend_fallback { "yes" } else { "no" },
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

/// Extended `capture_png_with` variant that adds an
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
/// attached to scrollback positions) are a separate, larger
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
        // Second pipelines for the `DebugScene::ContextMenu` overlay.
        // Allocated unconditionally (small, cheap) so the render pass
        // can always call `draw` / `render` on them — empty uploads
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
        // always line up with the glyphs. The old fixed 240px segments were
        // ~2× wider than the ~120px labels, so the second tab's text floated
        // inside the first tab's highlight.
        let tab_text_left = 0.0_f32;
        // Tabs FILL the bar (the live layout), NOT old compact label-width tabs —
        // the README hero/showcase must reflect the current style. v2.36.6: tabs
        // divide the FULL bar width so the tab1/tab2 boundary lands on the split
        // centre (`split_x`), so the vertical split divider visually continues
        // the tab boundary — matching the live full-width `tab_strip_layout`.
        // Only the last tab yields the trailing `+` button. Monospace, so a
        // label's pixel width is its char count × `cw`.
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
        // Active tab 0: themed background + a 2px left accent bar (live style).
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
        // 1px separators at each segment's right edge (live style).
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

        // Block cursor sitting at the end of the left pane's idle prompt
        // (`kevim@kettle:~/Repos/kettle$ ` = 29 columns, so the cursor's empty
        // input cell is column 29). The prompt text was lengthened but
        // this column wasn't, leaving the cursor stranded mid-path on the
        // "e" of `~/Repos/kettle`. Keep `cur_col` in sync with
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
        tab_buf.set_size(Some(wf), Some(tab_h));
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
        let mut left = TextBuffer::new(&mut font_system, metrics);
        left.set_size(Some(split_x - pad), Some(lh));
        left.set_rich_text(
            [
                ("kevim@kettle", grn.clone()),
                (":", fg.clone()),
                ("~/Repos/kettle", blu.clone()),
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
        right.set_size(Some(wf - split_x - pad), Some(lh));
        right.set_rich_text(
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

        // Optional caption overlay at the bottom of the
        // image. When `annotation` is Some, paint a translucent dark
        // strip across the bottom 24px + render the caption text in
        // theme.foreground. Useful for docs, README hero images, and
        // bug reports that want to caption a screenshot with a
        // version / repro / env note.
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

        // The pane body text (tab bar, left + right panes) is the imagery the
        // README hero/showcase ships, so it MUST render through whatever
        // `text-renderer` the config selects — the same branch the live
        // `render_frame` takes. Grid (the default) cell-locks every glyph via the
        // shared `emit_cell_locked_glyphs`; Legacy keeps glyphon. Either way the
        // origins below mirror the live pane layout so columns line up. The
        // annotation + context-menu chrome always go through glyphon.
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
                    cw,
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
        // with the same eight items the live `App::context_menu_items`
        // ships. Quads go through the shared `menu_chrome_quads`
        // helper; text areas are built inline here because the
        // capture-path text-buffer pool is local to this function.
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
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: "Paste".into(),
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
                    label: "Split Right".into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: "Split Down".into(),
                    separator: false,
                    enabled: true,
                    hint: String::new(),
                },
                ContextMenuRow {
                    label: "Close Pane".into(),
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
                    label: "New Tab".into(),
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
                        // path also honors background-type +
                        // background-darkness, and honor
                        // cfg.background_opacity here too. The live-window
                        // clear op already did, but the screenshot path
                        // hardcoded `a: 1.0` — so `kettle --screenshot
                        // --config /transparent.conf` produced an opaque PNG
                        // regardless.
                        //
                        // This target is ours end to end, so it clears
                        // premultiplied to match what the pipelines drawing
                        // over it expect, and `unpremultiply_srgb8` converts
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

/// Config-aware self-test entry point retained for embedders and focused
/// diagnostics. Repository CI calls [`offscreen_selftest`], which supplies
/// `Config::default()` so the gate never depends on developer configuration.
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

#[cfg(test)]
mod gpu_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Serializes the tests that stand up a real GPU device.
    ///
    /// libtest runs tests in parallel, so without this several of the tests
    /// below create wgpu instances, adapters, and devices in the same process
    /// at the same moment. On a host whose only adapter is a software or basic
    /// display driver — which is what the CI Windows runners have — that has
    /// taken the whole test binary down with `STATUS_ACCESS_VIOLATION`
    /// (`0xC0000005`), reported by cargo against `kettle-render` with no test
    /// having failed, because the fault is inside the driver rather than in
    /// Rust.
    ///
    /// The evidence is positional. libtest reports in name order, and every
    /// observed crash stopped immediately after the last `glyphpipe::` test —
    /// `gpu_tests` is the module that sorts next, so the process died exactly
    /// as several threads entered device creation together. The failure does
    /// not reproduce on a host with a real GPU driver: 175/175 pass
    /// single-threaded and 20 consecutive parallel runs are clean.
    ///
    /// One device at a time costs a little wall clock and removes the whole
    /// class. These tests have no reason to run concurrently with each other.
    static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the GPU lock, ignoring poisoning.
    ///
    /// A panicking GPU test must report its own failure — not turn every
    /// later GPU test into a poisoned-mutex error that buries it.
    fn gpu_test_guard() -> std::sync::MutexGuard<'static, ()> {
        GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Use the native Windows ARM software adapter for deterministic unit tests.
    ///
    /// Parallels' ARM64 WDDM adapter currently faults inside the driver while a
    /// headless wgpu device is being requested, taking down the whole libtest
    /// process with `STATUS_ACCESS_VIOLATION`. WARP renders the same pipelines
    /// and readback pixels successfully. Keep this scoped to these unit tests:
    /// the live renderer and the standalone smoke outside the affected Parallels
    /// guest retain hardware-first adapter selection.
    fn gpu_test_config() -> Config {
        Config {
            gpu_force_software: cfg!(all(target_os = "windows", target_arch = "aarch64")),
            ..Config::default()
        }
    }

    #[test]
    fn gpu_pipelines_compile_and_render_offscreen() {
        let _serialized = gpu_test_guard();
        match super::offscreen_selftest_with_config(&gpu_test_config()) {
            Ok(true) => {}
            Ok(false) => eprintln!("no GPU adapter on this host; skipped"),
            Err(e) => panic!("offscreen GPU self-test failed: {e}"),
        }
    }

    /// A half-opaque quad must contribute half its colour, not a quarter.
    ///
    /// The quad shader returns PREMULTIPLIED colour (`rgb * a`) while the
    /// pipeline was configured with `ALPHA_BLENDING`, whose source factor is
    /// `SrcAlpha` — so the GPU multiplied by alpha a second time. Every
    /// translucent surface kettle draws came out at alpha², darkening images,
    /// panels, highlights, separators, and the unfocused-pane dim overlay.
    ///
    /// This renders and reads the pixel back, so it is the convention as the
    /// hardware actually applies it. `tests/alpha_convention.rs` reads the
    /// source for the pipelines this cannot cheaply stand up; a source-token
    /// check can always be worked around, a rendered pixel cannot.
    #[test]
    fn a_half_opaque_quad_blends_at_half_not_a_quarter() {
        let _serialized = gpu_test_guard();
        let Some(pixel) = pollster::block_on(render_one_translucent_quad()) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };

        // White at 50% alpha over black: premultiplied source-over leaves 0.5
        // linear, which the sRGB target stores as ~188. Multiplying by alpha
        // twice leaves 0.25 linear, stored as ~137 — far outside any tolerance
        // a driver's rounding needs.
        let expected = 188_u8;
        let doubled = 137_u8;
        for (channel, value) in ["r", "g", "b"].into_iter().zip(pixel) {
            assert!(
                value.abs_diff(expected) <= 3,
                "channel {channel} came back {value}, expected ~{expected}; \
                 ~{doubled} means alpha was applied twice (premultiplied shader \
                 output paired with a straight-alpha blend state)"
            );
        }
    }

    /// Draw one 50%-opaque white quad over black and read the centre pixel
    /// back. `None` when the host has no usable adapter.
    async fn render_one_translucent_quad() -> Option<[u8; 3]> {
        let cfg = gpu_test_config();
        let (_instance, adapter) = resolve_headless_adapter(&cfg, "alpha_blend_test")
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-alpha-blend-test"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .ok()?;

        // Match the offscreen self-test's format so this measures the same
        // pipeline configuration the renderer builds for a real surface.
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let size = 8_u32;
        let mut quads = QuadPipeline::new(&device, format);
        quads.upload(
            &device,
            &queue,
            [size as f32, size as f32],
            &[QuadInstance {
                pos: [0.0, 0.0],
                size: [size as f32, size as f32],
                color: [1.0, 1.0, 1.0, 0.5],
            }],
        );

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-alpha-blend-target"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // 8 px * 4 bytes is below the 256-byte copy alignment, so pad the row.
        let bytes_per_row = 256_u32;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-alpha-blend-readback"),
            size: u64::from(bytes_per_row) * u64::from(size),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-alpha-blend-pass"),
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
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(size),
                },
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        // Centre row, centre pixel — well inside the quad.
        let offset = (size as usize / 2) * bytes_per_row as usize + (size as usize / 2) * 4;
        let pixel = [data[offset], data[offset + 1], data[offset + 2]];
        drop(data);
        readback.unmap();
        Some(pixel)
    }

    /// The starfield's stars must land on the GPU where the CPU put them.
    ///
    /// The model used to be evaluated inside the fragment loop; it is resolved
    /// once per frame on the CPU now and delivered through a hand-written
    /// uniform array. Nothing about that layout is checked by the compiler —
    /// a stride or alignment disagreement between the Rust struct and the WGSL
    /// is not an error, it is a star read out of the wrong bytes and drawn
    /// somewhere else. So this renders a real frame and looks for light where
    /// the CPU said a star would be.
    #[test]
    fn starfield_stars_land_where_the_cpu_placed_them() {
        let _serialized = gpu_test_guard();
        let Some((luma, expected, side)) = pollster::block_on(render_starfield_frame()) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };

        assert!(
            !expected.is_empty(),
            "fixture must pick a time with visible stars"
        );
        let (brightest_index, &peak) = luma
            .iter()
            .enumerate()
            .max_by_key(|&(_, &value)| value)
            .expect("a non-empty frame");
        let (x, y) = (brightest_index as u32 % side, brightest_index as u32 / side);
        assert!(
            peak > 24,
            "the field rendered essentially black ({peak}), so either the \
             shader did not compile the uniform array or nothing was uploaded"
        );

        // Positions are in pixels from the surface centre; the readback is in
        // pixels from the top-left.
        let centre = side as f32 * 0.5;
        let nearest = expected
            .iter()
            .map(|star| {
                let dx = (star[0] + centre) - x as f32;
                let dy = (star[1] + centre) - y as f32;
                (dx * dx + dy * dy).sqrt()
            })
            .fold(f32::INFINITY, f32::min);
        assert!(
            nearest <= 3.0,
            "the brightest pixel sits {nearest:.1} px from the nearest star \
             the CPU uploaded — the uniform block is being read at a \
             different offset than it was written"
        );

        // The star PROFILE, not just its position. The falloff was rewritten
        // in terms of squared distance and squared radii, and an algebra slip
        // there still leaves light near the star — it just stops being a
        // crisp core inside a soft halo.
        //
        // Measured globally rather than by sampling outward from the peak: a
        // fixed direction runs into the neighbouring star that happens to lie
        // that way, which is a property of where the field put its stars and
        // not of the falloff. Core radii are under 1.5 px, so the pixels above
        // half the peak are a handful per star; if the core term collapsed
        // into the bloom the bright region would spread across the halo's
        // 3–9 px instead.
        let half_peak = peak / 2;
        let bright = luma.iter().filter(|&&v| v > half_peak).count();
        // A collapsed core would light the halo's 3–9 px radius instead —
        // hundreds of pixels per star, an order of magnitude past this.
        assert!(
            bright <= 12 * expected.len(),
            "{bright} pixels are above half the peak across {} stars — the \
             core should be a few pixels each, not a soft orb",
            expected.len()
        );

        // A starfield is sparse points on a black sky, not a wash. If the
        // whole frame lifted, the accumulation is adding something it should
        // not.
        let lit = luma.iter().filter(|&&v| v > 8).count();
        assert!(
            lit * 4 < luma.len(),
            "{lit} of {} pixels are lit — the sky should be mostly black",
            luma.len()
        );
    }

    /// Render one starfield frame offscreen at a time chosen to have stars on
    /// screen, and return its luminance plane, the CPU's star positions, and
    /// the square side used. `None` when the host has no usable adapter.
    async fn render_starfield_frame() -> Option<(Vec<u8>, Vec<[f32; 2]>, u32)> {
        let cfg = gpu_test_config();
        let (_instance, adapter) = resolve_headless_adapter(&cfg, "starfield_layout_test")
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-starfield-test"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .ok()?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        // 256 px keeps the readback small while staying wide enough that a
        // star's few-pixel core is unambiguous, and 256*4 already meets the
        // copy alignment exactly.
        let side = 256_u32;
        let bytes_per_row = side * 4;
        let resolution = [side as f32, side as f32];
        let time = 40.0_f32;

        let pipeline = starfield::StarfieldPipeline::new(&device, format);
        let expected = pipeline.frame_positions(resolution, time);
        pipeline.upload(&queue, resolution, time);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-starfield-target"),
            size: wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-starfield-readback"),
            size: u64::from(bytes_per_row) * u64::from(side),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-starfield-pass"),
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
            pipeline.draw(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(side),
                },
            },
            wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        // Green dominates perceived luminance and every star colour carries
        // it, so it is a fine single-channel proxy for this whole test.
        let mut luma = vec![0_u8; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                luma[(y * side + x) as usize] = data[(y * bytes_per_row + x * 4) as usize + 1];
            }
        }
        drop(data);
        readback.unmap();
        // Keep only the stars that can actually be on this surface, so the
        // nearest-star search is not satisfied by one off screen.
        let half = side as f32 * 0.5;
        let visible = expected
            .into_iter()
            .filter(|s| s[0].abs() <= half && s[1].abs() <= half)
            .collect();
        Some((luma, visible, side))
    }

    /// `PreMultiplied` is the only mode that scales the clear.
    ///
    /// `Opaque` throws alpha away at composite time, so scaling would only
    /// darken the surface toward black; `PostMultiplied` divides alpha back
    /// out, so it wants the straight value.
    #[test]
    fn only_a_premultiplied_surface_scales_its_clear_by_alpha() {
        let bg = Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let alpha = 0.5;
        let linear = srgb(bg.r);
        // Precondition: the scale has to be observable at all. A black
        // background or a fully opaque alpha would make every mode agree and
        // this test could not fail.
        assert!(
            linear > 0.0 && alpha < 1.0,
            "fixture must use a non-black colour and a translucent alpha"
        );

        let premultiplied = surface_clear_color(bg, alpha, wgpu::CompositeAlphaMode::PreMultiplied);
        assert!(
            (premultiplied.r - linear * alpha).abs() < 1e-12,
            "PreMultiplied must scale the linear colour by alpha, got {}",
            premultiplied.r
        );
        assert!(
            (premultiplied.a - alpha).abs() < 1e-12,
            "the alpha channel itself is never scaled"
        );

        for mode in [
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
            wgpu::CompositeAlphaMode::Auto,
        ] {
            let straight = surface_clear_color(bg, alpha, mode);
            assert!(
                (straight.r - linear).abs() < 1e-12,
                "{mode:?} must keep the straight colour, got {}",
                straight.r
            );
        }
    }

    /// Un-premultiplying has to invert the multiply in the space the GPU
    /// applied it in, and the two spaces disagree by far more than rounding.
    #[test]
    fn unpremultiply_inverts_the_multiply_in_the_matching_space() {
        // A MID-GREY original, not white. Recovering white saturates the
        // divide in either space, so a white fixture reports success for the
        // wrong reason and cannot tell the two apart at all.
        let original = 128_u8;
        let byte_alpha = 128_u8;
        let alpha = byte_alpha as f64 / 255.0;

        // sRGB attachment: the GPU multiplied in LINEAR, so the stored byte is
        // the sRGB encoding of linear(original) * alpha.
        let stored = srgb_encode(srgb(original) * alpha);
        assert!(
            (stored as f64 / alpha) < 255.0,
            "fixture must not saturate the byte-space divide, or the two \
             spaces agree and this test cannot fail"
        );

        let mut srgb_texel = [stored, stored, stored, byte_alpha];
        unpremultiply_rgba8(&mut srgb_texel, true);
        for channel in &srgb_texel[..3] {
            assert!(
                channel.abs_diff(original) <= 2,
                "linear-space un-premultiply must recover ~{original}, got {channel}"
            );
        }

        // The same bytes read in the wrong space land nowhere near it — which
        // is what makes the `srgb_encoded` flag load-bearing rather than
        // decorative.
        let mut wrong_space = [stored, stored, stored, byte_alpha];
        unpremultiply_rgba8(&mut wrong_space, false);
        assert!(
            wrong_space[0].abs_diff(original) > 30,
            "byte-space division on an sRGB texel should NOT recover \
             {original}; got {}",
            wrong_space[0]
        );

        // Plain `Unorm` attachment: the multiply happened on the stored bytes,
        // so plain division is the inverse.
        let mut unorm_texel = [64, 64, 64, byte_alpha];
        unpremultiply_rgba8(&mut unorm_texel, false);
        assert_eq!(unorm_texel[..3], [128, 128, 128]);

        // Degenerate alphas: nothing to recover, and nothing to divide.
        let mut transparent = [200, 200, 200, 0];
        unpremultiply_rgba8(&mut transparent, true);
        assert_eq!(transparent, [0, 0, 0, 0]);
        let mut opaque = [200, 201, 202, 255];
        unpremultiply_rgba8(&mut opaque, true);
        assert_eq!(opaque, [200, 201, 202, 255]);
    }

    /// The clear is the one write in the frame that does not pass through a
    /// blend, so it is the only one that has to premultiply itself — and a
    /// straight clear is invisible until something blends over it.
    ///
    /// This renders the same scene twice, once with each clear convention, and
    /// asserts they disagree before asserting which one is right. A fixture
    /// that only checked the clear would pass either way: a translucent clear
    /// with nothing drawn on it reads back identically in both conventions.
    ///
    /// White background at 50% alpha, one 50%-alpha black quad over it:
    ///
    /// - premultiplied clear → dst `(0.5, 0.5)`, blend leaves
    ///   `rgb = 0 + 0.5·0.5 = 0.25` over `a = 0.75`; un-premultiplied that is
    ///   `1/3` linear, which the sRGB target stores as ~156.
    /// - straight clear → dst `(1.0, 0.5)`, blend leaves `rgb = 0.5`, stored
    ///   as ~188 with no conversion — the value kettle actually shipped.
    #[test]
    fn a_translucent_background_composites_against_a_premultiplied_clear() {
        let _serialized = gpu_test_guard();
        let Some((premultiplied, straight, _postmultiplied)) =
            pollster::block_on(render_black_quad_over_translucent_clear())
        else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };

        assert_ne!(
            premultiplied[0], straight[0],
            "the two clear conventions must produce different pixels, or this \
             fixture cannot detect the bug it exists for"
        );

        let expected = 156_u8;
        let shipped = 188_u8;
        for (channel, value) in ["r", "g", "b"].into_iter().zip(premultiplied) {
            assert!(
                value.abs_diff(expected) <= 3,
                "channel {channel} came back {value}, expected ~{expected}; \
                 ~{shipped} means the clear was written straight while every \
                 pipeline drawing over it treats the destination as \
                 premultiplied"
            );
        }
        assert!(
            premultiplied[3].abs_diff(191) <= 2,
            "alpha is stored linearly even on an sRGB target; got {}",
            premultiplied[3]
        );
    }

    #[test]
    fn postmultiplied_presentation_unpremultiplies_the_completed_scene() {
        let _serialized = gpu_test_guard();
        let Some((premultiplied, _wrong_clear, postmultiplied)) =
            pollster::block_on(render_black_quad_over_translucent_clear())
        else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };

        for channel in 0..4 {
            assert!(
                postmultiplied[channel].abs_diff(premultiplied[channel]) <= 2,
                "PostMultiplied output {postmultiplied:?} must be the straight-alpha form of the premultiplied scene {premultiplied:?}"
            );
        }
    }

    /// Render one 50%-alpha black quad over a 50%-alpha white clear, once with
    /// the premultiplied clear and once with the straight one, and return both
    /// centre pixels already converted back to straight alpha. `None` when the
    /// host has no usable adapter.
    async fn render_black_quad_over_translucent_clear() -> Option<([u8; 4], [u8; 4], [u8; 4])> {
        let cfg = gpu_test_config();
        let (_instance, adapter) = resolve_headless_adapter(&cfg, "premultiplied_clear_test")
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-premultiplied-clear-test"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .ok()?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let size = 8_u32;
        let bg = Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let alpha = 0.5;

        let render = |clear: wgpu::Color, postmultiplied: bool| -> Option<[u8; 4]> {
            let mut quads = QuadPipeline::new(&device, format);
            quads.upload(
                &device,
                &queue,
                [size as f32, size as f32],
                &[QuadInstance {
                    pos: [0.0, 0.0],
                    size: [size as f32, size as f32],
                    color: [0.0, 0.0, 0.0, 0.5],
                }],
            );
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("kettle-premultiplied-clear-target"),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut presentation = postmultiplied.then(|| {
                present::PresentationPipeline::new(
                    &device,
                    format,
                    kettle_core::GraphicsBudget::default(),
                )
            });
            if let Some(presentation) = presentation.as_mut()
                && !presentation.ensure_target(&device, size, size)
            {
                return None;
            }
            let scene_view = presentation
                .as_ref()
                .and_then(present::PresentationPipeline::scene_view)
                .unwrap_or(&view);
            let bytes_per_row = 256_u32;
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kettle-premultiplied-clear-readback"),
                size: u64::from(bytes_per_row) * u64::from(size),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("kettle-premultiplied-clear-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: scene_view,
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
                quads.draw(&mut pass);
            }
            if let Some(presentation) = presentation.as_ref() {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("kettle-postmultiplied-presentation-test-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
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
                presentation.draw(&mut pass);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(std::iter::once(encoder.finish()));

            let slice = readback.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            rx.recv().ok()?.ok()?;
            let data = slice.get_mapped_range().ok()?;
            let offset = (size as usize / 2) * bytes_per_row as usize + (size as usize / 2) * 4;
            let texel = [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ];
            drop(data);
            readback.unmap();
            Some(texel)
        };

        let mut premultiplied = render(
            surface_clear_color(bg, alpha, wgpu::CompositeAlphaMode::PreMultiplied),
            false,
        )?;
        unpremultiply_rgba8(&mut premultiplied, true);
        let postmultiplied = render(
            surface_clear_color(bg, alpha, wgpu::CompositeAlphaMode::PreMultiplied),
            true,
        )?;
        let mut straight = render(
            wgpu::Color {
                r: srgb(bg.r),
                g: srgb(bg.g),
                b: srgb(bg.b),
                a: alpha,
            },
            false,
        )?;
        unpremultiply_rgba8(&mut straight, true);
        Some((premultiplied, straight, postmultiplied))
    }

    #[test]
    fn pane_osc11_bases_replace_without_opacity_or_color_compounding() {
        let _serialized = gpu_test_guard();
        let replace = [
            QuadInstance {
                pos: [0.0, 0.0],
                size: [8.0, 4.0],
                color: [1.0, 0.0, 0.0, 0.5],
            },
            QuadInstance {
                pos: [4.0, 0.0],
                size: [4.0, 4.0],
                color: [0.0, 0.0, 1.0, 0.5],
            },
        ];
        let Some((left, right)) = pollster::block_on(render_quad_layers(&replace, &[])) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };

        assert!(left[0] >= 250 && left[1] <= 2 && left[2] <= 2);
        assert!(right[0] <= 2 && right[1] <= 2 && right[2] >= 250);
        assert!(left[3].abs_diff(128) <= 1, "left alpha was {}", left[3]);
        assert!(
            right[3].abs_diff(128) <= 1,
            "right alpha compounded instead of staying 0.5: {}",
            right[3]
        );
    }

    #[test]
    fn wallpaper_darkness_endpoints_are_painted_after_the_wallpaper() {
        let _serialized = gpu_test_guard();
        let wallpaper = [QuadInstance {
            pos: [0.0, 0.0],
            size: [8.0, 4.0],
            color: [1.0, 1.0, 1.0, 1.0],
        }];
        let pane_layers = [
            QuadInstance {
                pos: [0.0, 0.0],
                size: [4.0, 4.0],
                color: [0.0, 0.0, 0.0, 0.0],
            },
            QuadInstance {
                pos: [4.0, 0.0],
                size: [4.0, 4.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let Some((visible, covered)) =
            pollster::block_on(render_quad_layers(&wallpaper, &pane_layers))
        else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };

        assert!(visible[..3].iter().all(|&channel| channel >= 250));
        assert!(covered[..3].iter().all(|&channel| channel <= 2));
        assert_eq!(visible[3], 255);
        assert_eq!(covered[3], 255);
    }

    async fn render_quad_layers(
        replace: &[QuadInstance],
        blend: &[QuadInstance],
    ) -> Option<([u8; 4], [u8; 4])> {
        let cfg = gpu_test_config();
        let (_instance, adapter) = resolve_headless_adapter(&cfg, "pane_base_layers_test")
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .ok()?;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut replace_pipeline = QuadPipeline::new_replace(&device, format);
        let mut blend_pipeline = QuadPipeline::new(&device, format);
        replace_pipeline.upload(&device, &queue, [8.0, 4.0], replace);
        blend_pipeline.upload(&device, &queue, [8.0, 4.0], blend);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-pane-base-test-target"),
            size: wgpu::Extent3d {
                width: 8,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-pane-base-test-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-pane-base-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            replace_pipeline.draw(&mut pass);
            blend_pipeline.draw(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d {
                width: 8,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let sample = |x: usize| {
            let offset = 2 * 256 + x * 4;
            [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]
        };
        let mut left = sample(2);
        let mut right = sample(6);
        drop(data);
        readback.unmap();
        unpremultiply_rgba8(&mut left, true);
        unpremultiply_rgba8(&mut right, true);
        Some((left, right))
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_auto_prefers_dx12_without_preinitializing_vulkan() {
        let _serialized = gpu_test_guard();
        pollster::block_on(async {
            // Exercise the real default resolver first. In particular, do not
            // construct an all-backend discovery instance just to decide
            // whether this assertion should run: that setup used to initialize
            // Vulkan before the DX12-only path it purported to protect.
            let cfg = gpu_test_config();
            let (_instance, adapter) = resolve_headless_adapter(&cfg, "windows_auto_policy_test")
                .await
                .expect("resolve default Windows adapter");
            if adapter.get_info().backend != wgpu::Backend::Dx12 {
                eprintln!("DX12 hardware unavailable on this Windows host; skipped");
                return;
            }
            assert_eq!(adapter.get_info().backend, wgpu::Backend::Dx12);

            let unavailable = Config {
                gpu_backend: kettle_config::GpuBackend::Metal,
                ..gpu_test_config()
            };
            let (_instance, adapter) =
                resolve_headless_adapter(&unavailable, "windows_backend_fallback_test")
                    .await
                    .expect("fall back from unavailable Metal on Windows");
            assert_eq!(adapter.get_info().backend, wgpu::Backend::Dx12);
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_stale_auto_pin_preserves_dx12_platform_preference() {
        let _serialized = gpu_test_guard();
        if cfg!(target_arch = "aarch64") {
            eprintln!("hardware adapter policy is covered by Windows x64 CI; skipped on ARM64");
            return;
        }
        pollster::block_on(async {
            // Keep this regression DX12-only too. A stale pin forces the
            // cross-adapter resolver, but its Auto fallback must still select
            // the physical adapter returned by wgpu/the platform rather than
            // the lowest numeric PCI vendor id.
            let instance = gpu_instance_for_backends(wgpu::Backends::DX12);
            let cfg = Config {
                gpu_name: "__kettle_missing_adapter_regression__".to_string(),
                ..Config::default()
            };
            let Some(expected) = preferred_adapter_key(&instance, None, &cfg).await else {
                eprintln!("DX12 adapter unavailable on this Windows host; skipped");
                return;
            };
            let chosen = resolve_adapter(
                &instance,
                None,
                &cfg,
                AdapterEscalation::Preferred,
                None,
                "windows_stale_pin_policy_test",
            )
            .await
            .expect("stale pin must fall back to the Auto power policy");
            assert_eq!(
                GpuAdapterKey::from_info(&chosen.get_info()),
                expected,
                "stale Auto pin fallback must preserve wgpu's physical adapter"
            );
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_explicit_vulkan_needs_no_gpu_pin_when_available() {
        let _serialized = gpu_test_guard();
        if cfg!(target_arch = "aarch64") {
            eprintln!("hardware adapter policy is covered by Windows x64 CI; skipped on ARM64");
            return;
        }
        pollster::block_on(async {
            // This is deliberately isolated from the default-DX12 regression
            // above and enables only Vulkan while checking availability.
            let probe = gpu_instance_for_backends(wgpu::Backends::VULKAN);
            let has_vulkan_hardware = probe
                .enumerate_adapters(wgpu::Backends::VULKAN)
                .await
                .into_iter()
                .any(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu);
            if !has_vulkan_hardware {
                eprintln!("Vulkan hardware unavailable on this Windows host; skipped");
                return;
            }

            let cfg = Config {
                gpu_backend: kettle_config::GpuBackend::Vulkan,
                gpu_vendor_id: 0,
                gpu_device_id: 0,
                gpu_name: String::new(),
                ..Config::default()
            };
            let (_instance, adapter) =
                resolve_headless_adapter(&cfg, "windows_explicit_policy_test")
                    .await
                    .expect("resolve explicit Vulkan adapter without a GPU pin");
            assert_eq!(adapter.get_info().backend, wgpu::Backend::Vulkan);
        });
    }

    /// v2.32.0 fix #1: the shared `emit_cell_locked_glyphs` — the loop the
    /// default (Grid) `--screenshot` path now runs over the same `left`/`right`
    /// pane buffers it used to hand only to glyphon — must produce a NON-EMPTY
    /// cell-locked glyph set. Before the fix the screenshot path built no
    /// `GlyphPipeline` at all, so the README hero/showcase imagery (generated by
    /// this path) rendered through legacy glyphon and misrepresented the shipped
    /// cell-locked renderer. Shaping a prompt-like buffer exactly as the
    /// screenshot does and asserting glyphs come out proves the Grid screenshot
    /// path emits real cell-locked glyphs.
    #[test]
    fn screenshot_grid_emits_cell_locked_glyphs() {
        let _serialized = gpu_test_guard();
        pollster::block_on(async {
            let cfg = gpu_test_config();
            let (_instance, adapter) =
                match resolve_headless_adapter(&cfg, "screenshot_grid_emit").await {
                    Ok(value) => value,
                    Err(_) => {
                        eprintln!("no GPU adapter on this host; skipped");
                        return;
                    }
                };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-screenshot-grid-emit-test"),
                    required_limits: live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .expect("request_device");
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            // Default config selects the Grid renderer (the case the screenshot
            // path now honors). Guard that assumption so this stays meaningful.
            assert_eq!(
                cfg.text_renderer,
                TextRendererMode::Grid,
                "default text-renderer must be Grid for this fix to matter"
            );
            let family = cfg.font_family.clone();
            let mut font_system = FontSystem::new();
            for face in kettle_config::font::all() {
                load_bundled_font(&mut font_system, face);
            }
            let mut swash = SwashCache::new();
            let mut glyph_pipe = GlyphPipeline::new(&device, format);

            let metrics = Metrics::new(24.0, 30.0);
            let mut measure = TextBuffer::new(&mut font_system, metrics);
            let (cw, _ch) = measure_cell(&mut font_system, &mut measure, &family, metrics);

            let mut buf = TextBuffer::new(&mut font_system, metrics);
            buf.set_size(Some(2048.0), Some(512.0));
            buf.set_wrap(Wrap::None);
            buf.set_text(
                "kevim@kettle:~/Repos/kettle$ cargo test",
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut font_system, false);

            let mut instances = Vec::new();
            let mut starts = Vec::new();
            let default_color = GColor::rgb(
                cfg.theme.foreground.r,
                cfg.theme.foreground.g,
                cfg.theme.foreground.b,
            );
            emit_cell_locked_glyphs(
                &mut instances,
                &buf,
                (12.0, 12.0),
                cw,
                default_color,
                &mut glyph_pipe,
                &mut swash,
                &mut font_system,
                &device,
                &queue,
                &mut starts,
            );
            assert!(
                instances.len() > 20,
                "the Grid screenshot path must emit a non-empty cell-locked glyph \
                 set; got {} glyphs",
                instances.len()
            );
        });
    }

    /// v2.25.1 regression guard for the grid renderer/cursor interaction. The
    /// prompt glyphs are uploaded ONCE through the cell-locked glyph pipeline,
    /// then two offscreen frames are rendered while only the cursor quad toggles.
    /// Every non-cursor pixel must stay byte-identical; a blink may change the
    /// cursor cell only. Keep several prompt shapes here because the original
    /// bug was reported with a zsh prompt, but the invariant is renderer-wide.
    #[test]
    fn grid_prompt_pixels_survive_cursor_blink() {
        let _serialized = gpu_test_guard();
        let Some((off, on, cursor_rect, prompt_rects)) =
            grid_prompt_blink_frames().expect("grid prompt blink frames render")
        else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        assert_eq!(
            (off.width(), off.height()),
            (on.width(), on.height()),
            "both blink phases must render the same surface size"
        );

        let mut changed_outside_cursor = 0u64;
        let bg = Config::default().theme.background;
        for (idx, prompt_rect) in prompt_rects.iter().enumerate() {
            let mut prompt_ink = 0u64;
            for y in prompt_rect.1..prompt_rect.1 + prompt_rect.3 {
                for x in prompt_rect.0..prompt_rect.0 + prompt_rect.2 {
                    let in_cursor = x >= cursor_rect.0
                        && x < cursor_rect.0 + cursor_rect.2
                        && y >= cursor_rect.1
                        && y < cursor_rect.1 + cursor_rect.3;
                    let a = off.get_pixel(x, y);
                    let b = on.get_pixel(x, y);
                    if !in_cursor && a != b {
                        changed_outside_cursor += 1;
                    }
                    if !in_cursor
                        && ((a[0] as i16 - bg.r as i16).abs() > 6
                            || (a[1] as i16 - bg.g as i16).abs() > 6
                            || (a[2] as i16 - bg.b as i16).abs() > 6)
                    {
                        prompt_ink += 1;
                    }
                }
            }
            assert!(
                prompt_ink > 80,
                "expected visible prompt glyph ink outside cursor for fixture {idx}; got {prompt_ink} pixels"
            );
        }
        assert_eq!(
            changed_outside_cursor, 0,
            "cursor blink changed {changed_outside_cursor} non-cursor prompt pixels"
        );
    }

    type BlinkFrames = (
        image::RgbaImage,
        image::RgbaImage,
        (u32, u32, u32, u32),
        Vec<(u32, u32, u32, u32)>,
    );

    fn grid_prompt_blink_frames() -> Result<Option<BlinkFrames>> {
        pollster::block_on(async {
            let cfg = gpu_test_config();
            let (_instance, adapter) =
                match resolve_headless_adapter(&cfg, "grid_prompt_blink").await {
                    Ok(value) => value,
                    Err(_) => return Ok(None),
                };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-grid-prompt-blink"),
                    required_limits: live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow!("device: {e:?}"))?;
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let theme = cfg.theme;
            let family = cfg.font_family.clone();
            let mut font_system = FontSystem::new();
            for face in kettle_config::font::all() {
                load_bundled_font(&mut font_system, face);
            }
            let mut swash = SwashCache::new();
            let mut glyph_pipe = GlyphPipeline::new(&device, format);
            let mut quads = QuadPipeline::new(&device, format);

            let metrics = Metrics::new(24.0, 30.0);
            let mut measure = TextBuffer::new(&mut font_system, metrics);
            let (cw, ch) = measure_cell(&mut font_system, &mut measure, &family, metrics);
            let fixtures = [
                "➜  ~",
                "$ ~/project",
                "λ ~/src",
                "❯ git status",
                "PS C:\\Users\\dev>",
            ];
            let max_cols = fixtures
                .iter()
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(1);
            let w = (cw * (max_cols as f32 + 2.0) + 24.0).ceil() as u32;
            let h = (ch * fixtures.len() as f32 + 24.0).ceil() as u32;
            let origin = (12.0_f32, 12.0_f32);
            let cursor_col = 4usize;
            let cursor_rect = (
                (origin.0 + cursor_col as f32 * cw).round() as u32,
                origin.1.round() as u32,
                cw.ceil() as u32,
                ch.ceil() as u32,
            );
            let prompt_rects: Vec<(u32, u32, u32, u32)> = fixtures
                .iter()
                .enumerate()
                .map(|(row, text)| {
                    (
                        origin.0.round() as u32,
                        (origin.1 + row as f32 * ch).round() as u32,
                        (cw * (text.chars().count() as f32 + 1.0)).ceil() as u32,
                        ch.ceil() as u32,
                    )
                })
                .collect();

            let mut buf = TextBuffer::new(&mut font_system, metrics);
            buf.set_metrics(metrics);
            buf.set_size(Some(w as f32), Some(h as f32));
            buf.set_wrap(Wrap::None);
            buf.set_text(
                &fixtures.join("\n"),
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut font_system, false);

            let mut instances = Vec::new();
            let mut starts = Vec::new();
            let default_color =
                GColor::rgb(theme.foreground.r, theme.foreground.g, theme.foreground.b);
            // Same cell-lock emit the live renderer + screenshot path use.
            emit_cell_locked_glyphs(
                &mut instances,
                &buf,
                origin,
                cw,
                default_color,
                &mut glyph_pipe,
                &mut swash,
                &mut font_system,
                &device,
                &queue,
                &mut starts,
            );
            glyph_pipe.upload(&device, &queue, [w as f32, h as f32], &instances);
            let clips = [GlyphClip {
                rect: [0.0, 0.0, w as f32, h as f32],
                start: 0,
                count: instances.len() as u32,
            }];

            let off = render_grid_prompt_frame(
                &device,
                &queue,
                format,
                &mut quads,
                &glyph_pipe,
                &clips,
                (w, h),
                theme.background,
                None,
            )?;
            let on = render_grid_prompt_frame(
                &device,
                &queue,
                format,
                &mut quads,
                &glyph_pipe,
                &clips,
                (w, h),
                theme.background,
                Some((
                    cursor_rect.0 as f32,
                    cursor_rect.1 as f32,
                    cursor_rect.2 as f32,
                    cursor_rect.3 as f32,
                    theme.cursor,
                )),
            )?;
            Ok(Some((off, on, cursor_rect, prompt_rects)))
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_grid_prompt_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        quads: &mut QuadPipeline,
        glyph_pipe: &GlyphPipeline,
        clips: &[GlyphClip],
        size: (u32, u32),
        bg: Rgb,
        cursor: Option<(f32, f32, f32, f32, Rgb)>,
    ) -> Result<image::RgbaImage> {
        let (w, h) = size;
        let mut q = Vec::new();
        if let Some((x, y, cw, ch, color)) = cursor {
            q.push(rect(x, y, cw, ch, color, 1.0));
        }
        quads.upload(device, queue, [w as f32, h as f32], &q);
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-grid-prompt-target"),
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
            label: Some("kettle-grid-prompt-readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-grid-prompt-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: srgb(bg.r),
                            g: srgb(bg.g),
                            b: srgb(bg.b),
                            a: 1.0,
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
            glyph_pipe.draw(&mut pass, clips, [w, h]);
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
            .map_err(|e| anyhow!("grid fixture mapped range failed: {e:?}"))?;
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();
        image::RgbaImage::from_raw(w, h, pixels)
            .ok_or_else(|| anyhow!("image buffer size mismatch"))
    }

    /// Render one solid quad of a known *dark* sRGB color (#1a1b23) covering
    /// an sRGB target and read pixel (0,0) back. `Ok(None)` on a GPU-less host.
    fn srgb_quad_roundtrip_sample() -> Result<Option<[u8; 3]>> {
        pollster::block_on(async {
            let cfg = gpu_test_config();
            let (_instance, adapter) =
                match resolve_headless_adapter(&cfg, "srgb_quad_roundtrip").await {
                    Ok(value) => value,
                    Err(_) => return Ok(None),
                };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-srgb-test"),
                    required_limits: live_device_limits(adapter.limits()),
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
            let mapped = slice
                .get_mapped_range()
                .map_err(|e| anyhow!("sRGB mapped range failed: {e:?}"))?;
            let px = [mapped[0], mapped[1], mapped[2]];
            drop(mapped);
            staging.unmap();
            Ok(Some(px))
        })
    }

    /// Drift guard. A dark sRGB quad (#1a1b23) drawn to an sRGB
    /// target must read back ≈ #1a1b23, NOT the gamma-lifted ~#5a5f68 that the
    /// missing sRGB→linear decode in the quad shader produced (full-screen
    /// TUIs like AstroNvim set an explicit bg on every cell, so the lift
    /// washed out the whole screen). Allows a few units per channel for the
    /// linear↔sRGB round-trip + 8-bit quantization.
    #[test]
    fn quad_pipeline_does_not_gamma_lift_on_srgb_target() {
        let _serialized = gpu_test_guard();
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

    /// Drift guard. The README hero / UX showcase screenshots are
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
    /// Unfocused panes stay on their previous neutral fallbacks
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
        // 3. accent_color wins over palette[4].
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
    /// `palette[8]` (was a hardcoded `#c0bebf` grey that clashed with dark
    /// themes like the Catppuccin Mocha default). An explicit
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
    /// (`title-receive-bg-color → resolved accent`) — was a hardcoded `#0076c9`
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
mod live_surface_dimension_tests {
    use super::{live_device_limits, live_surface_dimensions};

    // wgpu's DEFAULT requested limit is 8192; a capable adapter reports more.
    // Requesting the adapter's real limit is what lets a large window keep its
    // true size.
    #[test]
    fn live_surface_keeps_real_window_dimensions_when_the_device_allows_them() {
        assert_eq!(
            live_surface_dimensions(12_000, 9_000, 16_384),
            (12_000, 9_000)
        );
        assert_eq!(live_surface_dimensions(0, 0, 16_384), (1, 1));
    }

    // The regression this guards: dropping the clamp made `Surface::configure`
    // reject an oversized window outright, leaving a surface that paints
    // nothing. Clipping to the visible top-left region is the better failure.
    #[test]
    fn live_surface_clips_to_a_device_that_cannot_present_the_window() {
        assert_eq!(
            live_surface_dimensions(10_000, 9_000, 8_192),
            (8_192, 8_192)
        );
        assert_eq!(live_surface_dimensions(4_000, 9_000, 8_192), (4_000, 8_192));
        // A degenerate limit must still produce a configurable surface.
        assert_eq!(live_surface_dimensions(4_000, 4_000, 0), (1, 1));
    }

    #[test]
    fn live_device_requests_the_adapters_full_texture_dimension_limit() {
        let adapter = wgpu::Limits {
            max_texture_dimension_2d: 16_384,
            ..Default::default()
        };
        let requested = live_device_limits(adapter.clone());
        assert_eq!(requested.max_texture_dimension_2d, 16_384);
        assert!(requested.check_limits(&adapter));
    }

    /// Parallels' virtual GLES adapter on Windows 11 ARM and Ubuntu ARM
    /// advertises a valid graphics device with no compute dispatch capacity.
    /// Kettle has no compute pipelines, so that must lower the device request
    /// rather than making an otherwise usable terminal fail at startup.
    #[test]
    fn live_device_clamps_unused_compute_limits_to_the_adapter() {
        let adapter = wgpu::Limits {
            max_texture_dimension_2d: 16_384,
            max_compute_workgroup_storage_size: 0,
            max_compute_invocations_per_workgroup: 0,
            max_compute_workgroup_size_x: 0,
            max_compute_workgroup_size_y: 0,
            max_compute_workgroup_size_z: 0,
            max_compute_workgroups_per_dimension: 0,
            ..Default::default()
        };
        let requested = live_device_limits(adapter.clone());

        assert_eq!(requested.max_texture_dimension_2d, 16_384);
        assert_eq!(requested.max_compute_workgroups_per_dimension, 0);
        assert!(
            requested.check_limits(&adapter),
            "every requested limit must fit the virtual adapter"
        );
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
        // Floor + ceiling pinned: 5.0 and 72.0. At one point only
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
mod renderer_recovery_state_tests {
    use super::{RendererRecoveryState, ScreenshotOutputPolicy, ScreenshotRequest};
    use kettle_config::Rgb;
    use std::sync::Arc;

    #[test]
    fn clone_retains_live_overrides_and_screenshot_completion() {
        let (completion, finished) = std::sync::mpsc::channel();
        let expected_path = std::path::PathBuf::from("recovered-screenshot.png");
        let state = RendererRecoveryState {
            font_family: Arc::from("Live Runtime Font"),
            font_size: 19.5,
            cell_scale_w: 1.25,
            cell_scale_h: 1.5,
            accent_override: Some(Rgb::new(0x12, 0x34, 0x56)),
            pending_screenshot: Some(ScreenshotRequest {
                out_path: expected_path.clone(),
                output_policy: ScreenshotOutputPolicy::UserSelected,
                crop: Some((1.0, 2.0, 3.0, 4.0)),
                completion: Some(completion),
                cancellation: None,
                recovery_wake: None,
            }),
        };

        let retained = state.clone();
        drop(state);
        assert_eq!(retained.font_family.as_ref(), "Live Runtime Font");
        assert_eq!(retained.font_size, 19.5);
        assert_eq!((retained.cell_scale_w, retained.cell_scale_h), (1.25, 1.5));
        assert_eq!(retained.accent_override, Some(Rgb::new(0x12, 0x34, 0x56)));
        let request = retained.pending_screenshot.expect("queued screenshot");
        assert_eq!(request.out_path, expected_path);
        assert_eq!(request.output_policy, ScreenshotOutputPolicy::UserSelected);
        assert_eq!(request.crop, Some((1.0, 2.0, 3.0, 4.0)));
        request
            .completion
            .expect("completion sender")
            .send(Ok(expected_path.clone()))
            .unwrap();
        assert_eq!(finished.recv().unwrap(), Ok(expected_path));
    }
}

#[cfg(test)]
mod hidpi_scale_tests {
    use super::{measure_cell, metrics_for, pane_metrics};
    use glyphon::{Buffer as TextBuffer, FontSystem};

    /// The pane text buffer must advance lines by the grid's `cell_h`
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

    /// Core invariant: a logical font size renders at
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
            crate::load_bundled_font(&mut fs, face);
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

    /// At a large font on a high-DPI display the 10-glyph
    /// measure probe (~1300px at 72pt×3) exceeded the old fixed 1000px measure
    /// box and wrapped, so `cell_w` came out too narrow and mis-gridded the
    /// terminal. With the metrics-relative box it must scale linearly.
    #[test]
    fn measured_cell_does_not_wrap_at_large_font_highdpi() {
        let mut fs = FontSystem::new();
        for face in kettle_config::font::all() {
            crate::load_bundled_font(&mut fs, face);
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
mod titlebar_glyph_fallback_tests {
    use super::load_bundled_font;
    use glyphon::{Attrs, Buffer as TextBuffer, Family, FontSystem, Metrics, Shaping};

    /// Shape `text` once under `shaping` against a fresh `FontSystem` that
    /// carries the bundled faces plus whatever `FontSystem::new()` loaded
    /// from the host via `fontdb`'s system-font scan — the same stack the
    /// chrome buffers shape with. Returns each resolved glyph id in source
    /// order; `0` is TrueType's reserved `.notdef` — the tofu box.
    fn shape_glyph_ids(text: &str, shaping: Shaping) -> Vec<u16> {
        let mut fs = FontSystem::new();
        for face in kettle_config::font::all() {
            load_bundled_font(&mut fs, face);
        }
        let metrics = Metrics::new(16.0, 20.0);
        let mut buf = TextBuffer::new(&mut fs, metrics);
        buf.set_size(Some(2000.0), Some(200.0));
        buf.set_text(
            text,
            &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
            shaping,
            None,
        );
        buf.shape_until_scroll(&mut fs, false);
        buf.layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|g| g.glyph_id))
            .collect()
    }

    /// A pane-titlebar-shaped label: leading status glyph (Claude Code's
    /// OSC 0/2 titles lead with one), title text, size text, and the
    /// U+1F514 bell kettle appends itself when `icon-bell` fires. Neither
    /// U+2733 nor U+1F514 exists in the bundled Nerd Font.
    const LABEL: &str = "  \u{2733} kettle  120x40  \u{1F514}";

    /// Cross-platform invariant: Advanced's fallback cascade is a strict
    /// superset of Basic's single generic-family retry, so for the same
    /// font db Advanced may only GAIN glyph coverage over Basic, never
    /// lose it. Holds on any host regardless of which fonts are
    /// installed, so it cannot flake on a font-poor CI image.
    #[test]
    fn advanced_shaping_never_loses_a_glyph_basic_resolves() {
        let basic = shape_glyph_ids(LABEL, Shaping::Basic);
        let advanced = shape_glyph_ids(LABEL, Shaping::Advanced);
        // Cluster counts can differ between shapers; compare per-char by
        // walking both in order only when lengths match, else just assert
        // Advanced produced no MORE notdefs than Basic.
        if basic.len() == advanced.len() {
            for (i, (b, a)) in basic.iter().zip(&advanced).enumerate() {
                assert!(
                    *b == 0 || *a != 0,
                    "glyph {i} resolved under Basic (id {b}) but not Advanced"
                );
            }
        }
        let basic_notdefs = basic.iter().filter(|&&id| id == 0).count();
        let advanced_notdefs = advanced.iter().filter(|&&id| id == 0).count();
        assert!(
            advanced_notdefs <= basic_notdefs,
            "Advanced produced more tofu than Basic: {advanced_notdefs} > {basic_notdefs}"
        );
    }

    /// The concrete Windows regression this closes: Segoe UI Emoji /
    /// Segoe UI Symbol are stock Windows fonts and cosmic-text's Windows
    /// fallback list names both, so Advanced must resolve every glyph in
    /// the label — this is exactly the split-pane titlebar tofu bug. The
    /// second assert documents the defect Basic still carries; it pins
    /// upstream cosmic-text behavior, so if it ever starts failing the
    /// upstream no-fallback contract changed — harmless here since Basic
    /// is no longer used, just drop that assert.
    #[cfg(windows)]
    #[test]
    fn advanced_shaping_resolves_titlebar_emoji_on_windows() {
        let advanced = shape_glyph_ids(LABEL, Shaping::Advanced);
        assert!(
            advanced.iter().all(|&id| id != 0),
            "Advanced shaping still produced a .notdef on Windows: {advanced:?}"
        );
        let basic = shape_glyph_ids(LABEL, Shaping::Basic);
        assert!(
            basic.contains(&0),
            "expected Basic shaping to tofu the bell/asterisk (no-fallback contract)"
        );
    }

    /// Source guard (same shape as pane_buffer_lifecycle_tests): Basic
    /// shaping skips the fallback cascade, which is how the titlebar
    /// tofu-boxed emoji while the tab bar rendered them. It must never
    /// reappear in production code. The only permitted uses are the two
    /// comparison calls in this module's tests above, so pin the exact
    /// count; the needle is assembled at runtime so this test's own
    /// source cannot satisfy the match. `production_source` excludes this
    /// module's two comparison calls, so production must contain zero uses.
    #[test]
    fn no_call_site_uses_basic_shaping() {
        let src = super::production_source();
        let needle = format!("Shaping::{}", "Basic");
        let count = src.matches(&needle).count();
        assert_eq!(
            count, 0,
            "expected no production uses of {needle} but found {count} — it \
             skips cosmic-text's font-fallback cascade (no CJK/emoji/symbol \
             fallback): the split-titlebar tofu-box bug"
        );
    }
}

#[cfg(test)]
mod pane_buffer_lifecycle_tests {
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
        let idle =
            super::prepared_text_areas_damage_key(&[area(super::GColor::rgb(80, 80, 80), 10.0)]);

        for (name, changed) in [
            (
                "close hover",
                super::prepared_text_areas_damage_key(&[area(
                    super::GColor::rgb(20, 20, 20),
                    10.0,
                )]),
            ),
            (
                "pane focus",
                super::prepared_text_areas_damage_key(&[area(
                    super::GColor::rgb(230, 230, 230),
                    10.0,
                )]),
            ),
            (
                "broadcast",
                super::prepared_text_areas_damage_key(&[area(
                    super::GColor::rgb(240, 210, 80),
                    10.0,
                )]),
            ),
            (
                "area position",
                super::prepared_text_areas_damage_key(&[area(
                    super::GColor::rgb(80, 80, 80),
                    11.0,
                )]),
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
    /// detach-never-joins guard): both truncate calls must stay present.
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

    /// v2.21.0 (idle perf): an idle repaint (cursor blink, bell decay, focus
    /// dim) must NOT re-run the whole-viewport glyphon `prepare`, which
    /// re-encodes every visible glyph's vertices. `build_pane` reports whether
    /// it reshaped a row; `render_frame_with_status` gates `prepare` (and the
    /// paired `atlas.trim`) on that + a chrome-text hash + any open overlay.
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
        // evict glyphs the cached vertices still reference. The trim now sits
        // inside its own `if need_prepare` after `frame.present()`.
        let trim_idx = src.find("self.atlas.trim();").expect("atlas.trim present");
        let before_trim = &src[trim_idx.saturating_sub(120)..trim_idx];
        assert!(
            before_trim.contains("if need_prepare {"),
            "atlas.trim must be guarded by `if need_prepare`"
        );
    }

    /// v2.23.0 fix: closing an overlay (settings/palette/search/menu) must force
    /// ONE clearing prepare, or the closed panel's cached text vertices linger
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

    /// v2.21.0 (idle perf): the inverted glyph under a focused SOLID block
    /// cursor is drawn in a dedicated 1-glyph renderer ON TOP of the block,
    /// NOT recolored into the pane text buffer. Recoloring it in-buffer dirtied
    /// the cursor row every blink and forced the whole-viewport prepare; the
    /// dedicated pass keeps the pane buffer byte-identical across a blink.
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
        // The old in-buffer recolor (`fg = if cursor_rt_override...`) is gone:
        // the glyph keeps its normal fg in the buffer and is overdrawn instead.
        assert!(
            src.contains("cursor_glyph_capture = Some((sc.c, cursor_fg))"),
            "the cursor cell must be captured for the overdraw pass, not \
             recolored into the pane span runs"
        );
    }

    /// An unfocused pane carrying its own OSC 11
    /// background must paint a backdrop over its interior, because the
    /// per-cell loop skips default-bg cells (they'd otherwise leak the
    /// focused pane's clear color). The backdrop rect must stay INSIDE the
    /// border and clear of the titlebar strip so it never overpaints the
    /// focus border or per-pane titlebar.
    #[test]
    fn pane_backdrop_rect_stays_inside_border_and_titlebar() {
        use super::{pane_backdrop_rect, pane_grid_origin};
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

        // The terminal grid follows the title position: top titles move row
        // zero down, bottom titles reserve the same height after the grid.
        assert_eq!(
            pane_grid_origin(pane, (6.0, 8.0), 18.0, false),
            (16.0, 46.0)
        );
        assert_eq!(pane_grid_origin(pane, (6.0, 8.0), 18.0, true), (16.0, 28.0));
    }

    /// The background-image cache must (a) key on blur
    /// radius so toggling `background-blur` reloads, and (b) be freed when
    /// the config moves away from `background-type = image`. Pinned at the
    /// source level — exercising it needs a full GPU `Renderer`.
    #[test]
    fn bg_image_cache_keys_on_blur_and_frees_on_disable() {
        let src = super::production_source();
        assert!(
            src.contains("bg_image_cache: Option<BgImageAnim>")
                && src.contains("struct BgImageAnim"),
            "bg_image_cache holds a BgImageAnim (path, blur, frames, gaps, started)"
        );
        assert!(
            src.contains("c.path != want") && src.contains("c.blur != blur_radius"),
            "need_reload must compare blur radius, not just the path"
        );
        assert!(
            src.contains("} else if self.bg_image_cache.is_some() {")
                && src.contains("self.bg_image_cache = None;"),
            "the decoded wallpaper must be freed when background-type leaves \
             image / the path is cleared"
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
        // v2.21.x: animated backgrounds advance on the media clock, gated for
        // proactive waking on focus (battery), and never index out of bounds.
        assert!(
            src.contains("bg_image::bg_current_frame(&c.gaps, c.started.elapsed().as_millis())")
                && src.contains("idx.min(c.frames.len() - 1)"),
            "animated bg must pick the clock frame, bounded to the frame count"
        );
    }

    /// v2.23.0: the wallpaper draws in its OWN pipeline (`bg_imgs`) BEFORE the
    /// cell/chrome `quads` pass, so chrome (tab bar/status/titlebar), cell
    /// backgrounds (selection/syntax/TUI), and borders composite opaquely on
    /// top of it instead of being hidden under an opaque wallpaper (and the
    /// animation no longer bleeds through the tab bar). Pinned at the source
    /// level since exercising the pass needs a full GPU `Renderer`.
    #[test]
    fn wallpaper_draws_behind_quads_in_its_own_pass() {
        let src = super::production_source();
        // A dedicated pipeline exists and is constructed.
        assert!(
            src.contains("bg_imgs: imgpipe::ImagePipeline,")
                && src.contains("ImagePipeline::new_with_budget_and_instance_limit("),
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
        let quads = src.find("self.quads.draw(&mut pass);").expect("quads draw");
        let inline = src.find("self.imgs.draw(&mut pass);").expect("imgs draw");
        assert!(
            bg < pane_bases && pane_bases < quads && quads < inline,
            "draw order must be wallpaper -> pane bases -> quads -> inline images"
        );
    }

    /// v2.23.0: `chrome-background` only recolors the chrome with a wallpaper;
    /// theme mode + the no-wallpaper case keep `palette[8]`; auto keeps the tab
    /// text readable; black/white are fixed.
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

        // v2.24.0: the starfield is a wallpaper too, so chrome modes apply.
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
            src.contains("pane_backdrop_rect(pv.rect, bw, pane_titlebar_h, cfg.title_at_bottom)"),
            "the backdrop must use the border/titlebar-aware geometry helper"
        );
    }

    /// Drift guard (audit B2/B3/B4). The overlay text-buffer pools
    /// are grown with `while len < N` exactly like the pane pools and must be
    /// truncated back down too, or each ratchets to its session high-water mark
    /// (peak menu rows / hint labels / tab count) holding idle shaped-glyph
    /// buffers. Pin all five truncate calls at the source level (a behavioral
    /// test would need a full GPU `Renderer`).
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

    /// Drift guard (audit). `build_pane`'s per-cell style-run scratch
    /// must be POOLED on `self` (taken + returned) and reuse each run's `String`
    /// buffer by index (clear + refill), not `Vec::new()` + `to_string()` per
    /// frame — otherwise a busy colored pane mints dozens–hundreds of `String`
    /// allocations on the 60 fps hot path. A behavioral test needs a full GPU
    /// `Renderer`; pin the pattern at the source level.
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

    /// Drift guard (audit C1). Image-placement draw must keep the `quota > 1`
    /// fast-path so a pane admitted zero or one visible image doesn't pay a
    /// per-frame `Vec` alloc + sort, AND must still z-sort the 2+ case so
    /// higher-z images land on top. A behavioral test needs a full GPU
    /// `Renderer`; pin both at the source level (same shape as the
    /// buffer-truncate guards above).
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

    /// Drift guard (audit). `render_frame_with_status` clones
    /// `self.font_family` every frame (to hold an owned handle while
    /// `&mut self.font_system` is borrowed across ~20 `Family::Name(&family)`
    /// reads). The field must stay `Arc<str>` so that clone is a refcount bump,
    /// not a per-frame heap alloc + memcpy at 60fps. A behavioral test needs a
    /// GPU `Renderer`; pin the field type at the source level.
    /// Drift guard. `PaneView` must *borrow* its per-frame
    /// images/title/group_name from the frame's `metas` collection (exactly as
    /// `snap` borrows the pooled `PaneSnapshot`), not own clones — otherwise
    /// `redraw()` double-clones every visible pane's image `Vec` + title
    /// `String` every frame. A behavioral test needs the full app frame loop;
    /// pin the borrowed field types at the source.
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
}

#[cfg(test)]
mod search_bar_tests {
    use super::{
        HighlightRect, SearchBarGeometry, SearchCaseMode, SearchControl, SearchOverlay,
        SearchStatus, drop_cols_front, search_bar_geometry, search_bar_text, search_editor_label,
        search_highlight_at,
    };

    fn center(rect: (f32, f32, f32, f32)) -> (f32, f32) {
        (rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5)
    }

    fn assert_inside(bar: SearchBarGeometry, rect: (f32, f32, f32, f32)) {
        assert!(rect.2 > 0.0, "control must retain a width: {rect:?}");
        assert!(rect.3 > 0.0, "control must retain a height: {rect:?}");
        assert!(rect.0 >= bar.rect.0 && rect.1 >= bar.rect.1);
        assert!(rect.0 + rect.2 <= bar.rect.0 + bar.rect.2 + f32::EPSILON);
        assert!(rect.1 + rect.3 <= bar.rect.1 + bar.rect.3 + f32::EPSILON);
    }

    #[test]
    fn signed_history_lines_project_into_the_viewport_without_casting() {
        assert_eq!(
            HighlightRect::from_grid_span(-40, 42, 24, 3, 0, true),
            Some(HighlightRect {
                row: 2,
                col: 3,
                width: 1,
                active: true,
            })
        );
        assert_eq!(
            HighlightRect::from_grid_span(-43, 42, 24, 0, 1, false),
            None
        );
        assert_eq!(HighlightRect::from_grid_span(0, 42, 24, 0, 1, false), None);
    }

    #[test]
    fn highlight_lookup_streams_across_visible_cells() {
        let spans = [
            HighlightRect {
                row: 1,
                col: 2,
                width: 3,
                active: false,
            },
            HighlightRect {
                row: 2,
                col: 0,
                width: 1,
                active: true,
            },
        ];
        let mut cursor = 0;
        assert_eq!(search_highlight_at(&spans, &mut cursor, 1, 1), None);
        assert_eq!(search_highlight_at(&spans, &mut cursor, 1, 2), Some(false));
        assert_eq!(search_highlight_at(&spans, &mut cursor, 1, 4), Some(false));
        assert_eq!(search_highlight_at(&spans, &mut cursor, 2, 0), Some(true));
        assert_eq!(search_highlight_at(&spans, &mut cursor, 2, 1), None);
        assert_eq!(cursor, spans.len());
    }

    #[test]
    fn wide_geometry_is_one_row_and_hit_testing_reuses_paint_rects() {
        let bar = search_bar_geometry(1200.0, 800.0, 10.0, 20.0);
        assert_eq!(bar.rows, 1);
        assert_eq!(bar.reserved_height, 30.0);
        for control in SearchControl::ALL {
            let rect = bar.control_rect(control);
            assert_inside(bar, rect);
            let (x, y) = center(rect);
            assert_eq!(bar.hit_test(x, y), Some(control));
        }
        assert_eq!(bar.content_rect(1200.0, 800.0), (0.0, 0.0, 1200.0, 770.0));
    }

    #[test]
    fn narrow_geometry_wraps_without_hiding_editor_or_close() {
        let bar = search_bar_geometry(320.0, 800.0, 8.0, 18.0);
        assert!(bar.rows >= 2);
        for control in SearchControl::ALL {
            assert_inside(bar, bar.control_rect(control));
        }
        // The invariant also holds at a deliberately pathological one-cell
        // surface; Close moves to another row instead of covering the editor.
        let tiny = search_bar_geometry(8.0, 800.0, 8.0, 18.0);
        assert_inside(tiny, tiny.editor);
        assert_inside(tiny, tiny.close);
        assert_ne!(tiny.editor.1, tiny.close.1);
    }

    #[test]
    fn rich_bar_is_status_only_and_never_emits_a_global_counter() {
        let search = SearchOverlay {
            query: "needle".into(),
            cursor_byte: 6,
            wrap: true,
            case_mode: SearchCaseMode::Ignore,
            invert: true,
            status: SearchStatus::Wrapped,
            focused: SearchControl::Editor,
            ..SearchOverlay::default()
        };
        let bar = search_bar_geometry(1200.0, 800.0, 10.0, 20.0);
        let text = search_bar_text(&search, bar, 10.0);
        assert!(text.contains("needle"));
        assert!(text.contains("Case: Ignore"));
        assert!(text.contains("[x] Wrap"));
        assert!(text.contains("[x] Invert"));
        assert!(text.contains("Wrapped"));
        assert!(text.contains("× Close"));
        assert!(
            !text.contains("/"),
            "global match totals must stay out of chrome"
        );
    }

    #[test]
    fn editor_defensively_normalizes_controls_and_bounds_its_line() {
        let search = SearchOverlay {
            query: "a\nb\tc".into(),
            cursor_byte: usize::MAX,
            focused: SearchControl::Editor,
            ..SearchOverlay::default()
        };
        let label = search_editor_label(&search, 8);
        assert!(!label.contains('\n'));
        assert!(!label.contains('\t'));
        assert!(super::display_width(&label) <= 8);
    }

    #[test]
    fn editor_horizontal_clip_never_splits_extended_graphemes() {
        assert_eq!(drop_cols_front("a\u{301}b", 1), "b");
        assert_eq!(drop_cols_front("👩‍💻x", 1), "👩‍💻x");
        assert_eq!(drop_cols_front("👩‍💻x", 2), "x");
    }

    #[test]
    fn status_vocabulary_is_bounded_and_semantic() {
        assert_eq!(SearchStatus::Typing.label(), "Type to search");
        assert_eq!(SearchStatus::Searching.label(), "Searching…");
        assert_eq!(SearchStatus::Match.label(), "Match");
        assert_eq!(SearchStatus::Wrapped.label(), "Wrapped");
        assert_eq!(SearchStatus::Start.label(), "Start reached");
        assert_eq!(SearchStatus::End.label(), "End reached");
        assert_eq!(SearchStatus::NoMatch.label(), "No match");
        assert_eq!(SearchStatus::Limited.label(), "Results limited");
        assert_eq!(SearchStatus::Invalid.label(), "Invalid pattern");
        assert_eq!(SearchStatus::TooComplex.label(), "Pattern too complex");
        assert_eq!(SearchStatus::TooLong.label(), "Query too long");

        let bar = search_bar_geometry(1200.0, 800.0, 10.0, 20.0);
        let status_columns = (bar.status.2 / 10.0).floor() as usize;
        for status in [
            SearchStatus::Typing,
            SearchStatus::Searching,
            SearchStatus::Match,
            SearchStatus::Wrapped,
            SearchStatus::Start,
            SearchStatus::End,
            SearchStatus::NoMatch,
            SearchStatus::Limited,
            SearchStatus::Invalid,
            SearchStatus::TooComplex,
            SearchStatus::TooLong,
        ] {
            assert!(
                super::display_width(status.label()) <= status_columns,
                "wide status lane clipped {}",
                status.label()
            );
        }
    }
}

#[cfg(test)]
mod completion_panel_tests {
    use super::{
        COMPLETION_MAX_COLUMNS, COMPLETION_MIN_COLUMNS, CompletionOverlay, CompletionOverlayRow,
        CompletionPanelPlacement, MAX_COMPLETION_ROWS, MediaPasteReceiptHit, MediaPasteReceiptKind,
        MediaPasteReceiptOverlay, Overlay, completion_header_columns, completion_header_count,
        completion_header_label, completion_match_span, completion_overlay_row_rects,
        completion_palette, completion_panel_geometry, completion_scroll_thumb,
        completion_selection_surface, display_width, media_paste_receipt_geometry,
        media_paste_receipt_text, production_source, push_completion_selection_quads, solid_blend,
        text_overlay_requires_continuous_prepare,
    };
    use crate::color;
    use kettle_config::Rgb;

    /// A 900x700 pane whose 884x656 grid starts at (108, 70) with an 8x16 cell.
    /// The prompt begins on visible row 20, so the grid's prompt row starts at
    /// y = 70 + 320 = 390.
    const CELL: (f32, f32) = (8.0, 16.0);
    const PROMPT_TOP: f32 = 390.0;
    /// `max(4, round(16 * 0.5))`.
    const GAP: f32 = 8.0;
    /// `max(16 + 4, round(16 * 1.35))`.
    const ROW_H: f32 = 22.0;
    /// Two borders, one header row, and the padding above and below the list.
    const CHROME_H: f32 = 30.0;

    fn overlay(selected: Option<usize>, count: usize) -> CompletionOverlay {
        CompletionOverlay {
            pane_rect: (100.0, 40.0, 900.0, 700.0),
            grid_rect: (108.0, 70.0, 884.0, 656.0),
            command_rows: (20, 20),
            anchor_col: None,
            kind: "Completions".to_string(),
            source: "fish".to_string(),
            selected,
            total: count,
            token: None,
            candidates: (0..count)
                .map(|index| CompletionOverlayRow {
                    label: format!("item-{index}"),
                    description: String::new(),
                    position: index,
                })
                .collect(),
        }
    }

    fn theme() -> kettle_config::Theme {
        kettle_config::Theme::default()
    }

    fn receipt(expanded: bool) -> MediaPasteReceiptOverlay {
        MediaPasteReceiptOverlay {
            pane_rect: (100.0, 40.0, 900.0, 700.0),
            grid_rect: (108.0, 70.0, 884.0, 656.0),
            right_gutter: 0.0,
            image: Some(kettle_core::ImageData::solid(256, 128, [40, 80, 120, 255]).unwrap()),
            kind: MediaPasteReceiptKind::Image {
                original_width: 1920,
                original_height: 960,
            },
            openable: true,
            remote: false,
            expanded,
            prefer_top: true,
        }
    }

    fn video_receipt(expanded: bool, preview: bool, count: usize) -> MediaPasteReceiptOverlay {
        let mut receipt = receipt(expanded);
        receipt.image =
            preview.then(|| kettle_core::ImageData::solid(256, 144, [36, 40, 52, 255]).unwrap());
        receipt.kind = MediaPasteReceiptKind::Video {
            extension: "MP4".to_string(),
            size: 34 * 1024 * 1024,
            count,
            preview_pending: !preview,
        };
        receipt.openable = false;
        receipt
    }

    #[test]
    fn media_receipt_stays_inside_the_grid_and_preserves_thumbnail_aspect() {
        let geometry =
            media_paste_receipt_geometry(&receipt(true), None, CELL, CELL.0, CELL.1).unwrap();
        assert!(!geometry.compact);
        let (gx, gy, gw, gh) = receipt(true).grid_rect;
        let (x, y, w, h) = geometry.rect;
        assert!(x >= gx && y >= gy && x + w <= gx + gw && y + h <= gy + gh);
        let image = geometry.preview_rect.expect("expanded card has thumbnail");
        assert!((image.2 / image.3 - 2.0).abs() < 0.001);
        assert!(super::rects_overlap(geometry.dismiss_rect, geometry.rect));
        assert!(geometry.dismiss_rect.2 >= 24.0 && geometry.dismiss_rect.3 >= 24.0);
        assert!(
            !super::rects_overlap(geometry.dismiss_rect, geometry.title_rect),
            "dismiss and title hit targets must not overlap"
        );
        assert!(
            !super::rects_overlap(geometry.dismiss_rect, geometry.detail_rect.unwrap()),
            "dismiss and detail text must not overlap"
        );
        assert_eq!(
            geometry.hit_test(
                geometry.dismiss_rect.0 + geometry.dismiss_rect.2 * 0.5,
                geometry.dismiss_rect.1 + geometry.dismiss_rect.3 * 0.5,
            ),
            Some(MediaPasteReceiptHit::Dismiss)
        );
        assert_eq!(
            geometry.hit_test(geometry.rect.0 + 4.0, geometry.rect.1 + 4.0),
            Some(MediaPasteReceiptHit::Open)
        );
    }

    #[test]
    fn video_receipt_body_is_inert_but_still_consumes_hidden_terminal_clicks() {
        let geometry =
            media_paste_receipt_geometry(&video_receipt(true, true, 1), None, CELL, CELL.0, CELL.1)
                .unwrap();
        assert_eq!(
            geometry.hit_test(geometry.rect.0 + 4.0, geometry.rect.1 + 4.0),
            Some(MediaPasteReceiptHit::Body)
        );
        assert_eq!(
            geometry.hit_test(
                geometry.dismiss_rect.0 + geometry.dismiss_rect.2 * 0.5,
                geometry.dismiss_rect.1 + geometry.dismiss_rect.3 * 0.5,
            ),
            Some(MediaPasteReceiptHit::Dismiss)
        );
    }

    #[test]
    fn bottom_lane_receipt_reserves_the_dismiss_corner_from_detail_text() {
        let mut lower = receipt(true);
        lower.prefer_top = false;
        // font-size=6 and cell-height=0.5 make all three text/button bands
        // overlap vertically, so horizontal reservation is load-bearing.
        let geometry = media_paste_receipt_geometry(&lower, None, (8.0, 3.75), 8.0, 7.5)
            .expect("the expanded receipt fits in the bottom lane");
        assert!(!geometry.compact);
        assert!(
            (geometry.dismiss_rect.1 + geometry.dismiss_rect.3
                - (geometry.rect.1 + geometry.rect.3 - 5.0))
                .abs()
                < 0.01,
            "the dismiss target must occupy the bottom lane"
        );
        assert!(
            !super::rects_overlap(geometry.dismiss_rect, geometry.detail_rect.unwrap()),
            "bottom-lane detail text must reserve the dismiss target"
        );
        assert!(
            !super::rects_overlap(geometry.dismiss_rect, geometry.title_rect),
            "bottom-lane title text must reserve the dismiss target"
        );
    }

    #[test]
    fn media_receipt_avoids_completion_and_compacts_in_a_short_grid() {
        let mut completion = overlay(Some(0), 10);
        completion.anchor_col = Some(100);
        let completion_rect = super::completion_overlay_rect(&completion, CELL).unwrap();
        let geometry =
            media_paste_receipt_geometry(&receipt(true), Some(&completion), CELL, CELL.0, CELL.1)
                .expect("opposite corner remains available");
        assert!(!super::rects_overlap(geometry.rect, completion_rect));

        let mut short = receipt(true);
        short.pane_rect.3 = 100.0;
        short.grid_rect.3 = 64.0;
        let geometry = media_paste_receipt_geometry(&short, None, CELL, CELL.0, CELL.1).unwrap();
        assert!(geometry.compact, "short panes degrade to a chip");
        assert!(geometry.preview_rect.is_none());
        assert!(geometry.dismiss_rect.2 >= 24.0 && geometry.dismiss_rect.3 >= 24.0);
        assert!(
            !super::rects_overlap(geometry.dismiss_rect, geometry.title_rect),
            "the compact label must leave room for dismissal"
        );
    }

    #[test]
    fn media_receipt_keeps_one_lane_across_compact_and_expanded_states() {
        let mut exercised = 0;
        for row in 0..40 {
            let mut completion = overlay(Some(0), 6);
            completion.command_rows = (row, row);
            completion.anchor_col = Some(100);
            let compact = media_paste_receipt_geometry(
                &receipt(false),
                Some(&completion),
                CELL,
                CELL.0,
                CELL.1,
            );
            let expanded = media_paste_receipt_geometry(
                &receipt(true),
                Some(&completion),
                CELL,
                CELL.0,
                CELL.1,
            );
            let (Some(compact), Some(expanded)) = (compact, expanded) else {
                continue;
            };
            if expanded.compact {
                continue;
            }
            exercised += 1;
            let grid_mid = receipt(true).grid_rect.1 + receipt(true).grid_rect.3 / 2.0;
            assert_eq!(
                compact.rect.1 < grid_mid,
                expanded.rect.1 < grid_mid,
                "hover must not move the receipt to the opposite corner at row {row}"
            );
            assert_eq!(
                compact.dismiss_rect.0, expanded.dismiss_rect.0,
                "hover must not move the dismiss target horizontally at row {row}"
            );
            assert_eq!(
                compact.dismiss_rect.1 < grid_mid,
                expanded.dismiss_rect.1 < grid_mid,
                "hover must keep the dismiss target against the same lane edge at row {row}"
            );
        }
        assert!(exercised > 0, "fixture never admitted an expanded receipt");
    }

    #[test]
    fn media_receipt_uses_the_corner_opposite_the_terminal_cursor() {
        let top = media_paste_receipt_geometry(&receipt(true), None, CELL, CELL.0, CELL.1).unwrap();
        let mut lower = receipt(true);
        lower.prefer_top = false;
        let bottom = media_paste_receipt_geometry(&lower, None, CELL, CELL.0, CELL.1).unwrap();
        assert!(top.rect.1 < bottom.rect.1);
        assert_eq!(top.rect.0, bottom.rect.0, "the right edge stays stable");
    }

    #[test]
    fn media_receipt_hides_when_even_the_chip_cannot_fit() {
        let mut tiny = receipt(false);
        tiny.pane_rect.2 = 80.0;
        tiny.grid_rect.2 = 64.0;
        assert!(media_paste_receipt_geometry(&tiny, None, CELL, CELL.0, CELL.1).is_none());
    }

    #[test]
    fn media_receipt_text_preserves_dimensions_and_the_remote_warning() {
        let mut compact = receipt(false);
        compact.kind = MediaPasteReceiptKind::Image {
            original_width: 16_384,
            original_height: 16_384,
        };
        let compact_geometry =
            media_paste_receipt_geometry(&compact, None, CELL, CELL.0, CELL.1).unwrap();
        let (compact_title, _) = media_paste_receipt_text(&compact, &compact_geometry, CELL.0);
        assert_eq!(compact_title, "Image path pasted");

        compact.remote = true;
        let compact_geometry =
            media_paste_receipt_geometry(&compact, None, CELL, CELL.0, CELL.1).unwrap();
        let (compact_title, _) = media_paste_receipt_text(&compact, &compact_geometry, CELL.0);
        assert!(
            compact_title.starts_with("Remote · local path"),
            "a compact remote receipt must identify both the remote pane and local path"
        );

        let mut remote = receipt(true);
        remote.remote = true;
        let geometry = media_paste_receipt_geometry(&remote, None, CELL, CELL.0, CELL.1).unwrap();
        let (_, detail) = media_paste_receipt_text(&remote, &geometry, CELL.0);
        assert!(detail.contains("Remote pane\nLocal path only"));
        let columns = (geometry.detail_rect.unwrap().2 / CELL.0).floor() as usize;
        assert!(
            detail.lines().all(|line| display_width(line) <= columns),
            "no warning line may be clipped into a misleading fragment"
        );

        let mut narrow = receipt(true);
        narrow.remote = true;
        narrow.pane_rect.2 = 148.0;
        narrow.grid_rect.2 = 148.0;
        let geometry = media_paste_receipt_geometry(&narrow, None, (5.0, 10.0), 10.0, 10.0)
            .expect("the compact receipt still fits");
        assert!(
            geometry.compact,
            "the full warning must not render when six-pixel padding leaves fewer than 15 columns"
        );

        let scaled =
            media_paste_receipt_geometry(&remote, None, (CELL.0 * 0.5, CELL.1), CELL.0, CELL.1)
                .expect("the compact receipt still fits at half-width terminal cells");
        assert!(
            scaled.compact,
            "terminal cell-width must not shrink the chrome text budget"
        );

        for pane_width in 140..=360 {
            let mut candidate = receipt(true);
            candidate.remote = true;
            candidate.pane_rect.2 = pane_width as f32;
            candidate.grid_rect.2 = pane_width as f32;
            let Some(geometry) =
                media_paste_receipt_geometry(&candidate, None, (6.0, CELL.1), CELL.0, CELL.1)
            else {
                continue;
            };
            if !geometry.compact {
                let (_, detail) = media_paste_receipt_text(&candidate, &geometry, CELL.0);
                assert!(
                    detail.lines().any(|line| line == "Local path only"),
                    "an admitted expanded card must preserve the full warning at width {pane_width}"
                );
            }
        }
    }

    #[test]
    fn video_receipt_has_a_stable_poster_and_never_projects_a_path() {
        let pending = video_receipt(true, false, 3);
        let pending_geometry =
            media_paste_receipt_geometry(&pending, None, CELL, CELL.0, CELL.1).unwrap();
        let poster = pending_geometry
            .preview_rect
            .expect("expanded video receipt has a generic poster");
        assert!((poster.2 / poster.3 - 16.0 / 9.0).abs() < 0.001);
        let (title, detail) = media_paste_receipt_text(&pending, &pending_geometry, CELL.0);
        assert_eq!(title, "Video path pasted");
        assert!(detail.contains("1 of 3 · MP4 · 35.7 MB"));
        assert!(detail.contains("Preparing poster"));
        assert!(!format!("{pending:?}{title}{detail}").contains("/Users/"));

        let ready = video_receipt(true, true, 1);
        let ready_geometry =
            media_paste_receipt_geometry(&ready, None, CELL, CELL.0, CELL.1).unwrap();
        let preview = ready_geometry
            .preview_rect
            .expect("native poster is visible");
        assert!((preview.2 / preview.3 - 16.0 / 9.0).abs() < 0.001);
        let (_, detail) = media_paste_receipt_text(&ready, &ready_geometry, CELL.0);
        assert!(detail.contains("Path on command line"));
    }

    #[test]
    fn receipt_can_move_to_the_left_when_completion_owns_both_right_corners() {
        let mut completion = overlay(Some(0), 40);
        completion.pane_rect = receipt(true).pane_rect;
        completion.grid_rect = receipt(true).grid_rect;
        completion.anchor_col = Some(70);
        let completion_rect = super::completion_overlay_rect(&completion, CELL).unwrap();
        let geometry =
            media_paste_receipt_geometry(&receipt(true), Some(&completion), CELL, CELL.0, CELL.1)
                .expect("the left side remains available");
        assert!(!super::rects_overlap(geometry.rect, completion_rect));
        assert!(
            geometry.rect.0 < completion_rect.0,
            "the receipt should use the free left side: receipt {:?}, completion {:?}",
            geometry.rect,
            completion_rect
        );
    }

    #[test]
    fn receipt_poster_width_uses_chrome_metrics_not_terminal_font_width() {
        let geometry = media_paste_receipt_geometry(
            &video_receipt(true, true, 1),
            None,
            (32.0, CELL.1),
            CELL.0,
            CELL.1,
        )
        .expect("wide terminal glyphs must not collapse the chrome card");
        assert!(!geometry.compact);
        assert!(geometry.preview_rect.unwrap().2 >= CELL.0 * 8.0);
    }

    #[test]
    fn media_sizes_use_decimal_units_without_rounding_to_whole_kilobytes() {
        assert_eq!(super::format_media_size(34 * 1024 * 1024), "35.7 MB");
        assert_eq!(super::format_media_size(34 * 1024 * 1024 + 1), "35.7 MB");
        assert_eq!(super::format_media_size(1_500), "1.5 KB");
    }

    #[test]
    fn media_receipt_budgets_real_text_lines_and_the_scrollbar_gutter() {
        let mut remote = receipt(true);
        remote.remote = true;
        remote.grid_rect.3 = 160.0;
        remote.pane_rect.3 = 176.0;
        remote.right_gutter = 24.0;

        // A supported 0.5 cell-height leaves chrome text at its ordinary
        // 16-pixel line height while terminal rows shrink to 8 pixels. The
        // remote warning must still remain wholly inside the expanded card.
        let geometry = media_paste_receipt_geometry(&remote, None, (8.0, 8.0), 8.0, 16.0)
            .expect("the text-aware card fits");
        assert!(!geometry.compact);
        for inner in [geometry.title_rect, geometry.detail_rect.unwrap()] {
            assert!(inner.0 >= geometry.rect.0 && inner.1 >= geometry.rect.1);
            assert!(inner.0 + inner.2 <= geometry.rect.0 + geometry.rect.2);
            assert!(inner.1 + inner.3 <= geometry.rect.1 + geometry.rect.3);
        }
        assert!(
            geometry.rect.0 + geometry.rect.2
                <= remote.pane_rect.0 + remote.pane_rect.2 - remote.right_gutter
        );

        remote.grid_rect.3 = 96.0;
        remote.pane_rect.3 = 112.0;
        let compact = media_paste_receipt_geometry(&remote, None, (8.0, 8.0), 8.0, 16.0)
            .expect("a short pane still has room for the compact receipt");
        assert!(
            compact.compact,
            "warning text must not spill from a short card"
        );

        for pane_width in 180..=360 {
            let mut widened = receipt(true);
            widened.pane_rect.2 = pane_width as f32;
            widened.grid_rect.2 = pane_width as f32;
            let Some(geometry) =
                media_paste_receipt_geometry(&widened, None, (24.0, 16.0), 8.0, 16.0)
            else {
                continue;
            };
            if geometry.compact {
                continue;
            }
            for inner in [geometry.title_rect, geometry.detail_rect.unwrap()] {
                assert!(
                    inner.0 + inner.2 <= geometry.rect.0 + geometry.rect.2,
                    "wide terminal cells must not push chrome text past the card at width {pane_width}"
                );
            }
        }
    }

    #[test]
    fn media_receipt_text_enters_the_retained_chrome_damage_key() {
        let source = production_source();
        let hash = source
            .split("let chrome_hash = {")
            .nth(1)
            .and_then(|rest| rest.split("let chrome_changed =").next())
            .expect("retained chrome hash body");
        assert!(hash.contains("self.media_receipt_title_text.hash(&mut h);"));
        assert!(hash.contains("self.media_receipt_detail_text.hash(&mut h);"));
    }

    #[test]
    fn card_hangs_off_the_prompt_and_grows_upward_from_the_grid_left_edge() {
        let geometry = completion_panel_geometry(&overlay(Some(19), 20), CELL).unwrap();
        assert_eq!(geometry.placement, CompletionPanelPlacement::Above);
        assert_eq!(geometry.rows, MAX_COMPLETION_ROWS);
        assert_eq!(
            geometry.rect.1 + geometry.rect.3,
            PROMPT_TOP - GAP,
            "the lower edge sits exactly a half-cell above the prompt's first row"
        );
        assert_eq!(geometry.rect.0, 108.0, "the card aligns with the grid left");
        assert!(
            geometry.rect.1 >= 70.0 + 8.0,
            "a half-cell top margin is preserved"
        );
        assert_eq!(geometry.row_h, ROW_H);
        assert_eq!(geometry.rect.3, CHROME_H + 10.0 * ROW_H);

        // Fewer candidates keep the same lower edge and grow upward only.
        let short = completion_panel_geometry(&overlay(Some(1), 3), CELL).unwrap();
        assert_eq!(short.placement, CompletionPanelPlacement::Above);
        assert_eq!(short.rect.1 + short.rect.3, PROMPT_TOP - GAP);
        assert_eq!(short.rows, 3);
        assert!(short.rect.1 > geometry.rect.1);
    }

    #[test]
    fn card_aligns_with_the_command_column_and_clamps_at_the_right_edge() {
        let mut card = overlay(None, 3);
        card.anchor_col = Some(7);
        let aligned = completion_panel_geometry(&card, CELL).unwrap();
        assert_eq!(
            aligned.rect.0,
            card.grid_rect.0 + 7.0 * CELL.0,
            "the card starts where editable input begins, not at the pane edge"
        );

        card.anchor_col = Some(10_000);
        let clamped = completion_panel_geometry(&card, CELL).unwrap();
        assert_eq!(
            clamped.rect.0 + clamped.rect.2,
            card.grid_rect.0 + card.grid_rect.2,
            "a prompt near the right edge keeps the whole card inside the grid"
        );
    }

    #[test]
    fn rows_begin_below_the_header_and_match_the_painted_band() {
        let card = overlay(Some(0), 4);
        let geometry = completion_panel_geometry(&card, CELL).unwrap();
        assert_eq!(geometry.header.1, geometry.rect.1 + 1.0);
        assert_eq!(geometry.header.3, ROW_H);
        assert_eq!(
            geometry.list_top,
            geometry.header.1 + geometry.header.3 + 3.0,
            "one restrained pad separates the header from the first row"
        );
        let rows = completion_overlay_row_rects(&card, CELL);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].1.1, geometry.list_top);
        assert_eq!(rows[1].1.1 - rows[0].1.1, ROW_H);
        assert_eq!(rows[0].1.3, ROW_H);
        let (_, list_y, _, list_h) = geometry.list_rect();
        assert_eq!(list_y, geometry.list_top);
        assert_eq!(list_h, 4.0 * ROW_H);
        assert_eq!(
            list_y + list_h + 3.0 + 1.0,
            geometry.rect.1 + geometry.rect.3,
            "the same pad and border close the card under the last row"
        );
    }

    #[test]
    fn selection_keeps_one_row_of_lookahead() {
        // A selection in the middle of a long result leaves exactly one row
        // visible underneath it.
        let geometry = completion_panel_geometry(&overlay(Some(14), 40), CELL).unwrap();
        assert_eq!(geometry.rows, MAX_COMPLETION_ROWS);
        assert_eq!(geometry.first, 6);
        assert_eq!(
            geometry.first + geometry.rows - 1,
            15,
            "one candidate after the selection stays on screen"
        );

        // At the end of the result there is nothing left to look ahead to.
        let tail = completion_panel_geometry(&overlay(Some(39), 40), CELL).unwrap();
        assert_eq!(tail.first, 30);

        // The unselected first page starts at the top of the result.
        let head = completion_panel_geometry(&overlay(None, 40), CELL).unwrap();
        assert_eq!(head.first, 0);
        let second = completion_panel_geometry(&overlay(Some(0), 40), CELL).unwrap();
        assert_eq!(second.first, 0);
    }

    #[test]
    fn scroll_indicator_tracks_the_shell_result_not_the_page() {
        let mut card = overlay(Some(0), 10);
        assert!(
            completion_scroll_thumb(&card, &completion_panel_geometry(&card, CELL).unwrap())
                .is_none(),
            "a result that fits needs no indicator"
        );

        card = overlay(Some(0), 40);
        card.total = 200;
        for (index, candidate) in card.candidates.iter_mut().enumerate() {
            candidate.position = index;
        }
        let geometry = completion_panel_geometry(&card, CELL).unwrap();
        let (_, track_y, _, track_h) = geometry.list_rect();
        let (top, height) = completion_scroll_thumb(&card, &geometry).expect("indicator");
        assert_eq!(top, track_y, "the first page pins the thumb to the top");
        assert!(height >= 11.0 && height < track_h);

        // A page deep inside the result moves the thumb down proportionally and
        // never past the end of the track.
        card.selected = Some(39);
        let geometry = completion_panel_geometry(&card, CELL).unwrap();
        let (deep_top, deep_height) = completion_scroll_thumb(&card, &geometry).expect("indicator");
        assert!(deep_top > track_y);
        assert!(deep_top + deep_height <= track_y + track_h + 0.5);
    }

    #[test]
    fn width_is_content_fit_inside_the_published_column_band() {
        let short = completion_panel_geometry(&overlay(None, 2), CELL).unwrap();
        // `Completions · fish` (18) + gap (2) + `2 matches` (9) + padding (2).
        assert_eq!(short.rect.2, 31.0 * CELL.0);
        assert!(short.rect.2 >= COMPLETION_MIN_COLUMNS as f32 * CELL.0);

        let mut long = overlay(None, 2);
        long.candidates[0].label = "x".repeat(200);
        long.candidates[0].description = "y".repeat(200);
        let long = completion_panel_geometry(&long, CELL).unwrap();
        assert_eq!(long.rect.2, 92.0 * CELL.0, "the card stays content-fit");
        assert!(long.rect.2 <= COMPLETION_MAX_COLUMNS as f32 * CELL.0);
        assert_eq!(long.label_columns, 40, "labels clamp at 40 columns");
        assert_eq!(
            long.description_columns, 48,
            "descriptions clamp at 48 columns"
        );
        assert!(long.divider_x.is_some());

        // A pane narrower than the content clamps to the grid instead of
        // overflowing it, and the lanes are redistributed to match.
        let mut narrow = overlay(None, 2);
        narrow.candidates[0].label = "x".repeat(200);
        narrow.candidates[0].description = "y".repeat(200);
        narrow.pane_rect = (0.0, 40.0, 300.0, 700.0);
        narrow.grid_rect = (0.0, 70.0, 300.0, 656.0);
        let narrow = completion_panel_geometry(&narrow, CELL).unwrap();
        assert_eq!(narrow.rect.2, 37.0 * CELL.0);
        assert_eq!(
            narrow.label_columns + narrow.description_columns,
            35,
            "the lanes exactly fill the clamped card"
        );
        assert_eq!(narrow.description_columns, 0);
        assert_eq!(narrow.divider_x, None, "no empty description divider");
        assert!(narrow.rect.0 + narrow.rect.2 <= 300.0);
    }

    #[test]
    fn header_reserves_the_count_lane_and_reports_absolute_positions() {
        let mut card = overlay(Some(2), 40);
        card.total = 87;
        for (index, candidate) in card.candidates.iter_mut().enumerate() {
            candidate.position = index;
        }
        assert_eq!(completion_header_label(&card), "Completions · fish");
        assert_eq!(completion_header_count(&card), "3/87");
        card.selected = None;
        assert_eq!(completion_header_count(&card), "87 matches");
        card.total = 1;
        card.candidates.truncate(1);
        assert_eq!(completion_header_count(&card), "1 match");

        let card = overlay(None, 2);
        let geometry = completion_panel_geometry(&card, CELL).unwrap();
        let count = completion_header_count(&card);
        let (label_columns, count_columns) = completion_header_columns(&geometry, &count);
        assert_eq!(count_columns, display_width(&count));
        assert_eq!(
            label_columns + 1 + count_columns,
            geometry.inner_columns,
            "the caption takes what the right-aligned count leaves"
        );
        assert!(label_columns >= display_width(&completion_header_label(&card)));
    }

    #[test]
    fn card_uses_the_roomier_lane_without_overlapping_a_multiline_command() {
        let mut completion = overlay(None, 8);
        // Only one candidate fits above this command while all eight fit below,
        // so the useful lane wins rather than reducing the card to one row.
        completion.command_rows = (5, 7);
        let below = completion_panel_geometry(&completion, CELL).unwrap();
        assert_eq!(below.placement, CompletionPanelPlacement::Below);
        assert_eq!(below.rows, 8);
        assert_eq!(below.rect.1, 70.0 + 8.0 * 16.0 + GAP);
        assert!(below.rect.1 > 70.0 + 7.0 * 16.0);

        // With no usable upper lane, a prompt at the top receives the same
        // detached card immediately after its final row.
        completion.command_rows = (0, 2);
        let top_edge = completion_panel_geometry(&completion, CELL).unwrap();
        assert_eq!(top_edge.placement, CompletionPanelPlacement::Below);
        assert_eq!(top_edge.rect.1, 70.0 + 3.0 * 16.0 + GAP);
        assert!(
            top_edge.rect.1 + top_edge.rect.3 <= 70.0 + 656.0 - 8.0,
            "the lower card keeps the same half-cell grid margin"
        );

        // A prompt near the bottom has the inverse constraint and keeps the
        // preferred upper placement.
        completion.command_rows = (38, 40);
        let bottom_edge = completion_panel_geometry(&completion, CELL).unwrap();
        assert_eq!(bottom_edge.placement, CompletionPanelPlacement::Above);
        assert_eq!(
            bottom_edge.rect.1 + bottom_edge.rect.3,
            70.0 + 38.0 * 16.0 - GAP
        );

        // Reversed metadata is normalized before either lane is measured.
        completion.command_rows = (3, 5);
        let forward = completion_panel_geometry(&completion, CELL).unwrap();
        completion.command_rows = (5, 3);
        let reversed = completion_panel_geometry(&completion, CELL).unwrap();
        assert_eq!(forward.rect, reversed.rect);
        assert_eq!(forward.placement, reversed.placement);

        completion.command_rows = (0, 39);
        assert!(
            completion_panel_geometry(&completion, CELL).is_none(),
            "a command spanning the pane leaves no safe lane on either side"
        );
    }

    #[test]
    fn a_top_edge_card_stays_below_pane_chrome_and_the_tab_bar() {
        let mut completion = overlay(None, 3);
        completion.command_rows = (0, 0);
        let geometry = completion_panel_geometry(&completion, CELL).unwrap();
        assert_eq!(geometry.placement, CompletionPanelPlacement::Below);
        assert_eq!(geometry.rect.1, completion.grid_rect.1 + CELL.1 + GAP);
        assert!(
            geometry.rect.1 >= completion.grid_rect.1,
            "placement is clipped to the terminal grid, never pane or tab chrome"
        );
    }

    #[test]
    fn card_declines_a_pane_too_small_for_one_row() {
        let mut completion = overlay(None, 1);
        completion.pane_rect = (0.0, 0.0, 60.0, 28.0);
        completion.grid_rect = (0.0, 0.0, 60.0, 28.0);
        assert!(completion_panel_geometry(&completion, CELL).is_none());

        // Tall enough, but narrower than the published minimum width.
        let mut narrow = overlay(None, 1);
        narrow.pane_rect = (0.0, 40.0, 152.0, 700.0);
        narrow.grid_rect = (0.0, 70.0, 152.0, 656.0);
        assert!(completion_panel_geometry(&narrow, CELL).is_none());

        narrow.pane_rect.2 = 160.0;
        narrow.grid_rect.2 = 160.0;
        assert!(
            completion_panel_geometry(&narrow, CELL).is_some(),
            "the former 20-column minimum remains usable"
        );
    }

    #[test]
    fn labels_keep_their_discriminating_path_tail() {
        let source = production_source();
        assert!(
            source.contains("middle_ellipsis(&candidate.label, geometry.label_columns)"),
            "completion labels must shed their middle, not their tail"
        );
        assert_eq!(
            super::middle_ellipsis("/home/kevim/Repos/kettle/crates/kettle-vt/src/lib.rs", 24),
            "/home/kevim/Repo…/lib.rs"
        );
    }

    #[test]
    fn emphasis_matches_only_the_first_safe_occurrence() {
        assert_eq!(completion_match_span("checkout", "ch"), Some((0, 2)));
        assert_eq!(
            completion_match_span("cherry-pick-cherry", "cherry"),
            Some((0, 6)),
            "only the first occurrence is emphasized"
        );
        assert_eq!(
            completion_match_span("CHECKOUT", "ch"),
            Some((0, 2)),
            "ASCII compares case-insensitively"
        );
        assert_eq!(completion_match_span("git-checkout", "CHECK"), Some((4, 9)));
        assert_eq!(completion_match_span("checkout", ""), None);
        assert_eq!(completion_match_span("ch", "checkout"), None);
        assert_eq!(completion_match_span("checkout", "zz"), None);
        // Non-ASCII on either side falls back to an exact substring so case
        // folding cannot desynchronize the span from the shaped bytes.
        assert_eq!(completion_match_span("Ünïcode", "Ünï"), Some((0, 5)));
        assert_eq!(completion_match_span("Ünïcode", "ünï"), None);
        let span = completion_match_span("café-münchen", "münchen").expect("exact tail");
        assert_eq!(&"café-münchen"[span.0..span.1], "münchen");
    }

    #[test]
    fn selection_surface_separates_from_the_panel_in_every_theme() {
        let panel = solid_blend(Rgb::new(205, 214, 244), Rgb::new(30, 30, 46), 9);
        let accent = Rgb::new(137, 180, 250);
        // A theme whose selection is already distinct is used verbatim.
        let distinct = Rgb::new(69, 71, 90);
        assert_eq!(
            completion_selection_surface(distinct, panel, accent),
            distinct
        );
        // A selection indistinguishable from the panel is replaced by an
        // accent blend that does separate.
        let flat = completion_selection_surface(panel, panel, accent);
        assert_ne!(flat, panel);
        assert!(color::contrast_ratio(flat, panel) >= 1.35);

        for (foreground, background) in [
            (Rgb::new(205, 214, 244), Rgb::new(30, 30, 46)),
            (Rgb::new(40, 42, 54), Rgb::new(250, 250, 250)),
        ] {
            let mut theme = theme();
            theme.foreground = foreground;
            theme.background = background;
            theme.selection_background = background;
            theme.selection_foreground = foreground;
            let palette = completion_palette(&theme, accent);
            assert!(color::contrast_ratio(palette.selection_bg, palette.panel_bg) >= 1.35);
            for (name, text, surface) in [
                ("label", palette.label, palette.panel_bg),
                ("header", palette.header, palette.panel_bg),
                ("emphasis", palette.emphasis, palette.panel_bg),
                (
                    "selected label",
                    palette.selected_label,
                    palette.selection_bg,
                ),
                (
                    "selected emphasis",
                    palette.selected_emphasis,
                    palette.selection_bg,
                ),
            ] {
                assert!(
                    color::contrast_ratio(text, surface) >= 4.49,
                    "{name} fell under 4.5 contrast"
                );
            }
            for (name, text, surface) in [
                ("description", palette.description, palette.panel_bg),
                (
                    "selected description",
                    palette.selected_description,
                    palette.selection_bg,
                ),
            ] {
                assert!(
                    color::contrast_ratio(text, surface) >= 3.99,
                    "{name} fell under 4.0 contrast"
                );
            }
        }
    }

    #[test]
    fn only_the_selected_row_carries_an_accent_rail() {
        let overlay = overlay(Some(0), 3);
        let geometry = completion_panel_geometry(&overlay, CELL).expect("completion geometry");
        let palette = completion_palette(&theme(), Rgb::new(137, 180, 250));
        let mut quads = Vec::new();

        push_completion_selection_quads(
            &mut quads,
            false,
            &geometry,
            geometry.list_top,
            0.0,
            &palette,
        );
        assert!(
            quads.is_empty(),
            "an unselected row must paint neither a selection surface nor a rail"
        );

        push_completion_selection_quads(
            &mut quads,
            true,
            &geometry,
            geometry.list_top,
            0.0,
            &palette,
        );
        assert_eq!(quads.len(), 2, "selection surface plus one accent rail");
        assert_eq!(quads[1].size, [super::COMPLETION_RAIL_W, geometry.row_h]);
        assert_eq!(quads[1].pos[1], geometry.list_top);
    }

    #[test]
    fn lifted_surface_moves_toward_foreground_in_both_theme_directions() {
        assert_eq!(
            solid_blend(Rgb::new(255, 255, 255), Rgb::new(0, 0, 0), 7),
            Rgb::new(18, 18, 18)
        );
        assert_eq!(
            solid_blend(Rgb::new(0, 0, 0), Rgb::new(255, 255, 255), 7),
            Rgb::new(237, 237, 237)
        );
    }

    #[test]
    fn a_static_completion_card_does_not_force_continuous_text_prepare() {
        let mut frame = Overlay {
            completion: Some(overlay(Some(0), 3)),
            ..Overlay::default()
        };
        assert!(!text_overlay_requires_continuous_prepare(&frame));

        frame.search_query = Some(String::new());
        assert!(text_overlay_requires_continuous_prepare(&frame));
    }
}

#[cfg(test)]
mod title_fit_tests {
    use super::{
        CONFIRM_BAR_MIN_CONTRAST, color, compose_confirm_bar_label, confirm_bar_text_color,
        display_width, fit_pane_titlebar_title, fit_single_line_label, fit_tab_path,
        fit_tab_segment_title, fit_tab_title, middle_ellipsis, overlay_label_cols,
        production_source,
    };

    /// A destructive confirmation has to be readable in EVERY bundled theme,
    /// not just the ones anyone happened to open.
    ///
    /// The bar paints `palette[1]` and used to draw the theme's ordinary
    /// foreground on it -- a color picked to contrast with the theme
    /// BACKGROUND. On the shipped TokyoNight Night default that is `#c0caf5`
    /// on `#f7768e`, about 1.6:1: the prompt, its buttons and its focus marker
    /// were all effectively invisible, so the question could not be answered
    /// and the window would not close. Iterate the whole bundled set, because
    /// the failure is per-theme and a single spot check is what missed it.
    #[test]
    fn confirm_bar_text_is_readable_in_every_bundled_theme() {
        let mut worst: Option<(&str, f64)> = None;
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let fg = confirm_bar_text_color(&theme);
            let ratio = color::contrast_ratio(fg, theme.palette[1]);
            assert!(
                ratio >= CONFIRM_BAR_MIN_CONTRAST,
                "confirm bar text is unreadable in theme {name:?}: {ratio:.2}:1 \
                 (fg {fg:?} on palette[1] {:?})",
                theme.palette[1]
            );
            if worst.is_none_or(|(_, w)| ratio < w) {
                worst = Some((name, ratio));
            }
        }
        let (name, ratio) = worst.expect("the bundled theme set must not be empty");
        assert!(
            ratio >= CONFIRM_BAR_MIN_CONTRAST,
            "worst bundled theme {name:?} at {ratio:.2}:1"
        );
    }

    /// The regression this fixes, pinned to the exact shipped default rather
    /// than to whatever `Theme::default()` happens to be: the raw theme
    /// foreground fails on the confirm bar, and the helper's output passes.
    #[test]
    fn tokyonight_night_confirm_bar_was_unreadable_before_the_lift() {
        let theme = kettle_config::Theme::by_name("TokyoNight Night");
        let raw = color::contrast_ratio(theme.foreground, theme.palette[1]);
        assert!(
            raw < CONFIRM_BAR_MIN_CONTRAST,
            "expected the shipped default's raw foreground to fail on palette[1], got {raw:.2}:1"
        );
        let lifted = color::contrast_ratio(confirm_bar_text_color(&theme), theme.palette[1]);
        assert!(
            lifted >= CONFIRM_BAR_MIN_CONTRAST,
            "confirm_bar_text_color did not reach AA on the shipped default: {lifted:.2}:1"
        );
    }

    /// Wiring guards. The two tests above prove `confirm_bar_text_color` and
    /// `bell-flash-intensity` compute the right values; neither proves the
    /// renderer USES them. Reverting the bar's text color to `theme.foreground`
    /// or hard-coding the bell peak back to a literal would leave every value
    /// test green, which is precisely the failure mode that shipped the
    /// unreadable bar in the first place.
    ///
    /// The bar's opacity is pinned here too, because the AA guarantee is
    /// computed against opaque `palette[1]`: a translucent bar composites over
    /// live terminal content and the real ratio drifts with the scrollback.
    #[test]
    fn the_renderer_actually_consumes_the_contrast_and_bell_helpers() {
        let src = production_source();
        assert!(
            src.contains("let bar_fg = confirm_bar_text_color(theme);"),
            "the confirm bar must take its text color from confirm_bar_text_color"
        );
        assert!(
            src.contains("search_text_color = Some(GColor::rgb(bar_fg.r, bar_fg.g, bar_fg.b));"),
            "the computed bar color must reach the shared bottom-bar text area"
        );
        assert!(
            src.contains("search_text_color.unwrap_or(GColor::rgb(fg.r, fg.g, fg.b))"),
            "the text area must prefer the per-overlay color over the theme foreground"
        );
        assert!(
            src.contains("sh - bar_h, sw, bar_h, theme.palette[1], 1.0)"),
            "the confirm bar must be painted opaque; the AA guarantee is \
             computed against palette[1] itself"
        );
        assert!(
            src.contains("overlay.bell * cfg.bell_flash_intensity"),
            "the visual bell peak must come from the config key, not a literal"
        );
    }

    /// The confirm bar must never clip its own button row.
    ///
    /// It composed to `floor(sw/cw)` and was then fitted to
    /// `overlay_label_cols(sw, cw)` = `floor(sw/cw) - 1`, so it overflowed by
    /// exactly one column at EVERY window size and `fit_single_line_label`
    /// dropped two columns for an ellipsis. The rightmost button therefore
    /// rendered as `[  Clos…` in every confirm dialog on every machine, and the
    /// click target from `confirm_dialog_button_hit` extended past the last
    /// painted glyph. Nothing caught it because the composition had no test at
    /// all -- it was inline in `redraw`, unreachable without a GPU device.
    #[test]
    fn confirm_bar_never_clips_its_button_row() {
        let buttons = "[▶ Cancel]  [  Close]";
        // Real geometries: 800/8.0, 1512/7.8 (this laptop), 1920/9.0, 640/8.0.
        for (sw, cw) in [
            (800.0f32, 8.0f32),
            (1512.0, 7.8),
            (1920.0, 9.0),
            (640.0, 8.0),
        ] {
            let cols = overlay_label_cols(sw, cw);
            let label = compose_confirm_bar_label(
                "  ⚠ Close this pane?",
                "  Tab/←→ · Enter · Esc",
                buttons,
                cols,
            );
            assert!(
                label.ends_with(buttons),
                "confirm bar clipped its buttons at {sw}x{cw} (cols={cols}): {label:?}"
            );
            assert!(
                !label.contains('…'),
                "confirm bar was ellipsised at {sw}x{cw}: {label:?}"
            );
            assert!(
                display_width(&label) <= cols,
                "confirm bar overflowed its budget at {sw}x{cw}: {} > {cols}",
                display_width(&label)
            );
        }
    }

    /// A genuinely narrow window must keep the interactive row intact or paint
    /// none of it. A clipped button row would disagree with the App hit-test.
    #[test]
    fn confirm_bar_still_fits_when_the_window_cannot_hold_the_buttons() {
        for buttons in ["[▶ Cancel]  [  Close]", "[▶ OK]"] {
            let buttons_cols = display_width(buttons);
            for cols in 0..buttons_cols {
                assert_eq!(
                    compose_confirm_bar_label("  ⚠ Close this pane?", "  Tab", buttons, cols),
                    "",
                    "a {cols}-column bar must not paint a partial {buttons_cols}-column button row"
                );
            }
            for cols in buttons_cols..=buttons_cols + 2 {
                let label =
                    compose_confirm_bar_label("  ⚠ Close this pane?", "  Tab", buttons, cols);
                assert!(
                    label.ends_with(buttons),
                    "the complete button row must win at cols={cols}: {label:?}"
                );
                assert_eq!(
                    display_width(&label),
                    cols,
                    "the intact button row must remain right-aligned"
                );
            }
        }
    }

    #[test]
    fn confirm_bar_uses_display_columns_for_wide_button_labels() {
        let prompt = "P";
        let buttons = "[▶ Cancel]  [  界]";
        let cols = display_width(prompt) + 2 + display_width(buttons);
        let label = compose_confirm_bar_label(prompt, "", buttons, cols);

        assert_eq!(display_width(&label), cols);
        assert!(
            label.ends_with(buttons),
            "a wide label must not make the fitter clip the button row: {label:?}"
        );
    }

    #[test]
    fn single_line_overlay_labels_fit_without_wrapping() {
        assert_eq!(overlay_label_cols(800.0, 8.0), 99);
        assert_eq!(overlay_label_cols(0.0, 8.0), 0);
        assert_eq!(overlay_label_cols(800.0, 0.0), 0);

        let fitted = fit_single_line_label("  ⌘ query_ ▸ 中文 command and controls", 20);
        assert_eq!(display_width(&fitted), 20);
        assert!(fitted.starts_with("  ⌘ query_"));
        assert!(fitted.ends_with('…'));
        assert_eq!(fit_single_line_label("abc", 3), "abc");
        assert_eq!(fit_single_line_label("abc", 1), "…");
        assert_eq!(fit_single_line_label("abc", 0), "");
    }

    #[test]
    fn middle_ellipsis_fits_and_keeps_both_ends() {
        // Fits unchanged.
        assert_eq!(middle_ellipsis("hello", 10), "hello");
        assert_eq!(middle_ellipsis("hello", 5), "hello");
        // A path keeps the LEAF (program name) whole + the drive root.
        let p = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
        let cut = middle_ellipsis(p, 16);
        assert!(cut.contains('…'), "must ellipsize: {cut}");
        assert!(cut.ends_with("pwsh.exe"), "leaf must survive: {cut}");
        assert!(cut.starts_with("C:\\"), "drive root must survive: {cut}");
        assert!(display_width(&cut) <= 16, "must fit the budget: {cut}");
    }

    #[test]
    fn middle_ellipsis_falls_back_to_symmetric_for_non_paths() {
        let s = "abcdefghijklmnopqrstuvwxyz"; // 26 cols, no separators
        let cut = middle_ellipsis(s, 11);
        assert!(cut.contains('…'));
        assert!(cut.starts_with('a'), "front kept: {cut}");
        assert!(cut.ends_with('z'), "back kept: {cut}");
        assert!(display_width(&cut) <= 11);
    }

    #[test]
    fn middle_ellipsis_respects_display_columns_not_chars() {
        // CJK: each char is 2 cells. "中文中文" = 8 cells; budget 8 fits.
        assert_eq!(middle_ellipsis("中文中文", 8), "中文中文");
        // Overflow never exceeds the column budget.
        for n in 0..=8 {
            assert!(display_width(&middle_ellipsis("中文中文路径", n)) <= n);
        }
        assert_eq!(middle_ellipsis("a", 0), "");
        assert_eq!(middle_ellipsis("abc", 1), "…");
    }

    #[test]
    fn tab_title_keeps_full_path_when_budget_fits() {
        let title = "~/Repos/SPT-1/flight-event-line-server-go";
        assert_eq!(fit_tab_title(title, display_width(title)), title);
        assert_eq!(fit_tab_title(title, display_width(title) + 10), title);
    }

    #[test]
    fn tab_title_preserves_posix_path_tail_with_ascii_ellipsis() {
        let title = "~/Repos/SPT-1/flight-event-line-server-go";
        let cut = fit_tab_title(title, 30);
        assert_eq!(cut, "...flight-event-line-server-go");
        assert!(cut.ends_with("flight-event-line-server-go"));
        assert!(cut.starts_with("..."));
        assert!(
            !cut.contains('…'),
            "tab path truncation uses ASCII dots: {cut}"
        );
        assert!(display_width(&cut) <= 30);
    }

    #[test]
    fn tab_title_preserves_windows_path_tail_with_ascii_ellipsis() {
        let title = r"C:\src\proj\flight-event-line-server-go";
        let cut = fit_tab_title(title, 30);
        assert_eq!(cut, r"...flight-event-line-server-go");
        assert!(cut.ends_with(r"flight-event-line-server-go"));
        assert!(cut.starts_with("..."));
        assert!(display_width(&cut) <= 30);
    }

    #[test]
    fn tab_title_uses_tail_only_for_tiny_path_budget() {
        let title = "~/Repos/SPT-1/flight-event-line-server-go";
        assert_eq!(fit_tab_title(title, 2), "go");
        assert_eq!(fit_tab_title(title, 1), "o");
        assert_eq!(fit_tab_title(title, 0), "");
    }

    #[test]
    fn tab_title_keeps_middle_ellipsis_for_non_paths() {
        let title = "abcdefghijklmnopqrstuvwxyz";
        let cut = fit_tab_title(title, 11);
        assert!(cut.contains('…'));
        assert!(cut.starts_with('a'), "front kept: {cut}");
        assert!(cut.ends_with('z'), "back kept: {cut}");
        assert!(display_width(&cut) <= 11);
    }

    #[test]
    fn fit_tab_path_tiers_full_then_leaf_then_tail() {
        let path = "~/Repos/kettle/crates/kettle-ui";
        // Tier 1: the whole path when it fits.
        assert_eq!(fit_tab_path(path, display_width(path)), path);
        assert_eq!(fit_tab_path(path, display_width(path) + 5), path);
        // Tier 2: the leaf dir name alone when the full path doesn't fit.
        assert_eq!(fit_tab_path(path, 12), "kettle-ui");
        // Tier 3: the tail of the leaf with a leading ellipsis when even the leaf
        // overflows.
        let cut = fit_tab_path(path, 6);
        assert!(cut.starts_with('…'), "tail marked: {cut}");
        assert!(cut.ends_with("ui"), "tail kept: {cut}");
        assert!(display_width(&cut) <= 6);
        // Degenerate widths.
        assert_eq!(fit_tab_path(path, 1), "…");
        assert_eq!(fit_tab_path(path, 0), "");
        // Backslash separators tier the same way.
        assert_eq!(fit_tab_path("~\\Repos\\kettle", 8), "kettle");
    }

    #[test]
    fn fit_tab_path_keeps_long_leaf_when_it_fits() {
        let path = "~/Repos/SPI-1/flight-event-line-server-go";
        let leaf = "flight-event-line-server-go";
        assert_eq!(fit_tab_path(path, display_width(leaf)), leaf);
        assert!(
            !fit_tab_path(path, display_width(leaf)).starts_with('…'),
            "leaf fits, so no leading ellipsis is needed"
        );
    }

    #[test]
    fn fit_tab_segment_title_uses_render_budget_and_path_tiers() {
        let fmt = "{n}: {title}";
        let path = "~/Repos/SPI-1/platform";
        let leaf = "platform";
        let cell = 10.0;
        let lane_for_cols = |cols: usize| cols as f32 * cell;
        let fixed = display_width("1: ");

        assert_eq!(
            fit_tab_segment_title(
                leaf,
                Some(path),
                0,
                fmt,
                lane_for_cols(fixed + display_width(path)),
                cell
            ),
            path
        );
        assert_eq!(
            fit_tab_segment_title(
                leaf,
                Some(path),
                0,
                fmt,
                lane_for_cols(fixed + display_width(leaf)),
                cell
            ),
            leaf
        );
        assert_eq!(
            fit_tab_segment_title(leaf, Some(path), 0, fmt, lane_for_cols(fixed + 5), cell),
            "…form"
        );
    }

    #[test]
    fn pane_title_sheds_size_then_group_then_ellipsizes() {
        let long = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
        // Wide budget: everything shows, including size + group.
        let wide =
            fit_pane_titlebar_title(Some("fleet"), "", long, None, Some("120x60"), None, 120);
        assert!(wide.contains("[fleet]") && wide.contains("120x60") && wide.contains(long));
        // Medium: size text is dropped first, group + full title still fit.
        let med_budget = display_width("  [fleet]  ") + long.chars().count() + 1;
        let med = fit_pane_titlebar_title(
            Some("fleet"),
            "",
            long,
            None,
            Some("120x60"),
            None,
            med_budget,
        );
        assert!(med.contains(long), "title intact: {med}");
        assert!(!med.contains("120x60"), "size shed first: {med}");
        // Narrow: group dropped too, and the title middle-ellipsized to the leaf.
        let narrow =
            fit_pane_titlebar_title(Some("fleet"), "", long, None, Some("120x60"), None, 18);
        assert!(!narrow.contains("[fleet]"), "group shed: {narrow}");
        assert!(narrow.ends_with("pwsh.exe"), "leaf survives: {narrow}");
        assert!(display_width(&narrow) <= 18, "fits budget: {narrow}");
    }

    #[test]
    fn pane_title_keeps_the_bell_through_shedding() {
        let long = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
        let narrow = fit_pane_titlebar_title(
            Some("fleet"),
            "",
            long,
            None,
            Some("120x60"),
            Some("\u{1F514}"),
            20,
        );
        assert!(narrow.contains('\u{1F514}'), "bell must survive: {narrow}");
    }

    #[test]
    fn pane_title_uses_path_tiers_after_metadata_sheds() {
        let raw = "..ine-server-go";
        let path = "~/Repos/SPI-1/flight-event-line-server-go";
        let leaf = "flight-event-line-server-go";
        let wide_budget = display_width("  [RO] ") + display_width(path);
        let wide = fit_pane_titlebar_title(
            None,
            "[RO] ",
            raw,
            Some(path),
            Some("131x30"),
            None,
            wide_budget,
        );
        assert_eq!(wide, format!("  [RO] {path}"));

        let leaf_budget = display_width("  [RO] ") + display_width(leaf);
        let leaf_fit = fit_pane_titlebar_title(
            None,
            "[RO] ",
            raw,
            Some(path),
            Some("131x30"),
            None,
            leaf_budget,
        );
        assert_eq!(leaf_fit, format!("  [RO] {leaf}"));

        let tail = fit_pane_titlebar_title(None, "", raw, Some(path), None, None, 12);
        assert_eq!(tail, "  …server-go");
    }
}

#[cfg(test)]
mod settings_hit_test_tests {
    use super::{
        SETTINGS_FIELD_START, SettingsHit, SettingsOverlay, SettingsRow, settings_display_lines,
        settings_hit_test, settings_panel_cols,
    };

    fn overlay() -> SettingsOverlay {
        SettingsOverlay {
            categories: vec!["Appearance".into(), "Graphics".into(), "Behavior".into()],
            active_category: 0,
            rows: vec![
                SettingsRow {
                    label: "Theme".into(),
                    value: "Mocha".into(),
                    disabled: false,
                },
                SettingsRow {
                    label: "Font size".into(),
                    value: "14".into(),
                    disabled: false,
                },
            ],
            focused_row: 0,
            vim_nav: false,
            footer_note: None,
        }
    }

    // Recompute the panel geometry the same way the draw + hit-test do, so the
    // probes target real on-screen positions.
    fn geom(set: &SettingsOverlay, cw: f32, ch: f32, sw: f32, sh: f32) -> (f32, f32, f32) {
        let lines = settings_display_lines(set);
        let row_h = ch + 6.0;
        let panel_w = (settings_panel_cols(&lines) * cw + 48.0).min((sw - 40.0).max(120.0));
        let panel_h = (lines.len() as f32 * row_h + 24.0).min((sh - 40.0).max(80.0));
        let px = ((sw - panel_w) * 0.5).max(0.0);
        let py = ((sh - panel_h) * 0.5).max(0.0);
        (px, py, row_h)
    }

    #[test]
    fn outside_the_panel_dismisses() {
        let set = overlay();
        assert_eq!(
            settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, 2.0, 2.0),
            SettingsHit::Outside
        );
    }

    #[test]
    fn field_rows_map_by_index() {
        let set = overlay();
        let (px, py, row_h) = geom(&set, 8.0, 16.0, 800.0, 600.0);
        for f in 0..set.rows.len() {
            let line = SETTINGS_FIELD_START + f;
            let y = py + 12.0 + line as f32 * row_h + row_h * 0.5;
            assert_eq!(
                settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 40.0, y),
                SettingsHit::Field(f),
                "field {f} at y={y}"
            );
        }
    }

    #[test]
    fn title_and_blank_rows_are_inert() {
        let set = overlay();
        let (px, py, row_h) = geom(&set, 8.0, 16.0, 800.0, 600.0);
        // Line 0 = title.
        let y0 = py + 12.0 + row_h * 0.5;
        assert_eq!(
            settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 40.0, y0),
            SettingsHit::Inert
        );
    }

    #[test]
    fn category_tabs_map_by_x() {
        let set = overlay();
        let (px, py, row_h) = geom(&set, 8.0, 16.0, 800.0, 600.0);
        let y = py + 12.0 + row_h * 1.5; // line 1 = tab strip
        // Tab 0 "[ Appearance ]" starts at text_left = px + 16.
        let hit0 = settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, px + 16.0 + 4.0, y);
        assert_eq!(hit0, SettingsHit::Category(0));
        // Tab 1 "  Graphics  " begins after tab0 (14 cols) + 1 separator = col 15.
        let x1 = px + 16.0 + (15.0 + 3.0) * 8.0;
        assert_eq!(
            settings_hit_test(&set, 8.0, 16.0, 800.0, 600.0, x1, y),
            SettingsHit::Category(1)
        );
    }
}

#[cfg(test)]
mod update_banner_top_tests {
    use super::{
        color, update_banner_chrome_colors, update_banner_top, update_banner_top_with_reserved,
    };

    /// Drift guard (audit). The passive update banner must stack
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
        // Rich Search stays bottommost; the banner also clears its responsive
        // reserved lane while preserving the legacy helper above.
        assert_eq!(
            update_banner_top_with_reserved(1000.0, 30.0, 28.0, 20.0, 60.0),
            862.0
        );
    }

    #[test]
    fn chrome_colors_are_readable_without_full_green_background() {
        let theme = kettle_config::Theme::default();
        let (bg, accent) = update_banner_chrome_colors(&theme);

        assert_ne!(
            bg, theme.palette[2],
            "the full banner background must not be the bright green update accent"
        );
        assert!(
            color::contrast_ratio(bg, theme.foreground) + 1e-6 >= 4.5,
            "banner text contrast should meet WCAG AA; bg={bg:?} fg={:?} ratio={}",
            theme.foreground,
            color::contrast_ratio(bg, theme.foreground)
        );
        assert!(
            color::contrast_ratio(accent, bg) + 1e-6 >= 3.0,
            "the accent strip should remain visible against the banner background"
        );
    }
}

#[cfg(test)]
mod background_image_geometry_tests {
    use super::{background_image_rect, rect_covers_surface};

    #[test]
    fn oversized_center_wallpaper_keeps_natural_size_and_crops() {
        assert_eq!(
            background_image_rect("center", "center", "middle", [100.0, 80.0], [140.0, 120.0]),
            [-20.0, -20.0, 140.0, 120.0]
        );
        assert_eq!(
            background_image_rect("center", "right", "bottom", [100.0, 80.0], [140.0, 120.0]),
            [-40.0, -40.0, 140.0, 120.0]
        );
    }

    #[test]
    fn wallpaper_coverage_requires_all_four_surface_edges() {
        let surface = [100.0, 80.0];
        assert!(rect_covers_surface([0.0, 0.0, 100.0, 80.0], surface));
        assert!(rect_covers_surface([-20.0, -10.0, 140.0, 100.0], surface));
        for rect in [
            [1.0, 0.0, 100.0, 80.0],
            [0.0, 1.0, 100.0, 80.0],
            [0.0, 0.0, 99.0, 80.0],
            [0.0, 0.0, 100.0, 79.0],
        ] {
            assert!(
                !rect_covers_surface(rect, surface),
                "a wallpaper missing any surface edge is not an opaque base: {rect:?}"
            );
        }
    }
}

#[cfg(test)]
mod inline_placement_budget_tests {
    use super::{
        PaneSnapshot, fair_placement_quotas, inline_image_clip, inline_placement_rect,
        pane_backdrop_rect, pane_grid_origin, placement_is_visible, placement_viewport_row,
    };
    use kettle_core::{ImageData, Placement};

    fn placement_at(abs_line: u64) -> Placement {
        Placement {
            abs_line,
            col: 0,
            cell_cols: 1,
            cell_rows: 1,
            x_offset_cells: 0.0,
            y_offset_cells: 0.0,
            display_cols: 1.0,
            display_rows: 1.0,
            img: ImageData::new(1, 1, vec![0, 0, 0, 0]).expect("pixel"),
            source_rect: None,
            source_crop: None,
            id: Some(1),
            placement_id: 0,
            kitty_params: None,
            z: 0,
        }
    }

    #[test]
    fn busy_early_pane_cannot_starve_later_panes() {
        assert_eq!(fair_placement_quotas(&[256, 1], 256), vec![255, 1]);
        assert_eq!(
            fair_placement_quotas(&[256, 256, 256, 256], 256),
            vec![64, 64, 64, 64]
        );
        assert_eq!(fair_placement_quotas(&[0, 3, 100], 4), vec![0, 2, 2]);
    }

    #[test]
    fn quotas_are_bounded_complete_and_deterministic() {
        for (counts, limit) in [
            (vec![], 10),
            (vec![0, 0], 10),
            (vec![1, 2, 3], 0),
            (vec![1, 2, 3], 99),
            (vec![5, 1, 8, 2], 9),
        ] {
            let first = fair_placement_quotas(&counts, limit);
            assert_eq!(first, fair_placement_quotas(&counts, limit));
            assert!(
                first
                    .iter()
                    .zip(&counts)
                    .all(|(quota, count)| quota <= count)
            );
            assert_eq!(
                first.iter().sum::<usize>(),
                counts.iter().sum::<usize>().min(limit)
            );
        }
    }

    #[test]
    fn kitty_offsets_and_fractional_extent_reach_the_gpu_destination() {
        let placement = Placement {
            abs_line: 12,
            col: 3,
            cell_cols: 3,
            cell_rows: 2,
            x_offset_cells: 0.25,
            y_offset_cells: 0.5,
            display_cols: 2.75,
            display_rows: 1.5,
            img: ImageData::new(1, 1, vec![0, 0, 0, 0]).expect("pixel"),
            source_rect: None,
            source_crop: None,
            id: Some(1),
            placement_id: 2,
            kitty_params: None,
            z: 0,
        };
        assert_eq!(
            inline_placement_rect(100.0, 200.0, 2, 10.0, 20.0, &placement),
            (132.5, 250.0, 27.5, 30.0)
        );
    }

    #[test]
    fn monotonic_history_origin_prevents_capped_scrollback_aliasing() {
        let mut snap = PaneSnapshot {
            history_origin: 100,
            history_size: 3,
            display_offset: 0,
            screen_lines: 3,
            ..PaneSnapshot::default()
        };
        let current = placement_at(103);
        assert!(placement_is_visible(&snap, &current));
        assert_eq!(placement_viewport_row(&snap, &current), Some(0));

        let stale_legacy_coordinate = placement_at(3);
        assert!(!placement_is_visible(&snap, &stale_legacy_coordinate));
        assert_eq!(
            placement_viewport_row(&snap, &stale_legacy_coordinate),
            Some(-100)
        );

        snap.display_offset = 2;
        let retained_history = placement_at(101);
        assert!(placement_is_visible(&snap, &retained_history));
        assert_eq!(placement_viewport_row(&snap, &retained_history), Some(0));
    }

    #[test]
    fn empty_viewport_never_admits_an_image_placement() {
        let snap = PaneSnapshot {
            history_origin: 100,
            history_size: 3,
            display_offset: 0,
            screen_lines: 0,
            ..PaneSnapshot::default()
        };
        let mut spanning = placement_at(100);
        spanning.cell_rows = 10;
        assert!(!placement_is_visible(&snap, &spanning));
    }

    #[test]
    fn inline_image_clip_excludes_padding_titlebar_and_pane_edges() {
        let pane = (10.0, 30.0, 200.0, 120.0);
        let padding = (8.0, 8.0);
        let titlebar_h = 20.0;
        // Pane interior after a 1px border and 20px top titlebar.
        let pane_body = pane_backdrop_rect(pane, 1.0, titlebar_h, false).expect("pane body");
        let top_origin = pane_grid_origin(pane, padding, titlebar_h, false);
        // The grid starts another 8px inside the pane and is wider/taller than
        // the remaining body. The intersection must start at the grid (so no
        // padding/titlebar paint) and end at the pane body (so no sibling/chrome
        // bleed).
        assert_eq!(
            inline_image_clip(pane_body, top_origin, (30, 10), (8.0, 16.0)),
            Some([18.0, 58.0, 191.0, 91.0])
        );
        assert_eq!(
            inline_image_clip(pane_body, top_origin, (30, 0), (8.0, 16.0)),
            None
        );

        let bottom_title_body =
            pane_backdrop_rect(pane, 1.0, titlebar_h, true).expect("bottom-title pane body");
        let bottom_origin = pane_grid_origin(pane, padding, titlebar_h, true);
        assert_eq!(
            inline_image_clip(bottom_title_body, bottom_origin, (30, 10), (8.0, 16.0)),
            Some([18.0, 38.0, 191.0, 91.0]),
            "moving the titlebar must translate the grid and clip together, \
             not shrink the drawable image viewport"
        );
    }
}

#[cfg(test)]
mod attributed_foreground_tests {
    use super::{CellHighlight, Rgb, attributed_foreground, color, resolved_cell_foreground};
    use kettle_config::{Config, Theme};

    /// `minimum-contrast` must survive `bold-is-bright`.
    ///
    /// The lift ran first and `bold_is_bright` then replaced the foreground
    /// outright with a palette entry, so the guarantee silently did nothing for
    /// bold text whenever `bold-is-bright` was on — the common configuration.
    /// And because the bright variant is the LIGHTER one, the case it discarded
    /// is exactly the one that needed it.
    #[test]
    fn the_contrast_lift_sees_the_colour_bold_is_bright_actually_produces() {
        let mut theme = Theme {
            background: Rgb::new(0xe8, 0xe8, 0xe8),
            ..Theme::default()
        };
        // A pale background, a low-palette colour that ALREADY meets the
        // ratio against it (9.44:1), and a bright variant that badly does not
        // (1.01:1).
        //
        // The base colour being compliant is what makes this load-bearing. The
        // lift is then a no-op, so running it before the remap leaves the remap
        // free to replace the result with something unreadable. A fixture whose
        // base is NON-compliant cannot show that: the lift moves the colour off
        // the palette entry, and the remap — which matches palette entries
        // exactly — finds nothing to remap and quietly does nothing.
        theme.palette[2] = Rgb::new(0x20, 0x40, 0x20);
        theme.palette[10] = Rgb::new(0xd8, 0xf0, 0xd8);
        let bg = theme.background;
        let fg = theme.palette[2];

        let cfg = Config {
            bold_is_bright: true,
            minimum_contrast: 4.5,
            ..Config::default()
        };

        // The bright variant on its own is far below the requested ratio —
        // otherwise this fixture would prove nothing.
        let bright = color::bright_for_bold(fg, &theme);
        assert_eq!(bright, theme.palette[10], "the fixture must actually remap");
        assert!(
            color::contrast_ratio(bright, bg) < 4.5,
            "the bright variant must start unreadable, got {:.2}",
            color::contrast_ratio(bright, bg)
        );

        let drawn = attributed_foreground(fg, bg, false, true, &cfg, &theme);
        assert!(
            color::contrast_ratio(drawn, bg) >= 4.5 - 1e-6,
            "minimum-contrast must hold for bold text under bold-is-bright, \
             got {:.2} from {drawn:?}",
            color::contrast_ratio(drawn, bg)
        );

        // Non-bold skips the remap, so the compliant base colour is left as
        // it is rather than lifted away from what the theme asked for.
        assert!(
            color::contrast_ratio(fg, bg) >= 4.5,
            "the fixture's base colour must already be compliant, got {:.2}",
            color::contrast_ratio(fg, bg)
        );
        assert_eq!(
            attributed_foreground(fg, bg, false, false, &cfg, &theme),
            fg,
            "a compliant non-bold colour must be left alone"
        );

        // With the lift off, bold-is-bright still does its job untouched.
        let no_lift = Config {
            bold_is_bright: true,
            ..Config::default()
        };
        assert_eq!(
            attributed_foreground(fg, bg, false, true, &no_lift, &theme),
            bright,
            "with minimum-contrast off, the bright variant must be what is drawn"
        );

        // Dim composes first, and the lift still applies to its result.
        let dimmed = attributed_foreground(fg, bg, true, false, &no_lift, &theme);
        assert_eq!(
            dimmed,
            color::dim(fg, bg),
            "dim must apply to the resolved fg"
        );
        assert!(
            color::contrast_ratio(attributed_foreground(fg, bg, true, false, &cfg, &theme), bg)
                >= 4.5 - 1e-6,
            "a dimmed foreground must still be lifted to the configured ratio"
        );
    }

    #[test]
    fn minimum_contrast_uses_the_background_painted_under_highlights() {
        let mut theme = Theme {
            background: Rgb::new(0, 0, 0),
            foreground: Rgb::new(255, 255, 255),
            selection_background: Rgb::new(255, 255, 255),
            selection_foreground: Rgb::new(255, 255, 255),
            ..Theme::default()
        };
        theme.palette[3] = Rgb::new(255, 255, 255);
        let cfg = Config {
            minimum_contrast: 4.5,
            search_background: Some(Rgb::new(255, 255, 255)),
            search_foreground: Some(Rgb::new(255, 255, 255)),
            ..Config::default()
        };
        let base_fg = Rgb::new(255, 255, 255);
        let base_bg = Rgb::new(0, 0, 0);

        let ordinary = resolved_cell_foreground(
            base_fg,
            base_bg,
            CellHighlight::None,
            false,
            false,
            &cfg,
            &theme,
        );
        assert_eq!(ordinary, base_fg);

        for (name, highlight, painted_bg) in [
            (
                "selection",
                CellHighlight::Selection,
                theme.selection_background,
            ),
            (
                "inactive search",
                CellHighlight::Search(false),
                theme.selection_background,
            ),
            (
                "active search",
                CellHighlight::Search(true),
                cfg.search_background.expect("search background"),
            ),
        ] {
            let drawn =
                resolved_cell_foreground(base_fg, base_bg, highlight, false, false, &cfg, &theme);
            assert!(
                color::contrast_ratio(drawn, painted_bg) >= 4.5 - 1e-6,
                "{name} must meet contrast against its final painted background; got {drawn:?}"
            );
        }
    }

    #[test]
    fn search_and_selection_foregrounds_ignore_underlying_dim_and_bold_attributes() {
        let mut theme = Theme {
            selection_background: Rgb::new(10, 20, 30),
            selection_foreground: Rgb::new(90, 100, 110),
            ..Theme::default()
        };
        // If BOLD reaches the highlight foreground, bold-is-bright remaps this
        // exact search colour to the deliberately distinct bright slot.
        theme.palette[2] = Rgb::new(20, 80, 20);
        theme.palette[10] = Rgb::new(180, 250, 180);
        let cfg = Config {
            bold_is_bright: true,
            search_background: Some(Rgb::new(40, 50, 60)),
            search_foreground: Some(theme.palette[2]),
            ..Config::default()
        };
        let base_fg = Rgb::new(200, 210, 220);
        let base_bg = Rgb::new(1, 2, 3);

        for (name, highlight, expected) in [
            (
                "active search",
                CellHighlight::Search(true),
                cfg.search_foreground.expect("search foreground"),
            ),
            (
                "inactive search",
                CellHighlight::Search(false),
                theme.selection_foreground,
            ),
            (
                "selection",
                CellHighlight::Selection,
                theme.selection_foreground,
            ),
        ] {
            for (dim, bold) in [(false, false), (true, false), (false, true), (true, true)] {
                assert_eq!(
                    resolved_cell_foreground(base_fg, base_bg, highlight, dim, bold, &cfg, &theme,),
                    expected,
                    "{name} must override the cell's DIM={dim} BOLD={bold} attributes"
                );
            }
        }

        assert_ne!(
            color::dim(
                cfg.search_foreground.unwrap(),
                cfg.search_background.unwrap()
            ),
            cfg.search_foreground.unwrap(),
            "the DIM fixture must visibly change the configured search foreground"
        );
        assert_ne!(
            color::bright_for_bold(cfg.search_foreground.unwrap(), &theme),
            cfg.search_foreground.unwrap(),
            "the bold-is-bright fixture must visibly remap the configured search foreground"
        );
    }
}

#[cfg(test)]
mod background_darkness_tests {
    use super::{
        QuadInstance, apply_quad_alpha_floor, composed_bg_alpha, desired_alpha_mode,
        final_scene_is_uniformly_opaque, live_underlay_clear_color,
        needs_postmultiplied_presentation, use_live_pane_bases, window_requires_alpha_surface,
    };
    use kettle_config::{BackgroundType, Config};

    #[test]
    fn unsupported_blur_floor_applies_only_to_the_live_window_underlay() {
        let background = kettle_config::Rgb::new(26, 27, 38);
        let live = live_underlay_clear_color(
            background,
            Some(0.99),
            wgpu::CompositeAlphaMode::PreMultiplied,
            true,
        );
        assert!((live.a - 0.99).abs() < 1e-6);
        assert!(live.r > 0.0 && live.g > 0.0 && live.b > 0.0);

        for offscreen in [
            live_underlay_clear_color(
                background,
                Some(0.99),
                wgpu::CompositeAlphaMode::PreMultiplied,
                false,
            ),
            live_underlay_clear_color(
                background,
                None,
                wgpu::CompositeAlphaMode::PreMultiplied,
                true,
            ),
        ] {
            assert_eq!(offscreen.a, 0.0);
            assert_eq!(offscreen.r, 0.0);
            assert_eq!(offscreen.g, 0.0);
            assert_eq!(offscreen.b, 0.0);
        }
    }

    #[test]
    fn unsupported_blur_floor_survives_the_replacing_pane_base_pass() {
        let mut bases = [QuadInstance {
            pos: [0.0, 0.0],
            size: [100.0, 100.0],
            color: [0.1, 0.2, 0.3, 0.55],
        }];
        apply_quad_alpha_floor(&mut bases, 0.99);
        assert!((bases[0].color[3] - 0.99).abs() < f32::EPSILON);

        bases[0].color[3] = 1.0;
        apply_quad_alpha_floor(&mut bases, 0.99);
        assert_eq!(bases[0].color[3], 1.0, "the floor must never reduce alpha");

        assert!(use_live_pane_bases(true, Some(0.99)));
        assert!(!use_live_pane_bases(false, Some(0.99)));
        assert!(!use_live_pane_bases(true, None));
    }

    /// `background-darkness` runs see-through → covered, and the docs must say
    /// so.
    ///
    /// Terminator assigns this value straight to the background colour's alpha,
    /// and its users lower it to get MORE transparency. Both `docs/CONFIG.md`
    /// and the field's own doc comment described the scale backwards — "1.0 =
    /// no tint, 0.0 = fully dark" — so anyone configuring kettle from its
    /// documentation reached for the wrong end. The code was right; the prose
    /// was not, and nothing tied the two together.
    #[test]
    fn darkness_scales_the_backdrop_toward_see_through() {
        let with = |background_type, darkness, opacity| {
            composed_bg_alpha(&Config {
                background_type,
                background_darkness: darkness,
                background_opacity: opacity,
                ..Config::default()
            })
        };

        for background_type in [
            BackgroundType::Transparent,
            BackgroundType::Image,
            BackgroundType::Starfield,
        ] {
            // 0.0 paints nothing over the backdrop: fully see-through.
            assert_eq!(with(background_type, 0.0, 1.0), 0.0, "{background_type:?}");
            // 1.0 paints the terminal background at full opacity: covered.
            assert_eq!(with(background_type, 1.0, 1.0), 1.0, "{background_type:?}");
            // And it is monotonic in between, not inverted or clamped flat.
            let quarter = with(background_type, 0.25, 1.0);
            let half = with(background_type, 0.5, 1.0);
            assert!(
                0.0 < quarter && quarter < half && half < 1.0,
                "{background_type:?}: darkness must increase coverage, got \
                 0.25 -> {quarter}, 0.5 -> {half}"
            );
            // It composes with background-opacity rather than replacing it.
            assert!(
                (with(background_type, 0.5, 0.5) - 0.25).abs() < 1e-9,
                "{background_type:?}: darkness and opacity must multiply"
            );
        }

        // A solid background ignores darkness entirely — there is no backdrop
        // for it to reveal. (Compared with a tolerance: the config stores these
        // as `f32` and the alpha is `f64`, so 0.8 does not round-trip exactly.)
        for darkness in [0.0, 0.5, 1.0] {
            assert!(
                (with(BackgroundType::Solid, darkness, 0.8) - 0.8).abs() < 1e-6,
                "a solid background must not consult darkness"
            );
        }
    }

    #[test]
    fn effective_alpha_mode_tracks_darkness_and_live_reload() {
        let macos_modes = [
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PostMultiplied,
        ];
        let opaque = Config {
            background_type: BackgroundType::Transparent,
            background_opacity: 1.0,
            background_darkness: 1.0,
            ..Config::default()
        };
        assert_eq!(
            desired_alpha_mode(&opaque, &macos_modes),
            wgpu::CompositeAlphaMode::Opaque
        );

        let transparent = Config {
            background_darkness: 0.5,
            ..opaque.clone()
        };
        assert_eq!(
            desired_alpha_mode(&transparent, &macos_modes),
            wgpu::CompositeAlphaMode::PostMultiplied,
            "effective opacity, not background-opacity alone, must select the surface mode"
        );
        assert!(
            window_requires_alpha_surface(&transparent),
            "transparent backgrounds must create an alpha-capable OS window"
        );

        let image = Config {
            background_type: BackgroundType::Image,
            background_opacity: 1.0,
            background_darkness: 1.0,
            ..Config::default()
        };
        assert!(
            window_requires_alpha_surface(&image),
            "wallpaper alpha is not known until decode, so image windows must be alpha-capable"
        );

        let starfield = Config {
            background_type: BackgroundType::Starfield,
            background_opacity: 1.0,
            background_darkness: 0.5,
            ..Config::default()
        };
        assert!(
            !window_requires_alpha_surface(&starfield),
            "the starfield shader covers every surface pixel with alpha 1"
        );

        let reloaded = Config {
            background_type: BackgroundType::Solid,
            background_opacity: 0.5,
            ..opaque
        };
        assert_eq!(
            desired_alpha_mode(&reloaded, &macos_modes),
            wgpu::CompositeAlphaMode::PostMultiplied,
            "an opaque-to-transparent reload must change the configured surface mode"
        );

        let src = super::production_source();
        let refresh = ["self.set_background_", "compositing(cfg);"].concat();
        assert!(
            src.contains(&refresh),
            "frame ingress must apply a changed effective-alpha mode before acquisition"
        );
    }

    #[test]
    fn postmultiplied_presentation_skips_only_provably_opaque_scenes() {
        let image = Config {
            background_type: BackgroundType::Image,
            background_opacity: 1.0,
            background_darkness: 1.0,
            ..Config::default()
        };
        let post = wgpu::CompositeAlphaMode::PostMultiplied;

        let opaque_image = final_scene_is_uniformly_opaque(&image, true);
        assert!(
            opaque_image,
            "an opaque fullscreen wallpaper proves alpha 1"
        );
        assert!(
            !needs_postmultiplied_presentation(post, opaque_image),
            "opaque wallpaper must skip the fullscreen allocation and conversion pass"
        );

        for reason in [
            "transparent source texel",
            "wallpaper does not cover the surface",
        ] {
            let possibly_translucent = final_scene_is_uniformly_opaque(&image, false);
            assert!(
                !possibly_translucent,
                "fixture must be non-opaque: {reason}"
            );
            assert!(
                needs_postmultiplied_presentation(post, possibly_translucent),
                "{reason}: a premultiplied scene must still be converted before a PostMultiplied surface"
            );
        }

        let translucent_solid = Config {
            background_type: BackgroundType::Solid,
            background_opacity: 0.5,
            ..Config::default()
        };
        assert!(needs_postmultiplied_presentation(
            post,
            final_scene_is_uniformly_opaque(&translucent_solid, false)
        ));
        assert!(
            !needs_postmultiplied_presentation(wgpu::CompositeAlphaMode::PreMultiplied, false),
            "a PreMultiplied surface consumes the scene directly even when translucent"
        );

        let starfield = Config {
            background_type: BackgroundType::Starfield,
            background_opacity: 0.2,
            background_darkness: 0.0,
            ..Config::default()
        };
        assert!(
            final_scene_is_uniformly_opaque(&starfield, false),
            "starfield opacity is a shader invariant, not an image-cover result"
        );
        assert!(!needs_postmultiplied_presentation(
            post,
            final_scene_is_uniformly_opaque(&starfield, false)
        ));
    }
}

#[cfg(test)]
mod selection_row_span_tests {
    use super::selection_row_span;

    /// The selection highlight quad's width = `(c1 + 1 - c0) * cell_w`, the same
    /// arithmetic the `build_pane` draw uses. Pin it here so a span change is
    /// caught at the source of truth.
    fn span_width(span: (usize, usize)) -> usize {
        span.1 + 1 - span.0
    }

    /// A BLOCK (Alt+drag) selection is a column rectangle: every row — including
    /// interior rows — spans only `min_col..=max_col`, so the highlight matches
    /// the rectangular text the copy yields. A linear selection drawn for the
    /// same endpoints would span the FULL row on an interior row, which is the
    /// bug this fix closes.
    #[test]
    fn block_selection_highlights_column_rectangle_on_every_row() {
        let cols = 200;
        // Block from (row 4, col 10) to (row 8, col 25): a 16-wide column band.
        let (start, end) = ((4, 10), (8, 25));
        let block_w = 25 + 1 - 10; // 16 columns
        for r in 4..=8 {
            let span = selection_row_span(r, start, end, cols, true);
            assert_eq!(
                span,
                (10, 25),
                "block row {r} must span the column rectangle, not the full line"
            );
            assert_eq!(span_width(span), block_w);
        }
        // The interior row (r = 6) is the load-bearing case: a linear selection
        // would span the whole row here.
        let interior = selection_row_span(6, start, end, cols, true);
        assert_eq!(span_width(interior), block_w);
        assert_ne!(
            span_width(interior),
            cols,
            "interior block row must NOT be a full-row highlight"
        );
    }

    /// A block selection's column endpoints are normalized, so dragging
    /// up-and-left (end col < start col) still yields the same `min..=max` band.
    #[test]
    fn block_selection_normalizes_reversed_columns() {
        let cols = 80;
        // end column (5) is left of the start column (20).
        let span = selection_row_span(3, (2, 20), (6, 5), cols, true);
        assert_eq!(span, (5, 20));
    }

    /// Linear (normal drag) selection is unchanged: the start row runs from the
    /// anchor to the last column, interior rows span the full width, and the end
    /// row runs from column 0 to the cursor.
    #[test]
    fn linear_selection_wraps_full_lines() {
        let cols = 120;
        let (start, end) = ((4, 10), (8, 25));
        assert_eq!(
            selection_row_span(4, start, end, cols, false),
            (10, cols - 1),
            "start row runs from the anchor to the last column"
        );
        assert_eq!(
            selection_row_span(6, start, end, cols, false),
            (0, cols - 1),
            "interior row spans the full width"
        );
        assert_eq!(
            selection_row_span(8, start, end, cols, false),
            (0, 25),
            "end row runs from column 0 to the cursor"
        );
        // A single-row linear selection stays within its endpoints.
        assert_eq!(selection_row_span(4, (4, 3), (4, 7), cols, false), (3, 7));
    }

    /// Grid-absolute lines are negative when scrolled into history; the span
    /// logic must still compare rows correctly with `i32` endpoints.
    #[test]
    fn negative_scrollback_rows_compare_correctly() {
        let cols = 100;
        // Linear selection entirely in scrollback (rows -5..=-3).
        let (start, end) = ((-5, 8), (-3, 12));
        assert_eq!(
            selection_row_span(-5, start, end, cols, false),
            (8, cols - 1)
        );
        assert_eq!(
            selection_row_span(-4, start, end, cols, false),
            (0, cols - 1)
        );
        assert_eq!(selection_row_span(-3, start, end, cols, false), (0, 12));
        // Block selection over the same scrollback rows is a column band.
        assert_eq!(selection_row_span(-4, start, end, cols, true), (8, 12));
    }

    /// Drawing a selection must cost what is drawn, not what is selected.
    ///
    /// `Ctrl+A` in a pane holding a million lines of build output is one
    /// gesture, and the loop walked `start..=end` and skipped the offscreen
    /// rows inside the body — a million iterations on every repaint, every
    /// blink, every keystroke, to draw at most `screen_lines` quads.
    #[test]
    fn selection_drawing_visits_only_the_rows_on_screen() {
        use super::visible_selection_rows;
        let screen_rows = 40;

        // The whole scrollback selected, viewport at the bottom: only the
        // visible rows are visited, not the million behind them.
        let rows = visible_selection_rows(-1_000_000, 39, 0, screen_rows);
        assert_eq!(rows.clone().count(), screen_rows as usize);
        assert_eq!((*rows.start(), *rows.end()), (0, 39));

        // Scrolled up by 100: the window moves with the viewport, and the same
        // bounded number of rows is visited.
        let rows = visible_selection_rows(-1_000_000, 39, 100, screen_rows);
        assert_eq!((*rows.start(), *rows.end()), (-100, -61));
        assert_eq!(rows.count(), screen_rows as usize);

        // A selection that fits on screen is unchanged — clamping must not
        // narrow what was already visible.
        assert_eq!(
            {
                let r = visible_selection_rows(5, 9, 0, screen_rows);
                (*r.start(), *r.end())
            },
            (5, 9)
        );

        // Partial overlap keeps exactly the overlapping part.
        assert_eq!(
            {
                let r = visible_selection_rows(-10, 3, 0, screen_rows);
                (*r.start(), *r.end())
            },
            (0, 3),
            "the part scrolled out of view is clipped, the rest still draws"
        );
        assert_eq!(
            {
                let r = visible_selection_rows(35, 80, 0, screen_rows);
                (*r.start(), *r.end())
            },
            (35, 39)
        );

        // Entirely off screen: nothing to draw and nothing to walk.
        assert_eq!(
            visible_selection_rows(-500, -100, 0, screen_rows).count(),
            0
        );
        assert_eq!(visible_selection_rows(100, 500, 0, screen_rows).count(), 0);
        // Including the row just past each edge.
        assert_eq!(visible_selection_rows(-5, -1, 0, screen_rows).count(), 0);
        assert_eq!(visible_selection_rows(40, 45, 0, screen_rows).count(), 0);

        // Every row this yields maps into the viewport, for a spread of
        // offsets — the loop body's `debug_assert` depends on it.
        for display_offset in [0, 1, 7, 100, 1_000_000] {
            for (start, end) in [(-2_000_000, 39), (-50, 50), (-100, -100), (39, 39)] {
                for r in visible_selection_rows(start, end, display_offset, screen_rows) {
                    let vrow = r + display_offset;
                    assert!(
                        (0..screen_rows).contains(&vrow),
                        "offset {display_offset}, selection {start}..={end}: row {r} \
                         maps to viewport row {vrow}"
                    );
                    assert!(
                        (start..=end).contains(&r),
                        "clamping must not invent rows outside the selection"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod run_attrs_tests {
    use super::{GColor, Rgb, Style, Weight, font_features, run_attrs};
    use glyphon::Family;
    use kettle_config::Config;

    /// v2.20.0 P1: `run_attrs` (the per-run half of the retired
    /// `build_rich_spans`) must map the SGR bits exactly as the old builder
    /// did — color from the resolved fg, BOLD → `Weight::BOLD`, ITALIC →
    /// `Style::Italic`, and the family routed through `cfg.family_for` so
    /// configured bold/italic font variants keep working.
    #[test]
    fn run_attrs_maps_color_weight_style_and_family() {
        let cfg = Config::default();
        let ff = font_features(&cfg);

        let plain = run_attrs(&cfg, &ff, Rgb::new(10, 20, 30), false, false);
        assert_eq!(plain.color_opt, Some(GColor::rgb(10, 20, 30)));
        assert_eq!(plain.weight, Weight::NORMAL);
        assert_eq!(plain.style, Style::Normal);
        assert_eq!(plain.family, Family::Name(cfg.family_for(false, false)));

        let bold_italic = run_attrs(&cfg, &ff, Rgb::new(40, 50, 60), true, true);
        assert_eq!(bold_italic.color_opt, Some(GColor::rgb(40, 50, 60)));
        assert_eq!(bold_italic.weight, Weight::BOLD);
        assert_eq!(bold_italic.style, Style::Italic);
        assert_eq!(bold_italic.family, Family::Name(cfg.family_for(true, true)));
    }

    /// v2.20.0 P1 drift guard: two identical run tuples must produce EQUAL
    /// `Attrs` (the per-line cache's `set_text` second guard compares
    /// `AttrsList`s — accidental per-call variation would defeat the cache
    /// and re-shape every row every frame), and differing tuples must
    /// produce UNEQUAL `Attrs` (or stale styling would survive).
    #[test]
    fn run_attrs_is_deterministic_and_distinguishes_runs() {
        let cfg = Config::default();
        let ff = font_features(&cfg);

        let a = run_attrs(&cfg, &ff, Rgb::new(1, 2, 3), true, false);
        let b = run_attrs(&cfg, &ff, Rgb::new(1, 2, 3), true, false);
        assert_eq!(a, b);

        let other_color = run_attrs(&cfg, &ff, Rgb::new(9, 2, 3), true, false);
        assert_ne!(a, other_color);
        let other_weight = run_attrs(&cfg, &ff, Rgb::new(1, 2, 3), false, false);
        assert_ne!(a, other_weight);
    }
}

#[cfg(test)]
mod settings_panel_cols_tests {
    use super::settings_panel_cols;
    use unicode_width::UnicodeWidthStr;

    // The settings panel must be wide enough for its two widest
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

#[cfg(test)]
mod text_layout_damage_tests {
    use super::{Config, PaneSnapshot, PaneView, text_layout_damage_key};

    /// A pane's rect can move while the window size, cell size, and every row's
    /// text stay identical — bottom chrome or a banner appearing, for instance.
    /// The layout damage key must still change, or the cached cell-locked glyph
    /// instances keep drawing at the old positions, which is the "leftover
    /// text" symptom. Cursor blink must NOT change it; that direction is pinned
    /// by `grid_upload_damage_excludes_cursor_blink`.
    #[test]
    fn pane_rect_shift_invalidates_layout_key() {
        let cfg = Config::default();
        let snap = PaneSnapshot::default();
        let pane = |rect: (f32, f32, f32, f32)| PaneView {
            id: 1,
            rect,
            snap: &snap,
            focused: true,
            images: &[],
            title: "",
            title_prefix: "",
            title_path: None,
            size_cols: 80,
            size_rows: 24,
            bell: false,
            group_name: None,
        };
        let surface = (800.0, 600.0);
        let cell = (8.0, 16.0);

        let full =
            text_layout_damage_key(&[pane((0.0, 0.0, 800.0, 600.0))], &cfg, surface, cell, 0.0);
        let shortened =
            text_layout_damage_key(&[pane((0.0, 0.0, 800.0, 560.0))], &cfg, surface, cell, 0.0);
        assert_ne!(
            full, shortened,
            "a pane losing height must invalidate the text layout damage key"
        );

        let moved =
            text_layout_damage_key(&[pane((0.0, 40.0, 800.0, 560.0))], &cfg, surface, cell, 0.0);
        assert_ne!(
            shortened, moved,
            "a pane of unchanged size at a new origin must invalidate the key too"
        );
    }
}

#[cfg(test)]
mod glyph_cell_lock_tests {
    use super::{cell_locked_pen_x, glyph_grid_col};

    /// `char index == grid column` because `build_pane` writes one char per cell
    /// (wide-char spacer included). The cluster→column map must resolve each
    /// glyph's start byte to its char's column, including a multi-byte char's
    /// interior byte and the spacer that follows a wide glyph.
    #[test]
    fn glyph_grid_col_maps_cluster_byte_to_column() {
        // Row text "a你 b": 'a'@0 (1B), '你'@1 (3B), spacer ' '@4 (1B), 'b'@5.
        // One char per cell ⇒ a=col0, 你=col1, spacer=col2, b=col3.
        let starts = [0u32, 1, 4, 5];
        assert_eq!(glyph_grid_col(&starts, 0), 0, "'a' → col 0");
        assert_eq!(glyph_grid_col(&starts, 1), 1, "'你' → col 1");
        assert_eq!(glyph_grid_col(&starts, 4), 2, "spacer → col 2");
        assert_eq!(
            glyph_grid_col(&starts, 5),
            3,
            "'b' → col 3 (past the wide cell)"
        );
        // A cluster byte inside the multi-byte char still maps to its column.
        assert_eq!(
            glyph_grid_col(&starts, 3),
            1,
            "interior byte of '你' → col 1"
        );
        // Defensive: a start before the first char clamps to column 0.
        assert_eq!(glyph_grid_col(&starts, 0), 0);
    }

    /// The pen is pinned to the cell and snapped to an integer pixel; a
    /// primary-face glyph (x_offset 0) lands exactly on `cell_left`, and a
    /// combining mark's offset is preserved before snapping.
    #[test]
    fn cell_locked_pen_snaps_to_the_cell() {
        assert_eq!(
            cell_locked_pen_x(40.0, 0.0),
            40.0,
            "integer cell_left is unchanged"
        );
        assert_eq!(cell_locked_pen_x(40.4, 0.0), 40.0, "snaps down");
        assert_eq!(cell_locked_pen_x(40.6, 0.0), 41.0, "snaps up");
        assert_eq!(
            cell_locked_pen_x(40.0, 2.0),
            42.0,
            "intra-cluster x_offset kept"
        );
        // The whole point: column N pins to N*cw regardless of upstream drift.
        let cw = 8.4_f32;
        for col in 0..80u32 {
            let cell_left = 10.0 + col as f32 * cw;
            assert_eq!(
                cell_locked_pen_x(cell_left, 0.0),
                cell_left.round(),
                "column {col} pen must be its own cell, never drifted by neighbors"
            );
        }
    }

    /// Wiring drift guards: the grid path must NOT push a pane TextArea (that
    /// would double-draw via glyphon), the cell-locked draw must sit in the
    /// shared scene pass, and the emit must be gated on grid mode.
    #[test]
    fn grid_path_wiring_is_present() {
        let src = super::production_source();
        assert!(
            src.contains("if cfg.text_renderer == TextRendererMode::Legacy {"),
            "pane TextArea must be pushed to glyphon ONLY in legacy mode"
        );
        assert!(
            src.contains(
                "let target_size = [self.config.width.max(1), self.config.height.max(1)];"
            ) && src.contains("fn encode_scene_pass(")
                && src.contains("self.glyph_pipeline")
                && src.contains(".draw(&mut pass, &self.glyph_clips, target_size);"),
            "the cell-locked glyph pipeline must draw against the live target size"
        );
        assert!(
            src.contains("if cfg.text_renderer == TextRendererMode::Grid {")
                && src.contains("self.emit_pane_glyphs("),
            "glyph emission must be gated on grid mode"
        );
        assert!(
            src.contains("buf.set_wrap(Wrap::None);"),
            "pane buffers must be Wrap::None so a row is one run + char==column holds"
        );
    }

    /// Cursor blink may force the separate cursor-glyph prepare, but it must not
    /// be part of the pane grid upload gate. Otherwise blink can clear/stale-draw
    /// ordinary prompt glyphs. Pane grid uploads are allowed only for text
    /// content damage or layout/style damage.
    #[test]
    fn grid_upload_damage_excludes_cursor_blink() {
        let src = super::production_source();
        assert!(
            src.contains("last_text_layout_key: Option<u64>"),
            "renderer must keep a layout damage key for cached text/grid vertices"
        );
        assert!(
            src.contains("grid_glyphs_dirty: bool"),
            "renderer must force a grid upload after clearing the glyph pipeline"
        );
        assert!(
            src.contains("let grid_upload_needed =")
                && src.contains(
                    "self.grid_glyphs_dirty || any_pane_text_changed || text_layout_changed",
                ),
            "grid glyph upload must be gated by forced dirtiness, pane text, or layout damage"
        );
        assert!(
            src.contains("self.glyph_pipeline.clear();")
                && src.contains("self.grid_glyphs_dirty = true;"),
            "clearing the grid glyph pipeline must dirty the next upload"
        );
        let gate = src
            .split("let grid_upload_needed =")
            .nth(1)
            .and_then(|s| s.split("if let Some((gx, gy, gch, gcolor, gclip))").next())
            .expect("grid upload block present before cursor-glyph prepare");
        assert!(
            !gate.contains("cursor_char_changed") && !gate.contains("cursor_visible"),
            "grid upload gate/block must not depend on cursor blink state"
        );
    }

    /// v2.32.0 fix #1 (durability): the cell-locked emit loop must live in ONE
    /// free function, `emit_cell_locked_glyphs`, called from both production
    /// sites (live panes and the screenshot path). Hand-copied loops could
    /// silently drift so the README imagery no longer matches the live
    /// renderer; pin the single source of truth here.
    #[test]
    fn cell_lock_emit_is_a_single_shared_fn() {
        let src = super::production_source();
        assert!(
            src.contains("fn emit_cell_locked_glyphs("),
            "the shared cell-locked emit function must exist"
        );
        // Exactly one definition plus the two production calls. The GPU blink
        // fixtures are deliberately absent from `production_source`.
        let calls = src.matches("emit_cell_locked_glyphs(").count();
        assert_eq!(
            calls, 3,
            "emit_cell_locked_glyphs must be the single emit shared by \
             emit_pane_glyphs and the screenshot path (definition + 2 calls); \
             found {calls} occurrences"
        );
    }

    /// v2.32.0 fix #1: the offscreen `--screenshot` path must honor
    /// `cfg.text_renderer`. In Grid mode (the default) it builds a GlyphPipeline
    /// and routes the pane body buffers through `emit_cell_locked_glyphs` +
    /// `grid_glyphs.draw`, leaving glyphon only the annotation/menu chrome; in
    /// Legacy mode it keeps glyphon. Without this the README hero/showcase
    /// imagery rendered through legacy glyphon regardless of the shipped default.
    #[test]
    fn screenshot_routes_pane_text_by_renderer_mode() {
        let src = super::production_source();
        // The capture path reads the renderer mode.
        assert!(
            src.contains("let grid = cfg.text_renderer == TextRendererMode::Grid;"),
            "capture_png path must branch on the configured text-renderer"
        );
        // Pane TextAreas only go to glyphon in Legacy mode.
        assert!(
            src.contains("if !grid {"),
            "pane body TextAreas must be glyphon-only in Legacy (`if !grid`) mode"
        );
        // Grid mode builds a GlyphPipeline and draws it in the pass.
        assert!(
            src.contains("let mut grid_glyphs =")
                && src.contains("GlyphPipeline::new_with_budget("),
            "Grid screenshot path must build a GlyphPipeline"
        );
        assert!(
            src.contains("grid_glyphs.draw(&mut pass, &grid_clips, [w, h]);"),
            "Grid screenshot path must draw the cell-locked pane glyphs in the pass"
        );
        // The annotation buffer (chrome) still goes through glyphon in both modes.
        assert!(
            src.contains("buffer: &annotate_buf,"),
            "the annotation chrome must still render via glyphon"
        );
    }

    #[test]
    fn gpu_recovery_escalates_to_software() {
        assert_eq!(
            super::escalation_for_attempt(0),
            super::AdapterEscalation::AlternateBackend
        );
        assert_eq!(
            super::escalation_for_attempt(1),
            super::AdapterEscalation::SurfacePreferred
        );
        assert_eq!(
            super::escalation_for_attempt(2),
            super::AdapterEscalation::AnyHardware
        );
        assert_eq!(
            super::escalation_for_attempt(3),
            super::AdapterEscalation::ForceSoftware
        );
        assert_eq!(
            super::escalation_for_attempt(99),
            super::AdapterEscalation::ForceSoftware
        );
    }

    #[test]
    fn gpu_fault_latch_keeps_first_error_and_bounds_message() {
        let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fault = std::sync::Arc::new(std::sync::Mutex::new(None));
        let long = format!("first\n{}", "x".repeat(3000));

        super::latch_gpu_fault(&lost, &fault, "device_lost", long);
        super::latch_gpu_fault(&lost, &fault, "internal", "second".to_string());

        assert!(lost.load(std::sync::atomic::Ordering::Acquire));
        let actual = fault.lock().unwrap().clone().expect("fault latched");
        assert_eq!(actual.kind, "device_lost");
        assert!(!actual.message.contains('\n'));
        assert_eq!(actual.message.chars().count(), 2048);
        assert!(!actual.message.contains("second"));
    }

    #[test]
    fn terminal_cursor_is_suppressed_while_window_is_unfocused() {
        assert!(super::cursor_focus_gate(true, true));
        assert!(!super::cursor_focus_gate(false, true));
        assert!(!super::cursor_focus_gate(true, false));
        assert!(!super::cursor_focus_gate(false, false));
    }
}
