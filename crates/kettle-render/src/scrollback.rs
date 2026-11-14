use super::{Rect4, Translator, display_width};
use kettle_config::Rgb;

/// The key a jump button shows after its count, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollbackPillHint {
    /// The chord bound to "scroll to bottom", spelled as menus spell it.
    pub text: String,
    pub text_origin: (f32, f32),
    /// The rule between the count and the chord.
    pub divider: Rect4,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollbackPill {
    pub rect: Rect4,
    pub label: String,
    pub accessible_label: String,
    pub text_origin: (f32, f32),
    /// The key that does what the button does, when one is bound and the
    /// pane has room for it after the count.
    pub hint: Option<ScrollbackPillHint>,
}

/// How a jump button is drawn: at rest, under the pointer, or held down
/// under it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScrollbackPillState {
    #[default]
    Rest,
    Hovered,
    Pressed,
}

impl ScrollbackPillState {
    /// `pane`'s button, given the pane whose button the pointer is over and
    /// the pane whose button took a press that has not been released. A
    /// press shows only while the pointer stays on the button, as a release
    /// anywhere else does nothing.
    pub fn of(pane: u64, hovered: Option<u64>, pressed: Option<u64>) -> Self {
        match (hovered == Some(pane), pressed == Some(pane)) {
            (true, true) => Self::Pressed,
            (true, false) => Self::Hovered,
            (false, _) => Self::Rest,
        }
    }

    /// The name `kettle ctl ui_geometry` reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Hovered => "hovered",
            Self::Pressed => "pressed",
        }
    }
}

/// The chord the jump button shows: the one bound to `scroll_to_bottom`, as
/// menus spell it. `None` when the action is unbound.
pub fn scrollback_pill_hint(bindings: &kettle_config::keybinds::Bindings) -> Option<String> {
    kettle_config::keybinds::ui_hint_label(
        bindings,
        &kettle_config::keybinds::Action::ScrollToBottom,
    )
}

/// A chord longer than this is not shown: it would crowd the count out.
const MAX_HINT_COLUMNS: usize = 32;

/// A line count as the button and the label show it: `1,234`.
fn grouped_count(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The room the button and the label leave on the pane's right for its
/// scrollbar: the bar's hit strip and 2 logical px, at least 14.
fn scrollback_gutter(scrollbar_width: f32, scale: f32) -> f32 {
    (scrollbar_hit_width(scrollbar_width, scale) + 2.0 * scale).max(14.0 * scale)
}

fn pane_text_fits(terminal: Rect4, scale: f32, (cell_width, line_height): (f32, f32)) -> bool {
    let (x, y, width, height) = terminal;
    [x, y, width, height, scale, cell_width, line_height]
        .into_iter()
        .all(f32::is_finite)
        && scale > 0.0
        && cell_width > 0.0
        && line_height > 0.0
}

/// Shared paint, pointer and accessibility geometry. Keep clear of the scrollbar.
///
/// `hint` is the chord bound to "scroll to bottom"
/// ([`scrollback_pill_hint`]). It follows the count behind a rule where the
/// pane has room for both; in a narrower pane the count shows alone.
pub fn scrollback_pill(
    terminal: Rect4,
    offset: usize,
    scale: f32,
    text_cell: (f32, f32),
    scrollbar_width: f32,
    hint: Option<&str>,
    tr: &Translator,
) -> Option<ScrollbackPill> {
    let (x, y, width, height) = terminal;
    let (cell_width, line_height) = text_cell;
    if offset == 0 || !pane_text_fits(terminal, scale, text_cell) {
        return None;
    }
    let grouped = grouped_count(offset);
    let label = tr.scrollback_return(&grouped, offset as u64);
    let pad = 6.0 * scale;
    let gutter = scrollback_gutter(scrollbar_width, scale);
    let label_w = display_width(&label) as f32 * cell_width;
    let h = line_height + pad;
    let fits = |w: f32| width >= w + gutter + pad;
    let w = label_w + 2.0 * pad;
    // Tiny panes cannot display the button without hiding its label.
    if !fits(w) || height < h + 2.0 * pad {
        return None;
    }
    let top = y + height - pad - h;
    let rule = scale.round().max(1.0);
    let hint = hint
        .map(str::trim)
        .filter(|text| !text.is_empty() && display_width(text) <= MAX_HINT_COLUMNS)
        .map(|text| (text, display_width(text) as f32 * cell_width))
        // The count always wins: the chord shows only where both fit.
        .filter(|(_, hint_w)| fits(w + rule + hint_w + 2.0 * pad));
    let w = hint.map_or(w, |(_, hint_w)| w + rule + hint_w + 2.0 * pad);
    let left = x + width - gutter - w;
    let hint = hint.map(|(text, _)| {
        let rule_x = (left + 2.0 * pad + label_w).round();
        ScrollbackPillHint {
            text: text.to_owned(),
            text_origin: (rule_x + rule + pad, top + pad / 2.0),
            divider: (rule_x, top + pad / 2.0, rule, h - pad),
        }
    });
    Some(ScrollbackPill {
        rect: (left, top, w, h),
        label,
        accessible_label: tr.scrollback_a11y(&grouped, offset as u64),
        text_origin: (left + pad, top + pad / 2.0),
        hint,
    })
}

/// Where a pane's view sits in its history: lines scrolled back, lines of
/// history the pane holds, and how many of its oldest lines are gone for
/// good (`Grid::history_origin`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HistoryView {
    pub offset: usize,
    pub size: usize,
    pub origin: u64,
}

impl HistoryView {
    /// The view `term` shows, read under its lock.
    pub fn of<L: alacritty_terminal::event::EventListener>(
        term: &alacritty_terminal::Term<L>,
    ) -> Self {
        use alacritty_terminal::grid::Dimensions as _;
        let grid = term.grid();
        Self {
            offset: grid.display_offset(),
            size: grid.history_size(),
            origin: grid.history_origin(),
        }
    }

    /// Whether the view shows the oldest line the pane still holds and older
    /// lines are gone: dropped at the scrollback limit, or cleared. A pane
    /// with no history is at its live prompt, so it never is, whatever was
    /// cleared before.
    pub fn at_start_of_kept_history(self) -> bool {
        self.size > 0 && self.offset == self.size && self.origin > 0
    }
}

/// The label at the top of a pane's history when older lines are gone.
#[derive(Clone, Debug, PartialEq)]
pub struct KeptHistoryLabel {
    pub rect: Rect4,
    pub label: String,
    pub text_origin: (f32, f32),
    /// Whether the label had room to say how many lines the pane holds.
    pub counted: bool,
}

/// The "Start of kept history" label for a pane viewing `view`: top right,
/// sized and kept clear of the scrollbar as the jump button is. It says how
/// many lines the pane holds where there is room, and stands alone in a
/// narrower pane. A pane too small for it and the jump button below it shows
/// only the button.
pub fn kept_history_label(
    terminal: Rect4,
    view: HistoryView,
    scale: f32,
    text_cell: (f32, f32),
    scrollbar_width: f32,
    tr: &Translator,
) -> Option<KeptHistoryLabel> {
    let (x, y, width, height) = terminal;
    let (cell_width, line_height) = text_cell;
    if !view.at_start_of_kept_history() || !pane_text_fits(terminal, scale, text_cell) {
        return None;
    }
    let pad = 6.0 * scale;
    let gutter = scrollback_gutter(scrollbar_width, scale);
    let h = line_height + pad;
    // The jump button keeps the bottom; the label needs its own row above
    // it, a pad apart.
    if height < 2.0 * h + 3.0 * pad {
        return None;
    }
    let width_of = |label: &str| display_width(label) as f32 * cell_width + 2.0 * pad;
    let fits = |label: &String| width >= width_of(label) + gutter + pad;
    let (counted, label) = [
        (
            true,
            tr.scrollback_kept_lines(&grouped_count(view.size), view.size as u64),
        ),
        (false, tr.text(kettle_i18n::Text::ScrollbackKept).to_owned()),
    ]
    .into_iter()
    .find(|(_, label)| fits(label))?;
    let w = width_of(&label);
    let left = x + width - gutter - w;
    let top = y + pad;
    Some(KeptHistoryLabel {
        rect: (left, top, w, h),
        label,
        text_origin: (left + pad, top + pad / 2.0),
        counted,
    })
}

/// The jump button's label ink on `fill`: the theme foreground, moved until
/// its painted stem pixel clears `TEXT_CONTRAST`.
fn pill_ink(theme: &kettle_config::Theme, fill: Rgb, linear: bool) -> Rgb {
    crate::color::with_min_contrast_at_coverage(
        theme.foreground,
        fill,
        crate::chrome::TEXT_CONTRAST,
        crate::color::STEM_COVERAGE,
        linear,
    )
}

/// Whether some ink clears the text floor on `fill` at its stem pixel.
fn pill_ink_clears(theme: &kettle_config::Theme, fill: Rgb, linear: bool) -> bool {
    use crate::color;
    let stem = color::over(
        fill,
        pill_ink(theme, fill, linear),
        color::STEM_COVERAGE,
        linear,
    );
    color::contrast_ratio(stem, fill) >= crate::chrome::TEXT_CONTRAST
}

/// The pill's fill, 3:1 off the terminal, and its label's ink. The label is
/// held to `TEXT_CONTRAST` at the painted stem pixel, not only at full ink, so
/// it does not pass by a rounding step. Where no ink can do that on the 3:1
/// fill, the fill steps back toward the terminal until one can: the label
/// names the button, so it outranks the fill's edge.
pub(crate) fn scrollback_pill_colors(
    theme: &kettle_config::Theme,
    linear: bool,
) -> (kettle_config::Rgb, kettle_config::Rgb) {
    use crate::color;
    let clears = |fill: kettle_config::Rgb| pill_ink_clears(theme, fill, linear);
    let raised = color::with_min_contrast(theme.palette[0], theme.background, 3.0);
    let toward_terminal = |t: f64| color::over(raised, theme.background, t, false);
    let mut fill = raised;
    if !clears(fill) {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..14 {
            let mid = (lo + hi) / 2.0;
            if clears(toward_terminal(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        fill = toward_terminal(hi);
    }
    (fill, pill_ink(theme, fill, linear))
}

/// One state of the jump button: its fill, the edge drawn around it and
/// between its count and its key, and the ink of its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PillSurface {
    pub(crate) fill: Rgb,
    pub(crate) edge: Rgb,
    pub(crate) ink: Rgb,
}

/// The jump button at rest, under the pointer and held down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollbackPillColors {
    pub(crate) rest: PillSurface,
    pub(crate) hover: PillSurface,
    pub(crate) pressed: PillSurface,
}

impl ScrollbackPillColors {
    pub(crate) fn surface(&self, state: ScrollbackPillState) -> PillSurface {
        match state {
            ScrollbackPillState::Rest => self.rest,
            ScrollbackPillState::Hovered => self.hover,
            ScrollbackPillState::Pressed => self.pressed,
        }
    }
}

/// The edge of the button in every state, against the terminal around it:
/// the WCAG 2.2 1.4.11 floor for a control's boundary.
pub(crate) const PILL_EDGE_CONTRAST: f64 = crate::chrome::ICON_CONTRAST;
/// The edge against the button's own fill, so the outline shows.
pub(crate) const PILL_EDGE_ON_FILL: f64 = 1.3;
/// A hovered or pressed fill differs from the one before it by at least
/// this, so the change of state shows.
pub(crate) const PILL_STATE_STEP: f64 = 1.15;

/// The jump button's colours. At rest it keeps the pill's fill and ink
/// ([`scrollback_pill_colors`]) and gains an edge: the fill moved a third of
/// the way to the ink, then as far as it takes to be 3:1 against the
/// terminal and 1.3:1 against the fill. On a theme where the fill had to
/// step back for its label, the edge still bounds the button.
///
/// Under the pointer the fill takes one step away from the terminal's
/// colour and the edge takes the ink's; held down, the fill takes a second
/// step. A step is 1.15:1, and is taken only where the label still clears
/// the text floor on the new fill without changing from light to dark.
/// Where the label leaves no room that way, the step goes toward the
/// terminal instead: on TokyoNight Night the fill lifts under the pointer
/// and sinks when pressed. In every state the label clears `TEXT_CONTRAST`
/// at its stem pixel and the edge is 3:1 against the terminal, whatever the
/// fill.
pub(crate) fn scrollback_button_colors(
    theme: &kettle_config::Theme,
    linear: bool,
) -> ScrollbackPillColors {
    use crate::color::{self, contrast_ratio, relative_luminance};
    let bg = theme.background;
    let (fill, _) = scrollback_pill_colors(theme, linear);
    let far = if relative_luminance(fill) >= relative_luminance(bg) {
        Rgb::new(255, 255, 255)
    } else {
        Rgb::new(0, 0, 0)
    };
    let lighter_ink =
        |fill: Rgb| relative_luminance(pill_ink(theme, fill, linear)) > relative_luminance(fill);
    // One visible step from `from` toward `end`, if the label clears it.
    let step = |from: Rgb, end: Rgb| {
        let shade = |t: f64| color::over(from, end, t, false);
        let shows = |c: Rgb| contrast_ratio(c, from) >= PILL_STATE_STEP;
        if !shows(end) {
            return None;
        }
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..14 {
            let mid = (lo + hi) / 2.0;
            if shows(shade(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        // The label keeps its floor and its side of the fill: light text
        // does not turn dark as the button is pressed.
        Some(shade(hi))
            .filter(|c| pill_ink_clears(theme, *c, linear) && lighter_ink(*c) == lighter_ink(fill))
    };
    let (hover_fill, pressed_fill) = match step(fill, far) {
        Some(hover) => (
            hover,
            step(hover, far).or_else(|| step(fill, bg)).unwrap_or(hover),
        ),
        None => match step(fill, bg) {
            Some(hover) => (hover, step(hover, bg).unwrap_or(hover)),
            None => (fill, fill),
        },
    };
    let surface = |fill: Rgb, strong: bool| {
        let ink = pill_ink(theme, fill, linear);
        let start = if strong {
            ink
        } else {
            color::over(fill, ink, 0.35, false)
        };
        PillSurface {
            fill,
            edge: pill_edge(start, bg, fill),
            ink,
        }
    };
    ScrollbackPillColors {
        rest: surface(fill, false),
        hover: surface(hover_fill, true),
        pressed: surface(pressed_fill, true),
    }
}

/// `start` moved toward white or black, whichever is nearer, until it is
/// [`PILL_EDGE_CONTRAST`] against the terminal and [`PILL_EDGE_ON_FILL`]
/// against the button's fill. Where no grey of it is both, the terminal's
/// floor is the one kept.
fn pill_edge(start: Rgb, bg: Rgb, fill: Rgb) -> Rgb {
    use crate::color::{self, contrast_ratio};
    let passes = |c: Rgb| {
        contrast_ratio(c, bg) >= PILL_EDGE_CONTRAST && contrast_ratio(c, fill) >= PILL_EDGE_ON_FILL
    };
    const STEPS: u32 = 64;
    let toward = |end: Rgb| {
        (0..=STEPS)
            .map(|step| {
                (
                    step,
                    color::over(start, end, f64::from(step) / f64::from(STEPS), false),
                )
            })
            .find(|(_, c)| passes(*c))
    };
    [toward(Rgb::new(255, 255, 255)), toward(Rgb::new(0, 0, 0))]
        .into_iter()
        .flatten()
        .min_by_key(|(step, _)| *step)
        .map_or_else(
            || color::with_min_contrast(start, bg, PILL_EDGE_CONTRAST),
            |(_, c)| c,
        )
}

/// The corner radius of the jump button, in logical px: a button's, where
/// the kept-history label keeps the pill's round ends.
const PILL_BUTTON_RADIUS: f32 = 6.0;

/// The jump button's quads: its edge, its fill inside it, and the rule
/// before its key hint.
pub(crate) fn scrollback_pill_quads(
    quads: &mut Vec<crate::QuadInstance>,
    pill: &ScrollbackPill,
    surface: PillSurface,
    scale: f32,
) {
    let (x, y, w, h) = pill.rect;
    let border = scale.round().max(1.0);
    let radius = PILL_BUTTON_RADIUS * scale;
    crate::tab_strip::rounded_fill(
        quads,
        (x, y, w, h),
        radius,
        (true, true),
        (surface.edge, 1.0),
        None,
    );
    crate::tab_strip::rounded_fill(
        quads,
        (x + border, y + border, w - 2.0 * border, h - 2.0 * border),
        (radius - border).max(0.0),
        (true, true),
        (surface.fill, 1.0),
        None,
    );
    if let Some(hint) = &pill.hint {
        let (dx, dy, dw, dh) = hint.divider;
        quads.push(crate::rect(dx, dy, dw, dh, surface.edge, 1.0));
    }
}

/// The kept-history label's quads: the pill's fill with round ends and no
/// edge, as it is a label and not a control.
pub(crate) fn kept_history_label_quads(
    quads: &mut Vec<crate::QuadInstance>,
    label: &KeptHistoryLabel,
    fill: Rgb,
) {
    crate::tab_strip::rounded_fill(
        quads,
        label.rect,
        label.rect.3 / 2.0,
        (true, true),
        (fill, 1.0),
        None,
    );
}

pub fn scrollbar_draw_width(configured: f32, scale: f32, hovered: bool) -> f32 {
    let width = configured.clamp(2.0, 40.0);
    (if hovered { width.max(10.0) } else { width }) * scale
}

pub fn scrollbar_hit_width(configured: f32, scale: f32) -> f32 {
    (configured.clamp(2.0, 40.0) + 2.0).max(12.0) * scale
}

/// How a pane's scrollbar looks: dim at rest, brighter while the pane is
/// scrolled back or the pointer hovers or drags the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollbarState {
    Rest,
    Active,
}

/// One pane's scrollbar in physical pixels. The painter, the pointer and the
/// AccessKit ScrollBar node all read it, so the bar that is drawn is the bar
/// that is grabbed and announced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneScrollbar {
    /// The bar's column, the terminal's full height, 2 logical px in from
    /// its right side: the channel paints here, command ticks lie across it,
    /// and it bounds the ScrollBar node.
    pub track_rect: Rect4,
    /// Whether the channel paints in `track_rect`.
    pub channel: bool,
    /// The thumb, when there is history to scroll.
    pub thumb_rect: Option<Rect4>,
    /// Which colours the thumb and channel take.
    pub state: ScrollbarState,
    /// The pointer's strip. It runs to the terminal's right edge, so a fling
    /// to the edge of the screen still lands, and is at least 12 logical px
    /// wide whatever the bar's width.
    pub hit_rect: Rect4,
    /// Paint solid colours rather than translucent ones.
    pub opaque: bool,
}

/// The pointer's strip along a terminal's right edge.
/// In a pane narrower than the strip, it is the whole pane.
pub fn scrollbar_hit_rect(terminal: Rect4, scale: f32, configured: f32) -> Rect4 {
    let (x, y, w, h) = terminal;
    if !pane_has_room(terminal, scale) {
        return (x, y, 0.0, 0.0);
    }
    let width = scrollbar_hit_width(configured, scale).min(w);
    (x + w - width, y, width, h)
}

/// Whether a terminal rect can hold a scrollbar at all: finite, with a
/// width, a height and a scale.
fn pane_has_room((x, y, w, h): Rect4, scale: f32) -> bool {
    [x, y, w, h, scale].into_iter().all(f32::is_finite) && w > 0.0 && h > 0.0 && scale > 0.0
}

/// A pane's scrollbar. `terminal` is the pane's terminal rect, `rows`,
/// `hist` and `off` its visible lines, history and lines scrolled back, and
/// `interacting` whether the pointer hovers or drags its bar.
///
/// With history, `auto` draws a faint full-height channel under a dim thumb
/// at rest. Scrolled back, hovered or dragged, the bar widens to at least 10
/// logical px and brightens, its channel as wide as the thumb. `always` keeps
/// one width while there is history, so hovering never makes it jump, and
/// with none draws the empty channel at the configured width, which hovering
/// leaves alone: there is nothing to grab.
///
/// Every rect stays inside `terminal`. In a pane narrower than the bar and
/// its edge, the bar narrows and the edge goes first, so nothing spills into
/// the pane beside it; a pane with no width or height draws no bar.
pub fn pane_scrollbar_geometry(
    terminal: Rect4,
    scale: f32,
    cfg: &kettle_config::Config,
    rows: usize,
    hist: usize,
    off: usize,
    interacting: bool,
) -> PaneScrollbar {
    use kettle_config::ScrollbarMode;
    let (x, y, w, h) = terminal;
    let mode = cfg.scrollbar;
    let scrollable = hist > 0;
    let room = pane_has_room(terminal, scale);
    let shown = room
        && match mode {
            ScrollbarMode::Never => false,
            ScrollbarMode::Auto => scrollable,
            ScrollbarMode::Always => true,
        };
    let active = scrollable && (off > 0 || interacting);
    let wide = match mode {
        // No bar: command ticks keep the resting column.
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => active,
        ScrollbarMode::Always => scrollable,
    };
    let bar_w = if room {
        scrollbar_draw_width(cfg.scrollbar_width, scale, wide).min(w)
    } else {
        0.0
    };
    let edge = 2.0 * scale;
    let bx = if room {
        (x + w - bar_w - edge).max(x)
    } else {
        x
    };
    let thumb_rect = if shown {
        kettle_core::scrollbar::thumb_with_min(
            rows,
            hist,
            off,
            h,
            kettle_core::scrollbar::MIN_THUMB_LOGICAL * scale,
        )
        .map(|(top, height)| (bx, y + top, bar_w, height))
    } else {
        None
    };
    PaneScrollbar {
        track_rect: (bx, y, bar_w, if room { h } else { 0.0 }),
        channel: shown,
        thumb_rect,
        state: if active {
            ScrollbarState::Active
        } else {
            ScrollbarState::Rest
        },
        hit_rect: scrollbar_hit_rect(terminal, scale, cfg.scrollbar_width),
        opaque: scrollbar_paints_opaque(cfg, scale),
    }
}

/// Whether the bar paints solid colours. Only where it cannot cover text,
/// at its widest and with its edge inside the `padding-x` the grid keeps
/// clear on its right, and only in an opaque window, where a solid bar does
/// not stand out as the one thing the desktop cannot show through. Elsewhere
/// it paints translucent layers that show as the same colours over the
/// background, so a glyph under it stays visible.
pub fn scrollbar_paints_opaque(cfg: &kettle_config::Config, scale: f32) -> bool {
    crate::theme::composed_bg_alpha(cfg) >= 1.0
        && scrollbar_draw_width(cfg.scrollbar_width, scale, true) + 2.0 * scale <= cfg.padding_x
}

/// The rest thumb against the background: the WCAG 2.2 1.4.11 floor for a
/// control.
pub(crate) const REST_THUMB_CONTRAST: f64 = crate::chrome::ICON_CONTRAST;
/// The thumb while scrolled back, hovered or dragged: the text floor.
pub(crate) const ACTIVE_THUMB_CONTRAST: f64 = crate::chrome::TEXT_CONTRAST;
/// The channel against the background: there, but quieter than the thumb.
pub(crate) const TRACK_CONTRAST: f64 = 1.5;
/// The channel at rest goes no further than this, so a pane with history
/// keeps a quiet edge.
pub(crate) const REST_TRACK_MAX_CONTRAST: f64 = 1.6;
/// The rest thumb against the channel it runs in.
pub(crate) const REST_THUMB_ON_TRACK: f64 = 2.0;
/// The active thumb against its channel.
pub(crate) const ACTIVE_THUMB_ON_TRACK: f64 = crate::chrome::ICON_CONTRAST;
/// A command tick against what it lies on: the background and the channel,
/// or the thumb.
pub(crate) const TICK_CONTRAST: f64 = crate::chrome::ICON_CONTRAST;
/// Each colour is resolved this much past its floors, so the 8-bit
/// rounding of a translucent layer cannot take it under one.
const ROUNDING_MARGIN: f64 = 1.02;

/// The scrollbar's colours as they show over the terminal background, and
/// the translucent ink and alpha that show as each one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollbarColors {
    pub(crate) rest: Rgb,
    pub(crate) active: Rgb,
    pub(crate) rest_track: Rgb,
    pub(crate) active_track: Rgb,
    /// `[rest, active, rest_track, active_track]` as translucent layers.
    pub(crate) layers: [(Rgb, f32); 4],
    /// Command ticks as they show, by the bar's state and by what they lie
    /// on: `ticks[state][surface] = (success, failure)`.
    pub(crate) ticks: [[(Rgb, Rgb); 2]; 2],
    /// The same as translucent layers over what each lies on:
    /// `tick_layers[state][surface] = [success, failure]`.
    pub(crate) tick_layers: [[[(Rgb, f32); 2]; 2]; 2],
}

/// What part of a drawn bar a command tick lies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickSurface {
    Channel,
    Thumb,
}

/// The parts of the tick at `rect` that lie on the channel and on `thumb`,
/// top to bottom: a tick that crosses an edge of the thumb is cut there, so
/// each part takes the colour that stands out against what it lies on.
pub(crate) fn tick_pieces(
    rect: [f32; 4],
    thumb: Option<Rect4>,
) -> impl Iterator<Item = (Rect4, TickSurface)> {
    let [x, y, w, h] = rect;
    let bottom = y + h;
    let (top_of_thumb, end_of_thumb) =
        thumb.map_or((bottom, bottom), |(_, ty, _, th)| (ty, ty + th));
    [
        (y, bottom.min(top_of_thumb), TickSurface::Channel),
        (
            y.max(top_of_thumb),
            bottom.min(end_of_thumb),
            TickSurface::Thumb,
        ),
        (y.max(end_of_thumb), bottom, TickSurface::Channel),
    ]
    .into_iter()
    .filter(|(from, to, _)| to > from)
    .map(move |(from, to, surface)| ((x, from, w, to - from), surface))
}

impl ScrollbarColors {
    /// The thumb's ink and alpha in `state`.
    pub(crate) fn thumb(&self, state: ScrollbarState, opaque: bool) -> (Rgb, f32) {
        match state {
            ScrollbarState::Rest => self.paint(self.rest, 0, opaque),
            ScrollbarState::Active => self.paint(self.active, 1, opaque),
        }
    }

    /// The channel's ink and alpha in `state`.
    pub(crate) fn track(&self, state: ScrollbarState, opaque: bool) -> (Rgb, f32) {
        match state {
            ScrollbarState::Rest => self.paint(self.rest_track, 2, opaque),
            ScrollbarState::Active => self.paint(self.active_track, 3, opaque),
        }
    }

    fn paint(&self, shown: Rgb, layer: usize, opaque: bool) -> (Rgb, f32) {
        if opaque {
            (shown, 1.0)
        } else {
            self.layers[layer]
        }
    }

    /// A command tick's colour as it shows: success or failure, on the
    /// channel or the thumb of a bar in `state`.
    pub(crate) fn tick(&self, state: ScrollbarState, on: TickSurface, success: bool) -> Rgb {
        let (ok, failed) = self.ticks[state as usize][on as usize];
        if success { ok } else { failed }
    }

    /// A command tick's ink and alpha: painted like the bar it crosses,
    /// solid where the bar is solid, and otherwise a layer of at most
    /// [`MAX_LAYER_ALPHA`] that shows as [`Self::tick`] over what it lies
    /// on, so a glyph under the bar still shows through the tick.
    pub(crate) fn tick_paint(
        &self,
        state: ScrollbarState,
        on: TickSurface,
        success: bool,
        opaque: bool,
    ) -> (Rgb, f32) {
        if opaque {
            (self.tick(state, on, success), 1.0)
        } else {
            self.tick_layers[state as usize][on as usize][usize::from(!success)]
        }
    }
}

/// The scrollbar's colours. Each starts from the theme foreground over its
/// background, as the bar was drawn before (0.42 for the rest thumb, 0.82
/// active, 0.07 and 0.14 for the channel), and moves only as far as its
/// floors need: the rest thumb 3:1 against the background and 2:1 against
/// the rest channel, the active thumb 4.5:1 and 3:1 against its channel, and
/// the channel 1.5:1, at rest no more than 1.6:1.
pub(crate) fn scrollbar_colors(theme: &kettle_config::Theme, linear: bool) -> ScrollbarColors {
    use crate::color::{self, with_min_contrast};
    let bg = theme.background;
    let mix = |alpha: f64| color::over(bg, theme.foreground, alpha, linear);
    let floor = |ratio: f64| ratio * ROUNDING_MARGIN;
    let rest_track = quiet_channel(mix(0.07), bg, linear);
    let active_track = with_min_contrast(mix(0.14), bg, floor(TRACK_CONTRAST));
    let thumb = |start: Rgb, on_bg: f64, track: Rgb, on_track: f64| {
        let raised = with_min_contrast(start, bg, floor(on_bg));
        let thumb = with_min_contrast(raised, track, floor(on_track));
        if color::contrast_ratio(thumb, bg) >= floor(on_bg) {
            thumb
        } else {
            color::with_min_contrast_on_both(raised, bg, track, floor(on_bg.max(on_track)), false)
        }
    };
    let rest = thumb(
        mix(0.42),
        REST_THUMB_CONTRAST,
        rest_track,
        REST_THUMB_ON_TRACK,
    );
    let active = thumb(
        mix(0.82),
        ACTIVE_THUMB_CONTRAST,
        active_track,
        ACTIVE_THUMB_ON_TRACK,
    );
    // A tick keeps its green or red: on the channel it stands out against
    // both the channel and the background beside it; on the thumb, against
    // the thumb alone, since a light thumb on a dark background can leave no
    // colour 3:1 from both.
    let on_channel = |ink: Rgb, track: Rgb| tick_shade(ink, track, Some(bg), linear);
    let on_thumb = |ink: Rgb, thumb: Rgb| tick_shade(ink, thumb, None, linear);
    let pair =
        |resolve: &dyn Fn(Rgb) -> Rgb| (resolve(theme.palette[2]), resolve(theme.palette[1]));
    let ticks = [(rest_track, rest), (active_track, active)].map(|(track, thumb)| {
        [
            pair(&|ink| on_channel(ink, track)),
            pair(&|ink| on_thumb(ink, thumb)),
        ]
    });
    let surfaces = [[rest_track, rest], [active_track, active]];
    let tick_layers: [[[(Rgb, f32); 2]; 2]; 2] = std::array::from_fn(|state| {
        std::array::from_fn(|on| {
            let (ok, failed) = ticks[state][on];
            let under = surfaces[state][on];
            [ok, failed].map(|shown| translucent_layer(under, shown, linear))
        })
    });
    let layer = |shown: Rgb| translucent_layer(bg, shown, linear);
    ScrollbarColors {
        rest,
        active,
        rest_track,
        active_track,
        layers: [
            layer(rest),
            layer(active),
            layer(rest_track),
            layer(active_track),
        ],
        ticks,
        tick_layers,
    }
}

/// A tick's colour on `under`, the channel or the thumb: `ink` in the same
/// hue, lighter or darker until it stands out against `under` and, on the
/// channel, against the background `beside` it too. Of the shades that do,
/// the one a translucent layer reaches over `under` with the least cover, so
/// a glyph under the bar shows through the tick as well as it can. Where
/// neither shade does, the best the hue can do.
fn tick_shade(ink: Rgb, under: Rgb, beside: Option<Rgb>, linear: bool) -> Rgb {
    use crate::color::{self, contrast_ratio};
    let floor = TICK_CONTRAST * ROUNDING_MARGIN;
    let ratio = |c: Rgb| {
        beside.map_or(contrast_ratio(c, under), |bg| {
            contrast_ratio(c, under).min(contrast_ratio(c, bg))
        })
    };
    let (hue, saturation, lightness) = color::hsl(ink);
    let shade =
        |end: f64, t: f64| color::from_hsl(hue, saturation, lightness + (end - lightness) * t);
    let reach = |end: f64| {
        if ratio(ink) >= floor {
            return Some(ink);
        }
        if ratio(shade(end, 1.0)) < floor {
            return None;
        }
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..14 {
            let mid = (lo + hi) / 2.0;
            if ratio(shade(end, mid)) >= floor {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Some(shade(end, hi))
    };
    let cover = |shown: Rgb| translucent_layer(under, shown, linear).1;
    [reach(1.0), reach(0.0)]
        .into_iter()
        .flatten()
        .min_by(|a, b| cover(*a).total_cmp(&cover(*b)))
        .unwrap_or_else(|| {
            color::with_min_contrast_on_both(ink, under, beside.unwrap_or(under), floor, true)
        })
}

/// The rest channel: the faint 0.07 mix raised to [`TRACK_CONTRAST`], or, on
/// a theme where that mix is already stronger than
/// [`REST_TRACK_MAX_CONTRAST`], eased back toward the background until it is
/// not.
fn quiet_channel(start: Rgb, bg: Rgb, linear: bool) -> Rgb {
    use crate::color::{self, contrast_ratio};
    let low = TRACK_CONTRAST * ROUNDING_MARGIN;
    if contrast_ratio(start, bg) < low {
        return color::with_min_contrast(start, bg, low);
    }
    if contrast_ratio(start, bg) <= REST_TRACK_MAX_CONTRAST / ROUNDING_MARGIN {
        return start;
    }
    // The least of `start` over the background that still clears the floor.
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..14 {
        let mid = (lo + hi) / 2.0;
        if contrast_ratio(color::over(bg, start, mid, linear), bg) >= low {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    color::over(bg, start, hi, linear)
}

/// The most a translucent layer covers: a glyph under the bar keeps at least
/// a tenth of its ink, so the rightmost column is never hidden.
pub(crate) const MAX_LAYER_ALPHA: f32 = 0.9;

/// The ink and alpha that show as `shown` painted over `bg`.
///
/// The ink lies on the line from `bg` through `shown`, as far out as colour
/// goes, at the lowest alpha that reaches `shown`, so the layer shows as
/// `shown` itself and lets as much of a glyph under it through as it can. The
/// ink rounds away from `bg`, so the layer never shows weaker than `shown`.
/// Where that needs more than [`MAX_LAYER_ALPHA`], white or black reaches the
/// same contrast against `bg` with less, giving up some of the tint; where
/// even that needs more, the layer stops at [`MAX_LAYER_ALPHA`] and falls
/// short of `shown`.
pub(crate) fn translucent_layer(bg: Rgb, shown: Rgb, linear: bool) -> (Rgb, f32) {
    use crate::color::{self, contrast_ratio, encode_srgb, relative_luminance, srgb_to_linear};
    let decode = |value: u8| {
        if linear {
            srgb_to_linear(value)
        } else {
            f64::from(value) / 255.0
        }
    };
    let channels = |c: Rgb| [c.r, c.g, c.b].map(decode);
    let (base, top) = (channels(bg), channels(shown));
    let alpha = base
        .iter()
        .zip(&top)
        .map(|(&b, &t)| {
            let room = if t > b { 1.0 - b } else { b };
            if room > 0.0 {
                (t - b).abs() / room
            } else {
                0.0
            }
        })
        .fold(0.0_f64, f64::max);
    if alpha <= 0.0 {
        return (shown, 0.0);
    }
    let ink = |b: f64, t: f64| {
        let value = (b + (t - b) / alpha).clamp(0.0, 1.0);
        let encoded = if linear {
            encode_srgb(value)
        } else {
            value * 255.0
        };
        // Round away from the background.
        let out = if t >= b {
            encoded.ceil()
        } else {
            encoded.floor()
        };
        out.clamp(0.0, 255.0) as u8
    };
    let tinted = Rgb::new(
        ink(base[0], top[0]),
        ink(base[1], top[1]),
        ink(base[2], top[2]),
    );
    if alpha <= f64::from(MAX_LAYER_ALPHA) {
        return (tinted, alpha as f32);
    }
    let plain = if relative_luminance(shown) > relative_luminance(bg) {
        Rgb::new(255, 255, 255)
    } else {
        Rgb::new(0, 0, 0)
    };
    let target = contrast_ratio(shown, bg);
    let reaches = |a: f64| contrast_ratio(color::over(bg, plain, a, linear), bg) >= target;
    if !reaches(f64::from(MAX_LAYER_ALPHA)) {
        return (plain, MAX_LAYER_ALPHA);
    }
    let (mut lo, mut hi) = (0.0_f64, f64::from(MAX_LAYER_ALPHA));
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if reaches(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (plain, hi as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_follows_pane_and_disappears_at_bottom() {
        let tr = Translator::default();
        assert!(
            scrollback_pill(
                (50.0, 80.0, 800.0, 600.0),
                0,
                2.0,
                (16.0, 32.0),
                4.0,
                None,
                &tr
            )
            .is_none()
        );
        let pill = scrollback_pill(
            (50.0, 80.0, 800.0, 600.0),
            1234,
            2.0,
            (16.0, 32.0),
            4.0,
            None,
            &tr,
        )
        .unwrap();
        assert_eq!(pill.label, "\u{2193} 1,234 lines");
        assert_eq!(pill.rect.0 + pill.rect.2, 822.0);
        assert_eq!(pill.rect.1 + pill.rect.3, 668.0);
        let es = Translator::new(kettle_i18n::Language::Es);
        assert!(
            scrollback_pill(
                (0.0, 0.0, 800.0, 600.0),
                1,
                1.0,
                (8.0, 16.0),
                4.0,
                None,
                &es
            )
            .unwrap()
            .label
            .ends_with("l\u{ed}nea")
        );
        assert!(
            scrollback_pill((0.0, 0.0, 1.0, 1.0), 10, 1.0, (8.0, 16.0), 4.0, None, &tr).is_none()
        );
    }

    #[test]
    fn pill_leaves_room_for_a_wide_scrollbar_and_scaled_hit_target() {
        let pill = scrollback_pill(
            (0.0, 0.0, 800.0, 600.0),
            1234,
            2.0,
            (16.0, 32.0),
            40.0,
            None,
            &Translator::default(),
        )
        .unwrap();
        assert!(pill.rect.0 + pill.rect.2 <= 800.0 - scrollbar_hit_width(40.0, 2.0));
        assert_eq!(scrollbar_hit_width(4.0, 2.0), 24.0);
        assert!(scrollbar_hit_width(40.0, 2.0) > scrollbar_draw_width(40.0, 2.0, true));
    }

    #[test]
    fn hovered_scrollbar_is_at_least_ten_scaled_pixels() {
        assert_eq!(scrollbar_draw_width(4.0, 2.0, false), 8.0);
        assert_eq!(scrollbar_draw_width(4.0, 2.0, true), 20.0);
        assert_eq!(scrollbar_draw_width(20.0, 2.0, true), 40.0);
    }
}
