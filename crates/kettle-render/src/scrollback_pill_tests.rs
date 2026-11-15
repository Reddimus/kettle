//! The jump-to-bottom button and the "Start of kept history" label.
//!
//! The 6.0 macOS comparison's judges read the `↓ N lines` pill as a badge,
//! not a control, and took the scrollbar's travel for the size of the
//! history. These pin the button's key hint and its states, and the label
//! that says where kept history starts and how much of it there is, as
//! resolved geometry and colours: no test here reads a glyph.

use crate::color::{self, Backing, contrast_ratio, relative_luminance};
use crate::scrollback::{
    PILL_EDGE_CONTRAST, PILL_EDGE_ON_FILL, PILL_STATE_STEP, kept_history_label_quads,
    pane_scrollbar_geometry, scrollback_button_colors, scrollback_pill_colors,
    scrollback_pill_quads,
};
use crate::{
    HistoryView, QuadInstance, Rect4, ScrollbackPillState, kept_history_label, scrollback_pill,
    scrollback_pill_hint, search_bar_geometry,
};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::Processor;
use kettle_config::keybinds::{Action, hint_label_for_platform};
use kettle_config::{Config, Rgb, ScrollbarMode, Theme};
use kettle_i18n::{Language, Translator};

/// The comparison's pane: 51 rows over a 920 px terminal, 360 lines back.
const SCROLLED_BACK: usize = 360;
const HISTORY: usize = 10_000;
/// The overlay font's cell at 1x.
const CELL: (f32, f32) = (8.0, 16.0);
const SCALES: [f32; 2] = [1.0, 2.0];

fn terminal(width: f32, scale: f32) -> Rect4 {
    (40.0 * scale, 30.0 * scale, width * scale, 920.0 * scale)
}

fn cell(scale: f32) -> (f32, f32) {
    (CELL.0 * scale, CELL.1 * scale)
}

fn right((x, _, w, _): Rect4) -> f32 {
    x + w
}

fn bottom((_, y, _, h): Rect4) -> f32 {
    y + h
}

fn overlaps(a: Rect4, b: Rect4) -> bool {
    a.0 < right(b) && b.0 < right(a) && a.1 < bottom(b) && b.1 < bottom(a)
}

fn inside(inner: Rect4, outer: Rect4) -> bool {
    inner.0 >= outer.0
        && inner.1 >= outer.1
        && right(inner) <= right(outer)
        && bottom(inner) <= bottom(outer)
}

/// The top of history with `size` lines kept and older lines gone.
fn top_of(size: usize) -> HistoryView {
    HistoryView {
        offset: size,
        size,
        origin: 90_000,
    }
}

/// The chord shows after the count while the pane has room for both, and
/// the count alone below that: narrowing a pane drops the chord first and
/// the button last, and the count-only button is the one a pane with no
/// chord bound shows at that width. At 1x and 2x.
#[test]
fn the_jump_button_shows_its_key_only_where_the_count_still_fits() {
    let tr = Translator::default();
    for scale in SCALES {
        let pill = |width: f32, hint: Option<&str>| {
            scrollback_pill(
                terminal(width, scale),
                SCROLLED_BACK,
                scale,
                cell(scale),
                6.0,
                hint,
                &tr,
            )
        };
        let wide = pill(800.0, Some("Ctrl+End")).expect("a button");
        let hint = wide.hint.as_ref().expect("room for the key at 800 px");
        assert_eq!(wide.label, "\u{2193} 360 lines");
        assert_eq!(hint.text, "Ctrl+End");
        // Count, rule, chord, left to right inside the button, a pad apart.
        let pad = 6.0 * scale;
        let (x, y, w, h) = wide.rect;
        let label_end = wide.text_origin.0 + 11.0 * cell(scale).0;
        let (dx, dy, dw, dh) = hint.divider;
        assert_eq!(wide.text_origin.0, x + pad, "scale {scale}");
        assert!((dx - (label_end + pad)).abs() <= 0.5, "scale {scale}");
        assert_eq!(dw, scale, "scale {scale}: a 1 logical px rule");
        assert!(dy > y && dy + dh < y + h, "scale {scale}: rule inside");
        assert!(
            (hint.text_origin.0 - (dx + dw + pad)).abs() <= 0.5,
            "scale {scale}: a pad after the rule"
        );
        assert_eq!(hint.text_origin.1, wide.text_origin.1, "scale {scale}");
        let hint_end = hint.text_origin.0 + 8.0 * cell(scale).0;
        assert_eq!(
            hint_end + pad,
            x + w,
            "scale {scale}: the chord ends a pad inside the button"
        );
        // The right edge and the height are the count-only button's.
        let alone = pill(800.0, None).expect("a button");
        assert!(alone.hint.is_none());
        assert_eq!(right(wide.rect), right(alone.rect), "scale {scale}");
        assert_eq!((wide.rect.1, wide.rect.3), (alone.rect.1, alone.rect.3));
        assert!(wide.rect.2 > alone.rect.2);

        let (mut with_key, mut count_only, mut none) = (0, 0, 0);
        for width in (20..=800).rev() {
            let width = width as f32;
            let bound = pill(width, Some("Ctrl+End"));
            let unbound = pill(width, None);
            match (&bound, &unbound) {
                (Some(bound), Some(unbound)) if bound.hint.is_some() => {
                    assert_eq!(
                        (count_only, none),
                        (0, 0),
                        "scale {scale} width {width}: the chord came back"
                    );
                    assert_eq!(bound.label, unbound.label);
                    assert!(inside(bound.rect, terminal(width, scale)));
                    with_key += 1;
                }
                (Some(bound), Some(unbound)) => {
                    assert_eq!(none, 0, "scale {scale} width {width}");
                    assert_eq!(bound, unbound, "scale {scale} width {width}");
                    count_only += 1;
                }
                (None, None) => none += 1,
                _ => panic!("scale {scale} width {width}: a bound key hid the count"),
            }
        }
        assert!(
            with_key > 0 && count_only > 0 && none > 0,
            "scale {scale}: {with_key} with the key, {count_only} count only, {none} none"
        );
        // An unbound action, an empty chord and an overlong one show none.
        let overlong = "Ctrl+".repeat(8);
        for hint in [None, Some(""), Some("  "), Some(overlong.as_str())] {
            assert_eq!(pill(800.0, hint), Some(alone.clone()), "{hint:?}");
        }
    }
}

/// The chord is the one bound to `scroll_to_bottom`, as menus spell it: the
/// default, then a rebind, then none once the action is unbound. The macOS
/// spelling fits the same layout.
#[test]
fn the_jump_button_key_follows_the_bindings() {
    let default = Config::default();
    let default_chord = if cfg!(target_os = "macos") {
        "\u{2303}End"
    } else {
        "Ctrl+End"
    };
    assert_eq!(
        scrollback_pill_hint(&default.keybinds).as_deref(),
        Some(default_chord)
    );
    let rebound =
        Config::parse_text("keybind = ctrl+end=unbind\nkeybind = ctrl+shift+b=scroll_to_bottom\n");
    let rebound_chord = if cfg!(target_os = "macos") {
        "\u{2303}\u{21e7}B"
    } else {
        "Ctrl+Shift+B"
    };
    assert_eq!(
        scrollback_pill_hint(&rebound.keybinds).as_deref(),
        Some(rebound_chord)
    );
    let unbound = Config::parse_text("keybind = ctrl+end=unbind\n");
    assert_eq!(scrollback_pill_hint(&unbound.keybinds), None);

    let tr = Translator::default();
    let pill = |cfg: &Config| {
        scrollback_pill(
            terminal(800.0, 1.0),
            SCROLLED_BACK,
            1.0,
            CELL,
            cfg.scrollbar_width,
            scrollback_pill_hint(&cfg.keybinds).as_deref(),
            &tr,
        )
        .expect("a button")
    };
    assert_eq!(
        pill(&default).hint.map(|hint| hint.text).as_deref(),
        Some(default_chord)
    );
    assert_eq!(
        pill(&rebound).hint.map(|hint| hint.text).as_deref(),
        Some(rebound_chord)
    );
    assert_eq!(pill(&unbound).hint, None);
    assert_eq!(right(pill(&unbound).rect), right(pill(&default).rect));

    // Every platform lays out the other's spelling too.
    for macos in [false, true] {
        let chord = hint_label_for_platform(&default.keybinds, &Action::ScrollToBottom, macos)
            .expect("bound by default");
        assert_eq!(chord, if macos { "\u{2303}End" } else { "Ctrl+End" });
        let pill = scrollback_pill(
            terminal(800.0, 2.0),
            SCROLLED_BACK,
            2.0,
            cell(2.0),
            6.0,
            Some(&chord),
            &tr,
        )
        .expect("a button");
        let hint = pill.hint.expect("room for the key");
        assert_eq!(hint.text, chord);
        let columns = chord.chars().count() as f32;
        assert!(hint.text_origin.0 + columns * cell(2.0).0 <= right(pill.rect) - 11.0);
    }
}

/// Which state a pane's button paints: hovered under the pointer, pressed
/// only while the pointer stays on the button that took the press.
#[test]
fn a_press_shows_only_under_the_pointer() {
    use ScrollbackPillState::{Hovered, Pressed, Rest};
    for (hovered, pressed, want) in [
        (None, None, Rest),
        (Some(7), None, Hovered),
        (Some(7), Some(7), Pressed),
        // Dragged off the button: a release here does nothing.
        (None, Some(7), Rest),
        // Another pane's button.
        (Some(8), None, Rest),
        (Some(8), Some(8), Rest),
        (Some(7), Some(8), Hovered),
    ] {
        assert_eq!(
            ScrollbackPillState::of(7, hovered, pressed),
            want,
            "hovered {hovered:?} pressed {pressed:?}"
        );
    }
    assert_eq!(
        [Rest, Hovered, Pressed].map(ScrollbackPillState::name),
        ["rest", "hovered", "pressed"]
    );
}

/// On every bundled theme, in both blend modes and in each state: the
/// label clears the text floor at its stem pixel, the edge is 3:1 against
/// the terminal and shows against the fill, and each state's fill differs
/// from the others by a visible step without the label changing from light
/// to dark. At rest the button keeps the pill's fill and ink.
#[test]
fn the_jump_button_states_keep_their_floors_on_every_theme() {
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            let colors =
                scrollback_button_colors(&theme, linear, Backing::opaque(theme.background));
            let bg = theme.background;
            assert_eq!(
                (colors.rest.fill, colors.rest.ink),
                scrollback_pill_colors(&theme, linear),
                "{name}"
            );
            let lighter_ink = |s: crate::scrollback::PillSurface| {
                relative_luminance(s.ink) > relative_luminance(s.fill)
            };
            for (state, surface) in [
                ("rest", colors.rest),
                ("hover", colors.hover),
                ("pressed", colors.pressed),
            ] {
                let stem = color::over(surface.fill, surface.ink, color::STEM_COVERAGE, linear);
                let mut check = |what: &str, ratio: f64, floor: f64| {
                    if ratio < floor {
                        failures.push(format!(
                            "{name} linear={linear} {state} {what}: {ratio:.3} < {floor}"
                        ));
                    }
                };
                check(
                    "label stem on fill",
                    contrast_ratio(stem, surface.fill),
                    crate::chrome::TEXT_CONTRAST,
                );
                check(
                    "edge on terminal",
                    contrast_ratio(surface.edge, bg),
                    PILL_EDGE_CONTRAST,
                );
                check(
                    "edge on fill",
                    contrast_ratio(surface.edge, surface.fill),
                    PILL_EDGE_ON_FILL,
                );
                if lighter_ink(surface) != lighter_ink(colors.rest) {
                    failures.push(format!("{name} linear={linear} {state}: the label flipped"));
                }
            }
            for (pair, a, b) in [
                ("hover from rest", colors.hover.fill, colors.rest.fill),
                ("pressed from hover", colors.pressed.fill, colors.hover.fill),
                ("pressed from rest", colors.pressed.fill, colors.rest.fill),
            ] {
                // A step is resolved to 8-bit colour, so allow its rounding.
                if contrast_ratio(a, b) < PILL_STATE_STEP - 0.02 {
                    failures.push(format!(
                        "{name} linear={linear} {pair}: {:.3}",
                        contrast_ratio(a, b)
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first: {}",
        failures.len(),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
}

/// The two default themes, as resolved: the fill is 3:1 against the
/// terminal at rest, a hovered button's edge takes the label's ink, and on
/// TokyoNight Night the fill lifts under the pointer and sinks when pressed.
#[test]
fn the_jump_button_states_on_the_default_themes() {
    for name in ["TokyoNight Night", "Kettle Day"] {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            let colors =
                scrollback_button_colors(&theme, linear, Backing::opaque(theme.background));
            let bg = theme.background;
            let on_bg = |c: Rgb| contrast_ratio(c, bg);
            assert!(on_bg(colors.rest.fill) >= 2.99, "{name} linear={linear}");
            assert_ne!(colors.rest.edge, colors.rest.fill, "{name}");
            assert_ne!(colors.hover.fill, colors.rest.fill, "{name}");
            assert_ne!(colors.pressed.fill, colors.hover.fill, "{name}");
            assert_ne!(colors.pressed.fill, colors.rest.fill, "{name}");
            for surface in [colors.hover, colors.pressed] {
                assert_eq!(surface.edge, surface.ink, "{name} linear={linear}");
                assert!(on_bg(surface.edge) >= crate::chrome::TEXT_CONTRAST);
            }
            // The rest edge is quieter than the hovered one.
            assert!(on_bg(colors.rest.edge) < on_bg(colors.hover.edge), "{name}");
        }
    }
    let night = Theme::by_name("TokyoNight Night");
    for linear in [true, false] {
        let colors = scrollback_button_colors(&night, linear, Backing::opaque(night.background));
        let on_bg = |c: Rgb| contrast_ratio(c, night.background);
        assert!(on_bg(colors.hover.fill) > on_bg(colors.rest.fill));
        assert!(on_bg(colors.pressed.fill) < on_bg(colors.rest.fill));
    }
}

/// The colour the last quad over `(x, y)` paints.
fn painted_at(quads: &[QuadInstance], x: f32, y: f32) -> Option<Rgb> {
    quads
        .iter()
        .rev()
        .find(|q| {
            x >= q.pos[0] && x < q.pos[0] + q.size[0] && y >= q.pos[1] && y < q.pos[1] + q.size[1]
        })
        .map(quad_color)
}

fn quad_color(quad: &QuadInstance) -> Rgb {
    let channel = |value: f32| (value * 255.0).round() as u8;
    Rgb::new(
        channel(quad.color[0]),
        channel(quad.color[1]),
        channel(quad.color[2]),
    )
}

fn bounds(quads: &[QuadInstance]) -> Rect4 {
    let fold = |pick: fn(&QuadInstance) -> f32, best: fn(f32, f32) -> f32, start: f32| {
        quads.iter().map(pick).fold(start, best)
    };
    let left = fold(|q| q.pos[0], f32::min, f32::MAX);
    let top = fold(|q| q.pos[1], f32::min, f32::MAX);
    let right = fold(|q| q.pos[0] + q.size[0], f32::max, f32::MIN);
    let bottom = fold(|q| q.pos[1] + q.size[1], f32::max, f32::MIN);
    (left, top, right - left, bottom - top)
}

/// What the painter draws for the button is the rect the pointer hits: an
/// edge one logical px wide around the state's fill, and the rule before
/// the key in the edge's colour. The label keeps the pill's round ends and
/// has no edge. At 1x and 2x, on both default themes, in every state.
#[test]
fn the_painted_button_and_label_fill_their_rects() {
    let tr = Translator::default();
    for name in ["TokyoNight Night", "Kettle Day"] {
        let theme = Theme::by_name(name);
        let colors = scrollback_button_colors(&theme, true, Backing::opaque(theme.background));
        for scale in SCALES {
            let pane = terminal(800.0, scale);
            let pill = scrollback_pill(
                pane,
                SCROLLED_BACK,
                scale,
                cell(scale),
                6.0,
                Some("Ctrl+End"),
                &tr,
            )
            .expect("a button");
            let (x, y, w, h) = pill.rect;
            let hint = pill.hint.clone().expect("a key");
            for state in [
                ScrollbackPillState::Rest,
                ScrollbackPillState::Hovered,
                ScrollbackPillState::Pressed,
            ] {
                let surface = colors.surface(state);
                let mut quads = Vec::new();
                scrollback_pill_quads(&mut quads, &pill, surface, scale);
                let context = format!("{name} scale {scale} {state:?}");
                assert_eq!(bounds(&quads), pill.rect, "{context}");
                let middle = y + h / 2.0;
                // The edge is `scale` px wide on every side; inside is fill.
                for (px, py) in [
                    (x + scale / 2.0, middle),
                    (x + w - scale / 2.0, middle),
                    (x + w / 4.0, y + scale / 2.0),
                    (x + w / 4.0, y + h - scale / 2.0),
                ] {
                    assert_eq!(painted_at(&quads, px, py), Some(surface.edge), "{context}");
                }
                for (px, py) in [
                    (x + scale + 0.5, middle),
                    (x + w - scale - 0.5, middle),
                    (x + w / 4.0, y + scale + 0.5),
                    (x + w / 4.0, y + h - scale - 0.5),
                ] {
                    assert_eq!(painted_at(&quads, px, py), Some(surface.fill), "{context}");
                }
                // Rounded: the corner pixel itself is not painted.
                assert_eq!(painted_at(&quads, x + 0.25, y + 0.25), None, "{context}");
                let (dx, dy, dw, dh) = hint.divider;
                assert_eq!(
                    painted_at(&quads, dx + dw / 2.0, dy + dh / 2.0),
                    Some(surface.edge),
                    "{context}: the rule"
                );
                assert_eq!(
                    painted_at(&quads, dx - 1.0, dy + dh / 2.0),
                    Some(surface.fill),
                    "{context}"
                );
            }
            // Without a key there is no rule: one quad fewer.
            let alone =
                scrollback_pill(pane, SCROLLED_BACK, scale, cell(scale), 6.0, None, &tr).unwrap();
            let (mut with_key, mut quads) = (Vec::new(), Vec::new());
            scrollback_pill_quads(&mut with_key, &pill, colors.rest, scale);
            scrollback_pill_quads(&mut quads, &alone, colors.rest, scale);
            assert_eq!(bounds(&quads), alone.rect);
            assert_eq!(with_key.len(), quads.len() + 1, "{name} scale {scale}");

            let label = kept_history_label(pane, top_of(HISTORY), scale, cell(scale), 6.0, &tr)
                .expect("a label");
            let mut quads = Vec::new();
            kept_history_label_quads(&mut quads, &label, colors.rest.fill);
            assert_eq!(bounds(&quads), label.rect, "{name} scale {scale}");
            let (lx, ly, lw, lh) = label.rect;
            // One colour, no edge, and round ends.
            assert!(quads.iter().all(|q| quad_color(q) == colors.rest.fill));
            assert_eq!(
                painted_at(&quads, lx + 0.5, ly + lh / 2.0),
                Some(colors.rest.fill)
            );
            assert_eq!(painted_at(&quads, lx + 0.5, ly + 0.5), None);
            assert_eq!(painted_at(&quads, lx + lw - 0.5, ly + lh - 0.5), None);
        }
    }
}

/// The label shows only at the very top of a history that has lost its
/// oldest lines. Not at a live prompt after Clear Scrollback, where nothing
/// is left to scroll; not one line below the top; not where nothing was
/// dropped.
#[test]
fn the_kept_history_label_condition() {
    let tr = Translator::default();
    for (offset, size, origin, want, why) in [
        (0, 0, 0, false, "a fresh pane"),
        (
            0,
            0,
            500,
            false,
            "a live prompt after Clear Scrollback: no history to scroll",
        ),
        (0, 10_000, 90_000, false, "live output over a full history"),
        (360, 10_000, 90_000, false, "scrolled back part of the way"),
        (9_999, 10_000, 90_000, false, "one line below the top"),
        (10_000, 10_000, 90_000, true, "the top of a full history"),
        (10_000, 10_000, 0, false, "the top, with nothing dropped"),
        (100, 100, 0, false, "the top of a short history"),
        (37, 37, 500, true, "the top of what was kept after a clear"),
        (1, 1, 1, true, "one line kept, one gone"),
    ] {
        let view = HistoryView {
            offset,
            size,
            origin,
        };
        assert_eq!(view.at_start_of_kept_history(), want, "{why}");
        assert_eq!(
            kept_history_label(terminal(800.0, 1.0), view, 1.0, CELL, 6.0, &tr).is_some(),
            want,
            "{why}"
        );
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// The same from a real terminal with a 50-line scrollback: 200 lines of
/// output drop the oldest, so the top of history is labelled with the 50
/// lines kept. Clear Scrollback (`CSI 3 J`) leaves a live prompt with no
/// label, and the top of the history that builds up afterwards says how
/// many lines that is, not the limit.
#[test]
fn the_kept_history_label_follows_a_real_terminal() {
    use alacritty_terminal::grid::Scroll;
    let tr = Translator::default();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = kettle_core::EventProxy::new(tx, std::sync::Arc::new(|| {}));
    let limits = TermConfig {
        scrolling_history: 50,
        ..TermConfig::default()
    };
    let mut term = alacritty_terminal::Term::new(limits, &Size { cols: 40, rows: 10 }, proxy);
    let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
    let mut feed = |term: &mut alacritty_terminal::Term<kettle_core::EventProxy>, bytes: &[u8]| {
        parser.advance(term, bytes);
    };
    let lines = |count: usize| -> String { (0..count).map(|line| format!("{line}\r\n")).collect() };
    let label = |term: &alacritty_terminal::Term<kettle_core::EventProxy>| {
        let view = HistoryView::of(term);
        // The renderer's snapshot reports the same view.
        let mut snap = crate::PaneSnapshot::default();
        snap.capture(term);
        assert_eq!(snap.history_view(), view);
        kept_history_label(terminal(800.0, 1.0), view, 1.0, CELL, 6.0, &tr).map(|label| label.label)
    };

    // Nothing dropped yet: the top of a short history has no label.
    feed(&mut term, lines(30).as_bytes());
    term.scroll_display(Scroll::Top);
    assert_eq!(HistoryView::of(&term).origin, 0);
    assert_eq!(label(&term), None);

    term.scroll_display(Scroll::Bottom);
    feed(&mut term, lines(200).as_bytes());
    assert_eq!(label(&term), None, "live output");
    term.scroll_display(Scroll::Top);
    assert_eq!(
        HistoryView::of(&term),
        HistoryView {
            offset: 50,
            size: 50,
            origin: HistoryView::of(&term).origin
        }
    );
    assert!(HistoryView::of(&term).origin > 0);
    assert_eq!(
        label(&term).as_deref(),
        Some("Start of kept history \u{b7} 50 lines")
    );
    term.scroll_display(Scroll::Delta(-1));
    assert_eq!(label(&term), None, "one line below the top");

    // Clear Scrollback: the origin stays advanced, but nothing is left to
    // scroll, so the live prompt is not labelled.
    term.scroll_display(Scroll::Bottom);
    feed(&mut term, b"\x1b[3J");
    let cleared = HistoryView::of(&term);
    assert_eq!((cleared.offset, cleared.size), (0, 0));
    assert!(cleared.origin > 0);
    assert_eq!(label(&term), None, "a live prompt after Clear Scrollback");

    // The history that builds up afterwards starts where the clear left it.
    feed(&mut term, lines(5).as_bytes());
    term.scroll_display(Scroll::Top);
    assert_eq!(HistoryView::of(&term).size, 5);
    assert_eq!(
        label(&term).as_deref(),
        Some("Start of kept history \u{b7} 5 lines")
    );
}

/// The label's text in both languages: the kept line count grouped as the
/// button groups its own, singular for one line, and the label alone where
/// the count does not fit.
#[test]
fn the_kept_history_label_speaks_english_and_spanish() {
    let en = Translator::default();
    let es = Translator::new(Language::Es);
    let label = |tr: &Translator, width: f32, size: usize| {
        kept_history_label(terminal(width, 1.0), top_of(size), 1.0, CELL, 6.0, tr)
    };
    for (tr, size, want) in [
        (&en, 10_000, "Start of kept history \u{b7} 10,000 lines"),
        (&en, 1, "Start of kept history \u{b7} 1 line"),
        (
            &en,
            1_234_567,
            "Start of kept history \u{b7} 1,234,567 lines",
        ),
        (
            &es,
            10_000,
            "Inicio del historial conservado \u{b7} 10,000 l\u{ed}neas",
        ),
        (
            &es,
            1,
            "Inicio del historial conservado \u{b7} 1 l\u{ed}nea",
        ),
    ] {
        let label = label(tr, 800.0, size).expect("a label");
        assert_eq!(label.label, want);
        assert!(label.counted);
    }
    // The button groups its count the same way.
    let pill = scrollback_pill(terminal(800.0, 1.0), 10_000, 1.0, CELL, 6.0, None, &en).unwrap();
    assert_eq!(pill.label, "\u{2193} 10,000 lines");

    // Too narrow for the count: the label alone; narrower still: nothing.
    for (tr, alone) in [
        (&en, "Start of kept history"),
        (&es, "Inicio del historial conservado"),
    ] {
        let (mut counted, mut plain, mut none) = (0, 0, 0);
        for width in (20..=800).rev() {
            match label(tr, width as f32, 10_000) {
                Some(label) if label.counted => {
                    assert_eq!((plain, none), (0, 0), "width {width}");
                    counted += 1;
                }
                Some(label) => {
                    assert_eq!(none, 0, "width {width}");
                    assert_eq!(label.label, alone);
                    plain += 1;
                }
                None => none += 1,
            }
        }
        assert!(counted > 0 && plain > 0 && none > 0, "{alone}");
    }
}

/// The label sits at the pane's top right inside the terminal, left of the
/// scrollbar's gutter as the button is, at every bar width, at rest and
/// with the bar widened, at 1x and 2x; and it never touches the button
/// below it: a pane too short for both shows only the button.
#[test]
fn the_kept_history_label_clears_the_scrollbar_and_the_button() {
    let tr = Translator::default();
    for width in 2..=40 {
        let cfg = Config {
            scrollbar: ScrollbarMode::Auto,
            scrollbar_width: width as f32,
            ..Config::default()
        };
        for scale in SCALES {
            let pane = terminal(800.0, scale);
            let label =
                kept_history_label(pane, top_of(HISTORY), scale, cell(scale), width as f32, &tr)
                    .expect("a label");
            let pill = scrollback_pill(pane, HISTORY, scale, cell(scale), width as f32, None, &tr)
                .expect("a button");
            let context = format!("bar {width} scale {scale}");
            assert!(inside(label.rect, pane), "{context}");
            assert_eq!(label.rect.1, pane.1 + 6.0 * scale, "{context}: at the top");
            assert_eq!(right(label.rect), right(pill.rect), "{context}");
            assert_eq!(label.rect.3, pill.rect.3, "{context}");
            assert_eq!(
                label.text_origin,
                (label.rect.0 + 6.0 * scale, label.rect.1 + 3.0 * scale)
            );
            for interacting in [false, true] {
                let bar =
                    pane_scrollbar_geometry(pane, scale, &cfg, 51, HISTORY, HISTORY, interacting);
                assert!(
                    right(label.rect) <= bar.track_rect.0,
                    "{context}: label ends at {} past the bar at {}",
                    right(label.rect),
                    bar.track_rect.0
                );
                assert!(!overlaps(label.rect, bar.hit_rect), "{context}");
            }
        }
    }
    for scale in SCALES {
        let (mut both, mut button_only) = (0, 0);
        for height in 10..=200 {
            let pane = (0.0, 0.0, 800.0 * scale, height as f32 * scale);
            let pill = scrollback_pill(pane, HISTORY, scale, cell(scale), 6.0, None, &tr);
            let label = kept_history_label(pane, top_of(HISTORY), scale, cell(scale), 6.0, &tr);
            match (pill, label) {
                (Some(pill), Some(label)) => {
                    assert!(
                        bottom(label.rect) + 6.0 * scale <= pill.rect.1,
                        "scale {scale} height {height}: the label touches the button"
                    );
                    both += 1;
                }
                (Some(_), None) => {
                    assert_eq!(both, 0, "scale {scale} height {height}");
                    button_only += 1;
                }
                (None, Some(_)) => panic!("scale {scale} height {height}: a label, no button"),
                (None, None) => assert_eq!((both, button_only), (0, 0)),
            }
        }
        assert!(both > 0 && button_only > 0, "scale {scale}");
    }
}

/// The search bar is a lane reserved under the panes, so a pane's terminal
/// ends where the bar begins: the label at its top and the button at its
/// bottom stay out of the bar at every window size, in every language,
/// however many rows the bar wraps to.
#[test]
fn the_label_and_the_button_stay_out_of_the_search_bar() {
    for tr in [Translator::default(), Translator::new(Language::Es)] {
        for (width, height) in [
            (1280.0, 820.0),
            (640.0, 400.0),
            (360.0, 300.0),
            (240.0, 200.0),
        ] {
            let bar = search_bar_geometry(width, height, CELL.0, 18.0, &tr);
            assert!(bar.reserved_height > 0.0);
            // What `App::area` leaves the panes while the bar is open.
            let pane = (0.0, 0.0, width, height - bar.reserved_height);
            let pill = scrollback_pill(pane, HISTORY, 1.0, CELL, 6.0, Some("Ctrl+End"), &tr);
            let label = kept_history_label(pane, top_of(HISTORY), 1.0, CELL, 6.0, &tr);
            let context = format!("{width}x{height} {:?}", tr.language());
            for rect in pill
                .iter()
                .map(|p| p.rect)
                .chain(label.iter().map(|l| l.rect))
            {
                assert!(inside(rect, pane), "{context}");
                assert!(!overlaps(rect, bar.rect), "{context}");
                assert!(bottom(rect) <= bar.rect.1, "{context}");
            }
            if width >= 640.0 {
                assert!(pill.is_some() && label.is_some(), "{context}");
            }
        }
    }
}

/// In a translucent window the button stays opaque, so its fill and label
/// are the opaque window's; its edge meets the terminal, which shows the
/// desktop through it. At the default opacities (0.99, and 0.86 where the
/// window has a native material) and opaque, on every bundled theme, in
/// both blend modes, with and without macOS's backdrop lift, the edge in
/// every state is 3:1 against everything the terminal can show over any
/// desktop, and 1.3:1 against the fill. Below that, where no grey can be
/// 3:1 against both a black and a white desktop seen through the terminal,
/// the edge is the white or black that comes closest.
#[test]
fn the_jump_button_edge_holds_over_a_translucent_window() {
    use crate::scrollback::{PILL_EDGE_CONTRAST, PILL_EDGE_ON_FILL, pill_edge_fallback};
    let mut failures = Vec::new();
    let mut short = 0;
    for lift in [color::NO_BACKDROP_LIFT, color::MACOS_BACKDROP_LIFT] {
        for alpha in [1.0, 0.99, 0.86, 0.7, 0.43] {
            for name in Theme::list() {
                let theme = Theme::by_name(name);
                for linear in [true, false] {
                    let terminal = Backing::translucent(theme.background, alpha, lift);
                    let colors = scrollback_button_colors(&theme, linear, terminal);
                    let opaque =
                        scrollback_button_colors(&theme, linear, Backing::opaque(theme.background));
                    let context = format!("{name} linear={linear} alpha {alpha} lift {lift:?}");
                    for (state, surface, opaque) in [
                        ("rest", colors.rest, opaque.rest),
                        ("hover", colors.hover, opaque.hover),
                        ("pressed", colors.pressed, opaque.pressed),
                    ] {
                        if (surface.fill, surface.ink) != (opaque.fill, opaque.ink) {
                            failures.push(format!("{context} {state}: the fill or label moved"));
                        }
                        let against = terminal.weakest_contrast(surface.edge);
                        let holds = against >= PILL_EDGE_CONTRAST
                            && contrast_ratio(surface.edge, surface.fill) >= PILL_EDGE_ON_FILL;
                        if holds {
                            continue;
                        }
                        if alpha >= 0.86 {
                            failures.push(format!("{context} {state}: edge {against:.3}"));
                        } else if surface.edge != pill_edge_fallback(terminal) {
                            failures.push(format!("{context} {state}: not the closest grey"));
                        } else {
                            short += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first: {}",
        failures.len(),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
    // Below the defaults some themes do run out of greys.
    assert!(short > 0);
}

/// The chord bound to `scroll_to_bottom` with all four modifiers, spelled
/// for macOS (`⌃⌥⇧⌘End`) or elsewhere (`Ctrl+Alt+Shift+Super+End`), and
/// with one modifier.
fn chords(macos: bool) -> Vec<String> {
    [
        "keybind = ctrl+end=unbind\nkeybind = ctrl+alt+shift+super+end=scroll_to_bottom\n",
        "keybind = ctrl+end=unbind\nkeybind = super+end=scroll_to_bottom\n",
        "",
    ]
    .into_iter()
    .map(|config| {
        hint_label_for_platform(
            &Config::parse_text(config).keybinds,
            &Action::ScrollToBottom,
            macos,
        )
        .expect("bound")
    })
    .collect()
}

/// The layout gives the chord one cell per column of `display_width`; the
/// bundled overlay face has every glyph a chord can hold (the macOS ⌃ ⌥ ⇧ ⌘
/// as well as ASCII) at one cell, so the shaped chord ends inside the
/// painted button, a pad short of its right edge, in either spelling. Shaped
/// here with only the bundled face, as every host has it, so a missing
/// glyph cannot hide behind a host font.
#[test]
fn the_shaped_chord_ends_inside_the_button_in_either_spelling() {
    use crate::{Attrs, Family, FontSystem, Metrics, Shaping, TextBuffer, Wrap, fontdb};
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    crate::fonts::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    let tr = Translator::default();
    for scale in SCALES {
        let metrics = Metrics::new(13.0 * scale, 17.0 * scale);
        let mut shape = |text: &str| {
            let mut buffer = TextBuffer::new(&mut fonts, metrics);
            buffer.set_size(None, Some(64.0 * scale));
            buffer.set_wrap(Wrap::None);
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut fonts, false);
            let run = buffer.layout_runs().next().expect("one line");
            let missing = run.glyphs.iter().any(|glyph| glyph.glyph_id == 0);
            (run.line_w, missing)
        };
        let (cell, _) = shape("M");
        for macos in [true, false] {
            for chord in chords(macos) {
                let (shaped, missing) = shape(&chord);
                let context = format!("{chord:?} scale {scale}");
                assert!(!missing, "{context}: a glyph the bundled face lacks");
                let pill = scrollback_pill(
                    terminal(800.0, scale),
                    SCROLLED_BACK,
                    scale,
                    (cell, 17.0 * scale),
                    6.0,
                    Some(&chord),
                    &tr,
                )
                .expect("a button");
                let hint = pill.hint.as_ref().expect("room for the chord");
                assert!(
                    hint.text_origin.0 + shaped <= right(pill.rect) - 6.0 * scale + 0.01,
                    "{context}: the chord ends at {} past the button's pad at {}",
                    hint.text_origin.0 + shaped,
                    right(pill.rect) - 6.0 * scale
                );
            }
        }
    }
}
