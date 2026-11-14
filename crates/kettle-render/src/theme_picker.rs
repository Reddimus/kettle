//! The theme picker's footer, under its list of themes: a sample of the
//! highlighted theme with its contrast readout, then a status line saying how
//! Kettle chooses the theme, which theme each appearance wears, and the
//! minimum-contrast control. The UI supplies the state. This module owns the
//! words, the layout and the colours, so the painter, pointer targets and the
//! accessible text all measure the same strings.

use crate::MinimumContrastCache;
use crate::Rect4;
use crate::chrome;
use crate::color;
use kettle_config::{Rgb, Theme};
use kettle_i18n::{Text, Translator};

/// How Kettle chooses between the light and the dark theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeFollow {
    /// `theme-mode = auto`: the system's light or dark appearance.
    System,
    /// `theme-schedule`: the clock or the sun.
    Schedule,
    Light,
    Dark,
    /// `theme-mode = explicit`: one theme, whatever the appearance.
    Fixed,
}

/// The appearance whose theme a pick replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSlot {
    Dark,
    Light,
}

/// What the theme picker shows under its list. The UI fills it in from the
/// config; `height` comes from [`theme_footer_height`] for the parts that fit.
#[derive(Clone, Debug)]
pub struct ThemePickerFooter {
    pub follow: ThemeFollow,
    /// The side a pick is saved for; `None` when it is saved as `theme`.
    pub editing: Option<ThemeSlot>,
    /// Config names of the themes, translated when shown.
    pub dark_theme: String,
    pub light_theme: String,
    /// The one theme fixed mode wears.
    pub fixed_theme: String,
    /// `minimum-contrast`; at or below 1 it is off.
    pub minimum_contrast: f32,
    /// The highlighted theme, with the user's colour overrides.
    pub sample: Theme,
    /// Whether the sample card fits.
    pub show_sample: bool,
    /// Status lines that fit: 2 (mode and contrast, then the themes), 1 (the
    /// themes) or 0.
    pub status_lines: usize,
    /// Physical pixels under the picker's rows.
    pub height: f32,
    /// The text cell the footer was fitted in, for laying it out where the
    /// painter has no say, such as the accessibility tree.
    pub cell: (f32, f32),
    /// Columns the card's lines and the status lines need, from
    /// [`theme_footer_min_columns`] in the UI's words.
    pub min_columns: (usize, usize),
}

impl ThemePickerFooter {
    /// Whether the sample card, and the status lines, fit a picker `width`
    /// wide in text cells `cw` wide.
    pub fn fits(&self, width: f32, cw: f32) -> (bool, bool) {
        if !(cw > 0.0 && width.is_finite()) {
            return (false, false);
        }
        let card = ((width - 2.0 * (CARD_INSET_X + CARD_PAD.0)) / cw).floor();
        let status = ((width - 2.0 * PAD_X) / cw).floor();
        (
            card >= self.min_columns.0 as f32,
            status >= self.min_columns.1 as f32,
        )
    }
}

/// Columns the sample card's lines and the status lines need at least, in
/// `tr`'s words: every sample line and the readout whole, the contrast button
/// whole, the mode at least `FOLLOW_MIN_COLS` columns, and each theme name
/// at least four columns.
pub fn theme_footer_min_columns(footer: &ThemePickerFooter, tr: &Translator) -> (usize, usize) {
    // The widest readout any theme gives, so the card does not come and go
    // as the selection moves.
    let widest = Readability {
        text: 21.0,
        lowest: 21.0,
        lowest_slot: 15,
        lifted: 0,
    };
    let sample = sample_lines()
        .iter()
        .map(|runs| runs.iter().map(|(text, _)| columns(text)).sum::<usize>())
        .max()
        .unwrap_or(0);
    // The readout, the square and its number, and room for the mark.
    let readout = columns(&readout_text(&widest, tr)) + 3 + 2 + 3;
    let palette = (16.0 * 1.75 + 2.0) as usize;
    // The widest the button gets, so stepping it never drops the line.
    let button = MINIMUM_CONTRAST_STEPS
        .into_iter()
        .chain([footer.minimum_contrast])
        .map(|value| columns(&contrast_button_text(value, tr)))
        .max()
        .unwrap_or(0);
    let follow = columns(theme_follow_label(footer.follow, tr)).min(FOLLOW_MIN_COLS);
    let mode = follow + 3 + button + 1;
    let slots = slot_texts(footer, tr);
    let themes = slots
        .iter()
        .map(|(label, _, _)| SLOT_MARK_COLS + columns(label) + 1 + 4)
        .sum::<usize>()
        + SLOT_GAP_COLS * (slots.len() - 1);
    (sample.max(readout).max(palette), mode.max(themes))
}

/// The contrast steps the picker's button and the Settings row offer.
pub const MINIMUM_CONTRAST_STEPS: [f32; 4] = [0.0, 3.0, 4.5, 7.0];

/// The `minimum-contrast` value after `current` when stepping forward (or
/// back): the next listed step above (or below) it, wrapping around. A value
/// set by hand between two steps moves to its neighbour.
pub fn step_minimum_contrast(current: f32, forward: bool) -> f32 {
    let current = if current.is_finite() && current > 1.0 {
        current
    } else {
        0.0
    };
    let steps = MINIMUM_CONTRAST_STEPS;
    if forward {
        steps
            .into_iter()
            .find(|&step| step > current + 1e-3)
            .unwrap_or(steps[0])
    } else {
        steps
            .into_iter()
            .rev()
            .find(|&step| step < current - 1e-3)
            .unwrap_or(steps[steps.len() - 1])
    }
}

/// A contrast ratio as Kettle writes it in config: `0` when off, else the
/// shortest decimal (`3`, `4.5`, `2.25`).
pub fn minimum_contrast_token(value: f32) -> String {
    if !(value.is_finite() && value > 1.0) {
        return "0".to_string();
    }
    let rounded = (f64::from(value) * 100.0).round() / 100.0;
    let mut text = format!("{rounded:.2}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

/// A ratio as people read it, such as `4.5:1`, rounded down so a theme
/// never reads better than it is. The minimum-contrast lift reaches its
/// ratio exactly, so a lifted colour still reads as the minimum.
pub(crate) fn ratio_text(ratio: f64) -> String {
    format!("{:.1}:1", (ratio * 10.0).floor() / 10.0)
}

/// How readable a theme's text is as the terminal draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readability {
    /// Contrast of the theme's text colour on its background.
    pub text: f64,
    /// The lowest contrast of palette colours 1-7 and 9-15 on the background.
    /// Colours 0 and 8 are background tones in most themes, as
    /// `kettle-config`'s default-theme check also treats them.
    pub lowest: f64,
    /// The palette slot with the lowest contrast.
    pub lowest_slot: usize,
    /// Of the text colour and those 14, how many `minimum-contrast` lifts.
    pub lifted: usize,
}

/// Contrast a reader can still make out; below it the picker marks the theme.
pub const LOW_CONTRAST: f64 = 3.0;

/// Measure `theme` as the terminal paints plain cells with
/// `minimum_contrast`: every colour goes through the same lift as cell text.
pub fn theme_readability(theme: &Theme, minimum_contrast: f32) -> Readability {
    let mut cache = MinimumContrastCache::default();
    let bg = theme.background;
    let minimum = f64::from(minimum_contrast);
    let mut lifted = 0;
    let mut drawn = |fg: Rgb| {
        let ink = cache.resolve(fg, bg, minimum);
        if ink != fg {
            lifted += 1;
        }
        color::contrast_ratio(ink, bg)
    };
    let text = drawn(theme.foreground);
    let (mut lowest, mut lowest_slot) = (f64::INFINITY, 1);
    for slot in (1..=7).chain(9..=15) {
        let ratio = drawn(theme.palette[slot]);
        if ratio < lowest {
            (lowest, lowest_slot) = (ratio, slot);
        }
    }
    Readability {
        text,
        lowest,
        lowest_slot,
        lifted,
    }
}

/// Space between the picker's rows and the sample card, and under the card.
const CARD_GAP: f32 = 8.0;
/// The card's inset from the panel's sides, and its own padding.
const CARD_INSET_X: f32 = 12.0;
const CARD_PAD: (f32, f32) = (10.0, 8.0);
/// Lines on the card: the prompt, four listing lines, a diff line, the
/// palette and the readout.
const CARD_LINES: usize = 8;
/// Extra space between footer lines, on top of the text cell's height.
const LINE_GAP: f32 = 4.0;
/// Space above the first status line (under its divider) and below the last.
const STATUS_PAD: (f32, f32) = (6.0, 8.0);
/// Text starts this far in from the panel's sides, as the rows' labels do.
const PAD_X: f32 = 16.0;
/// On the status line, the columns before each theme's label that hold the
/// edited side's mark, and the columns between the two themes.
const SLOT_MARK_COLS: usize = 2;
const SLOT_GAP_COLS: usize = 3;
/// The mode label is cut to fit beside the contrast button, to no fewer
/// columns than this. "Contraste: Desactivado" beside "Seguir el sistema"
/// is wider than the narrowest picker (360 px at 1x, and every picker at 2x).
pub(crate) const FOLLOW_MIN_COLS: usize = 12;

fn line_pitch(cell_h: f32) -> f32 {
    cell_h + LINE_GAP
}

fn card_height(cell_h: f32) -> f32 {
    CARD_LINES as f32 * line_pitch(cell_h) + 2.0 * CARD_PAD.1
}

/// The footer's height with the sample card or not and `status_lines` lines.
pub fn theme_footer_height(cell_h: f32, show_sample: bool, status_lines: usize) -> f32 {
    let card = if show_sample {
        2.0 * CARD_GAP + card_height(cell_h)
    } else {
        0.0
    };
    let status = if status_lines > 0 {
        STATUS_PAD.0 + status_lines as f32 * line_pitch(cell_h) + STATUS_PAD.1
    } else {
        0.0
    };
    card + status
}

/// Which colour a run of footer text is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FooterInk {
    /// A palette slot of the sample theme, lifted as terminal text is.
    Sample(usize),
    /// The sample theme's own text colour, lifted as terminal text is.
    SampleText,
    /// Readout text on the card.
    CardLabel,
    /// A quiet label on the panel.
    Label,
    /// A value on the panel: theme names and the contrast setting.
    Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FooterRun {
    pub text: String,
    pub ink: FooterInk,
}

/// One line of footer text: where its first column starts and what it says.
#[derive(Clone, Debug, PartialEq)]
pub struct FooterLine {
    pub left: f32,
    pub top: f32,
    /// Text outside it is clipped.
    pub bounds: Rect4,
    pub runs: Vec<FooterRun>,
}

impl FooterLine {
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

/// The footer laid out in surface pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeFooterLayout {
    pub rect: Rect4,
    pub card: Option<Rect4>,
    /// The palette squares on the card, with their slots.
    pub squares: Vec<(Rect4, usize)>,
    /// The square of the lowest-contrast colour, after the readout.
    pub lowest_square: Option<(Rect4, usize)>,
    /// A mark after the readout when a colour is below `LOW_CONTRAST`.
    pub low_mark: Option<Rect4>,
    /// The line above the status lines.
    pub divider: Option<Rect4>,
    /// The edited appearance: a mark before its label and a line under it.
    pub slot_mark: Option<Rect4>,
    pub slot_underline: Option<Rect4>,
    /// The minimum-contrast button; a click steps it.
    pub contrast_button: Option<Rect4>,
    /// Each side's label and theme on the status line; a click edits that
    /// side. Empty for a fixed theme.
    pub slot_chips: Vec<(Rect4, ThemeSlot)>,
    pub lines: Vec<FooterLine>,
    pub readability: Readability,
}

fn columns(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// `text` cut to `max` columns, ending in an ellipsis when cut.
fn elide(text: &str, max: usize) -> String {
    if columns(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// The label for how the theme is chosen.
pub fn theme_follow_label(follow: ThemeFollow, tr: &Translator) -> &'static str {
    tr.text(match follow {
        ThemeFollow::System => Text::SettingsValueFollowSystem,
        ThemeFollow::Schedule => Text::PickerThemeFollowSchedule,
        ThemeFollow::Light => Text::SettingsValueLightAppearance,
        ThemeFollow::Dark => Text::SettingsValueDarkAppearance,
        ThemeFollow::Fixed => Text::SettingsValueFixedTheme,
    })
}

/// The contrast button's text, such as `Contrast: 4.5:1`.
pub fn contrast_button_text(minimum_contrast: f32, tr: &Translator) -> String {
    let value = if minimum_contrast.is_finite() && minimum_contrast > 1.0 {
        format!("{}:1", minimum_contrast_token(minimum_contrast))
    } else {
        tr.text(Text::SettingsValueOff).to_string()
    };
    format!("{} {value}", tr.text(Text::PickerThemeContrast))
}

/// The readout under the sample: the text's contrast, then the lowest.
fn readout_text(readability: &Readability, tr: &Translator) -> String {
    format!(
        "{} · {}",
        tr.picker_theme_text_contrast(&ratio_text(readability.text)),
        tr.picker_theme_lowest_contrast(&ratio_text(readability.lowest)),
    )
}

/// What a screen reader hears for the footer: the same strings the painter
/// draws, plus what the swatch and the mark show.
pub fn theme_footer_description(footer: &ThemePickerFooter, tr: &Translator) -> String {
    let readability = theme_readability(&footer.sample, footer.minimum_contrast);
    let mut parts = vec![readout_text(&readability, tr)];
    parts.push(tr.picker_theme_a11y_lowest_slot(readability.lowest_slot as u64));
    if readability.lowest < LOW_CONTRAST {
        parts.push(tr.text(Text::PickerThemeA11yLowContrast).to_string());
    }
    if footer.minimum_contrast > 1.0 && readability.lifted > 0 {
        parts.push(tr.picker_theme_colours_lifted(readability.lifted as u64));
    }
    parts.push(theme_follow_label(footer.follow, tr).to_string());
    parts.extend(
        slot_texts(footer, tr)
            .into_iter()
            .map(|(label, name, _)| format!("{label} {}", tr.theme_name(&name))),
    );
    parts.push(contrast_button_text(footer.minimum_contrast, tr));
    parts.join(", ")
}

/// The sides on the status line, each with what a screen reader hears for
/// it (its label and theme, as painted) and whether a pick saves to it.
/// Empty for a fixed theme, which has no sides.
pub fn theme_footer_slots(
    footer: &ThemePickerFooter,
    tr: &Translator,
) -> Vec<(ThemeSlot, String, bool)> {
    if footer.follow == ThemeFollow::Fixed {
        return Vec::new();
    }
    [ThemeSlot::Dark, ThemeSlot::Light]
        .into_iter()
        .zip(slot_texts(footer, tr))
        .map(|(slot, (label, name, editing))| {
            (slot, format!("{label} {}", tr.theme_name(&name)), editing)
        })
        .collect()
}

/// The status line's theme labels, names and whether each is being edited.
fn slot_texts(footer: &ThemePickerFooter, tr: &Translator) -> Vec<(&'static str, String, bool)> {
    if footer.follow == ThemeFollow::Fixed {
        return vec![(
            tr.text(Text::PickerThemeFixedSlot),
            footer.fixed_theme.clone(),
            false,
        )];
    }
    vec![
        (
            tr.text(Text::PickerThemeDarkSlot),
            footer.dark_theme.clone(),
            footer.editing == Some(ThemeSlot::Dark),
        ),
        (
            tr.text(Text::PickerThemeLightSlot),
            footer.light_theme.clone(),
            footer.editing == Some(ThemeSlot::Light),
        ),
    ]
}

/// The sample output: what a shell and a few tools print in the theme. It is
/// program output, so it is not translated.
fn sample_lines() -> [Vec<(&'static str, FooterInk)>; 6] {
    use FooterInk::{Sample, SampleText};
    [
        vec![
            ("user@host", Sample(2)),
            (" ", SampleText),
            ("~/src", Sample(4)),
            (" ", SampleText),
            ("(main)", Sample(3)),
            ("$ ls -l", SampleText),
        ],
        vec![("drwxr-xr-x  ", SampleText), ("docs/", Sample(4))],
        vec![("-rwxr-xr-x  ", SampleText), ("build.sh", Sample(2))],
        vec![
            ("lrwxr-xr-x  ", SampleText),
            ("latest", Sample(6)),
            (" -> ", SampleText),
            ("docs/", Sample(4)),
        ],
        vec![
            ("-rw-r--r--  ", SampleText),
            ("logo.png", Sample(5)),
            ("  ", SampleText),
            ("old.tar.gz", Sample(1)),
        ],
        vec![
            ("- removed line", Sample(1)),
            ("  ", SampleText),
            ("+ added line", Sample(2)),
        ],
    ]
}

/// Lay out `footer` under a picker whose rows end at `rows_bottom`, across
/// the panel's `x` and `width`, in text cells of `cell`.
pub fn theme_footer_layout(
    footer: &ThemePickerFooter,
    tr: &Translator,
    (x, rows_bottom, width): (f32, f32, f32),
    (cw, ch): (f32, f32),
) -> ThemeFooterLayout {
    let readability = theme_readability(&footer.sample, footer.minimum_contrast);
    let rect = (x, rows_bottom, width, footer.height);
    let pitch = line_pitch(ch);
    let mut layout = ThemeFooterLayout {
        rect,
        card: None,
        squares: Vec::new(),
        lowest_square: None,
        low_mark: None,
        divider: None,
        slot_mark: None,
        slot_underline: None,
        contrast_button: None,
        slot_chips: Vec::new(),
        lines: Vec::new(),
        readability,
    };
    if !(cw > 0.0 && ch > 0.0 && width > 2.0 * CARD_INSET_X) {
        return layout;
    }
    let mut y = rows_bottom;
    if footer.show_sample {
        let card = (
            x + CARD_INSET_X,
            y + CARD_GAP,
            width - 2.0 * CARD_INSET_X,
            card_height(ch),
        );
        layout.card = Some(card);
        let left = card.0 + CARD_PAD.0;
        let bounds = (left, card.1, (card.2 - 2.0 * CARD_PAD.0).max(0.0), card.3);
        let mut top = card.1 + CARD_PAD.1;
        let line = |top: f32, runs: Vec<FooterRun>| FooterLine {
            left,
            top: top + LINE_GAP * 0.5,
            bounds,
            runs,
        };
        for runs in sample_lines() {
            layout.lines.push(line(
                top,
                runs.into_iter()
                    .map(|(text, ink)| FooterRun {
                        text: text.to_string(),
                        ink,
                    })
                    .collect(),
            ));
            top += pitch;
        }
        // The 16 palette colours, normal then bright, as squares.
        let side = (cw * 1.3).min(ch * 0.8).round().max(2.0);
        let step = (cw * 1.75).max(side + 3.0);
        let square_top = (top + (pitch - side) * 0.5).round();
        for slot in 0..16 {
            let gap = if slot >= 8 { cw } else { 0.0 };
            let sx = (left + slot as f32 * step + gap).round();
            if sx + side <= bounds.0 + bounds.2 {
                layout.squares.push(((sx, square_top, side, side), slot));
            }
        }
        top += pitch;
        // The readout, then the lowest colour's square and number, then a
        // mark when it is hard to read.
        let readout = readout_text(&readability, tr);
        let max = (bounds.2 / cw).floor() as usize;
        let mut text = readout.clone();
        let square_col = columns(&text) + 1;
        let slot_text = format!("{}", readability.lowest_slot);
        text.push_str("   ");
        text.push_str(&slot_text);
        let mut mark_col = None;
        if readability.lowest < LOW_CONTRAST {
            mark_col = Some(columns(&text) + 1);
            text.push_str("   ");
        }
        if footer.minimum_contrast > 1.0 && readability.lifted > 0 {
            let lifted = format!(
                " · {}",
                tr.picker_theme_colours_lifted(readability.lifted as u64)
            );
            if columns(&text) + columns(&lifted) <= max {
                text.push_str(&lifted);
            }
        }
        let side = side.min(ch * 0.7).round();
        let mark_top = (top + LINE_GAP * 0.5 + (ch - side) * 0.5).round();
        if square_col + 2 <= max {
            layout.lowest_square = Some((
                (
                    (left + square_col as f32 * cw).round(),
                    mark_top,
                    side,
                    side,
                ),
                readability.lowest_slot,
            ));
        }
        if let Some(col) = mark_col.filter(|&col| col < max) {
            layout.low_mark = Some(((left + col as f32 * cw).round(), mark_top, side, side));
        }
        layout.lines.push(line(
            top,
            vec![FooterRun {
                text,
                ink: FooterInk::CardLabel,
            }],
        ));
        y = card.1 + card.3 + CARD_GAP;
    }
    if footer.status_lines == 0 {
        return layout;
    }
    layout.divider = Some((x + 1.0, y + 1.0, width - 2.0, 1.0));
    let left = x + PAD_X;
    let inner = (width - 2.0 * PAD_X).max(0.0);
    let max = (inner / cw).floor() as usize;
    let bounds = (left, y, inner, footer.height - (y - rows_bottom));
    let mut top = y + STATUS_PAD.0;
    if footer.status_lines >= 2 {
        let button = contrast_button_text(footer.minimum_contrast, tr);
        let button_cols = columns(&button);
        let follow = elide(
            theme_follow_label(footer.follow, tr),
            max.saturating_sub(button_cols + 3),
        );
        layout.lines.push(FooterLine {
            left,
            top: top + LINE_GAP * 0.5,
            bounds,
            runs: vec![FooterRun {
                text: follow,
                ink: FooterInk::Label,
            }],
        });
        if button_cols < max {
            let button_left = left + (max - button_cols) as f32 * cw;
            let pad = (cw * 0.5).round();
            layout.contrast_button = Some((
                button_left - pad,
                top + 1.0,
                button_cols as f32 * cw + 2.0 * pad,
                pitch - 2.0,
            ));
            layout.lines.push(FooterLine {
                left: button_left,
                top: top + LINE_GAP * 0.5,
                bounds,
                runs: vec![FooterRun {
                    text: button,
                    ink: FooterInk::Value,
                }],
            });
        }
        top += pitch;
    }
    // The themes: a two-column mark slot before each label, then the label
    // and the name. Names are shortened before anything else.
    let slots = slot_texts(footer, tr);
    let fixed: usize = slots
        .iter()
        .map(|(label, _, _)| SLOT_MARK_COLS + columns(label) + 1)
        .sum::<usize>()
        + SLOT_GAP_COLS * (slots.len() - 1);
    let mut names: Vec<String> = slots
        .iter()
        .map(|(_, name, _)| tr.theme_name(name).to_string())
        .collect();
    let budget = max.saturating_sub(fixed);
    while names.iter().map(|name| columns(name)).sum::<usize>() > budget {
        let Some((longest, cols)) = names
            .iter()
            .map(|name| columns(name))
            .enumerate()
            .max_by_key(|&(_, cols)| cols)
            .filter(|&(_, cols)| cols > 2)
        else {
            break;
        };
        names[longest] = elide(&names[longest], cols - 1);
    }
    let sides = if footer.follow == ThemeFollow::Fixed {
        [None, None]
    } else {
        [Some(ThemeSlot::Dark), Some(ThemeSlot::Light)]
    };
    let mut runs = Vec::new();
    let mut col = 0;
    for (index, ((label, _, editing), name)) in slots.iter().zip(&names).enumerate() {
        if index > 0 {
            runs.push(FooterRun {
                text: " ".repeat(SLOT_GAP_COLS),
                ink: FooterInk::Label,
            });
            col += SLOT_GAP_COLS;
        }
        let text_top = top + LINE_GAP * 0.5;
        if *editing {
            let side = (ch * 0.4).round().max(3.0);
            layout.slot_mark = Some((
                (left + col as f32 * cw + (cw * 1.5 - side) * 0.5).round(),
                (text_top + (ch - side) * 0.5).round(),
                side,
                side,
            ));
            let underline_cols = columns(label) + 1 + columns(name);
            layout.slot_underline = Some((
                (left + (col + SLOT_MARK_COLS) as f32 * cw).round(),
                (text_top + ch + 1.0).round(),
                underline_cols as f32 * cw,
                2.0,
            ));
        }
        if let Some(slot) = sides[index] {
            let cols = SLOT_MARK_COLS + columns(label) + 1 + columns(name);
            let chip_left = (left + col as f32 * cw).round();
            let chip_right = (left + (col + cols) as f32 * cw).min(left + inner);
            layout.slot_chips.push((
                (chip_left, top + 1.0, chip_right - chip_left, pitch - 2.0),
                slot,
            ));
        }
        runs.push(FooterRun {
            text: format!("{}{label} ", " ".repeat(SLOT_MARK_COLS)),
            ink: FooterInk::Label,
        });
        runs.push(FooterRun {
            text: name.clone(),
            ink: FooterInk::Value,
        });
        col += SLOT_MARK_COLS + columns(label) + 1 + columns(name);
    }
    layout.lines.push(FooterLine {
        left,
        top: top + LINE_GAP * 0.5,
        bounds,
        runs,
    });
    layout
}

/// The resolved colours of a footer, given the panel it sits on.
pub(crate) struct FooterColors {
    pub card_fill: Rgb,
    pub card_edge: Rgb,
    pub panel_edge: Rgb,
    pub underline: Rgb,
    pub mark: Rgb,
    pub low_mark: Rgb,
}

pub(crate) fn footer_colors(
    footer: &ThemePickerFooter,
    panel: &Theme,
    accent: Rgb,
    colors: &mut chrome::Colors,
) -> FooterColors {
    let sample = &footer.sample;
    let card_fill = sample.background;
    FooterColors {
        card_fill,
        card_edge: colors.icon(
            color::over(card_fill, sample.foreground, 0.4, false),
            card_fill,
        ),
        panel_edge: colors.icon(
            color::over(panel.background, panel.foreground, 0.4, false),
            panel.background,
        ),
        underline: colors.icon(accent, panel.background),
        mark: colors.icon(accent, panel.background),
        low_mark: colors.icon(sample.palette[1], card_fill),
    }
}

/// The colour a run is drawn in.
pub(crate) fn footer_ink(
    ink: FooterInk,
    footer: &ThemePickerFooter,
    panel: &Theme,
    colors: &mut chrome::Colors,
    cache: &mut MinimumContrastCache,
) -> Rgb {
    let sample = &footer.sample;
    let minimum = f64::from(footer.minimum_contrast);
    match ink {
        FooterInk::Sample(slot) => cache.resolve(sample.palette[slot], sample.background, minimum),
        FooterInk::SampleText => cache.resolve(sample.foreground, sample.background, minimum),
        FooterInk::CardLabel => colors.secondary(sample.foreground, sample.background),
        FooterInk::Label => colors.secondary(panel.foreground, panel.background),
        FooterInk::Value => colors.text(panel.foreground, panel.background),
    }
}

/// The most rounded surfaces a footer adds to the swatch layer.
pub(crate) const FOOTER_SURFACES: usize = 24;

/// The footer's rounded surfaces, in draw order: the card, its palette
/// squares, the lowest colour's square, the marks and the contrast button.
pub(crate) fn footer_surfaces(
    layout: &ThemeFooterLayout,
    footer: &ThemePickerFooter,
    fc: &FooterColors,
    scale: f32,
) -> Vec<crate::rounded_surface::RoundedSurface> {
    use crate::rounded_surface::RoundedSurface;
    let rgba = |c: Rgb, a: f32| {
        [
            f32::from(c.r) / 255.0,
            f32::from(c.g) / 255.0,
            f32::from(c.b) / 255.0,
            a,
        ]
    };
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let mut out = Vec::with_capacity(FOOTER_SURFACES);
    if let Some((x, y, w, h)) = layout.card {
        out.push(RoundedSurface {
            pos: [x, y],
            size: [w, h],
            fill: rgba(fc.card_fill, 1.0),
            stroke: rgba(fc.card_edge, 1.0),
            params: [(6.0 * scale).min(w * 0.5).min(h * 0.5), 1.0, 0.0, 0.0],
        });
    }
    let square = |(x, y, w, h): Rect4, slot: usize| RoundedSurface {
        pos: [x, y],
        size: [w, h],
        fill: rgba(footer.sample.palette[slot], 1.0),
        // The sample's text colour outlines a square that matches the card.
        stroke: rgba(footer.sample.foreground, 0.45),
        params: [(w * 0.25).min(3.0 * scale), 1.0, 0.0, 0.0],
    };
    for &(rect, slot) in layout.squares.iter().chain(&layout.lowest_square) {
        out.push(square(rect, slot));
    }
    let dot = |(x, y, w, h): Rect4, color: Rgb| RoundedSurface {
        pos: [x, y],
        size: [w, h],
        fill: rgba(color, 1.0),
        stroke: [0.0; 4],
        params: [w.min(h) * 0.5, 0.0, 0.0, 0.0],
    };
    if let Some(rect) = layout.low_mark {
        out.push(dot(rect, fc.low_mark));
    }
    if let Some(rect) = layout.slot_mark {
        out.push(dot(rect, fc.mark));
    }
    if let Some((x, y, w, h)) = layout.contrast_button {
        out.push(RoundedSurface {
            pos: [x, y],
            size: [w, h],
            fill: [0.0; 4],
            stroke: rgba(fc.panel_edge, 1.0),
            params: [(4.0 * scale).min(h * 0.5), 1.0, 0.0, 0.0],
        });
    }
    out.truncate(FOOTER_SURFACES);
    out
}
