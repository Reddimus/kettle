use super::theme::pane_inactive_amount;
use super::{
    Attrs, AttrsList, AttrsOwned, BufferLine, CellHighlight, Config, Family, Flags, FontFeatures,
    GColor, GlyphClip, GlyphInstance, GlyphPositioning, InlineCards, LineEnding, Metrics,
    PaneSnapshot, QuadInstance, Rect4, Renderer, Rgb, Shaping, Style, TextBuffer, TextRendererMode,
    Translator, Weight, Wrap, background_has_wallpaper, card_scene, clip_quads, color,
    composed_bg_alpha, cursor_policy, emit_cell_locked_glyphs, font_features, inline_cards,
    inline_image_clip, pane_backdrop_rect, pane_grid_origin, pane_metrics, rect,
    resolved_cell_foreground_cached, selection_row_span, single_column_text_presentation,
    visible_selection_rows,
};

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
pub(crate) fn search_highlight_at(
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

#[inline]
pub(crate) fn cursor_focus_gate(window_focused: bool, terminal_requests_cursor: bool) -> bool {
    window_focused && terminal_requests_cursor
}

pub(crate) fn ime_cursor_shape(
    shape: alacritty_terminal::vte::ansi::CursorShape,
) -> alacritty_terminal::vte::ansi::CursorShape {
    use alacritty_terminal::vte::ansi::CursorShape;
    match shape {
        CursorShape::Hidden => CursorShape::Hidden,
        _ => CursorShape::Beam,
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

/// The foreground glyph drawn on top of a focused solid block cursor this
/// frame. It is rendered in its own tiny renderer and 1-line buffer rather than
/// recolored into the pane text buffer, so a cursor blink does not mutate the
/// pane buffer and force the expensive whole-viewport `prepare`. The glyph
/// bitmap is already in the atlas (it is part of the visible pane text), so the
/// 1-glyph prepare never grows it.
pub(crate) struct PendingCursorGlyph {
    /// Surface-pixel top-left of the cursor cell.
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// Owning row baseline, relative to the cursor cell top.
    pub(crate) row_baseline: f32,
    /// The character under the cursor (drawn in `color`).
    pub(crate) ch: char,
    /// Base plus the snapshot's bounded attached marks.
    pub(crate) text: String,
    /// Whether that cell carried an explicit U+FE0F. The cursor pass makes the
    /// same presentation choice the pane does and has to make it on the same
    /// evidence: `ch` alone cannot show a variation selector, so a qualified
    /// sequence would draw in colour underneath and monochrome on top.
    pub(crate) emoji_qualified: bool,
    pub(crate) attrs: AttrsOwned,
    /// Cursor foreground (theme `cursor_text`, or the cell bg under an OSC 12
    /// runtime cursor color so the inverted glyph follows reverse-video).
    pub(crate) color: Rgb,
    /// Pane rect `(x, y, w, h)`, narrowed to the cell for clustered text.
    pub(crate) clip: (f32, f32, f32, f32),
}

pub(crate) fn invalidate_pane_titlebar_texts(texts: &mut [String]) {
    texts.iter_mut().for_each(String::clear);
}

pub(crate) fn cursor_glyph_top(cell_top: f32, row_baseline: f32, buffer: &TextBuffer) -> f32 {
    let own_baseline = buffer.layout_runs().next().map_or(0.0, |run| run.line_y);
    cell_top + row_baseline - own_baseline
}

pub(crate) fn cursor_glyph_damage_key(
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

impl Renderer {
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
    pub(crate) fn commit_painted_cards(&mut self) {
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

    /// Build one pane's cells, cursor and text, and keep every quad it adds
    /// inside the pane's terminal: a snapshot can be briefly larger than a
    /// terminal that has just shrunk for its preview lane, and its extra
    /// rows must not paint over the lane or past the pane.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_pane(
        &mut self,
        idx: usize,
        pv: &PaneView<'_>,
        cfg: &Config,
        inactive: f32,
        family: &str,
        window_focused: bool,
        composing: bool,
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
            composing,
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
        composing: bool,
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
        let mut cell_positions: Vec<Option<Vec<CellGlyphSpan>>> = if snap.has_graphemes() {
            vec![None; screen_rows.max(0) as usize]
        } else {
            Vec::new()
        };
        if !cell_positions.is_empty() {
            for cell in &snap.cells {
                if cell.flags.contains(Flags::GRAPHEME)
                    && let Ok(row) = usize::try_from(cell.line + display_off)
                    && let Some(positions) = cell_positions.get_mut(row)
                {
                    positions.get_or_insert_with(Vec::new);
                }
            }
        }
        let mut row_bytes = 0;

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
        let shape = if composing {
            ime_cursor_shape(shape)
        } else {
            shape
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
                row_bytes = 0;
                cur = None; // runs never span a line break
                for _ in cur_row..row {
                    span_line_breaks.push(n);
                }
                cur_row = row;
            }

            let card_cell = card_frame.cell(snap, snapshot_index);
            let owned_cell = !matches!(card_cell, inline_cards::CardCell::Ordinary);
            let (fg_spec, bg_spec, flags) = if owned_cell {
                (sc.fg, sc.bg, Flags::empty())
            } else {
                color::special_attributes(sc.fg, sc.bg, sc.flags, theme, term_colors)
            };
            // Terminator parity, terminatorlib/config.py:111
            // `allow_bold`: when false, suppress bold attr entirely.
            // Useful on fonts without a bold companion.
            let bold = cfg.allow_bold && flags.contains(Flags::BOLD);
            let (raw_fg, raw_bg) = if flags.contains(Flags::INVERSE) {
                (bg_spec, fg_spec)
            } else {
                (fg_spec, bg_spec)
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
            let mut dc = card_cell.glyph(sc.c, hidden);
            let mut marks: &[char] = if hidden || owned_cell {
                &[]
            } else {
                sc.zerowidth()
            };
            let text_presentation = flags.contains(Flags::GRAPHEME)
                && marks
                    .iter()
                    .rev()
                    .find(|&&c| matches!(c, '\u{fe0e}' | '\u{fe0f}'))
                    == Some(&'\u{fe0e}');
            let text_family = if text_presentation {
                let family = super::fonts::grapheme_text_family(
                    &mut self.font_system,
                    cfg.family_for(bold, italic),
                    dc,
                    marks,
                );
                if family.is_none() {
                    dc = '\u{fffd}';
                    marks = &[];
                }
                family
            } else {
                None
            };
            if let Ok(row) = usize::try_from(vrow)
                && let Some(Some(positions)) = cell_positions.get_mut(row)
            {
                let end =
                    row_bytes + dc.len_utf8() + marks.iter().map(|c| c.len_utf8()).sum::<usize>();
                positions.push(CellGlyphSpan {
                    bytes: row_bytes..end,
                    cell: super::geometry::GridGlyphCell {
                        column: col,
                        width: if flags.contains(Flags::WIDE_CHAR) {
                            2
                        } else {
                            1
                        },
                        clustered: flags.contains(Flags::GRAPHEME),
                        text_presentation,
                        single_image: flags.contains(Flags::GRAPHEME | Flags::WIDE_CHAR)
                            && !text_presentation
                            && marks.contains(&'\u{200d}'),
                    },
                    family: text_family.clone(),
                });
                row_bytes = end;
            }
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
                if let Some(symbol_family) = text_family
                    .as_deref()
                    .or(self.text_symbol_family)
                    .filter(|_| {
                        text_presentation
                            || (!marks.contains(&'\u{FE0F}') && single_column_text_presentation(dc))
                    })
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
        let mut reshaped_grapheme_rows = Vec::new();
        for row in 0..rows {
            let end = span_line_breaks.get(row).copied().unwrap_or(n).min(n);
            let runs = &spans[start.min(end)..end];
            start = end;
            // Shape the row only up to its last inked cell (see `ShapedRow`).
            // The key covers exactly what is shaped, so recolouring the
            // blanks after that cell reshapes nothing.
            let shaped = ShapedRow::of(runs);
            let positions = cell_positions
                .get(row)
                .and_then(Option::as_deref)
                .unwrap_or_default();
            let key = shaped.key_with_positions(runs, positions);
            let prev = keys.get(row).copied();
            if prev == Some(key) {
                continue;
            }
            // This row reshapes — the buffer's glyph vertices will differ.
            text_changed = true;
            if !positions.is_empty() {
                reshaped_grapheme_rows.push(row);
            }
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
            apply_cell_positions(
                &mut attrs_list,
                positions,
                row_text.len(),
                text_symbol_family,
            );
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
        let replaced = replace_unsupported_graphemes(
            buf,
            &mut self.font_system,
            &mut self.swash,
            &reshaped_grapheme_rows,
        );
        if replaced {
            buf.shape_until_scroll(&mut self.font_system, false);
        }
        if recolor_cursor_cell.is_some()
            && let Some(cursor) = self.pending_cursor_glyph.as_mut()
            && let Some(line) = usize::try_from(cvrow)
                .ok()
                .and_then(|row| buf.lines.get(row))
            && let Some((range, attrs, cell)) =
                line.attrs_list().spans_iter().find_map(|(range, attrs)| {
                    super::geometry::GridGlyphCell::from_metadata(attrs.metadata)
                        .filter(|cell| {
                            cell.clustered
                                && cell.column <= cp.column.0
                                && cp.column.0 < cell.column + cell.width
                        })
                        .map(|cell| (range, attrs, cell))
                })
        {
            let text = &line.text()[range.clone()];
            cursor.ch = text.chars().next().unwrap_or(' ');
            cursor.text = text.to_owned();
            cursor.emoji_qualified = text.contains('\u{fe0f}');
            cursor.attrs = AttrsOwned::new(&attrs.as_attrs().metadata(0).color(GColor::rgb(
                cursor.color.r,
                cursor.color.g,
                cursor.color.b,
            )));
            let left = cursor.x.round().max(cursor.clip.0);
            let right = (cursor.x + cell.width as f32 * cw)
                .round()
                .min(cursor.clip.0 + cursor.clip.2);
            cursor.clip = (left, cursor.clip.1, (right - left).max(0.0), cursor.clip.3);
            let offset = alacritty_terminal::term::grapheme_base_offset(text);
            if offset > 0
                && let Some(run) = buf.layout_runs().find(|run| run.line_i == cvrow as usize)
            {
                let glyphs: Vec<_> = run
                    .glyphs
                    .iter()
                    .filter(|glyph| glyph.metadata == attrs.metadata)
                    .collect();
                let byte = range.start + offset;
                if let Some(base) = glyphs
                    .iter()
                    .find(|glyph| glyph.start <= byte && byte < glyph.end)
                    && let Some(origin) = glyphs.iter().map(|glyph| glyph.x).min_by(f32::total_cmp)
                {
                    cursor.x -= base.x - origin;
                }
            }
        }
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
    pub(crate) fn emit_pane_glyphs(
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
            let graphemes = pv.snap.has_graphemes();
            if cfg.text_renderer == TextRendererMode::Legacy && inactive == 0.0 && !graphemes {
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
                if cfg.text_renderer == TextRendererMode::Grid {
                    GlyphPositioning::Grid(cw)
                } else if graphemes {
                    GlyphPositioning::GraphemeRows(cw)
                } else {
                    GlyphPositioning::Continuous
                },
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

/// Attrs for one style run: the family picks the bold/italic variant
/// (`cfg.family_for`), the color is the run's resolved fg, weight/style
/// mirror the SGR bold/italic bits. The per-line shaping cache builds one
/// row's `AttrsList` at a time, so unchanged rows never construct an `Attrs`.
pub(crate) fn run_attrs<'a>(
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
pub(crate) struct ShapedRow {
    /// Runs shaped, from the start of the row.
    pub(crate) runs: usize,
    /// Bytes shaped of the last of those runs.
    pub(crate) last_len: usize,
    /// The faces of the cut blanks, one bit per `(bold, italic)` pair (see
    /// `ShapedRow::face_bit`). Each bit adds one pad blank after the runs.
    pub(crate) pad_faces: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CellGlyphSpan {
    pub bytes: std::ops::Range<usize>,
    pub cell: super::geometry::GridGlyphCell,
    pub family: Option<String>,
}

pub(crate) fn apply_cell_positions(
    attrs: &mut AttrsList,
    positions: &[CellGlyphSpan],
    len: usize,
    text_family: Option<&str>,
) {
    for position in positions
        .iter()
        .take_while(|position| position.bytes.start < len)
    {
        let owned = AttrsOwned::new(&attrs.get_span(position.bytes.start));
        let mut cell_attrs = owned.as_attrs().metadata(position.cell.metadata());
        if position.cell.text_presentation
            && let Some(family) = position.family.as_deref().or(text_family)
        {
            cell_attrs = cell_attrs.family(Family::Name(family));
        }
        attrs.add_span(
            position.bytes.start..position.bytes.end.min(len),
            &cell_attrs,
        );
    }
}

fn replace_unsupported_graphemes(
    buffer: &mut TextBuffer,
    fonts: &mut super::FontSystem,
    swash: &mut super::SwashCache,
    rows: &[usize],
) -> bool {
    use super::geometry::GridGlyphCell;
    use std::collections::{HashMap, HashSet};
    let mut replacements = Vec::new();
    for run in buffer
        .layout_runs()
        .filter(|run| rows.binary_search(&run.line_i).is_ok())
    {
        let mut images: HashMap<usize, usize> = buffer.lines[run.line_i]
            .attrs_list()
            .spans_iter()
            .filter_map(|(_, attrs)| {
                GridGlyphCell::from_metadata(attrs.metadata)
                    .filter(|cell| cell.single_image)
                    .map(|_| (attrs.metadata, 0))
            })
            .collect();
        for glyph in run.glyphs {
            if let Some(count) = images.get_mut(&glyph.metadata) {
                let key = glyph.physical((0.0, 0.0), 1.0).cache_key;
                if swash
                    .get_image_uncached(fonts, key)
                    .as_ref()
                    .is_some_and(|image| image.placement.width > 0 && image.placement.height > 0)
                {
                    *count += 1;
                }
            }
        }
        let bad: HashSet<_> = images
            .into_iter()
            .filter(|(_, count)| *count != 1)
            .map(|(metadata, _)| metadata)
            .collect();
        if !bad.is_empty() {
            replacements.push((run.line_i, bad));
        }
    }
    let changed = !replacements.is_empty();
    for (row, bad) in replacements {
        let line = &buffer.lines[row];
        let mut text = String::new();
        let mut attrs = AttrsList::new(&line.attrs_list().defaults());
        let mut replaced = HashSet::new();
        for (byte, ch) in line.text().char_indices() {
            let style = line.attrs_list().get_span(byte);
            let ch = if bad.contains(&style.metadata) {
                if !replaced.insert(style.metadata) {
                    continue;
                }
                '\u{fffd}'
            } else {
                ch
            };
            let start = text.len();
            text.push(ch);
            attrs.add_span(start..text.len(), &style);
        }
        let ending = line.ending();
        buffer.lines[row].set_text(text, ending, attrs);
    }
    changed
}

impl ShapedRow {
    /// The pad's colour. A blank draws no ink, so a fixed colour changes
    /// nothing on screen and keeps a recolour of the cut blanks out of the
    /// shaper's input.
    const PAD_FG: Rgb = Rgb::new(0, 0, 0);

    pub(crate) fn of(runs: &[(String, Rgb, bool, bool)]) -> Self {
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
    pub(crate) fn face_bit(bold: bool, italic: bool) -> u8 {
        1 << (u8::from(bold) | (u8::from(italic) << 1))
    }

    /// The shaped part of the run at `index`, one of the first `self.runs`.
    pub(crate) fn text(self, index: usize, text: &str) -> &str {
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
    pub(crate) fn key(self, runs: &[(String, Rgb, bool, bool)]) -> u64 {
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

    pub(crate) fn key_with_positions(
        self,
        runs: &[(String, Rgb, bool, bool)],
        positions: &[CellGlyphSpan],
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let key = self.key(runs);
        if positions.is_empty() {
            return key;
        }
        let len: usize = runs[..self.runs]
            .iter()
            .enumerate()
            .map(|(i, (text, ..))| self.text(i, text).len())
            .sum();
        let mut hash = std::hash::DefaultHasher::new();
        key.hash(&mut hash);
        for position in positions
            .iter()
            .take_while(|position| position.bytes.start < len)
        {
            position.hash(&mut hash);
        }
        hash.finish()
    }

    /// Appends the pad after the shaped runs: one blank per face in
    /// `pad_faces`, each with that face's attrs from `run_attrs`, the source
    /// the runs and the style key use, in the fixed pad colour.
    pub(crate) fn push_pad(
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

pub(crate) fn push_cell_decorations(
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
pub(crate) fn push_underline_quads(
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
