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

mod bg_image;
mod card_marks;
mod card_scene;
mod inline_cards;
/// A card a frame drew: its pane, its nonce, and the part of it inside its
/// pane in surface pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintedCard {
    pub pane: u64,
    pub nonce: kettle_core::InlineNonce,
    pub rect: [f32; 4],
}

impl PaintedCard {
    /// Whether surface point (`x`, `y`) falls on it.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let [left, top, width, height] = self.rect;
        x >= left && x < left + width && y >= top && y < top + height
    }
}

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

/// Monochrome faces that carry text-presentation symbols, most specific first.
///
/// A colour-emoji face is deliberately absent: it is what these steer away
/// from. Only a family the system actually has is used, so an unusual install
/// keeps its existing fallback cascade rather than tofu-boxing anything.
#[cfg(target_os = "macos")]
const TEXT_SYMBOL_FAMILIES: &[&str] = &["STIX Two Math", "Apple Symbols"];
#[cfg(target_os = "windows")]
const TEXT_SYMBOL_FAMILIES: &[&str] = &["Segoe UI Symbol"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const TEXT_SYMBOL_FAMILIES: &[&str] = &[
    "Noto Sans Symbols2",
    "Noto Sans Symbols 2",
    "Symbola",
    "DejaVu Sans",
];

/// The codepoint the candidate faces are probed with.
///
/// Being installed is not enough. A stock Ubuntu runner has DejaVu Sans and
/// DejaVu Sans does not have U+23FA, so requesting it there resolves nothing
/// and shaping falls straight back through the cascade to the colour face,
/// which is the state this whole change exists to leave. One probe is
/// deliberate rather than exhaustive: this is the codepoint the defect was
/// reported against, and a family that lacks it is not a useful text-symbol
/// face here. A host where no candidate passes keeps the cascade it had.
const TEXT_SYMBOL_PROBE: char = '\u{23FA}';

/// The first [`TEXT_SYMBOL_FAMILIES`] entry this system has AND that carries a
/// glyph for [`TEXT_SYMBOL_PROBE`].
fn resolve_text_symbol_family(font_system: &mut FontSystem) -> Option<&'static str> {
    let probe = TEXT_SYMBOL_PROBE;
    for wanted in TEXT_SYMBOL_FAMILIES.iter().copied() {
        let ids: Vec<fontdb::ID> = font_system
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(wanted))
            })
            .map(|face| face.id)
            .collect();
        for id in ids {
            // `Font::unicode_codepoints` is empty unless cosmic-text's
            // `monospace_fallback` feature is on, which it is not here, so ask
            // the face's own character map instead.
            if font_system
                .get_font(id, fontdb::Weight::NORMAL)
                .is_some_and(|font| font.as_swash().charmap().map(probe) != 0)
            {
                return Some(wanted);
            }
        }
    }
    None
}

/// Whether this codepoint is emoji-capable, text by default, and occupies one
/// column.
///
/// Derived from the width table rather than a vendored `Emoji_Presentation`
/// list. Every `Emoji_Presentation=Yes` codepoint is East Asian Wide, so
/// `unicode-width` reports two columns for it, and `unicode-width` also widens
/// an emoji-capable codepoint to two when U+FE0F follows. One column alone and
/// two with U+FE0F therefore means `Emoji=Yes` with `Emoji_Presentation=No`.
/// `⏺` U+23FA is one; `⏰` U+23F0 is not, because it is already two columns.
///
/// **One column is part of the rule, not an accident of the derivation.** A
/// handful of default-text emoji are East Asian Wide and stay at two columns
/// with or without U+FE0F, U+3030 and U+1F202 among them, and width cannot tell
/// those apart from `Emoji_Presentation=Yes`. They are out of scope here: a
/// colour glyph in a two-column cell occupies the space it was given, so it
/// neither overflows nor misaligns anything. This guards against a two-cell
/// colour bitmap painted into a one-cell slot.
///
/// Nothing here decides how a cell is drawn on its own. It only says which
/// cells must not be handed to a colour-emoji face, which is a font-selection
/// question the shaper answers.
fn single_column_text_presentation(ch: char) -> bool {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    // ASCII is the overwhelming majority of terminal output and none of it is
    // emoji-capable, so this is the early-out that keeps the row walk cheap.
    if ch.is_ascii() || ch.width() != Some(1) {
        return false;
    }
    // At most four bytes for the base plus three for U+FE0F.
    let mut buf = [0u8; 8];
    let base = ch.encode_utf8(&mut buf).len();
    let mut selector = [0u8; 4];
    let vs16 = '\u{FE0F}'.encode_utf8(&mut selector).len();
    buf[base..base + vs16].copy_from_slice(&selector[..vs16]);
    std::str::from_utf8(&buf[..base + vs16]).is_ok_and(|qualified| qualified.width() == 2)
}

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

// Named constants for the right-click context-menu chrome. Public so
// `kettle-ui` reads the same values through `kettle_render::menu` instead
// of redeclaring them.
pub mod menu {
    /// Vertical padding inside each context-menu row. Cell height +
    /// `ROW_PAD` = total row height (~28-32 px on default cell
    /// metrics, a comfortable click target).
    pub const ROW_PAD: f32 = 12.0;
    /// Separator row height. Smaller than a regular row so the menu
    /// reads as grouped without wasting vertical space.
    pub const SEP_H: f32 = 8.0;
    /// Height of each pointer target at the cut edges of a scrolling menu.
    pub const SCROLL_H: f32 = 18.0;

    /// Content inset and height shared by paint, pointer targets and navigation.
    pub fn scroll_content(natural_h: f32, panel_h: f32, row_h: f32) -> (f32, f32) {
        let inset = if natural_h > panel_h {
            SCROLL_H.min(((panel_h - row_h) * 0.5).max(0.0))
        } else {
            0.0
        };
        (inset, (panel_h - 2.0 * inset).max(0.0))
    }
    /// Horizontal padding inside the panel: `max_columns * cw + H_PAD`.
    /// Gives the longest label breathing room and lets short labels
    /// (Copy) still feel like a real menu surface.
    pub const H_PAD: f32 = 40.0;
    /// Minimum panel width — overrides the chars-based math when the
    /// longest label is tiny.
    pub const MIN_W: f32 = 180.0;
    /// Convert a content-column budget into the natural context-menu width.
    pub fn panel_width(max_columns: f32, cell_width: f32) -> f32 {
        (max_columns * cell_width + H_PAD).max(MIN_W)
    }

    /// Recover the number of text columns that fit inside a panel width.
    pub fn panel_columns(panel_width: f32, cell_width: f32) -> usize {
        if cell_width <= 0.0 {
            return 0;
        }
        ((panel_width - H_PAD).max(0.0) / cell_width).floor() as usize
    }
    /// Top + bottom breathing room reserved when clamping the panel
    /// height to the surface (for scrollable submenus). Keeps
    /// the menu from kissing the window edge.
    pub const PANEL_BREATHING: f32 = 80.0;
}

// Tab-drag rendering constants, public for kettle-ui like `menu` above.
// Only paint geometry/opacity lives here. Gesture *thresholds* (arm
// distance, tear hysteresis) stay in kettle-ui with the drag FSM.
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

    /// Re-dock insertion-marker line thickness. 3px survives dark-on-dark
    /// themes at a glance.
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
mod font_reload_tests;

#[cfg(test)]
mod dim_blend_tests;

#[cfg(test)]
mod context_menu_row_width_tests;

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
        || !overlay.hint_labels.is_empty()
        || overlay.ime_preedit.is_some()
        || overlay.ssh_query.is_some()
        || overlay.palette_query.is_some()
        || overlay.layout_picker_query.is_some()
        || overlay.theme_picker_query.is_some()
        || overlay.edit_title.is_some()
        || overlay.settings.is_some()
        || overlay.resize_overlay.is_some()
}

/// Disabled and secondary menu text: 45% foreground over the panel
/// background, so it reads as about 55% transparent without alpha-blending
/// through to whatever lives under the panel.
fn dim_blend(fg: Rgb, bg: Rgb) -> Rgb {
    solid_blend(fg, bg, 45)
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

/// Vertical menu panel used by the right-click menu and the command, layout,
/// and SSH pickers. The UI clamps the anchor and remains the source of truth
/// for selection and dispatch; the renderer owns only shared list paint.
pub struct ContextMenu {
    /// Top-left of the panel in surface pixels (already clamped).
    pub anchor: (f32, f32),
    pub rows: Vec<ContextMenuRow>,
    /// Index of the currently highlighted row. A disabled status row is the
    /// only non-empty case where this does not point at a selectable item.
    pub highlight: usize,
    /// Index of the first row the renderer should paint (Terminator
    /// menu UX parity). Rows `0..scroll_offset` are scrolled
    /// off-panel; the renderer also stops drawing when the
    /// accumulated row height exceeds `panel_h_clamped`. Zero means
    /// "show from the top".
    pub scroll_offset: usize,
    /// Panel width after the surface clamp. Zero means use natural width.
    pub panel_w_clamped: f32,
    /// Panel height after the surface clamp (App-side
    /// `context_menu_geometry` already applies the clamp); the
    /// renderer reuses it to decide which rows are visible + to
    /// position the ▲/▼ arrows. Zero means "no clamp", in which
    /// case the renderer falls back to the natural panel height.
    pub panel_h_clamped: f32,
    /// What a screen reader calls the menu, when it is not the terminal's.
    pub a11y_label: Option<String>,
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
        menu::panel_width(max_columns, cell_width)
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
    /// The key hint after the input, already in the UI's language.
    pub hint: String,
    pub rect: Rect4,
}

/// The title editor's text before its input: the pencil and the label. The
/// line is this prefix, the input with its caret, then the hint. The UI places
/// the input-method window after it, so both measure the same text.
pub fn title_edit_prefix(label: &str) -> String {
    format!("  ✎ {label} ")
}

/// The theme picker input bar's text before the query, which the UI measures
/// to place an input method's candidate window at the painted caret.
pub fn theme_picker_prefix(tr: &Translator) -> String {
    format!("  ◐ {} ", tr.text(Text::PickerThemePrompt))
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
    pub const ALL: [Self; 3] = [Self::Smart, Self::Match, Self::Ignore];

    /// The mode's name in `tr`'s language, as the Settings row shows it.
    pub fn label(self, tr: &Translator) -> &'static str {
        tr.text(match self {
            Self::Smart => Text::SettingsValueSmart,
            Self::Match => Text::SettingsValueMatchCase,
            Self::Ignore => Text::SettingsValueIgnoreCase,
        })
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
    pub const ALL: [Self; 11] = [
        Self::Typing,
        Self::Searching,
        Self::Match,
        Self::Wrapped,
        Self::Start,
        Self::End,
        Self::NoMatch,
        Self::Limited,
        Self::Invalid,
        Self::TooComplex,
        Self::TooLong,
    ];

    pub fn label(self, tr: &Translator) -> &'static str {
        tr.text(match self {
            Self::Typing => Text::SearchStatusTyping,
            Self::Searching => Text::SearchStatusSearching,
            Self::Match => Text::SearchStatusMatch,
            Self::Wrapped => Text::SearchStatusWrapped,
            Self::Start => Text::SearchStatusStart,
            Self::End => Text::SearchStatusEnd,
            Self::NoMatch => Text::SearchStatusNoMatch,
            Self::Limited => Text::SearchStatusLimited,
            Self::Invalid => Text::SearchStatusInvalid,
            Self::TooComplex => Text::SearchStatusTooComplex,
            Self::TooLong => Text::SearchStatusTooLong,
        })
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

    pub fn accessible_label(self, tr: &Translator) -> &'static str {
        tr.text(match self {
            Self::Editor => Text::SearchA11yEditor,
            Self::Previous => Text::SearchA11yPrevious,
            Self::Next => Text::SearchA11yNext,
            Self::Wrap => Text::SearchA11yWrap,
            Self::Case => Text::SearchA11yCase,
            Self::Invert => Text::SearchA11yInvert,
            Self::Close => Text::SearchA11yClose,
        })
    }
}

/// Full renderer projection for the in-window search lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOverlay {
    /// The UI's language. The bar's words and widths follow it.
    pub tr: Translator,
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
    /// Control under the pointer, drawn with a hover fill.
    pub hovered: Option<SearchControl>,
    /// Button held down by the pointer. It activates on release over it.
    pub pressed: Option<SearchControl>,
}

/// A shell-owned completion list projected over the focused pane. Kettle only
/// presents these rows; accepting or executing a candidate remains the shell's
/// job.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionOverlay {
    /// The UI's language, for the card's header count.
    pub tr: Translator,
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
    /// The UI language of the card's text, its accessible name, and the
    /// width its safety warning needs.
    pub tr: Translator,
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

#[cfg(any(test, feature = "test-defaults"))]
impl Default for SearchOverlay {
    fn default() -> Self {
        Self {
            tr: Translator::default(),
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
            hovered: None,
            pressed: None,
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

/// Overlay state projected by the UI, covering search, links, hints, pickers,
/// menus, dialogs, and banners.
///
/// There is no `Default` outside tests (the `test-defaults` feature): a
/// default overlay speaks English and carries no state, so the UI lists every
/// field. A production build, which never enables the feature, rejects any
/// overlay built from a default.
#[cfg_attr(any(test, feature = "test-defaults"), derive(Default))]
pub struct Overlay {
    /// The UI's language. Text the renderer words itself (the search bar, the
    /// completion count, the layout picker prompt) goes through it.
    pub tr: Translator,
    /// Search-lane projection.
    pub search: Option<SearchOverlay>,
    /// Visible, non-overlapping match spans in `(row, col)` order. Keeping this
    /// list viewport-bounded lets both quad paint and glyph recoloring stay
    /// linear in visible work.
    pub highlights: Vec<HighlightRect>,
    pub links: Vec<LinkRect>,
    /// Quick-select hint labels (drawn over the focused pane).
    pub hint_labels: Vec<HintLabel>,
    /// Input-method preedit text drawn at the focused terminal cursor.
    pub ime_preedit: Option<ImePreedit>,
    /// Shell completion card shown above or below the focused pane's command.
    pub completion: Option<CompletionOverlay>,
    /// Visual receipt for the focused pane's most recent image or video paste.
    pub media_paste_receipt: Option<MediaPasteReceiptOverlay>,
    /// The preview lanes the user opened, one per pane at most, each in the
    /// share of its pane the layout carved for it.
    pub media_lanes: Vec<MediaLanePanel>,
    /// The centered viewer excluded from the window-modal backdrop.
    pub media_viewer_rect: Option<Rect4>,
    /// Native pointer for hover feedback in a focused video player.
    pub media_pointer: Option<(f32, f32)>,
    pub media_scrubbing: bool,
    /// The inline card a click at the pointer would open, as a frame drew
    /// it; its visible part is outlined in the accent.
    pub card_hover: Option<PaintedCard>,
    /// `Some(typed)` while the SSH launcher is open.
    pub ssh_query: Option<String>,
    /// Keyboard help for the SSH input lane. Ranked hosts are projected through
    /// `context_menu` so each match gets its own row.
    pub ssh_hint: String,
    /// `Some(typed)` while the command palette is open.
    pub palette_query: Option<String>,
    /// Keyboard help for the palette input lane. Ranked commands are projected
    /// through `context_menu`, including their live shortcut hints.
    pub palette_hint: String,
    /// Terminator parity (`layoutlauncher.py`): `Some(typed)` while
    /// the layout picker is open. Its rows use the same menu projection as the
    /// command palette.
    pub layout_picker_query: Option<String>,
    pub layout_picker_hint: String,
    /// `Some(typed)` while the theme picker is open. Its rows use the same
    /// menu projection as the command palette.
    pub theme_picker_query: Option<String>,
    pub theme_picker_hint: String,
    /// Terminator parity (edit-title overlay UX): the in-progress
    /// title-edit text + a scope label and chrome rect.
    /// `None` when no edit is in progress.
    pub edit_title: Option<TitleEditOverlay>,
    /// Window has keyboard focus. Terminal cursors are suppressed entirely
    /// while false and resume from the unchanged DEC state when focus returns.
    pub window_focused: bool,
    /// The focused pane's scrollbar should paint in its bright (interacting)
    /// state because the pointer is over the scrollbar gutter or the thumb is
    /// being dragged. At rest the bar is dim. The renderer brightens a
    /// scrolled-back pane (`display_offset > 0`) itself from the snapshot, so
    /// this carries only the hover/drag signal.
    pub scrollbar_active: bool,
    /// Cursor is in its "on" blink phase.
    pub cursor_visible: bool,
    /// `Some` while the right-click context menu is open. Rendered on
    /// top of everything else so an overlapping pane border doesn't
    /// occlude the menu.
    pub context_menu: Option<ContextMenu>,
    /// Renderer projection of the confirmation dialog's prompt, buttons and focused index.
    pub confirm_dialog: Option<ConfirmDialogOverlay>,
    /// `Some` while the in-app settings overlay is open. Painted
    /// centered, above panes but below the confirm dialog.
    pub settings: Option<SettingsOverlay>,
    /// Ghostty `resize-overlay` parity: `Some((cols, rows))` while the
    /// transient size chip should paint (the app owns the timing; the
    /// renderer just draws a centered `cols×rows` chip above everything).
    pub resize_overlay: Option<(u16, u16)>,
    pub update_toast: Option<UpdateToastOverlay>,
    /// Terminator parity (drag a terminal elsewhere in its tab): `Some(rect)`
    /// while a live pane drag has a drop target latched. The rect is the half
    /// of the target pane the dropped terminal would take, washed in accent
    /// with a border — the same UI-computes-geometry contract as
    /// [`TabBar::insert_marker`], so the renderer never has to know how splits
    /// are laid out.
    pub pane_drop_hint: Option<Rect4>,
}

/// Window-local update notification. Its geometry is shared with input and accessibility.
#[derive(Clone, Debug)]
pub struct UpdateToastOverlay {
    pub geometry: UpdateToastGeometry,
    pub message: String,
    pub details: String,
    pub close: String,
    pub opacity: f32,
    pub focused: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpdateToastGeometry {
    pub rect: Rect4,
    pub message: Rect4,
    pub details: Rect4,
    pub close: Rect4,
}

pub fn update_toast_geometry(
    surface: (f32, f32),
    top: f32,
    scale: f32,
    cell: (f32, f32),
    details_cells: usize,
    message: &str,
) -> Option<UpdateToastGeometry> {
    if ![surface.0, surface.1, top, scale, cell.0, cell.1]
        .iter()
        .all(|v| v.is_finite())
        || scale <= 0.0
        || cell.0 <= 0.0
        || cell.1 <= 0.0
    {
        return None;
    }
    let width = (surface.0 - 32.0 * scale).min(480.0 * scale);
    let y = top + 12.0 * scale;
    if width < 320.0 * scale || y < 0.0 || message.len() > 1024 {
        return None;
    }
    let x = (surface.0 - width) * 0.5;
    let close_w = (32.0 * scale).max(cell.0 + 16.0 * scale);
    let details_w = details_cells as f32 * cell.0 + 16.0 * scale;
    let message_w = width - close_w - details_w - 28.0 * scale;
    let rows = |available: f32| {
        let columns = ((available / cell.0).floor() as usize).saturating_sub(1);
        toast_text_rows(message, columns)
    };
    let inline = rows(message_w).is_some_and(|rows| rows <= 2);
    let text_width = if inline {
        message_w
    } else {
        width - 24.0 * scale
    };
    let text_rows = rows(text_width)?.max(2);
    if text_rows > 6 || details_w + close_w + 24.0 * scale > width {
        return None;
    }
    let controls_h = (32.0 * scale).max(cell.1 + 16.0 * scale);
    let height = text_rows as f32 * cell.1 + 24.0 * scale + if inline { 0.0 } else { controls_h };
    if y + height > surface.1 - 16.0 * scale {
        return None;
    }
    let controls_y = if inline { y } else { y + height - controls_h };
    let controls_h = if inline { height } else { controls_h };
    Some(UpdateToastGeometry {
        rect: (x, y, width, height),
        message: (
            x + 12.0 * scale,
            y + 12.0 * scale,
            text_width,
            cell.1 * text_rows as f32,
        ),
        details: (
            x + width - close_w - details_w,
            controls_y,
            details_w,
            controls_h,
        ),
        close: (x + width - close_w, controls_y, close_w, controls_h),
    })
}

fn toast_text_rows(text: &str, columns: usize) -> Option<usize> {
    toast_wrapped_text(text, columns).map(|(_, rows)| rows)
}

fn toast_wrapped_text(text: &str, columns: usize) -> Option<(String, usize)> {
    if columns == 0 {
        return None;
    }
    let mut rows = 0;
    let mut displayed = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            displayed.push('\n');
        }
        rows += 1;
        let mut used = 0;
        for word in line.split_whitespace() {
            let width = unicode_width::UnicodeWidthStr::width(word);
            if used > 0 && used + 1 + width > columns {
                rows += 1;
                used = 0;
                displayed.push('\n');
            }
            if used > 0 {
                used += 1;
                displayed.push(' ');
            }
            if width > columns {
                rows += (width - 1) / columns;
                used = (width - 1) % columns + 1;
            } else {
                used += width;
            }
            displayed.push_str(word);
        }
    }
    Some((displayed, rows))
}

fn toast_buffer_text(
    text: &str,
    width: f32,
    cell_width: f32,
    message: bool,
) -> (std::borrow::Cow<'_, str>, Wrap) {
    if message {
        let columns = ((width / cell_width).floor() as usize).saturating_sub(1);
        let displayed = toast_wrapped_text(text, columns)
            .map(|(text, _)| text)
            .unwrap_or_default();
        // Share preferred word breaks with geometry; long version tokens use glyph wrapping.
        (std::borrow::Cow::Owned(displayed), Wrap::Glyph)
    } else {
        (std::borrow::Cow::Borrowed(text), Wrap::None)
    }
}

#[inline]
fn cursor_focus_gate(window_focused: bool, terminal_requests_cursor: bool) -> bool {
    window_focused && terminal_requests_cursor
}

/// Renderer-side projection of `WindowState::confirm_dialog`.
/// Stripped of dispatch state — just the bits needed to paint.
#[derive(Debug, Clone)]
pub struct ConfirmDialogOverlay {
    /// Prompt text shown at the top of the modal.
    pub heading: String,
    pub body: String,
    /// Button labels in display order (Cancel typically first).
    pub buttons: Vec<ConfirmDialogButton>,
    /// Which button has focus (idx into `buttons`).
    pub focus_idx: usize,
    pub hover_idx: Option<usize>,
    pub pressed_idx: Option<usize>,
    pub scroll: usize,
}

/// Paint-side button shape. Destructive buttons use a warning accent;
/// the focused button also has a visible outline.
#[derive(Debug, Clone)]
pub struct ConfirmDialogButton {
    pub label: String,
    pub destructive: bool,
}

/// Renderer-side projection of `WindowState::settings_nav` + the resolved
/// field values. The UI computes labels/values (reading `Config`); the renderer
/// just paints a centered panel — a row of category tabs, then label/value
/// rows for the active category, with the focused row highlighted.
// `PartialEq` lets the renderer memoize `settings_display_lines`'s output
// against the last `SettingsOverlay` it was computed from, instead of
// re-running a `format!()` per display line on every painted frame the
// Settings overlay stays open.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsOverlay {
    /// Category tab names, in order.
    pub categories: Vec<String>,
    /// Longest label and display value across all settings categories.
    pub label_cols: usize,
    pub value_cols: usize,
    /// Index of the active category (its tab is highlighted, its rows shown).
    pub active_category: usize,
    /// Whether the active category previews the terminal background.
    pub background_page: bool,
    /// The active category's fields as (label, current-value) pairs.
    pub rows: Vec<SettingsRow>,
    /// Index into `rows` of the focused field (gets the accent highlight).
    pub focused_row: usize,
    /// The panel title, already in the UI's language ("Settings — Tabs").
    pub title: String,
    /// The key-hint footer, already in the UI's language. It names the vim
    /// keys when `vim-menu-nav` is on.
    pub hints: String,
    /// An optional contextual note shown below the keybind footer, e.g. the
    /// Graphics category's active GPU and pending restart. `None`
    /// on categories that don't need it.
    pub footer_note: Option<String>,
}

impl SettingsOverlay {
    fn backdrop_alpha(&self) -> f32 {
        if self.background_page { 0.30 } else { 0.55 }
    }
}

/// One settings row — a human label and its current value string.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRow {
    pub label: String,
    pub value: String,
    /// `true` when this row doesn't apply to the current state (e.g. the image
    /// path while `background-type != image`). Drawn dimmed and skipped by
    /// keyboard/mouse nav.
    pub disabled: bool,
}

/// Pixel rectangle `(x, y, w, h)`.
pub type Rect4 = (f32, f32, f32, f32);

/// Most lane images one frame draws: one per pane with a lane.
const MAX_LANE_IMAGES: usize = 64;

pub use media_lane::{
    MediaCanvas, MediaLaneEdge, MediaLaneFrame, MediaLaneGeometry, MediaLaneHit, MediaLaneMode,
    MediaLanePanel, MediaLanePlay, MediaLaneScrubber, MediaLaneSender, MediaLaneSound,
    MediaLaneSource, MediaLaneTile, media_lane_geometry, media_lane_geometry_scaled,
};
pub use media_viewport::{MAX_MEDIA_ZOOM, MEDIA_ZOOM_STEP, MIN_MEDIA_ZOOM, MediaViewport};

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
    overlay.tr.completion_matches(overlay.total as u64)
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

#[cfg(test)]
fn completion_palette(theme: &kettle_config::Theme, accent: Rgb) -> CompletionPalette {
    chrome::Colors::default().completion(theme, accent)
}

fn completion_palette_with_colors(
    theme: &kettle_config::Theme,
    accent: Rgb,
    colors: &mut chrome::Colors,
) -> CompletionPalette {
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
        rail: colors.icon(accent, selection_bg),
        header: colors.muted(theme.foreground, panel_bg),
        label: colors.text(theme.foreground, panel_bg),
        description: colors.muted(theme.foreground, panel_bg),
        emphasis: colors.text(accent, panel_bg),
        selected_label: colors.text(selected_fg, selection_bg),
        selected_description: colors.muted(selected_fg, selection_bg),
        selected_emphasis: colors.text(accent, selection_bg),
        scroll_track: solid_blend(theme.foreground, panel_bg, 12),
        scroll_thumb: colors.icon(
            solid_blend(theme.foreground, panel_bg, 45),
            solid_blend(theme.foreground, panel_bg, 12),
        ),
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
    let min_detail_columns = if receipt.remote {
        remote_warning_columns(&receipt.tr)
    } else {
        5.0
    };
    let choose_corner = |card_w: f32, card_h: f32, expanded: bool| {
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
        candidates.into_iter().find(|(candidate, top_lane)| {
            let final_detail_w = if *top_lane {
                detail_w
            } else {
                detail_w + pad - 5.0 - 24.0_f32.max(text_line_height * 1.5) - text_cell_width * 0.5
            };
            (!expanded || !receipt.remote || final_detail_w >= text_cell_width * min_detail_columns)
                && completion_rect.is_none_or(|other| !rects_overlap(*candidate, other))
        })
    };

    let expanded_corner = (expanded_w >= text_cell_width * min_expanded_columns
        && detail_w >= text_cell_width * min_detail_columns
        && expanded_h + inset_y * 2.0 <= height)
        .then(|| choose_corner(expanded_w, expanded_h, true))
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

    let dismiss_size = 24.0;
    let dismiss_inset = 5.0;
    // 28 cells, or wider when the title needs it: in the remote case the
    // title is a safety warning, so a longer language widens the card rather
    // than cutting it. The sum mirrors `title_rect` below, plus a pixel so the
    // column floor cannot lose the last one. Growth stops at the expanded
    // card's width when they share a lane, so the whole chip stays inside the
    // card it expands into, and at the pane; past that the title is cut.
    let title_w =
        display_width(&media_paste_receipt_compact_title(receipt)) as f32 * text_cell_width;
    let title_card_w = cw + title_w + cw * 0.5 + dismiss_size + dismiss_inset + 1.0;
    let max_card_w = expanded_corner.map_or(width - inset_x * 2.0, |(rect, _)| rect.2);
    let card_w = (cw * 28.0)
        .max(title_card_w)
        .min(max_card_w)
        .min(width - inset_x * 2.0);
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
        choose_corner(card_w, card_h, false)?
    };
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

/// Detail columns an expanded remote card needs: its widest warning line in
/// the UI's language, plus the dismiss column the bottom lane reserves. In
/// English that is the 18 columns the card has always required.
fn remote_warning_columns(tr: &Translator) -> f32 {
    let widest = [Text::ReceiptRemotePane, Text::ReceiptLocalPathOnly]
        .into_iter()
        .map(|line| display_width(tr.text(line)))
        .max()
        .unwrap_or(0);
    (widest + 3).max(18) as f32
}

/// The compact card's title, which also sizes the card.
fn media_paste_receipt_compact_title(receipt: &MediaPasteReceiptOverlay) -> String {
    let tr = &receipt.tr;
    match &receipt.kind {
        _ if receipt.remote => tr.text(Text::ReceiptTitleRemote).to_string(),
        MediaPasteReceiptKind::Video { count, .. } if *count > 1 => {
            tr.receipt_title_videos(*count as u64)
        }
        MediaPasteReceiptKind::Image { .. } => tr.text(Text::ReceiptTitleImage).to_string(),
        MediaPasteReceiptKind::Video { .. } => tr.text(Text::ReceiptTitleVideo).to_string(),
    }
}

fn media_paste_receipt_text(
    receipt: &MediaPasteReceiptOverlay,
    geometry: &MediaPasteReceiptGeometry,
    text_cell_width: f32,
) -> (String, String) {
    let tr = &receipt.tr;
    let title_columns = (geometry.title_rect.2 / text_cell_width).floor().max(0.0) as usize;
    let title = match &receipt.kind {
        _ if geometry.compact => media_paste_receipt_compact_title(receipt),
        MediaPasteReceiptKind::Image { .. } => tr.text(Text::ReceiptTitleImage).to_string(),
        MediaPasteReceiptKind::Video { .. } => tr.text(Text::ReceiptTitleVideo).to_string(),
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
                tr.text(Text::ReceiptPathOnCommandLine).to_string(),
            ],
            MediaPasteReceiptKind::Video {
                extension,
                size,
                count,
                preview_pending,
            } => {
                let mut lines = Vec::with_capacity(4);
                lines.push(if *count > 1 {
                    format!(
                        "{} · {extension} · {}",
                        tr.receipt_first_of(*count as u64),
                        format_media_size(*size)
                    )
                } else {
                    format!("{extension} · {}", format_media_size(*size))
                });
                lines.push(
                    tr.text(if *preview_pending {
                        Text::ReceiptPreparingPoster
                    } else {
                        Text::ReceiptPathOnCommandLine
                    })
                    .to_string(),
                );
                lines
            }
        };
        if receipt.remote {
            lines.push(tr.text(Text::ReceiptRemotePane).to_string());
            lines.push(tr.text(Text::ReceiptLocalPathOnly).to_string());
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
    /// Home-abbreviated full cwd path when the label is directory-derived,
    /// enabling width-aware tiering (full path → leaf dir name → tail). `None`
    /// for explicit/override and shell-set (OSC 2) titles, which use the
    /// `fit_tab_title` middle-ellipsis path.
    pub path: Option<String>,
    pub active: bool,
    /// Inactive-tab activity. Always `Normal` on the
    /// active segment so the focused-tab accent isn't doubled-up by
    /// a redundant dot.
    pub activity: TabActivity,
}

/// Thin status-bar strip at the top or bottom of the
/// surface. Disabled by default; when on, the App sets `height` > 0
/// and supplies a pre-formatted single-line string.
///
/// Content is a free-form `String` so the App can compose whatever
/// it wants (the default: "HH:MM:SS UTC · theme · pane title").
/// Renderer just draws background + text; layout / refresh / content
/// composition all live in the App.
pub struct StatusBar {
    /// Height in px (0 = hidden).
    pub height: f32,
    /// Top-left y of the strip. At the top it is 0, or just below a top tab
    /// bar. At the bottom the App places it above the search lane.
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
    /// Top-left Y of the bar. 0 at the top; at the bottom the App places it
    /// above the search lane and any bottom status bar.
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
    /// 0.0..=1.0 tear-off lift, measuring how far the drag has moved from
    /// the tab band toward the tear threshold. 0.0 at/inside the band,
    /// 1.0 at (or past) the distance `tear_threshold_crossed` fires at.
    /// Escalates the ghost's shadow/opacity (`tab_drag::*_LIFT*`) so a
    /// release visibly reads as "this will tear off". Only non-zero while
    /// `drag_cursor_x` is live and the drag FSM is in a Dragging state;
    /// on non-Wayland the tear consumes the gesture at 1.0, so the full
    /// range is mostly visible on Wayland's at-release path.
    pub tear_lift: f32,
    /// `Some(rect)` while a torn-off
    /// window hovers this window's tab band — the accent-colored
    /// insertion marker showing where the dropped tab will land. The
    /// UI computes the rect (a `tab_drag::INSERT_MARKER_PX` line between
    /// segments, oriented per `tab-bar-pos`) so the renderer stays
    /// geometry-free, same contract as `hovered_close_idx`.
    pub insert_marker: Option<Rect4>,
    /// The strip's full band rect in surface
    /// coords, both orientations — the canvas for the dock-target
    /// highlight (`tab_drag::DOCK_HIGHLIGHT_*`) drawn while
    /// `insert_marker` is latched. UI-computed, same geometry-free
    /// contract as `insert_marker`.
    pub band: Rect4,
    /// The back scroll button: `‹` at the start of a top or bottom bar, `⌃`
    /// above the rows of a side bar. Present (non-zero) only while the bar
    /// overflows; clicking it steps to the previous tab, which scrolls the
    /// bar. `(0,0,0,0)` when every tab fits.
    pub scroll_left: Rect4,
    /// The forward scroll button, `›` or `⌄` (see `scroll_left`).
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
    /// Pixel rect `(x, y, w, h)` within the surface: the pane's whole
    /// leaf, its titlebar and preview lane included.
    pub rect: (f32, f32, f32, f32),
    /// The pane's terminal within `rect`: its grid with the padding around
    /// it, clear of its titlebar and preview lane. Everything drawn from the
    /// terminal's cells is placed and clipped by this.
    pub terminal: (f32, f32, f32, f32),
    /// The pane's titlebar strip within `rect`, when it shows one.
    pub titlebar: Option<(f32, f32, f32, f32)>,
    /// Raw terminal state captured under the Term lock by `redraw` (µs-scale
    /// flat copy, pooled per window), borrowed here so the whole GPU frame
    /// runs with the lock released and the PTY reader never stalls behind
    /// shaping/acquire/present.
    pub snap: &'a PaneSnapshot,
    /// Per-pane registration; diagnostic snapshots may omit it.
    pub inline_cards: Option<&'a InlineCards>,
    pub tr: Translator,
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
    /// Remaining visual-bell flash for this pane, 1.0 the frame it rang
    /// down to 0.0 when the flash has decayed. Scales the configured
    /// `bell-flash-intensity` lightness step; only the pane that rang
    /// flashes, so a bell in a sibling split tells you which one it was.
    pub bell_flash: f32,
    /// Terminator parity, titlebar: optional named broadcast group.
    /// When `Some(name)`, the titlebar prefixes `[name]` (group
    /// label in brackets) before the pane title. Borrowed from
    /// `metas`.
    pub group_name: Option<&'a str>,
}

/// The process-wide GPU objects shared by every window's
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
    /// Set `true` when wgpu reports a fatal uncaptured error or a device loss
    /// (a GPU driver TDR/reset, VRAM exhaustion, or internal backend fault).
    /// Validation errors are logged but deliberately excluded. The handlers
    /// installed in `install_gpu_error_handlers` set this flag instead of
    /// letting wgpu's default handler panic, which under the release profile's
    /// `panic = "abort"` would kill kettle on a GPU reset with no crash log.
    /// The App checks this (a refcount-shared `Arc`, so every window's clone
    /// sees it) to stop rendering on a dead device and surface a "GPU device
    /// lost" state rather than spin or crash. Reset by rebuilding the renderer
    /// on a fresh context.
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
    /// Whether the GPU device was lost (driver reset / TDR) or hit a fatal error
    /// (e.g. VRAM exhaustion). Once `true`, no rendering will succeed against
    /// this context.
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

    /// Whether per-frame instances go through mapped buffers on this device
    /// (see `upload::mapped_upload_features`).
    pub fn mapped_uploads(&self) -> bool {
        upload::mapped_uploads(&self.device)
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

/// Installs wgpu's uncaptured-error and device-lost handlers so a GPU fault is
/// logged instead of hitting wgpu's default panic. Under the release
/// `panic = "abort"`, that panic kills kettle with no unwind and no log on a
/// driver TDR/reset or a VRAM allocation failure. A fatal fault sets the shared
/// `gpu_lost` flag, and the App degrades gracefully.
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
    // wgpu delivers a real device loss through `set_device_lost_callback`
    // below, not here; this handler only sees `Validation`, `OutOfMemory`, and
    // `Internal`. A `Validation` error is a kettle code/data bug (bad
    // descriptor, over-limit dim, stale bind group) on a healthy device, so it
    // is logged loudly but does not set `gpu_lost`, which would falsely brick
    // the window with "GPU device lost". Only `OutOfMemory` (VRAM exhaustion)
    // and `Internal` are device-fatal. `Fn`; must not panic.
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
        // `Destroyed` fires only after our own `device.destroy()` (the wedged
        // screenshot reset, which latches its own fault first), so it needs no
        // flag here. Only an `Unknown` loss (driver TDR/reset) flags.
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
    /// The live adapter's identity, in kettle's vocabulary. It feeds the
    /// settings `Active GPU: <gpu> (<kind>, <backend>)` line, so the user sees
    /// which GPU is actually in use rather than the pinned/preferred one, which
    /// only takes effect on restart.
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

/// The foreground glyph drawn on top of a focused solid block cursor this
/// frame. It is rendered in its own tiny renderer and 1-line buffer rather than
/// recolored into the pane text buffer, so a cursor blink does not mutate the
/// pane buffer and force the expensive whole-viewport `prepare`. The glyph
/// bitmap is already in the atlas (it is part of the visible pane text), so the
/// 1-glyph prepare never grows it.
struct PendingCursorGlyph {
    /// Surface-pixel top-left of the cursor cell.
    x: f32,
    y: f32,
    /// Owning row baseline, relative to the cursor cell top.
    row_baseline: f32,
    /// The character under the cursor (drawn in `color`).
    ch: char,
    /// Base plus the snapshot's bounded attached marks.
    text: String,
    /// Whether that cell carried an explicit U+FE0F. The cursor pass makes the
    /// same presentation choice the pane does and has to make it on the same
    /// evidence: `ch` alone cannot show a variation selector, so a qualified
    /// sequence would draw in colour underneath and monochrome on top.
    emoji_qualified: bool,
    attrs: AttrsOwned,
    /// Cursor foreground (theme `cursor_text`, or the cell bg under an OSC 12
    /// runtime cursor color so the inverted glyph follows reverse-video).
    color: Rgb,
    /// Pane rect `(x, y, w, h)` used to clip the glyph to its pane.
    clip: (f32, f32, f32, f32),
}

fn invalidate_pane_titlebar_texts(texts: &mut [String]) {
    texts.iter_mut().for_each(String::clear);
}

fn cursor_glyph_top(cell_top: f32, row_baseline: f32, buffer: &TextBuffer) -> f32 {
    let own_baseline = buffer.layout_runs().next().map_or(0.0, |run| run.line_y);
    cell_top + row_baseline - own_baseline
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
    cursor.row_baseline.to_bits().hash(&mut hash);
    cursor.ch.hash(&mut hash);
    cursor.text.hash(&mut hash);
    cursor.emoji_qualified.hash(&mut hash);
    cursor.attrs.hash(&mut hash);
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

/// The decoded background-image, animated. A still image is one frame; an
/// animated GIF / APNG / animated WebP is many. `frames.is_empty()` encodes a
/// failed decode, which drives the retry throttle.
struct BgImageAnim {
    /// The configured path this was decoded from (cache key part 1).
    path: String,
    /// The blur radius this was decoded with (cache key part 2).
    blur: u32,
    /// Arc-shared pixels keyed by identity in imgpipe's texture cache.
    /// Repeated displays can reuse the uploaded texture.
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

/// CPU-owned renderer state that must survive a surface/device rebuild.
///
/// GPU resources and retained draw caches deliberately stay out of this
/// snapshot: they are tied to the failed device and are rebuilt lazily. The
/// values here are the live per-window overrides that may have diverged from
/// [`Config`] since launch, plus a screenshot request that has not yet been
/// submitted to the worker. The window accent is not among them: the App owns
/// it, can change it while no renderer exists, and applies it to every
/// replacement renderer, so a copy here could only go stale.
#[derive(Clone, Debug)]
pub struct RendererRecoveryState {
    font_family: Arc<str>,
    font_size: f32,
    cell_scale_w: f32,
    cell_scale_h: f32,
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
        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let acquired = outstanding.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |current| (current < MAX_SCREENSHOT_PERSISTENCE_JOBS).then_some(current + 1),
        );
        acquired.ok().map(|_| Self(outstanding.clone()))
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
    /// error), so `apply_bg_image_worker_result` caches the failed key and
    /// throttles the next retry.
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
    fn start(redraw: Option<Arc<dyn Fn() + Send + Sync>>) -> std::io::Result<Self> {
        let (sender, job_rx) = std::sync::mpsc::sync_channel::<BgImageJob>(1);
        let (result_tx, receiver) = std::sync::mpsc::channel::<BgImageResult>();
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_busy = busy.clone();
        std::thread::Builder::new()
            .name("kettle-bg-image".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
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
                    if let Some(redraw) = redraw.as_ref() {
                        redraw();
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
/// `finish_live_screenshot_persistence`.
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
mod live_screenshot_tests;

/// The async background-image decode worker. `render_frame` must never block
/// the render thread on `decode_bg_image_frames_with_blur` (see
/// `Renderer::request_bg_image_reload` / `Renderer::apply_bg_image_worker_result`).
/// Pure CPU + `std::sync::mpsc`, so unlike `ScreenshotWorker`, which needs a
/// real `wgpu::Device`, this is fully unit-testable without a GPU.
#[cfg(test)]
mod bg_image_worker_tests;

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

/// The quad and image pipelines a renderer compiles once and hands to every
/// layer that draws with them. Its five quad layers use two blend modes and
/// its three image layers one, so a window compiles three render pipelines
/// here instead of eight. The offscreen self-test and `--screenshot`, which
/// draw one frame, keep the standalone constructors.
struct SharedPipelines {
    quads: quad::QuadShared,
    images: imgpipe::ImageShared,
}

impl SharedPipelines {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            quads: quad::QuadShared::new(device, format),
            images: imgpipe::ImageShared::new(device, format),
        }
    }
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

/// The starfield shader's clock. `background-animation = off` freezes it on
/// its first frame, as the image path shows frame 0. Otherwise it runs, and a
/// repaint while animation is paused for focus shows the time-correct frame.
fn starfield_time(
    animation: kettle_config::BackgroundAnimation,
    elapsed: std::time::Duration,
) -> f32 {
    if animation == kettle_config::BackgroundAnimation::Off {
        0.0
    } else {
        elapsed.as_secs_f32()
    }
}

#[cfg(test)]
mod starfield_time_tests;

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
    ///
    /// `fonts`, when loaded for the same font and scale, are taken over
    /// instead of loading the system fonts again.
    pub async fn new_with_display_handle<W, D>(
        window: Arc<W>,
        display_handle: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
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
            AdapterSelection::new(AdapterEscalation::Preferred, None),
            fonts,
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
            None,
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
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn new_with_escalation_inner<W>(
        window: Arc<W>,
        display_handle: Option<OwnedGpuDisplayHandle>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        selection: AdapterSelection,
        fonts: Option<StartupFonts>,
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
        // On shared memory the renderer writes per-frame instances through
        // mapped buffers, which needs a feature; elsewhere this is empty.
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-device"),
                required_features: upload::mapped_upload_features(&adapter),
                required_limits,
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("failed to create device: {e:?}"))?;
        // Turn a GPU driver reset (TDR) / VRAM exhaustion into a logged,
        // observable event instead of wgpu's default panic, which `panic=abort`
        // makes a hard crash. Installed once on the shared device.
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
        let built = Self::with_gpu_and_surface(gpu, surface, width, height, scale, cfg, fonts);
        // This span covers everything after device creation, INCLUDING the
        // font-system time logged separately, so do not add the two together.
        log::info!(
            "renderer init: adapter {adapter_ms:.1}ms, device {device_ms:.1}ms,              surface+fonts+pipelines {:.1}ms (font init logged separately is              part of it), total {:.1}ms",
            t_rest.elapsed().as_secs_f64() * 1000.0,
            t_start.elapsed().as_secs_f64() * 1000.0
        );
        built
    }

    /// Synchronous constructor for windows 2..N. Reuses the shared
    /// [`GpuContext`] instead of requesting an adapter/device, so it never
    /// blocks the event loop (the ~1.5s async init and its hung-driver
    /// watchdog are a window-1-only cost). Fails cleanly if the shared adapter
    /// can't present to the new window's surface (e.g. a window on a display
    /// driven by a different GPU); the caller then keeps the tab where it was.
    pub fn new_with_gpu<W>(
        gpu: &GpuContext,
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
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
        Self::with_gpu_and_surface(gpu.clone(), surface, width, height, scale, cfg, fonts)
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
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer> {
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        Self::with_gpu(
            gpu,
            Some(surface),
            format,
            caps.alpha_modes,
            width,
            height,
            scale,
            cfg,
            fonts,
        )
    }

    /// A renderer with no window, drawing only into the offscreen capture
    /// target, so tests can render real panes through the live frame path.
    /// `None` when the host has no GPU adapter.
    #[cfg(test)]
    pub(crate) fn headless_for_tests(
        cfg: &Config,
        width: u32,
        height: u32,
    ) -> Result<Option<Renderer>> {
        Self::headless_for_tests_with(cfg, width, height, 1.0, false)
    }

    /// [`Self::headless_for_tests`] at `scale`. With `translucent_surface`
    /// the pretend surface also offers PostMultiplied, as a macOS window does,
    /// so a translucent config takes the translucent path.
    #[cfg(test)]
    pub(crate) fn headless_for_tests_with(
        cfg: &Config,
        width: u32,
        height: u32,
        scale: f32,
        translucent_surface: bool,
    ) -> Result<Option<Renderer>> {
        Self::headless_for_tests_with_mapping(cfg, width, height, scale, translucent_surface, true)
    }

    /// [`Self::headless_for_tests_with`], and with `allow_mapping` false the
    /// queue upload path on the same adapter the live upload path uses.
    #[cfg(test)]
    pub(crate) fn headless_for_tests_with_mapping(
        cfg: &Config,
        width: u32,
        height: u32,
        scale: f32,
        translucent_surface: bool,
        allow_mapping: bool,
    ) -> Result<Option<Renderer>> {
        pollster::block_on(async {
            let Ok((instance, adapter)) = resolve_headless_adapter(cfg, "headless-renderer").await
            else {
                return Ok(None);
            };
            // The live device's features, so these tests take the upload
            // path a window on this host takes.
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-headless-renderer"),
                    required_features: if allow_mapping {
                        upload::mapped_upload_features(&adapter)
                    } else {
                        wgpu::Features::empty()
                    },
                    required_limits: live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow!("device: {e:?}"))?;
            let gpu = GpuContext {
                instance,
                adapter,
                device,
                queue,
                gpu_lost: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                gpu_fault: std::sync::Arc::new(std::sync::Mutex::new(None)),
                recovery_wake: std::sync::Arc::new(std::sync::Mutex::new(None)),
            };
            let mut alpha_modes = vec![wgpu::CompositeAlphaMode::Opaque];
            if translucent_surface {
                alpha_modes.push(wgpu::CompositeAlphaMode::PostMultiplied);
            }
            Self::with_gpu(
                gpu,
                None,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                alpha_modes,
                width,
                height,
                scale,
                cfg,
                None,
            )
            .map(Some)
        })
    }

    /// Shared tail of every constructor, once a GPU and (for a window) a
    /// surface exist.
    #[allow(clippy::too_many_arguments)]
    fn with_gpu(
        gpu: GpuContext,
        surface: Option<wgpu::Surface<'static>>,
        format: wgpu::TextureFormat,
        supported_alpha_modes: Vec<wgpu::CompositeAlphaMode>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer> {
        let GpuContext { device, queue, .. } = gpu.clone();
        let alpha_mode = desired_alpha_mode(cfg, &supported_alpha_modes);
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
        if let Some(surface) = &surface {
            surface.configure(&device, &config);
        }

        // Fonts loaded before the window reach here already measured. Ones
        // measured for another scale or font are measured again, without
        // enumerating the system fonts a second time.
        let StartupFonts {
            mut font_system,
            text_symbol_family,
            cell: (cell_w, cell_h),
            ..
        } = match fonts {
            Some(fonts) if fonts.matches(cfg, scale) => fonts,
            Some(fonts) => fonts.remeasure(cfg, scale),
            None => StartupFonts::load(cfg, scale),
        };
        let cell_scale_w = cfg.cell_width.max(0.01);
        let cell_scale_h = cfg.cell_height.max(0.01);

        let swash = SwashCache::new();
        let cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let viewport = Viewport::new(&device, &cache);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        // Clamp `cfg.font_size` to the same [5.0, 72.0] range as the runtime
        // `set_font_size` path. Without this, a user config of
        // `font-size = 200` boots the renderer with 200pt cells and hits the
        // wgpu 8192px-per-side texture limit (or floods the window with one
        // giant glyph). 5.0 is below "tiny but legible"; 72.0 is "billboard".
        let font_size = clamp_font_size(cfg.font_size);
        // Physical-pixel metrics — logical font size × DPI scale.
        let metrics = metrics_for(font_size, scale);
        let tabbar_buffer = TextBuffer::new(&mut font_system, metrics);
        let new_tab_arrow_buffer = TextBuffer::new(&mut font_system, metrics);
        let scroll_left_buffer = TextBuffer::new(&mut font_system, metrics);
        let scroll_right_buffer = TextBuffer::new(&mut font_system, metrics);
        let tab_close_buffer = TextBuffer::new(&mut font_system, metrics);
        let mut search_buffer = TextBuffer::new(&mut font_system, metrics);
        search_buffer.set_wrap(Wrap::None);
        let search_segment_buffers = (0..SEARCH_SEGMENTS)
            .map(|_| {
                let mut buffer = TextBuffer::new(&mut font_system, metrics);
                buffer.set_wrap(Wrap::None);
                buffer
            })
            .collect();
        let status_bar_buffer = TextBuffer::new(&mut font_system, metrics);
        let resize_overlay_buffer = TextBuffer::new(&mut font_system, metrics);
        let update_toast_buffers =
            std::array::from_fn(|_| TextBuffer::new(&mut font_system, metrics));
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

        let shared = SharedPipelines::new(&device, format);
        let pane_bases = shared.quads.replace(&device);
        let live_pane_bases = shared.quads.replace(&device);
        let quads = shared.quads.blend(&device);
        let pane_outlines = OutlinePipeline::new(&device, format);
        let pane_dimming = shared.quads.blend(&device);
        let text_decorations = shared.quads.blend(&device);
        let overlay_quads = shared.quads.blend(&device);
        let menu_quads = shared.quads.blend(&device);
        let menu_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let rounded_surfaces = rounded_surface::RoundedSurfacePipeline::new(&device, format);
        let confirmation_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let confirmation_text = confirm::ConfirmText::new();
        let cursor_glyph_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let cursor_glyph_buffer = TextBuffer::new(&mut font_system, metrics);
        let graphics_budget = kettle_core::GraphicsBudget::default();
        let card_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let card_base = shared.quads.blend(&device);
        let lane_quads = shared.quads.blend(&device);
        let lane_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let card_cursors = shared.quads.blend(&device);
        let card_decoration = shared.quads.blend(&device);
        let presentation =
            present::PresentationPipeline::new(&device, format, graphics_budget.clone());
        let imgs = shared
            .images
            .layer(&device, graphics_budget.clone())
            .ok_or_else(|| {
                anyhow!("GPU graphics budget exhausted while creating image pipeline")
            })?;
        let media_receipt_img = shared
            .images
            .layer_with_instance_limit(&device, graphics_budget.clone(), 1)
            .ok_or_else(|| {
                anyhow!("GPU graphics budget exhausted while creating image-receipt pipeline")
            })?;
        // Separate image layer so the wallpaper draws behind cell/chrome quads
        // (see the `bg_imgs` field docs).
        let bg_imgs = shared
            .images
            .layer_with_instance_limit(&device, graphics_budget.clone(), 1)
            .ok_or_else(|| {
                anyhow!("GPU graphics budget exhausted while creating background image pipeline")
            })?;
        // Procedural starfield wallpaper, same back-most slot.
        let starfield = starfield::StarfieldPipeline::new(&device, format);
        // Cell-locked pane-text pipeline (the `text-renderer=grid` default).
        // Always constructed; only emitted/drawn in grid mode.
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
            text_symbol_family,
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
            cursor_quad_range: None,
            text_prepares: 0,
            chrome_prepares: 0,
            frames_presented: 0,
            last_scene: None,
            cursor_patch: None,
            #[cfg(target_os = "macos")]
            cursor_layer: None,
            span_breaks_scratch: Vec::new(),
            minimum_contrast_cache: MinimumContrastCache::default(),
            chrome_colors: chrome::Colors::default(),
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
            update_toast_buffers,
            update_toast_text: std::array::from_fn(|_| String::new()),
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
            search_segment_buffers,
            search_segment_texts: vec![String::new(); SEARCH_SEGMENTS],
            search_editor_stops: Vec::new(),
            search_editor_clusters: Vec::new(),
            search_editor_rtl: false,
            status_bar_buffer,
            pane_bases,
            live_pane_bases,
            quads,
            pane_outlines,
            pane_dimming,
            text_decorations,
            overlay_quads,
            menu_quads,
            menu_text_renderer,
            rounded_surfaces,
            transport_surface_count: 0,
            confirmation_text_renderer,
            confirmation_text,
            confirmation_labels_prepared: false,
            imgs,
            card_base,
            card_cursors,
            card_cursor_quad_range: None,
            card_decoration,
            card_posters: None,
            lane_quads,
            lane_img: None,
            lane_text_renderer,
            lane_texts: std::collections::HashMap::new(),
            lane_labels_prepared: false,
            card_image_shared: shared.images,
            card_frames: Vec::new(),
            card_scene: card_scene::CardScene::default(),
            drawn_cards: Vec::new(),
            painted_cards: Vec::new(),
            painted_shown: Vec::new(),
            card_text_renderer,
            card_label_buffers: Vec::new(),
            card_label_texts: Vec::new(),
            card_labels_prepared: false,
            media_receipt_img,
            bg_imgs,
            starfield,
            presentation,
            starfield_started: std::time::Instant::now(),
            bg_image_cache: None,
            bg_image_retry_at: None,
            bg_image_worker: None,
            bg_image_redraw: None,
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

    /// The shared GPU handles, for spawning another window's Renderer via
    /// [`Renderer::new_with_gpu`]. Cloning the returned context is a refcount
    /// bump.
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
        self.last_scene = None;
        self.font_family = state.font_family.clone();
        self.font_size = clamp_font_size(state.font_size);
        self.cell_scale_w = state.cell_scale_w.max(0.01);
        self.cell_scale_h = state.cell_scale_h.max(0.01);
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
        self.last_scene = None;
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
        self.last_scene = None;
        // Resize recomputes wallpaper geometry without re-decoding its cached source frames.
        // `bg_image_cache` stores the DECODED image, not a window-sized
        // texture, and every frame recomputes the image rect from the current
        // surface dims (see `background_image_rect`), so a resize takes effect
        // on the next frame.
        //
        // Floor at 1 (`surface.configure(0, ...)` panics) and ceiling at the
        // device's max-texture-dimension-2d. The device requests the ADAPTER's
        // full 2D limit rather than wgpu's default 8192, so a window stretched
        // across multiple 4K monitors keeps its real physical size on hardware
        // that can present it. The ceiling stays because, on an adapter whose
        // genuine limit is smaller than the window, configuring past it fails
        // validation and leaves a stale surface that paints nothing at all.
        // Clipping to the visible top-left region is the better failure.
        // Sibling to `cap_axis_cells` (same bug class on the `--screenshot`
        // path).
        let (width, height) = live_surface_dimensions(
            width,
            height,
            self.gpu.device.limits().max_texture_dimension_2d,
        );
        self.config.width = width;
        self.config.height = height;
        self.configure_surface();
    }

    fn configure_surface(&self) {
        if let Some(surface) = &self.surface {
            surface.configure(&self.gpu.device, &self.config);
        }
    }

    pub fn set_background_compositing(&mut self, cfg: &Config) {
        let alpha_mode = desired_alpha_mode(cfg, &self.supported_alpha_modes);
        if self.config.alpha_mode == alpha_mode {
            return;
        }
        self.last_scene = None;
        self.config.alpha_mode = alpha_mode;
        self.configure_surface();
    }

    /// Glyph boundaries of the search query painted last frame, as (query
    /// byte, window x), sorted by byte. Empty while the search lane is closed.
    /// Pointer input maps a click to the nearest one, so the caret lands where
    /// it is drawn whatever width the font gives each glyph.
    pub fn search_editor_stops(&self) -> &[(usize, f32)] {
        &self.search_editor_stops
    }

    /// The clusters of the search query painted last frame, sorted by byte.
    /// Pointer input uses them to find the character under a click.
    pub fn search_editor_clusters(&self) -> &[SearchEditorCluster] {
        &self.search_editor_clusters
    }

    /// Whether the search query painted last frame reads right to left, so
    /// its start is at its right end.
    pub fn search_editor_rtl(&self) -> bool {
        self.search_editor_rtl
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// The cards the last frame drew, whether or not it reached the screen.
    pub fn drawn_cards(&self) -> &[PaintedCard] {
        &self.drawn_cards
    }

    /// The cards on screen: those the last presented frame drew.
    pub fn painted_cards(&self) -> &[PaintedCard] {
        &self.painted_cards
    }

    /// The cards on screen whose image the last presented frame showed: the
    /// poster reached the GPU, not only the card's frame and status.
    pub fn shown_cards(&self) -> impl Iterator<Item = &PaintedCard> + '_ {
        self.painted_cards
            .iter()
            .zip(&self.painted_shown)
            .filter_map(|(card, shown)| shown.then_some(card))
    }

    /// Put the frame's cards on screen, as presented: where each is, and
    /// whether its image showed.
    fn commit_painted_cards(&mut self) {
        debug_assert_eq!(self.drawn_cards.len(), self.card_scene.shown.len());
        self.painted_cards.clone_from(&self.drawn_cards);
        self.painted_shown.clone_from(&self.card_scene.shown);
    }

    /// The card on screen under surface point (`x`, `y`), if any.
    pub fn card_at(&self, x: f32, y: f32) -> Option<PaintedCard> {
        self.painted_cards
            .iter()
            .rev()
            .find(|card| card.contains(x, y))
            .copied()
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
        self.last_scene = None;
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

    /// The face this renderer requests for codepoints Unicode renders as text,
    /// or `None` when the system has no candidate that carries them.
    ///
    /// Surfaced through `ui_geometry` because the answer depends entirely on
    /// which fonts the host has. A verification scenario that asserts the
    /// bullet is monochrome is asserting something the host may be unable to
    /// produce, and it should say so rather than fail.
    pub fn text_presentation_face(&self) -> Option<&'static str> {
        self.text_symbol_family
    }

    /// Update the device-pixel scale factor (DPI). Wired to winit's
    /// `ScaleFactorChanged`, fired at startup and whenever the window moves to
    /// a monitor with a different scale. Recomputes physical metrics from the
    /// unchanged *logical* `font_size` and re-measures the cell, so glyphs keep
    /// the same visual size across DPI changes. No-op when the scale is
    /// unchanged. The caller must re-grid afterward (cell_w/cell_h change),
    /// e.g. via `App::resize_all`.
    pub fn set_scale(&mut self, scale: f32) {
        self.last_scene = None;
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
    /// takes effect at runtime, not only the `font-size` part of a reload.
    pub fn set_font_family(&mut self, family: String) {
        self.last_scene = None;
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
        self.swash = SwashCache::new();
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
        self.last_scene = None;
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

    /// Whether the selected background should advance proactively. Focus gates
    /// `when-focused`; the event loop separately suppresses hidden-window wakes.
    /// Other repaints still select the time-correct frame.
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

    /// Milliseconds until the animated background's displayed frame next
    /// changes. The event loop uses this as the bg-animation wake interval.
    /// Waking at the actual frame boundary caps repaints at the GIF's own fps,
    /// where a fixed 30 fps tick would repaint the SAME frame ~22×/s for a
    /// typical 8 fps GIF (wasted full-surface `present()`s). `None` when the
    /// background isn't animating. Floored at 16 ms so a degenerate fast GIF
    /// can't drive the loop past ~60 fps.
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

    /// The animated background's currently-displayed frame index, or `None`
    /// when it isn't animating. The event loop compares this against the
    /// last-painted index and requests a redraw ONLY when it changes (an
    /// edge-trigger, like the cursor blink). Calling `request_redraw` on every
    /// `about_to_wait` while the bg animates would make winit redraw
    /// continuously (vsync-bound) instead of at the GIF's fps, which keeps
    /// animated-idle CPU high.
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

    /// Install the owning window's redraw callback before its first frame.
    pub fn ensure_background_redraw(&mut self, redraw: impl Fn() + Send + Sync + 'static) {
        if self.bg_image_redraw.is_none() {
            self.bg_image_redraw = Some(Arc::new(redraw));
        }
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
            match BgImageWorker::start(self.bg_image_redraw.clone()) {
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

    /// Drain finished decodes, installing only the pending job whose key also
    /// matches the current configuration. Clear a completed pending job even
    /// when its result is obsolete, so the same key can be requested again.
    fn apply_bg_image_worker_result(&mut self, path: &str, blur_radius: u32) {
        let Some(worker) = self.bg_image_worker.as_ref() else {
            return;
        };
        while let Some(result) = worker.try_recv() {
            let is_pending = self
                .bg_image_pending
                .as_ref()
                .is_some_and(|(p, b)| *p == result.path && *b == result.blur_radius);
            if !is_pending {
                continue;
            }
            self.bg_image_pending = None;
            if result.path != path || result.blur_radius != blur_radius {
                continue;
            }
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

        if overlay.confirm_dialog.is_none()
            && let Some(receipt) = &overlay.media_paste_receipt
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
        let focus_origin = panes.iter().find(|p| p.focused).map(|p| p.terminal);

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
        if let Some(toast) = &overlay.update_toast {
            let text_line_height = self.overlay_text_line_height();
            let (x, y, w, h) = toast.geometry.rect;
            let alpha = toast.opacity.clamp(0.0, 1.0);
            let background = theme.palette[0];
            let foreground = self.chrome_colors.text(theme.foreground, background);
            menu_q.push(rect(x, y, w, h, background, alpha));
            let accent = self.ui_accent(cfg, theme);
            let accent = self.chrome_colors.icon(accent, background);
            menu_q.push(rect(x, y, w, 1.0, accent, alpha));
            if let Some(index) = toast.focused.filter(|i| *i == 1 || *i == 2) {
                let (bx, by, bw, bh) = if index == 1 {
                    toast.geometry.details
                } else {
                    toast.geometry.close
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

            for (i, area) in [
                toast.geometry.message,
                toast.geometry.details,
                toast.geometry.close,
            ]
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
            && let Some(geometry) = media_paste_receipt_geometry(
                receipt,
                overlay.completion.as_ref(),
                (cw, ch),
                self.overlay_text_cell_width(),
                self.metrics.line_height,
            )
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

    /// Everything this renderer has sent to the GPU so far (see
    /// `RenderUploads`), summed over its pipelines.
    pub fn render_uploads(&self) -> RenderUploads {
        let counts: UploadCounts = [
            self.glyph_pipeline.upload_counts(),
            self.pane_bases.upload_counts(),
            self.live_pane_bases.upload_counts(),
            self.quads.upload_counts(),
            self.pane_dimming.upload_counts(),
            self.text_decorations.upload_counts(),
            self.pane_outlines.upload_counts(),
            self.overlay_quads.upload_counts(),
            self.menu_quads.upload_counts(),
            self.rounded_surfaces.upload_counts(),
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

    /// Encode the whole scene into `target`. With a `window`, `target` holds
    /// only that rect of the `target_size` scene (a cursor patch pass): the
    /// viewport maps the scene 1:1 with the window's top-left at the origin,
    /// so every pipeline keeps its full-size screen uniform.
    #[allow(clippy::too_many_arguments)]
    fn encode_scene_pass(
        &self,
        target: &wgpu::TextureView,
        target_size: [u32; 2],
        cfg: &Config,
        live_window: bool,
        cursor_on: bool,
        window: Option<PatchRect>,
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
        if use_live_pane_bases(live_window, self.live_background_opacity_floor) {
            self.live_pane_bases.draw(&mut pass);
        } else {
            self.pane_bases.draw(&mut pass);
        }
        // The blink's off phase skips the cursor's quads; the same instances
        // stay uploaded, and the blend order of everything else is unchanged.
        let hidden_cursor = if cursor_on {
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
        match window {
            None => {
                self.glyph_pipeline
                    .draw(&mut pass, &self.glyph_clips, target_size);
            }
            Some(window) => self.glyph_pipeline.draw_in_window(
                &mut pass,
                &self.glyph_clips,
                target_size,
                [window.x, window.y, window.width, window.height],
            ),
        }
        self.text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        self.card_cursors.draw_hiding(
            &mut pass,
            if cursor_on {
                None
            } else {
                self.card_cursor_quad_range.clone()
            },
        );
        // The focused solid-block cursor's inverted glyph sits on its block
        // quad and the normal glyph, but below every overlay and chrome layer:
        // an opaque menu or receipt must cover all of the terminal cursor. The
        // off phase shows the glyph beneath in its normal colour instead.
        if cursor_on && self.pending_cursor_glyph.is_some() {
            self.cursor_glyph_renderer
                .render(&self.atlas, &self.viewport, &mut pass)?;
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
        self.menu_quads.draw(&mut pass);
        self.media_receipt_img.draw(&mut pass);
        self.menu_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
        self.rounded_surfaces
            .draw_range(&mut pass, self.transport_surface_count..u32::MAX);
        self.confirmation_text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)?;
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

    /// Build one pane's cells, cursor and text, and keep every quad it adds
    /// inside the pane's terminal: a snapshot can be briefly larger than a
    /// terminal that has just shrunk for its preview lane, and its extra
    /// rows must not paint over the lane or past the pane.
    #[allow(clippy::too_many_arguments)]
    fn build_pane(
        &mut self,
        idx: usize,
        pv: &PaneView<'_>,
        cfg: &Config,
        inactive: f32,
        family: &str,
        window_focused: bool,
        search_highlights: &[HighlightRect],
        quads: &mut Vec<QuadInstance>,
        decorations: &mut Vec<QuadInstance>,
        pane_bases: &mut Vec<QuadInstance>,
        card_frame: &inline_cards::CardFrame,
        card_base: &mut Vec<QuadInstance>,
        card_cursors: &mut Vec<QuadInstance>,
    ) -> bool {
        let starts = (
            quads.len(),
            pane_bases.len(),
            card_base.len(),
            card_cursors.len(),
            decorations.len(),
        );
        let text_changed = self.build_pane_cells(
            idx,
            pv,
            cfg,
            inactive,
            family,
            window_focused,
            search_highlights,
            quads,
            decorations,
            pane_bases,
            card_frame,
            card_base,
            card_cursors,
        );
        clip_quads(&mut quads[starts.0..], pv.terminal);
        clip_quads(&mut decorations[starts.4..], pv.terminal);
        clip_quads(&mut pane_bases[starts.1..], pv.terminal);
        clip_quads(&mut card_base[starts.2..], pv.terminal);
        clip_quads(&mut card_cursors[starts.3..], pv.terminal);
        text_changed
    }

    /// Build one pane's text buffer + background/cursor/selection/search quads.
    #[allow(clippy::too_many_arguments)]
    fn build_pane_cells(
        &mut self,
        idx: usize,
        pv: &PaneView<'_>,
        cfg: &Config,
        inactive: f32,
        family: &str,
        window_focused: bool,
        search_highlights: &[HighlightRect],
        quads: &mut Vec<QuadInstance>,
        decorations: &mut Vec<QuadInstance>,
        pane_bases: &mut Vec<QuadInstance>,
        card_frame: &inline_cards::CardFrame,
        card_base: &mut Vec<QuadInstance>,
        card_cursors: &mut Vec<QuadInstance>,
    ) -> bool {
        // True iff this pane mutated its text buffer this frame (a row
        // reshaped, or the line count changed). When no pane changed, chrome
        // text is identical, and no overlay is open, `render_frame_with_status`
        // skips the whole-viewport glyphon `prepare` and re-renders the cached
        // glyph vertices. A cursor blink that doesn't touch text
        // (bar/underline/hollow, or any steady cursor) therefore costs no
        // reshape and no glyph re-encode.
        let mut text_changed = false;
        let theme = &cfg.theme;
        let (_, _, rw, rh) = pv.terminal;
        let (ox, oy) = pane_grid_origin(pv.terminal, (cfg.padding_x, cfg.padding_y));
        let cw = self.cell_w;
        let ch = self.cell_h;
        // Everything below reads the lock-free snapshot captured by `redraw`,
        // so the Term mutex is not held while we process it.
        let snap = pv.snap;
        let term_colors = &snap.colors;
        let cols = snap.columns;
        // Cells inside the selection range get their fg swapped to
        // `theme.selection_foreground` so dark-on-dark themes stay readable
        // under the highlight.
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
        if let Some((bx, by, bwid, bhgt)) = pane_backdrop_rect(pv.terminal, bw) {
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

        let fallback_clip = pane_backdrop_rect(pv.terminal, bw).and_then(|body| {
            inline_image_clip(body, (ox, oy), (cols, snap.screen_lines), (cw, ch))
        });

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
        // Built in its on phase whatever the blink phase: the off phase skips
        // the cursor's quads at draw time (`cursor_quad_range`), so a blink
        // changes no uploaded instance and no prepared text.
        let base_draw_cursor = cursor_focus_gate(
            window_focused,
            shape != EShape::Hidden && (0..screen_rows).contains(&cvrow) && pv.focused,
        );
        // Codex's native-Windows cursor compatibility shim applies to the
        // application's writing cursor, never to the user-controlled vi
        // cursor.
        let draw_cursor = !card_frame.suppress_point(snap, cp.line.0, cp.column.0)
            && if snap.vi_mode {
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
        // Capture the glyph under a focused solid block cursor (with its
        // color) and draw it in the dedicated cursor-glyph pass on top of the
        // block. Recoloring it in the pane text buffer would dirty the cursor
        // row's shaping cache every blink and force a whole-viewport
        // re-prepare. This way the buffer stays byte-identical across a blink,
        // so the prepare is skipped.
        let mut cursor_glyph_capture: Option<(char, Rgb, &[char], AttrsOwned)> = None;
        let mut fallback_run = None;

        for (snapshot_index, sc) in snap.cells.iter().enumerate() {
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

            let card_cell = card_frame.cell(snap, snapshot_index);
            let owned_cell = !matches!(card_cell, inline_cards::CardCell::Ordinary);
            let flags = if owned_cell { Flags::empty() } else { sc.flags };
            // Terminator parity, terminatorlib/config.py:111
            // `allow_bold`: when false, suppress bold attr entirely.
            // Useful on fonts without a bold companion.
            let bold = cfg.allow_bold && flags.contains(Flags::BOLD);
            let (raw_fg, raw_bg) = if flags.contains(Flags::INVERSE) {
                (sc.bg, sc.fg)
            } else {
                (sc.fg, sc.bg)
            };
            let raw_fg = if bold && cfg.bold_is_bright {
                color::bright_for_bold(raw_fg)
            } else {
                raw_fg
            };
            let mut fg = if owned_cell {
                theme.foreground
            } else {
                color::resolve(raw_fg, theme, term_colors)
            };
            let bg = if owned_cell {
                theme.background
            } else {
                color::resolve(raw_bg, theme, term_colors)
            };
            let selected = selection_range.is_some_and(|r| r.contains(sc.point()));
            let italic = flags.contains(Flags::ITALIC);
            saw_styled_text |= bold || italic;
            let hidden = flags.contains(Flags::HIDDEN);
            let dc = card_cell.glyph(sc.c, hidden);
            let marks: &[char] = if hidden || owned_cell {
                &[]
            } else {
                sc.zerowidth()
            };
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
            if matches!(
                card_cell,
                inline_cards::CardCell::Neutral | inline_cards::CardCell::Unknown
            ) {
                let backdrop = match highlight {
                    CellHighlight::Search(true) => {
                        cfg.search_background.unwrap_or(theme.palette[3])
                    }
                    CellHighlight::Search(false) | CellHighlight::Selection => {
                        theme.selection_background
                    }
                    CellHighlight::None => theme.background,
                };
                if let Some(clip) = fallback_clip {
                    card_scene::append_fallback_background(
                        card_base,
                        &mut fallback_run,
                        row,
                        col,
                        backdrop,
                        [ox + col as f32 * cw, oy + vrow as f32 * ch, cw, ch],
                        clip,
                    );
                }
            }
            fg = resolved_cell_foreground_cached(
                fg,
                bg,
                highlight,
                (flags.contains(Flags::DIM), bold),
                cfg,
                theme,
                &mut self.minimum_contrast_cache,
            );
            let cell_background = match highlight {
                CellHighlight::Search(true) => cfg.search_background.unwrap_or(theme.palette[3]),
                CellHighlight::Search(false) | CellHighlight::Selection => {
                    theme.selection_background
                }
                CellHighlight::None => bg,
            };
            if inactive > 0.0 {
                fg = self.chrome_colors.inactive(
                    fg,
                    cell_background,
                    theme.background,
                    inactive,
                    self.config.format.is_srgb(),
                );
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
                // The glyph keeps its NORMAL `fg` in the pane buffer; the
                // cursor pass draws this recolored copy on top of the block.
                // See `color::cursor_glyph_color` for which colour and why.
                let cursor_fg = color::cursor_glyph_color(theme, term_colors, bg);
                let mut attrs = run_attrs(cfg, &font_features(cfg), cursor_fg, bold, italic);
                if let Some(symbol_family) = self
                    .text_symbol_family
                    .filter(|_| !marks.contains(&'\u{FE0F}') && single_column_text_presentation(dc))
                {
                    attrs = attrs.family(Family::Name(symbol_family));
                }
                cursor_glyph_capture = Some((dc, cursor_fg, marks, AttrsOwned::new(&attrs)));
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
            // SGR 4 underline family / SGR 9 strikeout, both engine-tracked
            // (see the `sgr_underline_dim_strike` test).
            //
            // Underline color: SGR 58 (`\e[58;2;r;g;bm` / `[58;5;Nm`) sets
            // a per-cell `underline_color`, used by neovim spell-check to
            // draw red squiggles on otherwise-normal text. Resolve it via
            // the same path as fg/bg; fall back to `fg` when unset.
            //
            // Underline style: alacritty exposes five style bits (UNDERLINE,
            // DOUBLE_UNDERLINE, UNDERCURL, DOTTED_UNDERLINE, DASHED_UNDERLINE),
            // all reached via `Flags::ALL_UNDERLINES` and mutually exclusive
            // (alacritty_terminal clears the others whenever SGR 4 sets a new
            // sub-style; see `term/mod.rs`'s `Attr::Underline*` arms).
            // `push_underline_quads` draws each style as a distinct shape,
            // including DOUBLE_UNDERLINE's second line; see its doc comment
            // for the undercurl approximation's limits.
            let line_color = sc
                .underline_color
                .map(|c| {
                    let line = color::resolve(c, theme, term_colors);
                    self.chrome_colors.inactive(
                        line,
                        cell_background,
                        theme.background,
                        inactive,
                        self.config.format.is_srgb(),
                    )
                })
                .unwrap_or(fg);
            let decoration_quads = if inactive > 0.0 {
                &mut *decorations
            } else {
                &mut *quads
            };
            push_cell_decorations(
                decoration_quads,
                (ox + col as f32 * cw, oy + vrow as f32 * ch, cw, ch),
                col,
                flags,
                fg,
                line_color,
            );
            // Combining (zero-width) marks layered on this cell — a decomposed
            // accent (`e`+U+0301), an emoji ZWJ sequence, a variation selector.
            // Append them right after the base char so the shaper composes the
            // full grapheme; skip on a HIDDEN cell (the base became a space, so
            // the marks have nothing to attach to).
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
        // DEC ?25l (vim, less, fzf) reaches us as `EShape::Hidden`, which
        // `draw_cursor` excludes, so outside vi mode it hides every shape,
        // including the HollowBlock outline.
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
            // takes precedence over the theme, the same precedence rule the
            // OSC 4/10/11/12 *query* path returns.
            let cursor_color = if snap.vi_mode {
                // Keep vi navigation distinct from both the application
                // cursor and broadcast-mode yellow.
                theme.palette[5]
            } else {
                color::cursor_block_color(theme, term_colors)
            };
            let on_card_fallback = matches!(
                card_frame.cell_at(snap, cp.line.0, cp.column.0),
                inline_cards::CardCell::Neutral | inline_cards::CardCell::Unknown
            );
            let cursor_quads = if on_card_fallback {
                card_cursors
            } else {
                quads
            };
            let cursor_start = cursor_quads.len() as u32;
            // HollowBlock comes from vi mode. DECSCUSR selects block, beam
            // or underline. Window focus gates drawing without changing DEC state.
            if shape == EShape::HollowBlock {
                cursor_quads.push(rect(bx, by, cw, 1.0, cursor_color, 1.0));
                cursor_quads.push(rect(bx, by + ch - 1.0, cw, 1.0, cursor_color, 1.0));
                cursor_quads.push(rect(bx, by, 1.0, ch, cursor_color, 1.0));
                cursor_quads.push(rect(bx + cw - 1.0, by, 1.0, ch, cursor_color, 1.0));
            } else {
                let (cwidth, alpha, cheight, yoff) = match shape {
                    EShape::Beam => (cw * 0.15, 1.0, ch, 0.0),
                    EShape::Underline => (cw, 1.0, 2.0, ch - 2.0),
                    // A focused block cursor is solid. The inverted glyph under
                    // it is drawn in the dedicated cursor-glyph pass (see below),
                    // not recolored into the pane buffer, so a blink does not
                    // reshape the row. `bcells` widens it over a wide
                    // (CJK/emoji) glyph so the right half stays covered.
                    EShape::Block | EShape::HollowBlock | EShape::Hidden => {
                        (cw * bcells, 1.0, ch, 0.0)
                    }
                };
                cursor_quads.push(rect(bx, by + yoff, cwidth, cheight, cursor_color, alpha));
                // Queue the inverted foreground glyph to be
                // drawn ON TOP of the solid block in its own pass. Only the
                // full Block shape covers the glyph; beam/underline leave it
                // visible in its normal color, so they need no overdraw.
                if matches!(shape, EShape::Block)
                    && let Some((gch, gcolor, gmarks, gattrs)) = cursor_glyph_capture
                {
                    self.pending_cursor_glyph = Some(PendingCursorGlyph {
                        x: bx,
                        y: by,
                        row_baseline: 0.0,
                        ch: gch,
                        text: std::iter::once(gch).chain(gmarks.iter().copied()).collect(),
                        emoji_qualified: gmarks.contains(&'\u{FE0F}'),
                        attrs: gattrs,
                        color: gcolor,
                        clip: pv.terminal,
                    });
                }
            }
            // vi mode's cursor never blinks, so only an application cursor
            // gets a range the off phase can skip.
            if !snap.vi_mode {
                let range = Some(cursor_start..cursor_quads.len() as u32);
                if on_card_fallback {
                    self.card_cursor_quad_range = range;
                } else {
                    self.cursor_quad_range = range;
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
            Some((rh - cfg.padding_y * 2.0).max(1.0)),
        );
        // Terminal rows are hard-wrapped by the VT engine at `cols`; the renderer
        // must NEVER soft-wrap. Wrap::None keeps exactly one layout run per buffer
        // line so a too-wide fallback glyph can't push the line onto a
        // phantom second visual row. cosmic-text no-ops this when unchanged.
        buf.set_wrap(Wrap::None);
        let ff = font_features(cfg);
        // Copied out before the `&mut self` borrows below.
        let text_symbol_family = self.text_symbol_family;
        let default_attrs = Attrs::new()
            .family(Family::Name(family))
            .font_features(ff.clone());
        // Always Advanced: it is the only shaping mode that walks
        // cosmic-text's platform font-fallback cascade (CJK, emoji, symbols),
        // so Basic would tofu-box every fallback glyph. The ligature toggle is
        // fully expressed as OpenType features (`font_features()` emits
        // liga/clig/calt/dlig=0 when off), so it needs no shaping-mode switch.
        // Cost is bounded by the per-line shaping cache below: only rows whose
        // content changed re-shape.
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
        // Row r's runs are `spans[breaks[r-1]..breaks[r]]`. `span_line_breaks`
        // records the live run count at each row transition (one entry per
        // crossed row, `rows - 1` total).
        let mut start = 0usize;
        for row in 0..rows {
            let end = span_line_breaks.get(row).copied().unwrap_or(n).min(n);
            let runs = &spans[start.min(end)..end];
            start = end;
            // Shape the row only up to its last inked cell (see `ShapedRow`).
            // The key covers exactly what is shaped, so recolouring the
            // blanks after that cell reshapes nothing.
            let shaped = ShapedRow::of(runs);
            let key = shaped.key(runs);
            let prev = keys.get(row).copied();
            if prev == Some(key) {
                continue;
            }
            // This row reshapes — the buffer's glyph vertices will differ.
            text_changed = true;
            row_text.clear();
            let mut attrs_list = AttrsList::new(&default_attrs);
            for (i, (text, fg, bold, italic)) in runs[..shaped.runs].iter().enumerate() {
                let text = shaped.text(i, text);
                let s = row_text.len();
                row_text.push_str(text);
                let a = run_attrs(cfg, &ff, *fg, *bold, *italic);
                // Mirror `set_rich_text`: only record a span when it differs
                // from the row defaults (fewer spans = cheaper compares).
                if a != attrs_list.defaults() {
                    attrs_list.add_span(s..row_text.len(), &a);
                }
                // Ask for a text face on the cells Unicode renders as text by
                // default. Nothing in the stack below consults
                // `Emoji_Presentation`: cosmic-text takes the first family in
                // the cascade whose cmap has the codepoint, and on macOS that
                // is Apple Color Emoji for anything the text faces lack. `⏺`
                // U+23FA is one cell wide, so a square colour bitmap both
                // reads wrong and covers the next column. Narrower spans are
                // added after the run's own, so they win.
                if let Some(symbol_family) = text_symbol_family
                    && !text.is_ascii()
                {
                    let mut at = s;
                    let mut chars = text.chars().peekable();
                    while let Some(ch) = chars.next() {
                        let len = ch.len_utf8();
                        // The cell's zero-width marks follow its base char in
                        // this run, so U+FE0F is simply the next one. It is the
                        // user asking for emoji presentation explicitly, and
                        // overriding that would be the same class of bug in the
                        // other direction.
                        let qualified = chars.peek() == Some(&'\u{FE0F}');
                        if !qualified && single_column_text_presentation(ch) {
                            let text_attrs = run_attrs(cfg, &ff, *fg, *bold, *italic)
                                .family(Family::Name(symbol_family));
                            attrs_list.add_span(at..at + len, &text_attrs);
                        }
                        at += len;
                    }
                }
            }
            shaped.push_pad(&mut row_text, &mut attrs_list, cfg, &ff);
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
        if draw_cursor
            && matches!(shape, EShape::Block)
            && let Some(cursor) = self.pending_cursor_glyph.as_mut()
            && let Some(run) = buf.layout_runs().find(|run| run.line_i == cvrow as usize)
        {
            cursor.row_baseline = run.line_y - cvrow as f32 * ch;
        }
        // Return the scratch (with its grown String buffers) to the pool for the
        // next frame/pane.
        self.span_scratch = spans;
        self.span_breaks_scratch = span_line_breaks;
        text_changed
    }

    /// Walk each pane buffer routed through the glyph pipeline. Grid mode
    /// emits ONE pinned glyph instance per laid-out glyph, positioned at its
    /// grid cell (`pane_origin + col*cell_w`) instead of cosmic-text's
    /// continuous advance. Rasterization, the cache key, and the vertical /
    /// bearing math are byte-identical to glyphon (see `glyphpipe.rs`); only
    /// the X differs from the legacy path. A primary-face monospace glyph
    /// already has advance == cell_w, so its position is unchanged. The drift
    /// cases (fallback punctuation, Nerd icons, color emoji, CJK, ligatures,
    /// mismatched-width bold/italic) are what get pinned.
    fn emit_pane_glyphs(
        &mut self,
        panes: &[PaneView<'_>],
        cfg: &Config,
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
            let inactive = pane_inactive_amount(pv, panes.len(), cfg);
            if cfg.text_renderer == TextRendererMode::Legacy && inactive == 0.0 {
                continue;
            }
            let (rx, ry, rw, rh) = pv.terminal;
            let (ox, oy) = pane_grid_origin(pv.terminal, (pad_x, pad_y));
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
            // Each cell contributes a base scalar and any attached marks.
            // The shared emit keeps those marks on their base cell's column.
            emit_cell_locked_glyphs(
                out,
                buf,
                (ox, oy),
                (cfg.text_renderer == TextRendererMode::Grid).then_some(cw),
                default_color,
                glyph_pipeline,
                swash,
                font_system,
                device,
                queue,
                glyph_char_starts,
            );
            for glyph in &mut out[clip_start as usize..] {
                glyph.dim_color(cfg.theme.background, inactive);
            }
            clips.push(GlyphClip {
                rect: [rx, ry, rw, rh],
                start: clip_start,
                count: out.len() as u32 - clip_start,
            });
        }
    }
}

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

/// OpenType features to shape pane text with: the coarse ligature toggle
/// expressed as `liga/clig/calt/dlig = 0` when off, then the user's explicit
/// `font-feature` overrides applied on top (so they can re-enable or tune
/// individual features). Cited: Ghostty `font-feature`, kitty `font_features`.
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
/// mirror the SGR bold/italic bits. The per-line shaping cache builds one
/// row's `AttrsList` at a time, so unchanged rows never construct an `Attrs`.
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

/// The row's inked extent and the faces of its trailing U+0020 cells.
/// Interior spaces and spaces carrying combining marks stay in the extent.
/// One blank per cut face preserves the maximum ascent and descent that
/// cosmic-text uses for the baseline, including configured variant families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShapedRow {
    /// Runs shaped, from the start of the row.
    runs: usize,
    /// Bytes shaped of the last of those runs.
    last_len: usize,
    /// The faces of the cut blanks, one bit per `(bold, italic)` pair (see
    /// `ShapedRow::face_bit`). Each bit adds one pad blank after the runs.
    pad_faces: u8,
}

impl ShapedRow {
    /// The pad's colour. A blank draws no ink, so a fixed colour changes
    /// nothing on screen and keeps a recolour of the cut blanks out of the
    /// shaper's input.
    const PAD_FG: Rgb = Rgb::new(0, 0, 0);

    fn of(runs: &[(String, Rgb, bool, bool)]) -> Self {
        let mut pad_faces = 0;
        for (i, (text, _, bold, italic)) in runs.iter().enumerate().rev() {
            let inked = text.trim_end_matches(' ').len();
            if inked < text.len() {
                pad_faces |= Self::face_bit(*bold, *italic);
            }
            if inked > 0 {
                return Self {
                    runs: i + 1,
                    last_len: inked,
                    pad_faces,
                };
            }
        }
        Self {
            runs: 0,
            last_len: 0,
            pad_faces: 0,
        }
    }

    /// The bit of the `(bold, italic)` face in `pad_faces`.
    fn face_bit(bold: bool, italic: bool) -> u8 {
        1 << (u8::from(bold) | (u8::from(italic) << 1))
    }

    /// The shaped part of the run at `index`, one of the first `self.runs`.
    fn text(self, index: usize, text: &str) -> &str {
        if index + 1 == self.runs {
            &text[..self.last_len]
        } else {
            text
        }
    }

    /// The row key: the shaped part of each run with its colour and face,
    /// then the pad's faces. The cut blanks' colours do not reach it, so a
    /// row whose trailing blanks only change colour keeps its key and is not
    /// reshaped.
    fn key(self, runs: &[(String, Rgb, bool, bool)]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        for (i, (text, fg, bold, italic)) in runs[..self.runs].iter().enumerate() {
            self.text(i, text).hash(&mut h);
            (fg.r, fg.g, fg.b).hash(&mut h);
            (bold, italic).hash(&mut h);
        }
        self.pad_faces.hash(&mut h);
        h.finish()
    }

    /// Appends the pad after the shaped runs: one blank per face in
    /// `pad_faces`, each with that face's attrs from `run_attrs`, the source
    /// the runs and the style key use, in the fixed pad colour.
    fn push_pad(
        self,
        row_text: &mut String,
        attrs_list: &mut AttrsList,
        cfg: &Config,
        ff: &FontFeatures,
    ) {
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            if self.pad_faces & Self::face_bit(bold, italic) != 0 {
                let s = row_text.len();
                row_text.push(' ');
                let a = run_attrs(cfg, ff, Self::PAD_FG, bold, italic);
                attrs_list.add_span(s..row_text.len(), &a);
            }
        }
    }
}

#[cfg(test)]
mod shaped_row_tests;

/// Pick the per-pane titlebar background color from the focus / broadcast state.
///
/// Explicit `title-*-bg-color` config wins; otherwise every state derives from
/// the theme. Focused falls back to `focused_split_color`, then the resolved
/// accent, the same cascade as the pane border and screenshot accent. Broadcast
/// falls back to the accent. Inactive falls back to the theme's surface
/// `palette[8]`.
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

/// Resolve the opaque fill color for the window chrome strips (tab bar,
/// status bar, new-tab button). Without a wallpaper, or with
/// `chrome-background = theme`, this is the theme's chrome color (`palette[8]`).
/// With a wallpaper, the other modes let the chrome read deliberately against
/// the moving background:
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
    // `f32::clamp` passes NaN through, and cosmic-text would silently turn it
    // into zero-sized cells. Route NaN to the floor instead.
    if size.is_nan() {
        return 5.0;
    }
    size.clamp(5.0, 72.0)
}

/// Build glyphon [`Metrics`] for a *logical* `font_size` at a given
/// device-pixel `scale` (the window's `scale_factor`). glyphon shapes and
/// rasterizes in the same coordinate space as the wgpu surface, which winit
/// sizes in **physical** pixels, so a logical `font_size` must be multiplied
/// by the scale factor or text renders at `1/scale` of its intended size on
/// HiDPI displays. The line height is 1.25 times the size. `scale` is
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

/// A detected GPU adapter, described in kettle's own vocabulary so
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

/// Rank an adapter against a saved pin: IDs, exact name, then name substring.
/// `u8::MAX` means no match. Shared with the Settings adapter picker.
pub fn pin_match_rank_fields(vendor: u32, device: u32, name: &str, cfg: &Config) -> u8 {
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
    // choices, and the GPU device picker offers them.
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
/// projection — title, a category-tab strip (active category underlined), a
/// blank, one `"▸ label        value"` line per field (focused row marked),
/// a blank, then the keybind footer. Shared by the buffer-text pass and the
/// quad/area pass so they stay in lockstep (same row count + ordering).
fn settings_display_lines(set: &SettingsOverlay) -> Vec<String> {
    let mut lines = Vec::with_capacity(set.rows.len() + SETTINGS_FIELD_START + 2);
    lines.push(format!("⚙  {}", set.title));
    lines.push(set.categories.join("  "));
    lines.push(String::new());
    for (i, row) in set.rows.iter().enumerate() {
        let mark = if i == set.focused_row { "▸ " } else { "  " };
        lines.push(settings_row_line(set, row, mark, usize::MAX));
    }
    lines.push(String::new());
    lines.push(set.hints.clone());
    // Notes follow navigation in the buffer list. The vertical layout pins
    // them below the scrolling content, keeping pending changes visible.
    if let Some(note) = &set.footer_note {
        lines.extend(note.lines().map(str::to_string));
    }
    lines
}

/// Stable panel width from all-category column budgets and category names.
/// Contextual notes and capture prompts fit to it instead of moving the panel.
fn settings_panel_cols(set: &SettingsOverlay, lines: &[String]) -> f32 {
    let columns = 2 + set.label_cols + SETTINGS_COLUMN_GAP + set.value_cols;
    let tabs = lines.get(1).map_or(0, |line| display_width(line));
    columns.max(tabs).max(display_width(&set.hints)).max(44) as f32
}

fn settings_fit_line(line: &str, panel_w: f32, cell_w: f32) -> String {
    let text_w = (panel_w - 32.0).max(cell_w);
    let text_cols = (text_w / cell_w).floor().max(1.0) as usize;
    fit_single_line_label(line, text_cols)
}

const SETTINGS_COLUMN_GAP: usize = 2;
const SETTINGS_TAB_GAP: usize = 2;

fn settings_row_line(
    set: &SettingsOverlay,
    row: &SettingsRow,
    mark: &str,
    text_cols: usize,
) -> String {
    let fixed_cols = display_width(mark) + SETTINGS_COLUMN_GAP;
    if text_cols <= fixed_cols + 1 {
        return fit_single_line_label(&format!("{mark}{}", row.value), text_cols);
    }
    let available = text_cols - fixed_cols;
    // Keep at least half the remaining cells for a long value. Labels shrink
    // first when the longest value fits, then both columns ellipsize.
    let reserved_value = set.value_cols.max(1).min(available / 2);
    let label_cols = set.label_cols.min(available - reserved_value);
    let value_cols = available - label_cols;
    let label = fit_single_line_label(&row.label, label_cols);
    let value = settings_fit_value(&row.value, value_cols);
    format!(
        "{mark}{label}{}{value}",
        " ".repeat(label_cols - display_width(&label) + SETTINGS_COLUMN_GAP),
    )
}

fn settings_fit_value(value: &str, cols: usize) -> String {
    if value.ends_with('▏') && display_width(value) > cols {
        if cols <= 1 {
            return take_cols_back(value, cols);
        }
        // An inline editor must retain the current input end and its caret.
        format!("…{}", take_cols_back(value, cols - 1))
    } else {
        fit_single_line_label(value, cols)
    }
}

fn settings_fit_display_line(
    set: &SettingsOverlay,
    line_index: usize,
    line: &str,
    panel_w: f32,
    cell_w: f32,
) -> String {
    if let Some(row) = line_index
        .checked_sub(SETTINGS_FIELD_START)
        .and_then(|i| set.rows.get(i))
    {
        let text_cols = ((panel_w - 32.0).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let mark = if line_index == SETTINGS_FIELD_START + set.focused_row {
            "▸ "
        } else {
            "  "
        };
        settings_row_line(set, row, mark, text_cols)
    } else {
        settings_fit_line(line, panel_w, cell_w)
    }
}

fn settings_tab_span(set: &SettingsOverlay, index: usize) -> (usize, usize) {
    let start = set
        .categories
        .iter()
        .take(index)
        .map(|c| display_width(c) + SETTINGS_TAB_GAP)
        .sum();
    let width = set.categories.get(index).map_or(0, |c| display_width(c));
    (start, width)
}

#[derive(Debug, Clone, Copy)]
struct SettingsPanelLayout {
    panel_w: f32,
    panel_h: f32,
    px: f32,
    py: f32,
    row_h: f32,
    first_line: usize,
    visible_lines: usize,
    note_first: usize,
    note_lines: usize,
}

/// Live settings geometry shared with `ui_geometry` diagnostics. The renderer
/// remains the source of truth for scrolling, so automation addresses the row
/// that is actually painted instead of reconstructing the panel math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingsPanelGeometry {
    pub rect: Rect4,
    pub focused_rect: Rect4,
    pub first_line: usize,
    /// Visible scrolling content lines, excluding the pinned notes.
    pub visible_lines: usize,
}

/// The text-area pass and hit testing share this mapping from painted slots
/// to buffer indices. Notes occupy the last slots, outside field scrolling.
fn settings_painted_lines(layout: &SettingsPanelLayout) -> impl Iterator<Item = (usize, usize)> {
    (layout.first_line..layout.first_line + layout.visible_lines)
        .chain(layout.note_first..layout.note_first + layout.note_lines)
        .enumerate()
}

fn settings_panel_layout(
    set: &SettingsOverlay,
    lines: &[String],
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
) -> SettingsPanelLayout {
    let row_h = cell_h + 6.0;
    let panel_w =
        (settings_panel_cols(set, lines) * cell_w + 48.0).min((surface_w - 40.0).max(120.0));
    let panel_h = (lines.len() as f32 * row_h + 24.0).min((surface_h - 40.0).max(80.0));
    let px = ((surface_w - panel_w) * 0.5).max(0.0);
    let py = ((surface_h - panel_h) * 0.5).max(0.0);
    let capacity =
        (((panel_h - 24.0).max(row_h) / row_h).floor() as usize).clamp(1, lines.len().max(1));
    let content_end = (SETTINGS_FIELD_START + set.rows.len() + 2).min(lines.len());
    // Keep one slot for the focused field. At extreme heights, retain the
    // last notes first because the pending notice is appended last.
    let note_lines = (lines.len() - content_end).min(capacity.saturating_sub(1));
    let note_first = lines.len() - note_lines;
    let visible_lines = (capacity - note_lines).min(content_end);
    let focused_line = (SETTINGS_FIELD_START + set.focused_row).min(content_end.saturating_sub(1));
    let max_first = content_end.saturating_sub(visible_lines);
    let first_line = focused_line
        .saturating_add(1)
        .saturating_sub(visible_lines)
        .min(max_first);
    SettingsPanelLayout {
        panel_w,
        panel_h,
        px,
        py,
        row_h,
        first_line,
        visible_lines,
        note_first,
        note_lines,
    }
}

pub fn settings_panel_geometry(
    set: &SettingsOverlay,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
) -> SettingsPanelGeometry {
    let lines = settings_display_lines(set);
    let layout = settings_panel_layout(set, &lines, cell_w, cell_h, surface_w, surface_h);
    let focused_line = (SETTINGS_FIELD_START + set.focused_row).min(lines.len().saturating_sub(1));
    let focused_y =
        layout.py + 12.0 + focused_line.saturating_sub(layout.first_line) as f32 * layout.row_h;
    SettingsPanelGeometry {
        rect: (layout.px, layout.py, layout.panel_w, layout.panel_h),
        focused_rect: (
            layout.px + 6.0,
            focused_y,
            layout.panel_w - 12.0,
            layout.row_h,
        ),
        first_line: layout.first_line,
        visible_lines: layout.visible_lines,
    }
}

/// What a click on the settings overlay landed on. The hit-test recomputes the
/// geometry from the SAME inputs the draw uses (`settings_display_lines` +
/// `settings_panel_layout`), so it can't drift from what's painted.
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
    let layout = settings_panel_layout(set, &lines, cw, ch, sw, sh);
    let (row_h, panel_w, panel_h, px, py) = (
        layout.row_h,
        layout.panel_w,
        layout.panel_h,
        layout.px,
        layout.py,
    );
    if cursor_x < px || cursor_x >= px + panel_w || cursor_y < py || cursor_y >= py + panel_h {
        return SettingsHit::Outside;
    }
    // Rows are laid out from `py + 12.0`, each `row_h` tall (mirrors the draw).
    let rel = cursor_y - (py + 12.0);
    if rel < 0.0 {
        return SettingsHit::Inert;
    }
    let slot = (rel / row_h) as usize;
    let Some((_, line)) = settings_painted_lines(&layout).nth(slot) else {
        return SettingsHit::Inert;
    };
    // Line 1 is the category-tab strip.
    if line == 1 {
        let text_left = px + 16.0;
        let strip_cols = ((panel_w - 32.0).max(cw) / cw).floor().max(1.0) as usize;
        let visible_cols = if display_width(&lines[1]) > strip_cols {
            strip_cols.saturating_sub(1) // the ellipsis is inert
        } else {
            strip_cols
        };
        for i in 0..set.categories.len() {
            let (col, width) = settings_tab_span(set, i);
            let start = text_left + col as f32 * cw;
            let end = text_left + (col + width).min(visible_cols) as f32 * cw;
            if cursor_x >= start && cursor_x < end {
                return SettingsHit::Category(i);
            }
        }
        return SettingsHit::Inert;
    }
    // Field rows: SETTINGS_FIELD_START .. SETTINGS_FIELD_START + rows.len().
    if line >= SETTINGS_FIELD_START && line < SETTINGS_FIELD_START + set.rows.len() {
        return SettingsHit::Field(line - SETTINGS_FIELD_START);
    }
    SettingsHit::Inert
}

/// Column widths of the search bar's fixed controls, from their labels in a
/// language. Each control keeps the fill it has around its longest English
/// label and is never narrower than in English, so English lays out exactly as
/// it always has and a longer translation gets room instead of an ellipsis.
/// Status is text, not a button, and has no fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SearchBarWidths {
    label: usize,
    previous: usize,
    next: usize,
    wrap: usize,
    case: usize,
    invert: usize,
    status: usize,
    close: usize,
}

impl SearchBarWidths {
    /// The English widths: each control's longest label plus its fill.
    const ENGLISH: Self = Self {
        label: 7,
        previous: 8,
        next: 8,
        wrap: 10,
        case: 15,
        invert: 12,
        status: 19,
        close: 8,
    };

    fn new(tr: &Translator) -> Self {
        let en = Translator::new(Language::En);
        let longest = |tr: &Translator| SearchBarLabels::new(tr).widest();
        let (english, here) = (longest(&en), longest(tr));
        let fit = |width: usize, english: usize, here: usize| {
            width.max(here + width.saturating_sub(english))
        };
        let w = Self::ENGLISH;
        Self {
            label: fit(w.label, english.label, here.label),
            previous: fit(w.previous, english.previous, here.previous),
            next: fit(w.next, english.next, here.next),
            wrap: fit(w.wrap, english.wrap, here.wrap),
            case: fit(w.case, english.case, here.case),
            invert: fit(w.invert, english.invert, here.invert),
            status: fit(w.status, english.status, here.status),
            close: fit(w.close, english.close, here.close),
        }
    }
}

/// The search bar's words in one language.
struct SearchBarLabels {
    label: &'static str,
    previous: &'static str,
    next: &'static str,
    wrap: [&'static str; 2],
    case: Vec<String>,
    invert: [&'static str; 2],
    close: &'static str,
    tr: Translator,
}

impl SearchBarLabels {
    fn new(tr: &Translator) -> Self {
        Self {
            label: tr.text(Text::SearchLabel),
            previous: tr.text(Text::SearchPrev),
            next: tr.text(Text::SearchNext),
            wrap: [tr.text(Text::SearchWrapOn), tr.text(Text::SearchWrapOff)],
            case: SearchCaseMode::ALL
                .iter()
                .map(|mode| search_case_text(tr, *mode))
                .collect(),
            invert: [
                tr.text(Text::SearchEnterNext),
                tr.text(Text::SearchEnterPrev),
            ],
            close: tr.text(Text::SearchClose),
            tr: *tr,
        }
    }

    /// Each control's widest label, in display columns.
    fn widest(&self) -> SearchBarWidths {
        let widest = |labels: &[&str]| labels.iter().map(|l| display_width(l)).max().unwrap_or(0);
        SearchBarWidths {
            label: display_width(self.label),
            previous: display_width(self.previous),
            next: display_width(self.next),
            wrap: widest(&self.wrap),
            case: self
                .case
                .iter()
                .map(|l| display_width(l))
                .max()
                .unwrap_or(0),
            invert: widest(&self.invert),
            status: SearchStatus::ALL
                .iter()
                .map(|status| display_width(status.label(&self.tr)))
                .max()
                .unwrap_or(0),
            close: display_width(self.close),
        }
    }
}

/// The Case button's text. The mode name is padded to the longest mode in the
/// same language, so the chevron stays put as the mode cycles.
fn search_case_text(tr: &Translator, mode: SearchCaseMode) -> String {
    let width = SearchCaseMode::ALL
        .iter()
        .map(|mode| mode.label(tr).chars().count())
        .max()
        .unwrap_or(0);
    tr.search_case(&format!("{:<width$}", mode.label(tr)))
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
    tr: &Translator,
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

    // The widths read as the layout's column constants they used to be.
    #[allow(non_snake_case)]
    let SearchBarWidths {
        label: LABEL,
        previous: PREVIOUS,
        next: NEXT,
        wrap: WRAP,
        case: CASE,
        invert: INVERT,
        status: STATUS,
        close: CLOSE,
    } = SearchBarWidths::new(tr);
    const EDITOR_MIN: usize = 12;
    // 8 single-column gaps between the nine controls, plus one extra column
    // for each of the four gaps that separate groups rather than siblings
    // (after the editor, after Next, after Invert, after Status).
    let wide_min =
        2 + LABEL + PREVIOUS + NEXT + WRAP + CASE + INVERT + STATUS + CLOSE + EDITOR_MIN + 8 + 4;

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
    ) = if columns >= wide_min {
        let mut col = pad;
        // `gap` is the run of blank columns AFTER a control. One column
        // separates controls inside a group; two separate the groups
        // themselves, so the row reads as query / navigation / options /
        // outcome / close instead of one undifferentiated strip. Without the
        // wider gap the option group runs straight into the status text and
        // `Enter: Prev Match` reads as a single phrase.
        let mut place = |width: usize, gap: usize| {
            let here = col;
            col += width + gap;
            (here, 0, width)
        };
        const IN_GROUP: usize = 1;
        const BETWEEN_GROUPS: usize = 2;
        let label_pos = place(LABEL, IN_GROUP);
        // All fixed controls plus their inter-control gaps have already been
        // budgeted by WIDE_MIN; the editor gets every surplus column.
        let editor_width = EDITOR_MIN + (columns - wide_min);
        let editor_pos = place(editor_width, BETWEEN_GROUPS);
        let previous_pos = place(PREVIOUS, IN_GROUP);
        let next_pos = place(NEXT, BETWEEN_GROUPS);
        let wrap_pos = place(WRAP, IN_GROUP);
        let case_pos = place(CASE, IN_GROUP);
        let invert_pos = place(INVERT, BETWEEN_GROUPS);
        let status_pos = place(STATUS, BETWEEN_GROUPS);
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
        // Same grouping the wide branch uses: one column between siblings, two
        // where one group ends and the next begins. Without it the narrow rows
        // render `Enter: Next No match`, because `Enter: Next` fills its slot
        // exactly and a single space does not read as a boundary between a
        // control and the outcome text.
        let desired = [
            (PREVIOUS, 1usize),
            (NEXT, 2),
            (WRAP, 1),
            (CASE, 1),
            (INVERT, 2),
            (STATUS, 1),
        ];
        let usable = inner.max(1);
        let mut packed = [(0usize, 0usize, 1usize); 6];
        let mut row = if close_on_own_row { 2 } else { 1 };
        let mut col = pad;
        for (idx, (wanted, gap)) in desired.into_iter().enumerate() {
            let width = wanted.min(usable).max(1);
            if col > pad && col + width > right {
                row += 1;
                col = pad;
            }
            packed[idx] = (col, row, width);
            // A group boundary never justifies pushing a control off the row.
            // `right` is an exclusive end, so a control fits while
            // `col + width <= right`; the next one starts at `col + width +
            // gap` and is already off the row when that equals `right`. Compare
            // with `>=`, not `>`, or the wider gap wraps a control that the
            // single-column gap would have kept.
            let gap = if col + width + gap >= right { 1 } else { gap };
            col = col.saturating_add(width + gap);
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

/// Recover a control's width in cells from the pixel rect the geometry built.
///
/// `search_bar_geometry` lays the bar out in whole columns and then multiplies
/// by `cell_width` to store pixels, so this has to divide back. A plain
/// `floor` loses a column whenever `n * cell_width / cell_width` lands a few
/// ULP below `n` in f32, which for a cell width like `7.007` turns an 11-column
/// slot into 10 and ellipsizes the label inside it. Sweeping cell widths
/// 6.000–16.000 in 0.001 steps, that bit the widths used by Close (910 of
/// 10001), Case (910) and Invert (415).
///
/// The epsilon is far larger than the f32 error (~1e-7 relative) and far
/// smaller than the fraction of a cell that a genuinely clamped control loses
/// at the surface edge, so a control cut off by the window still floors down.
pub fn search_bar_columns(rect_width: f32, cell_width: f32) -> usize {
    if cell_width <= 0.0 {
        return 0;
    }
    (rect_width / cell_width + 1e-3).floor().max(0.0) as usize
}

/// Number of text segments in the search lane: label, editor, Previous, Next,
/// Wrap, Case, Invert, status and Close.
const SEARCH_SEGMENTS: usize = 9;

/// One piece of search-lane text, placed in pixels and clipped to its slot.
#[derive(Debug, Clone, PartialEq)]
struct SearchBarSegment {
    /// The control this text labels, if it is one.
    control: Option<SearchControl>,
    /// The slot the text is clipped to.
    rect: Rect4,
    /// Left edge of the text.
    left: f32,
    text: String,
    /// The editor's whole query reads right to left (`starts_right_to_left`),
    /// so its visible slice is shaped as a right-to-left paragraph.
    rtl: bool,
}

/// Bytes of the direction mark that starts the editor's shaped text.
const EDITOR_MARK_LEN: usize = 3;

impl SearchBarSegment {
    /// The text shaped for this segment. The editor's slice starts with a
    /// zero-width direction mark (U+200E or U+200F) taken from the whole
    /// query, so the slice keeps the query's reading order however far it is
    /// scrolled; cosmic-text would otherwise take the direction from the
    /// slice's own first letter.
    fn shaped_text(&self) -> std::borrow::Cow<'_, str> {
        if self.control == Some(SearchControl::Editor) {
            let mark = if self.rtl { '\u{200F}' } else { '\u{200E}' };
            std::borrow::Cow::Owned(format!("{mark}{}", self.text))
        } else {
            std::borrow::Cow::Borrowed(&self.text)
        }
    }
}

/// Whether `text` reads right to left: its first strong character is
/// right-to-left (Unicode bidi rule P2). Digits, marks and punctuation do not
/// decide it, and text with no strong character reads left to right.
fn starts_right_to_left(text: &str) -> bool {
    unicode_bidi::get_base_direction(text) == unicode_bidi::Direction::Rtl
}

/// Opacity of a focused search button's accent fill.
const SEARCH_FOCUSED_FILL_ALPHA: f32 = 0.92;

/// The label color for the focused search button.
///
/// The button sits on the accent, which the theme foreground was never chosen
/// to contrast with. The theme's cursor text is held to AA against the fill
/// as painted: the accent over the bar, then any hover or press `tint`,
/// composited the way the surface blends (`linear` for an sRGB surface).
#[cfg(test)]
fn search_focused_label(
    cursor_text: Rgb,
    bar: Rgb,
    accent: Rgb,
    tint: Option<(Rgb, f32)>,
    linear: bool,
) -> Rgb {
    let fill = color::over(bar, accent, f64::from(SEARCH_FOCUSED_FILL_ALPHA), linear);
    let fill = tint.map_or(fill, |(tint, alpha)| {
        color::over(fill, tint, f64::from(alpha), linear)
    });
    color::with_min_contrast(cursor_text, fill, 4.5)
}

/// One cluster of the painted search query: the bytes it covers and the x
/// span its glyphs cover.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchEditorCluster {
    pub start: usize,
    pub end: usize,
    pub left: f32,
    pub right: f32,
    /// Right-to-left text starts at `right` and ends at `left`.
    pub rtl: bool,
}

/// Lay out one piece of search-lane text in its own buffer.
///
/// The line is as tall as `metrics` says, and cosmic-text centers glyphs in
/// their line, so text placed at the row's top is centered in the row. Text is
/// set flush left: a query that starts with Hebrew or Arabic is a
/// right-to-left paragraph, which would otherwise be pushed against the far
/// edge of the slot and clipped.
fn shape_search_segment(
    font_system: &mut FontSystem,
    buffer: &mut TextBuffer,
    segment: &SearchBarSegment,
    metrics: Metrics,
    family: Family<'_>,
    text_changed: bool,
) {
    buffer.set_metrics(metrics);
    // No width: a right-to-left line is laid out against the width it is
    // given, so one wider than its slot would start past the slot's far edge.
    // The text area's bounds clip it instead.
    buffer.set_size(None, Some(metrics.line_height));
    if text_changed {
        buffer.set_text(
            &segment.shaped_text(),
            &Attrs::new().family(family),
            Shaping::Advanced,
            Some(Align::Left),
        );
    }
    buffer.shape_until_scroll(font_system, false);
}

/// The clusters of a shaped segment in the segment's own text bytes: the
/// editor's direction mark is dropped.
fn segment_clusters(buffer: &TextBuffer, segment: &SearchBarSegment) -> Vec<SearchEditorCluster> {
    let clusters = glyph_clusters(buffer);
    if segment.control != Some(SearchControl::Editor) {
        return clusters;
    }
    let clusters = clusters
        .into_iter()
        .filter(|cluster| cluster.end > EDITOR_MARK_LEN)
        .map(|cluster| SearchEditorCluster {
            start: cluster.start.max(EDITOR_MARK_LEN) - EDITOR_MARK_LEN,
            end: cluster.end - EDITOR_MARK_LEN,
            ..cluster
        })
        .collect();
    grapheme_clusters(&segment.text, clusters)
}

fn grapheme_clusters(text: &str, clusters: Vec<SearchEditorCluster>) -> Vec<SearchEditorCluster> {
    use unicode_segmentation::UnicodeSegmentation as _;
    let mut result = Vec::new();
    for cluster in clusters {
        let Some(text) = text.get(cluster.start..cluster.end) else {
            result.push(cluster);
            continue;
        };
        let count = text.graphemes(true).count();
        if count <= 1 {
            result.push(cluster);
            continue;
        }
        let count = count as f32;
        for (index, (byte, grapheme)) in text.grapheme_indices(true).enumerate() {
            let width = (cluster.right - cluster.left) / count;
            let left = if cluster.rtl {
                cluster.right - (index + 1) as f32 * width
            } else {
                cluster.left + index as f32 * width
            };
            result.push(SearchEditorCluster {
                start: cluster.start + byte,
                end: cluster.start + byte + grapheme.len(),
                left,
                right: left + width,
                rtl: cluster.rtl,
            });
        }
    }
    result
}

/// The clusters in the first line of `buffer`, sorted by byte. The glyphs of
/// one cluster (a split vowel, an emoji sequence the font does not ligate)
/// share its bytes and sit next to each other, so the cluster spans them all.
fn glyph_clusters(buffer: &TextBuffer) -> Vec<SearchEditorCluster> {
    let glyphs = buffer.layout_runs().next().into_iter().flat_map(|run| {
        run.glyphs.iter().map(|glyph| SearchEditorCluster {
            start: glyph.start,
            end: glyph.end,
            left: glyph.x,
            right: glyph.x + glyph.w,
            rtl: glyph.level.is_rtl(),
        })
    });
    merge_clusters(glyphs)
}

/// Merge neighbouring glyphs that share their bytes into one cluster, and
/// sort the clusters by byte.
fn merge_clusters(glyphs: impl Iterator<Item = SearchEditorCluster>) -> Vec<SearchEditorCluster> {
    let mut clusters: Vec<SearchEditorCluster> = Vec::new();
    for glyph in glyphs {
        match clusters.last_mut() {
            Some(last) if last.start == glyph.start && last.end == glyph.end => {
                last.left = last.left.min(glyph.left);
                last.right = last.right.max(glyph.right);
            }
            _ => clusters.push(glyph),
        }
    }
    clusters.sort_by_key(|cluster| cluster.start);
    clusters
}

/// Every caret position among `clusters`, as (byte, x), sorted by byte.
///
/// A byte sits at the leading edge of the cluster it starts, or, at the end
/// of the text, at the trailing edge of the cluster it ends. Right-to-left
/// clusters lead from their right edge.
fn cluster_stops(clusters: &[SearchEditorCluster]) -> Vec<(usize, f32)> {
    let leading = clusters.iter().map(|cluster| {
        let x = if cluster.rtl {
            cluster.right
        } else {
            cluster.left
        };
        (cluster.start, x)
    });
    let trailing = clusters.iter().map(|cluster| {
        let x = if cluster.rtl {
            cluster.left
        } else {
            cluster.right
        };
        (cluster.end, x)
    });
    let mut stops: Vec<(usize, f32)> = leading.chain(trailing).collect();
    // The sort is stable, so a leading edge stays ahead of a trailing edge at
    // the same byte and survives the dedup.
    stops.sort_by_key(|stop| stop.0);
    stops.dedup_by_key(|stop| stop.0);
    if stops.is_empty() {
        stops.push((0, 0.0));
    }
    stops
}

/// The x of `byte` among `stops`: exact when it is a caret position, else the
/// position before it (a byte inside a cluster), else the end of the text.
fn stop_x(stops: &[(usize, f32)], byte: usize) -> f32 {
    match stops.binary_search_by_key(&byte, |stop| stop.0) {
        Ok(index) => stops[index].1,
        Err(0) => stops.first().map_or(0.0, |stop| stop.1),
        Err(index) => stops[index - 1].1,
    }
}

/// The x ranges covered by the clusters that overlap bytes `start..end`,
/// merged where they touch. Text that mixes directions can need several.
fn cluster_spans(clusters: &[SearchEditorCluster], start: usize, end: usize) -> Vec<(f32, f32)> {
    if start >= end {
        return Vec::new();
    }
    let mut spans: Vec<(f32, f32)> = clusters
        .iter()
        .filter(|cluster| cluster.start < end && cluster.end > start)
        .map(|cluster| (cluster.left, cluster.right))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f32, f32)> = Vec::with_capacity(spans.len());
    for (left, right) in spans {
        match merged.last_mut() {
            Some(last) if left <= last.1 + 0.5 => last.1 = last.1.max(right),
            _ => merged.push((left, right)),
        }
    }
    merged
}

/// Lay out every piece of search-lane text in its own slot.
///
/// Each control is placed from its own rectangle instead of padding one line
/// per row with spaces, so a glyph whose advance is not a whole cell (a CJK
/// fallback in the query, for one) cannot push the controls after it. Button
/// labels are centered as a block as wide as the control's widest label, so a
/// toggle such as `Wrap: On` / `Wrap: Off` changes its value without moving
/// its name.
fn search_bar_segments(
    search: &SearchOverlay,
    geometry: SearchBarGeometry,
    cell_width: f32,
) -> Vec<SearchBarSegment> {
    let mut segments = Vec::with_capacity(SEARCH_SEGMENTS);
    let rtl = starts_right_to_left(&search.query);
    let mut push = |control: Option<SearchControl>, rect: Rect4, text: String, left: f32| {
        segments.push(SearchBarSegment {
            control,
            rect,
            left,
            text,
            rtl: rtl && control == Some(SearchControl::Editor),
        });
    };
    let cols = |rect: Rect4| search_bar_columns(rect.2, cell_width);
    // Centre `text` in `rect` as part of a block `block` columns wide.
    let centered = |rect: Rect4, block: usize| -> f32 {
        let block = block.min(cols(rect)) as f32 * cell_width;
        rect.0 + ((rect.2 - block) * 0.5).max(0.0).round()
    };
    let button = |rect: Rect4, text: String, variants: &[&str]| -> (String, f32) {
        let block = variants
            .iter()
            .map(|variant| display_width(variant))
            .max()
            .unwrap_or(0);
        let fitted = fit_single_line_label(&text, cols(rect).max(1));
        (fitted, centered(rect, block))
    };

    let labels = SearchBarLabels::new(&search.tr);
    let label_cols = cols(geometry.label);
    push(
        None,
        geometry.label,
        if label_cols >= display_width(labels.label) {
            labels.label
        } else {
            "?"
        }
        .to_string(),
        geometry.label.0,
    );
    let editor_cols = search_bar_columns(geometry.editor.2, cell_width).max(1);
    push(
        Some(SearchControl::Editor),
        geometry.editor,
        search_editor_body(search, editor_cols).0,
        geometry.editor.0 + cell_width,
    );
    let (text, left) = button(
        geometry.previous,
        labels.previous.to_string(),
        &[labels.previous],
    );
    push(Some(SearchControl::Previous), geometry.previous, text, left);
    let (text, left) = button(geometry.next, labels.next.to_string(), &[labels.next]);
    push(Some(SearchControl::Next), geometry.next, text, left);
    // Say the state, not a checkbox. `[x] Wrap` makes someone decode a TUI
    // idiom to learn whether wrapping is on; `Wrap: On` just tells them. The
    // trailing chevron on Case is the affordance for "clicking cycles this",
    // which nothing else in the bar communicated.
    let (text, left) = button(
        geometry.wrap,
        labels.wrap[usize::from(!search.wrap)].to_string(),
        &labels.wrap,
    );
    push(Some(SearchControl::Wrap), geometry.wrap, text, left);
    // The mode name is padded to the longest one, so the chevron stays put as
    // the mode cycles.
    let case_variants: Vec<&str> = labels.case.iter().map(String::as_str).collect();
    let (text, left) = button(
        geometry.case_mode,
        search_case_text(&search.tr, search.case_mode),
        &case_variants,
    );
    push(Some(SearchControl::Case), geometry.case_mode, text, left);
    // The invert toggle flips which direction Enter searches, so its label
    // names that direction. It also reminds users of the Enter keybinding.
    let (text, left) = button(
        geometry.invert,
        labels.invert[usize::from(search.invert)].to_string(),
        &labels.invert,
    );
    push(Some(SearchControl::Invert), geometry.invert, text, left);
    // `Match` is the one status the user can already see: the hit is
    // highlighted in the pane and the viewport has jumped to it. Printing it
    // too spends the widest slot in the bar restating the obvious, and it
    // makes the states that DO need attention — `No match`, `Invalid pattern`,
    // `Results limited` — look like just another word in the strip rather than
    // an answer. The slot keeps its width either way, so nothing shifts when
    // the status appears and disappears.
    let status = if search.status == SearchStatus::Match {
        String::new()
    } else {
        fit_single_line_label(
            search.status.label(&search.tr),
            cols(geometry.status).max(1),
        )
    };
    push(None, geometry.status, status, geometry.status.0);
    let (text, left) = button(geometry.close, labels.close.to_string(), &[labels.close]);
    push(Some(SearchControl::Close), geometry.close, text, left);
    segments
}

/// The search lane as text, one line per row, each segment at its column.
/// Tests read layout invariants from it; paint uses the segments directly.
#[cfg(test)]
fn search_bar_text(search: &SearchOverlay, geometry: SearchBarGeometry, cell_width: f32) -> String {
    let row_height = geometry.reserved_height / geometry.rows.max(1) as f32;
    let mut rows = vec![Vec::<(usize, usize, String)>::new(); geometry.rows];
    for segment in search_bar_segments(search, geometry, cell_width) {
        let rect = segment.rect;
        if rect.2 <= 0.0 || rect.3 <= 0.0 {
            continue;
        }
        let row = if row_height > 0.0 {
            ((rect.1 - geometry.rect.1) / row_height).round().max(0.0) as usize
        } else {
            0
        }
        .min(rows.len().saturating_sub(1));
        let col = (segment.left / cell_width).round().max(0.0) as usize;
        let width = search_bar_columns(rect.2, cell_width).max(1);
        rows[row].push((col, width, segment.text));
    }
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

/// The editor's visible query text, without padding or caret, and the query
/// byte it starts at.
///
/// The caret is painted as a bar between glyphs, so moving it or focusing
/// another control never shifts the characters. Text past the right edge is
/// clipped, as in any text field: an ellipsis there would cover the character
/// next to the caret.
fn search_editor_body(search: &SearchOverlay, max_cols: usize) -> (String, usize) {
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    let inner = max_cols.saturating_sub(2);
    if inner == 0 {
        return (String::new(), 0);
    }
    // Do not let a pasted line break create extra chrome rows. The UI also
    // normalizes paste, but paint is a trust boundary for callers of this crate.
    // Each control character becomes as many spaces as it has bytes, so body
    // bytes still map one to one onto query bytes.
    let mut query = String::with_capacity(search.query.len().min(4096));
    for ch in search.query.chars() {
        if query.len() + ch.len_utf8() > 4096 {
            break;
        }
        if ch.is_control() {
            query.extend(std::iter::repeat_n(' ', ch.len_utf8()));
        } else {
            query.push(ch);
        }
    }
    let visible = drop_cols_front(&query, search.horizontal_scroll);
    let skip = query.len() - visible.len();
    let mut body = String::with_capacity(visible.len());
    let mut cols = 0usize;
    for grapheme in visible.graphemes(true) {
        let width = grapheme.width();
        if cols + width > inner {
            break;
        }
        cols += width;
        body.push_str(grapheme);
    }
    (body, skip)
}

/// The editor text with its one-cell padding on each side, as tests see it.
#[cfg(test)]
fn search_editor_label(search: &SearchOverlay, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let label = format!(" {} ", search_editor_body(search, max_cols).0);
    fit_single_line_label(&label, max_cols)
}

fn drop_cols_front(s: &str, cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    let mut skipped = 0usize;
    let mut byte = s.len();
    // Stop at the first grapheme that is not wholly inside the dropped
    // columns, so a zero-width one at the boundary (a leading combining mark,
    // a zero-width space) stays.
    for (idx, grapheme) in s.grapheme_indices(true) {
        let width = grapheme.width();
        if skipped >= cols || skipped + width > cols {
            byte = idx;
            break;
        }
        skipped += width;
    }
    s[byte..].to_string()
}

/// Text on the opaque confirmation panel keeps WCAG AA contrast in custom themes.
pub fn confirm_dialog_text_color(theme: &kettle_config::Theme) -> Rgb {
    color::with_min_contrast(theme.foreground, theme.background, 4.5)
}

fn confirm_dialog_border_color(theme: &kettle_config::Theme) -> Rgb {
    color::with_min_contrast(
        color::dim(theme.foreground, theme.background),
        theme.background,
        1.5,
    )
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
/// and is more useful than the home or drive prefix. Non-path titles use a
/// middle ellipsis.
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

/// Fit a directory-derived tab label into `n` columns by progressively
/// shedding detail: tier 1 the full (home-abbreviated) path; tier 2 the leaf
/// directory name alone; tier 3 the tail of the leaf with a leading `…` once
/// even the name doesn't fit. The rightmost part (the project / current
/// directory) is the most identifying, so it is kept to the end. This mirrors
/// `fit_pane_titlebar_title`'s progressive-shed shape.
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
/// the least-useful parts first: full → drop the `WxH` size text → drop the
/// `[group]` tag → shorten the title, keeping the program/leaf name (a middle
/// ellipsis, or `fit_tab_path`'s tiers when `title_path` is set). The bell
/// indicator (if any) is kept throughout because it is small and important.
/// The full label reads `"  [g]  title  WxH  🔔"`.
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

/// Wash alpha for a visual-bell flash `ramp` (1.0 at the peak, 0.0 when it
/// has decayed) on this theme: the configured lightness step scaled by the
/// ramp, converted for linear-light compositing of the foreground over the
/// background. Shared by the live frame and the `DebugScene::BellFlash`
/// fixture so the offscreen pixel check measures the shipped math.
pub fn bell_flash_alpha(theme: &kettle_config::Theme, cfg: &Config, ramp: f32) -> f32 {
    let step = cfg.bell_flash_intensity.clamp(0.0, 1.0) * ramp.clamp(0.0, 1.0);
    color::perceptual_wash_alpha(theme.background, theme.foreground, step)
}

/// Shrink each of `quads` to its part inside `clip`, to nothing when it lies
/// wholly outside. In place, so ranges into the list stay valid.
fn clip_quads(quads: &mut [QuadInstance], clip: (f32, f32, f32, f32)) {
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
/// The tab bar's overflow arrows, back and forward: chevrons across a top or
/// bottom bar, and their upright forms down a side one.
fn scroll_arrow_glyphs(vertical: bool) -> (&'static str, &'static str) {
    if vertical {
        ("⌃", "⌄")
    } else {
        (" ‹", " ›")
    }
}

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
mod pane_window_corner_tests;

fn push_cell_decorations(
    quads: &mut Vec<QuadInstance>,
    cell: Rect4,
    col: usize,
    flags: Flags,
    fg: Rgb,
    line_color: Rgb,
) {
    if flags.contains(Flags::HIDDEN) {
        return;
    }
    let (x, y, cw, ch) = cell;
    if flags.intersects(Flags::ALL_UNDERLINES) {
        push_underline_quads(quads, x, y, cw, ch, col, flags, line_color);
    }
    if flags.contains(Flags::STRIKEOUT) {
        quads.push(rect(x, y + ch * 0.5, cw, 1.0, fg, 1.0));
    }
}

/// Draws one cell's underline segment(s), giving each `Flags::ALL_UNDERLINES`
/// style its own shape instead of one straight 1px line. This renderer's only
/// chrome/cell primitive is the axis-aligned quad (`QuadInstance`/`rect`), so
/// every style here is built from small rects rather than a real path/curve:
///
/// - UNDERCURL is a stepped zigzag (a triangle wave built from 1px-tall quads)
///   spanning a 2-cell period. `col` (the run's ABSOLUTE grid column, not
///   cell-local) phases each cell's 4 segments into its half of that period,
///   so a run of undercurl cells shows one continuous wave. A smooth sine
///   would need a dedicated shader/SDF path in `quad.rs`, but the zigzag is
///   still visibly distinct from a straight line. Undercurl must remain visible
///   for Neovim/AstroNvim diagnostic underlines; see docs/TESTING.md.
/// - DOTTED_UNDERLINE / DASHED_UNDERLINE tile short marks with gaps across
///   the cell (sparser + shorter marks for dotted, longer + denser for
///   dashed). They phase on the absolute `x` pixel position rather than `col`,
///   because a mark width in px doesn't generally divide `cw` evenly.
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
            // `col` phases each cell into its half of the shared 8-step
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
        // Plain UNDERLINE (and DOUBLE_UNDERLINE's primary line, below) is a
        // solid line.
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

/// Underline styles must not render pixel-identical to a plain line. Pure
/// functions, no GPU needed.
#[cfg(test)]
mod underline_style_tests;

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
    colors: &mut chrome::Colors,
    linear: bool,
) -> Vec<QuadInstance> {
    let mut out: Vec<QuadInstance> = Vec::new();
    let panel_w = context_menu_panel_width(menu, cw);
    let row_h = ch + 12.0;
    let sep_h = 8.0_f32;
    // Terminator menu UX: natural panel height (sum of every row) may exceed
    // the surface. App-side `context_menu_geometry` already computed the
    // clamped height; if non-zero we honor it, otherwise use the unclamped
    // natural sum.
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
    let (inset, content_h) = menu::scroll_content(natural_h, panel_h, row_h);
    let (clipped_top, clipped_bottom) =
        context_menu_clip_indicators(&menu.rows, menu.scroll_offset, content_h, row_h, sep_h);

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
    let mut row_y = ay + inset;
    let start = menu.scroll_offset.min(menu.rows.len());
    for (i, row) in menu.rows.iter().enumerate().skip(start) {
        let h = if row.separator { sep_h } else { row_h };
        if row_y + h > ay + inset + content_h {
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
            let selected_bg = colors.surface(theme.background, accent, 0.18, linear);
            let rail = colors.icon(accent, selected_bg);
            out.push(rect(ax + 1.0, row_y, 2.0, row_h, rail, 1.0));
        }
        row_y += row_h;
    }
    let arrow = colors.icon(accent, theme.background);
    for (visible, top, down) in [
        (clipped_top, ay, false),
        (clipped_bottom, ay + panel_h - inset, true),
    ] {
        if !visible || inset <= 0.0 {
            continue;
        }
        let center = ax + panel_w * 0.5;
        let size = (inset * 0.7).min(panel_w * 0.25);
        let step_h = (inset / 3.0).min(2.0);
        for step in 0..3 {
            let width = size * (step + 1) as f32 / 3.0;
            let dy = if down { 2 - step } else { step } as f32 * step_h;
            let y = top + (inset - 3.0 * step_h) * 0.5 + dy;
            out.push(rect(center - width * 0.5, y, width, step_h, arrow, 1.0));
        }
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
/// it is the only one that has to premultiply itself. Skipping that leaves a
/// translucent background too bright by exactly the skipped alpha factor.
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

/// Surface-pixel origin of a pane's terminal grid: inside the padding of its
/// terminal rectangle, which the layout has already carved clear of the
/// pane's titlebar (above or below) and preview lane.
pub fn pane_grid_origin(terminal: (f32, f32, f32, f32), padding: (f32, f32)) -> (f32, f32) {
    (terminal.0 + padding.0, terminal.1 + padding.1)
}

/// The interior of a pane's terminal to paint with its own default
/// background: the terminal rectangle `(x, y, w, h)` inset by the border
/// width `bw`, so the backdrop never overpaints the focus border, the
/// titlebar or the preview lane beside it. `None` when the interior would be
/// empty (a degenerate pane, or a border at least half its size).
fn pane_backdrop_rect(terminal: (f32, f32, f32, f32), bw: f32) -> Option<(f32, f32, f32, f32)> {
    let (rx, ry, rw, rh) = terminal;
    let bwid = (rw - 2.0 * bw).max(0.0);
    let bhgt = (rh - 2.0 * bw).max(0.0);
    (bwid > 0.0 && bhgt > 0.0).then_some((rx + bw, ry + bw, bwid, bhgt))
}

fn inactive_pane_amount(cfg: &Config) -> f32 {
    (1.0 - cfg.inactive_color_offset)
        .clamp(0.0, 0.95)
        .max((1.0 - cfg.inactive_bg_color_offset).clamp(0.0, 0.95))
        .max((1.0 - cfg.unfocused_split_opacity).clamp(0.0, 0.95))
}

fn pane_inactive_amount(pane: &PaneView<'_>, count: usize, cfg: &Config) -> f32 {
    if !pane.focused && count > 1 {
        inactive_pane_amount(cfg)
    } else {
        0.0
    }
}

/// Apply dim after palette brightening and RGB resolution. The caller applies
/// minimum contrast last, against the background actually painted under it.
fn transformed_foreground(fg: Rgb, bg: Rgb, dim: bool) -> Rgb {
    if dim { color::dim(fg, bg) } else { fg }
}

#[cfg(test)]
fn attributed_foreground(
    fg: alacritty_terminal::vte::ansi::Color,
    bg: Rgb,
    dim: bool,
    bold: bool,
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
) -> Rgb {
    let fg = if bold && cfg.bold_is_bright {
        color::bright_for_bold(fg)
    } else {
        fg
    };
    let fg = color::resolve(fg, theme, &Default::default());
    let fg = transformed_foreground(fg, bg, dim);
    if cfg.minimum_contrast > 1.0 {
        color::with_min_contrast(fg, bg, cfg.minimum_contrast as f64)
    } else {
        fg
    }
}

#[derive(Default)]
struct MinimumContrastCache {
    entries: std::collections::HashMap<(u32, u32, u64), Rgb>,
    misses: usize,
}

impl MinimumContrastCache {
    fn color_key(color: Rgb) -> u32 {
        (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
    }

    fn resolve(&mut self, fg: Rgb, bg: Rgb, minimum: f64) -> Rgb {
        if minimum <= 1.0 {
            return fg;
        }
        let key = (Self::color_key(fg), Self::color_key(bg), minimum.to_bits());
        if let Some(resolved) = self.entries.get(&key) {
            return *resolved;
        }
        let resolved = color::with_min_contrast(fg, bg, minimum);
        self.entries.insert(key, resolved);
        self.misses += 1;
        resolved
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.misses = 0;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Copy)]
enum CellHighlight {
    None,
    Selection,
    Search(bool),
}

#[cfg(test)]
fn resolved_cell_foreground(
    fg: Rgb,
    bg: Rgb,
    highlight: CellHighlight,
    dim: bool,
    bold: bool,
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
) -> Rgb {
    use alacritty_terminal::vte::ansi::{Color as AnsiColor, Rgb as AnsiRgb};

    let spec = |c: Rgb| {
        AnsiColor::Spec(AnsiRgb {
            r: c.r,
            g: c.g,
            b: c.b,
        })
    };
    match highlight {
        CellHighlight::Search(true) => attributed_foreground(
            spec(cfg.search_foreground.unwrap_or(theme.background)),
            cfg.search_background.unwrap_or(theme.palette[3]),
            false,
            false,
            cfg,
            theme,
        ),
        CellHighlight::Search(false) | CellHighlight::Selection => attributed_foreground(
            spec(theme.selection_foreground),
            theme.selection_background,
            false,
            false,
            cfg,
            theme,
        ),
        CellHighlight::None => attributed_foreground(spec(fg), bg, dim, bold, cfg, theme),
    }
}

fn resolved_cell_foreground_cached(
    fg: Rgb,
    bg: Rgb,
    highlight: CellHighlight,
    attributes: (bool, bool),
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
    cache: &mut MinimumContrastCache,
) -> Rgb {
    let (dim, _) = attributes;
    let (fg, bg, dim) = match highlight {
        CellHighlight::Search(true) => (
            cfg.search_foreground.unwrap_or(theme.background),
            cfg.search_background.unwrap_or(theme.palette[3]),
            false,
        ),
        CellHighlight::Search(false) | CellHighlight::Selection => (
            theme.selection_foreground,
            theme.selection_background,
            false,
        ),
        CellHighlight::None => (fg, bg, dim),
    };
    let fg = transformed_foreground(fg, bg, dim);
    cache.resolve(fg, bg, cfg.minimum_contrast as f64)
}

/// Terminator parity, terminatorlib/config.py:106 + 117: compose the kettle
/// background opacity with Terminator's `background_darkness` and
/// `background_type`:
///
///   bg-type = solid (default):  alpha = background_opacity
///   bg-type = transparent:      alpha = background_opacity * background_darkness
///   bg-type = image/starfield:  same as transparent; darkness lets the
///                               backdrop show through the terminal area, not
///                               only behind the chrome
///
/// `background_darkness` runs from see-through (`0.0`) to fully covered
/// (`1.0`), because Terminator assigns it straight to the background colour's
/// alpha and its users lower it for more transparency.
/// `darkness_scales_the_backdrop_toward_see_through` pins that direction.
///
/// Parsing already clamps every input.
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

/// Grid column of the cluster a laid-out glyph belongs to (cell-locked
/// rendering). `char_starts[k]` is the byte offset of cell k's base scalar,
/// including wide-char spacers but excluding attached zero-width marks.
/// A mark's cluster start therefore maps to the same column as its base.
fn glyph_grid_col(char_starts: &[u32], cluster_start: usize) -> usize {
    char_starts
        .partition_point(|&bs| (bs as usize) <= cluster_start)
        .saturating_sub(1)
}

/// Cell-locked logical pen X (physical px), snapped to an integer pixel so the
/// glyph is crisp and shares one subpixel-bin (x_bin = 0) cache slot regardless
/// of cell: the grid cell's left edge plus any intra-cluster `x_offset` (kept so
/// a combining mark still stacks on its base). It replaces cosmic-text's
/// advance-accumulated `glyph.x`. The two agree for a primary-face monospace
/// glyph (advance == cell_w), so only advance-mismatched glyphs (fallback / CJK /
/// ligature) move.
fn cell_locked_pen_x(cell_left: f32, x_offset_px: f32) -> f32 {
    (cell_left + x_offset_px).round()
}

fn cell_locked_glyph_offset(
    cell_left: f32,
    glyph: &glyphon::cosmic_text::LayoutGlyph,
    cluster_x: f32,
) -> f32 {
    let x_off_px = glyph.font_size * glyph.x_offset;
    cell_locked_pen_x(cell_left + (glyph.x - cluster_x), x_off_px) - glyph.x - x_off_px
}

/// Emit cell-locked glyph instances for ONE shaped `TextBuffer`, appending to
/// `out`. This is the single source of truth for the Grid (cell-locked)
/// renderer's per-glyph emit: `emit_pane_glyphs` (live panes), the offscreen
/// `capture_png_with_annotation` screenshot path, and the `grid_prompt_blink`
/// test fixture all call it so they can never drift apart.
///
/// A `None` cell width retains glyphon's continuous layout for inactive Legacy
/// panes, whose color bitmaps need this pipeline's dimming shader.
/// `origin` is the buffer's top-left in physical pixels (the same coordinate a
/// glyphon `TextArea`'s `left`/`top` would use). With a cell width, each glyph is
/// pinned to its grid cell and snapped to an integer pixel, preserving any
/// intra-cluster `x_offset` (combining marks). `default_color` is
/// the fallback for a glyph with no explicit color span (mirrors a `TextArea`'s
/// `default_color`). `char_starts` is a scratch buffer reused across runs to
/// avoid per-line allocation. Glyphs are appended in buffer order, so the
/// caller can wrap the appended range in one `GlyphClip` for scissor clipping.
#[allow(clippy::too_many_arguments)]
fn emit_cell_locked_glyphs(
    out: &mut Vec<GlyphInstance>,
    buf: &TextBuffer,
    origin: (f32, f32),
    cell_w: Option<f32>,
    default_color: GColor,
    glyph_pipeline: &mut GlyphPipeline,
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    char_starts: &mut Vec<u32>,
) {
    for run in buf.layout_runs() {
        // Use the engine's width classifier: zero-width scalars attach to
        // the preceding cell; wide bases still have an explicit spacer cell.
        char_starts.clear();
        for (b, ch) in run.text.char_indices() {
            if unicode_width::UnicodeWidthChar::width(ch) != Some(0) {
                char_starts.push(b as u32);
            }
        }
        let line_y = run.line_y;
        let mut active_cell = None;
        let mut cluster_x = 0.0;
        for glyph in run.glyphs.iter() {
            let col = glyph_grid_col(char_starts, glyph.start);
            if active_cell != Some(col) {
                active_cell = Some(col);
                cluster_x = glyph.x;
            }
            // Pin the pen to the cell, snapped to an integer physical pixel
            // (crisp + cache-friendly: x_bin = 0), while keeping any intra-
            // cluster x_offset. Feeding cosmic-text's `physical()` an offset that
            // lands the logical X exactly there reuses its exact cache-key +
            // vertical + subpixel math, so the rasterized bitmap is byte-
            // identical to glyphon's.
            let off_x = cell_w.map_or(origin.0, |cw| {
                let cell_left = origin.0 + col as f32 * cw;
                cell_locked_glyph_offset(cell_left, glyph, cluster_x)
            });
            let phys = glyph.physical((off_x, origin.1), 1.0);
            let key = phys.cache_key;
            let mut image = None;
            let slot = match glyph_pipeline.ensure_glyph(device, queue, key, || {
                image = swash.get_image_uncached(font_system, key);
                RasterGlyph::from_swash(image.as_ref()?)
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

/// The fonts and cell size a window needs before its GPU exists.
///
/// A pane's grid depends only on the cell size, so loading these first lets
/// the first pane start while the GPU initializes. A renderer built with them
/// takes them over instead of enumerating the system fonts a second time.
pub struct StartupFonts {
    font_system: FontSystem,
    text_symbol_family: Option<&'static str>,
    /// Physical-pixel cell size, with `cell-width` and `cell-height` applied.
    pub cell: (f32, f32),
    /// The inputs `cell` was measured from.
    key: StartupFontsKey,
}

#[derive(Clone, Debug, PartialEq)]
struct StartupFontsKey {
    family: String,
    font_size: f32,
    scale: f32,
    cell_width: f32,
    cell_height: f32,
}

impl StartupFontsKey {
    fn new(cfg: &Config, scale: f32) -> Self {
        Self {
            family: cfg.font_family.clone(),
            font_size: clamp_font_size(cfg.font_size),
            scale,
            cell_width: cfg.cell_width,
            cell_height: cfg.cell_height,
        }
    }
}

/// The system fonts, before the display's scale is known.
///
/// Nothing here depends on the scale, so these can be loaded on another
/// thread while the event loop starts. [`PreparedFonts::measure`] then
/// measures the cell once the scale is known.
pub struct PreparedFonts {
    font_system: FontSystem,
    text_symbol_family: Option<&'static str>,
    /// Families [`PreparedFonts::warm_family`] has already shaped.
    warm: Vec<String>,
}

impl PreparedFonts {
    /// Enumerate the system fonts, load the bundled face, and resolve the
    /// text-presentation face.
    pub fn enumerate() -> Self {
        let t_font_system = std::time::Instant::now();
        let mut font_system = FontSystem::new();
        let font_system_ms = t_font_system.elapsed().as_secs_f64() * 1000.0;
        let t_bundled = std::time::Instant::now();
        load_bundled_font(&mut font_system, kettle_config::font::REGULAR);
        // Resolved once: fontdb is already populated and the answer cannot
        // change while the renderer lives.
        let text_symbol_family = resolve_text_symbol_family(&mut font_system);
        log::info!("renderer init: text-presentation face {text_symbol_family:?}");
        // Split, because `FontSystem::new()` is the one people suspect (it
        // enumerates system fonts) and a combined figure cannot exonerate it.
        log::info!(
            "renderer init: FontSystem::new {font_system_ms:.1}ms, bundled font {:.1}ms",
            t_bundled.elapsed().as_secs_f64() * 1000.0
        );
        Self {
            font_system,
            text_symbol_family,
            warm: Vec::new(),
        }
    }

    /// Shape the cell probe in `family` once, so that measuring it later
    /// finds cosmic-text's font matches and faces already loaded: those for
    /// the default attributes every text buffer starts with, and those for
    /// `family`.
    ///
    /// Bold is not warmed. The first styled cell loads the bundled bold
    /// faces, and adding a face clears cosmic-text's match cache.
    pub fn warm_family(&mut self, family: &str) {
        if self.warm.iter().any(|warm| warm == family) {
            return;
        }
        // Matches and faces do not depend on the size, so any metrics do.
        let metrics = metrics_for(13.0, 1.0);
        let mut probe = TextBuffer::new(&mut self.font_system, metrics);
        measure_cell(&mut self.font_system, &mut probe, family, metrics);
        self.warm.push(family.to_owned());
    }

    /// Measure the cell for `cfg` at `scale`, the display's scale factor.
    pub fn measure(self, cfg: &Config, scale: f32) -> StartupFonts {
        let Self {
            mut font_system,
            text_symbol_family,
            ..
        } = self;
        let key = StartupFontsKey::new(cfg, scale);
        let metrics = metrics_for(key.font_size, scale);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let (cell_w, cell_h) = measure_cell(&mut font_system, &mut measure, &key.family, metrics);
        // `cell-width` and `cell-height` are clamped to [0.5, 3.0] at parse
        // time, so the cell cannot degenerate to 0 here.
        let cell = (
            cell_w * key.cell_width.max(0.01),
            cell_h * key.cell_height.max(0.01),
        );
        StartupFonts {
            font_system,
            text_symbol_family,
            cell,
            key,
        }
    }
}

// The font preload builds these on its own thread and hands them to the
// event-loop thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<PreparedFonts>();
    assert_send::<StartupFonts>();
};

impl StartupFonts {
    /// Enumerate the system fonts, load the bundled face, and measure the
    /// cell for `cfg` at `scale`, the display's scale factor.
    pub fn load(cfg: &Config, scale: f32) -> Self {
        PreparedFonts::enumerate().measure(cfg, scale)
    }

    /// These fonts, measured for `cfg` at `scale` instead. The system fonts
    /// are not enumerated again.
    pub fn remeasure(self, cfg: &Config, scale: f32) -> Self {
        let Self {
            font_system,
            text_symbol_family,
            ..
        } = self;
        PreparedFonts {
            font_system,
            text_symbol_family,
            warm: Vec::new(),
        }
        .measure(cfg, scale)
    }

    /// Whether these fonts were measured for `cfg` at `scale`.
    pub fn matches(&self, cfg: &Config, scale: f32) -> bool {
        self.key == StartupFontsKey::new(cfg, scale)
    }
}

fn measure_cell(
    fs: &mut FontSystem,
    buf: &mut TextBuffer,
    family: &str,
    metrics: Metrics,
) -> (f32, f32) {
    buf.set_metrics(metrics);
    // Size the measure box from the (physical) metrics so the probe never
    // wraps. At a large font on a high-DPI display the physical font size can
    // be ~200px, making the 10-glyph probe ~1300px wide. If it wrapped,
    // `line_w` would cover only the first line and `cell_w` would come out too
    // narrow, mis-gridding the terminal. A monospace `M` is ~0.6em, so 10 fit
    // in ~6em; 20em + slack never wraps at any size or scale.
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

fn screenshot_text_buffer(
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
fn screenshot_meter(value: u32, total: u32) -> (String, String) {
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

/// The fill and edge of a bar that asks for input.
/// The fill is a neutral the theme's own foreground reads on at WCAG AA
/// (4.5:1); the edge is the bar's `accent`, kept at the 3:1 minimum for
/// non-text UI against that fill. Each bar keeps its own accent, so the
/// palette, the pickers and the title editor still look different.
#[cfg(test)]
fn input_bar_colors(theme: &kettle_config::Theme, accent: Rgb) -> (Rgb, Rgb) {
    let mut colors = chrome::Colors::default();
    let fill = colors.text(theme.palette[8], theme.foreground);
    let edge = colors.icon(accent, fill);
    (fill, edge)
}

/// Queue an input bar at `(x, y, w, h)`: the opaque fill from
/// [`input_bar_colors`] with 2px top and 4px left edges. Opaque because the
/// contrast is computed against the fill alone; content showing through would
/// change it.
fn push_input_bar(
    quads: &mut Vec<QuadInstance>,
    (x, y, w, h): (f32, f32, f32, f32),
    theme: &kettle_config::Theme,
    accent: Rgb,
    colors: &mut chrome::Colors,
) {
    let fill = colors.text(theme.palette[8], theme.foreground);
    let edge = colors.icon(accent, fill);
    quads.push(rect(x, y, w, h, fill, 1.0));
    quads.push(rect(x, y, w, 2.0, edge, 1.0));
    quads.push(rect(x, y, 4.0, h, edge, 1.0));
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
