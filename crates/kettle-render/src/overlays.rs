use super::{
    CompletionOverlay, HighlightRect, LinkRect, MediaLanePanel, PaintedCard, QuadInstance, Rect4,
    Rgb, SearchOverlay, SettingsOverlay, Text, Translator, Wrap, chrome, completion_overlay_rect,
    display_width, fit_single_line_label, rect, rects_overlap,
};

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
pub(crate) fn context_menu_panel_width(menu: &ContextMenu, cell_width: f32) -> f32 {
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

pub(crate) fn context_menu_clip_indicators(
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

/// Overlay state projected by the UI, covering search, links, hints, pickers,
/// menus, dialogs, and banners.
///
/// There is no `Default` outside tests (the `test-defaults` feature): a
/// default overlay speaks English and carries no state, so the UI lists every
/// field. A production build, which never enables the feature, rejects any
/// overlay built from a default.
#[cfg_attr(any(test, feature = "test-defaults"), derive(Default))]
pub struct Overlay {
    pub text_field: Option<crate::TextFieldOverlay>,
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

pub(crate) fn toast_buffer_text(
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

/// Most lane images one frame draws: one per pane with a lane.
pub(crate) const MAX_LANE_IMAGES: usize = 64;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NotificationGeometry {
    pub toast: Option<UpdateToastGeometry>,
    pub receipt: Option<MediaPasteReceiptGeometry>,
}

pub fn notification_geometry(
    toast: Option<UpdateToastGeometry>,
    receipt: Option<&MediaPasteReceiptOverlay>,
    completion: Option<&CompletionOverlay>,
    cell: (f32, f32),
    text_cell_width: f32,
    text_line_height: f32,
) -> NotificationGeometry {
    NotificationGeometry {
        toast,
        receipt: receipt.and_then(|receipt| {
            media_paste_receipt_geometry_avoiding(
                receipt,
                completion,
                toast.map(|g| g.rect),
                cell,
                text_cell_width,
                text_line_height,
            )
        }),
    }
}

/// Place the receipt inside its terminal grid without covering completion UI.
pub fn media_paste_receipt_geometry(
    receipt: &MediaPasteReceiptOverlay,
    completion: Option<&CompletionOverlay>,
    cell: (f32, f32),
    text_cell_width: f32,
    text_line_height: f32,
) -> Option<MediaPasteReceiptGeometry> {
    media_paste_receipt_geometry_avoiding(
        receipt,
        completion,
        None,
        cell,
        text_cell_width,
        text_line_height,
    )
}

fn media_paste_receipt_geometry_avoiding(
    receipt: &MediaPasteReceiptOverlay,
    completion: Option<&CompletionOverlay>,
    toast: Option<Rect4>,
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
        let mut rect = if top_lane {
            (x, top + inset_y, card_w, card_h)
        } else {
            (x, bottom - inset_y - card_h, card_w, card_h)
        };
        if let Some(toast) = toast
            && rects_overlap(rect, toast)
        {
            rect.1 = toast.1 + toast.3 + inset_y;
        }
        rect
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
                && candidate.1 >= top + inset_y
                && candidate.1 + card_h <= bottom - inset_y
                && toast.is_none_or(|other| !rects_overlap(*candidate, other))
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
pub(crate) fn remote_warning_columns(tr: &Translator) -> f32 {
    let widest = [Text::ReceiptRemotePane, Text::ReceiptLocalPathOnly]
        .into_iter()
        .map(|line| display_width(tr.text(line)))
        .max()
        .unwrap_or(0);
    (widest + 3).max(18) as f32
}

/// The compact card's title, which also sizes the card.
pub(crate) fn media_paste_receipt_compact_title(receipt: &MediaPasteReceiptOverlay) -> String {
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

pub(crate) fn media_paste_receipt_text(
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

pub(crate) const NEW_TAB_MENU_GLYPH: &str = "▾";

pub(crate) const NEW_TAB_PLUS_GLYPH: &str = "+";

/// Center one shaped, single-line chrome glyph inside its interactive rect.
/// Keeping this independent of the font's advance and the tab-bar height is
/// what makes visually different symbols such as `▾` and `+` share one optical
/// center without hard-coded spaces or padding that drift across fonts/scales.
/// The tab bar's overflow arrows, back and forward: chevrons across a top or
/// bottom bar, and their upright forms down a side one.
pub(crate) fn scroll_arrow_glyphs(vertical: bool) -> (&'static str, &'static str) {
    if vertical {
        ("⌃", "⌄")
    } else {
        (" ‹", " ›")
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
pub(crate) fn menu_chrome_quads(
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

/// The fill and edge of a bar that asks for input.
/// The fill is a neutral the theme's own foreground reads on at WCAG AA
/// (4.5:1); the edge is the bar's `accent`, kept at the 3:1 minimum for
/// non-text UI against that fill. Each bar keeps its own accent, so the
/// palette, the pickers and the title editor still look different.
#[cfg(test)]
pub(crate) fn input_bar_colors(theme: &kettle_config::Theme, accent: Rgb) -> (Rgb, Rgb) {
    let mut colors = chrome::Colors::default();
    let fill = colors.text(theme.palette[8], theme.foreground);
    let edge = colors.icon(accent, fill);
    (fill, edge)
}

/// Queue an input bar at `(x, y, w, h)`: the opaque fill from
/// [`input_bar_colors`] with 2px top and 4px left edges. Opaque because the
/// contrast is computed against the fill alone; content showing through would
/// change it.
pub(crate) fn push_input_bar(
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
