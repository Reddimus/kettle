//! The pane scrollbar: its colours on every bundled theme and its geometry.
//!
//! The 6.0 Linux comparison found the bar hard to see in a still: a 6 x 24 px
//! thumb in the corner with no channel at rest, and on Kettle Day, the light
//! default, a rest thumb at 1.55:1. These pin the colours as they show over
//! the background, painted solid or as translucent layers, and the rects the
//! painter, the pointer and AccessKit share.

use crate::color::{self, contrast_ratio};
use crate::scrollback::{
    ACTIVE_THUMB_CONTRAST, ACTIVE_THUMB_ON_TRACK, MAX_LAYER_ALPHA, PaneScrollbar,
    REST_THUMB_CONTRAST, REST_THUMB_ON_TRACK, REST_TRACK_MAX_CONTRAST, ScrollbarState,
    TICK_CONTRAST, TRACK_CONTRAST, pane_scrollbar_geometry, scrollback_pill, scrollbar_colors,
};
use kettle_config::{Config, Rgb, ScrollbarMode, Theme};
use kettle_i18n::Translator;

/// The comparison's window: 51 rows over a 920 px terminal.
const ROWS: usize = 51;
const HISTORY: usize = 10_000;
const SCROLLED_BACK: usize = 360;

fn config(mode: ScrollbarMode, width: f32) -> Config {
    Config {
        scrollbar: mode,
        scrollbar_width: width,
        ..Config::default()
    }
}

fn terminal(scale: f32) -> (f32, f32, f32, f32) {
    (40.0 * scale, 30.0 * scale, 800.0 * scale, 920.0 * scale)
}

fn bar(cfg: &Config, scale: f32, hist: usize, off: usize, interacting: bool) -> PaneScrollbar {
    pane_scrollbar_geometry(terminal(scale), scale, cfg, ROWS, hist, off, interacting)
}

/// Every part as it shows over the background, painted solid or as a layer:
/// rest thumb, active thumb, rest channel, active channel.
fn painted(theme: &Theme, linear: bool, opaque: bool) -> [(Rgb, f32); 4] {
    let colors = scrollbar_colors(theme, linear);
    let shown = |(ink, alpha): (Rgb, f32)| {
        (
            color::over(theme.background, ink, alpha.into(), linear),
            alpha,
        )
    };
    [
        shown(colors.thumb(ScrollbarState::Rest, opaque)),
        shown(colors.thumb(ScrollbarState::Active, opaque)),
        shown(colors.track(ScrollbarState::Rest, opaque)),
        shown(colors.track(ScrollbarState::Active, opaque)),
    ]
}

/// The floors from the design, met on every bundled theme in both blend
/// modes, solid and translucent: rest thumb 3:1 and active 4.5:1 against the
/// background, the channel 1.5:1 and at rest no more than 1.6:1, the active
/// thumb 3:1 against its channel and the rest thumb 2:1 against its.
///
/// A translucent layer covers at most [`MAX_LAYER_ALPHA`], so a glyph under
/// the bar shows. On the few themes where a floor needs more, the layer
/// stops there; only such a layer may fall short, and never on the defaults.
#[test]
fn scrollbar_colours_meet_their_floors_on_every_bundled_theme() {
    let mut failures = Vec::new();
    let mut capped = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let bg = theme.background;
        for linear in [true, false] {
            for opaque in [true, false] {
                let [
                    (rest, rest_a),
                    (active, active_a),
                    (rest_track, rest_track_a),
                    (active_track, active_track_a),
                ] = painted(&theme, linear, opaque);
                let alphas = [rest_a, active_a, rest_track_a, active_track_a];
                let at_cap = |layers: &[usize]| {
                    !opaque && layers.iter().any(|&i| alphas[i] >= MAX_LAYER_ALPHA)
                };
                let rest_track_ratio = contrast_ratio(rest_track, bg);
                for (role, ratio, floor, layers) in [
                    (
                        "rest thumb",
                        contrast_ratio(rest, bg),
                        REST_THUMB_CONTRAST,
                        &[0][..],
                    ),
                    (
                        "active thumb",
                        contrast_ratio(active, bg),
                        ACTIVE_THUMB_CONTRAST,
                        &[1],
                    ),
                    ("rest channel", rest_track_ratio, TRACK_CONTRAST, &[2]),
                    (
                        "active channel",
                        contrast_ratio(active_track, bg),
                        TRACK_CONTRAST,
                        &[3],
                    ),
                    (
                        "active thumb on its channel",
                        contrast_ratio(active, active_track),
                        ACTIVE_THUMB_ON_TRACK,
                        &[1, 3],
                    ),
                    (
                        "rest thumb on its channel",
                        contrast_ratio(rest, rest_track),
                        REST_THUMB_ON_TRACK,
                        &[0, 2],
                    ),
                ] {
                    if ratio >= floor {
                        continue;
                    }
                    let line = format!(
                        "{name} linear={linear} opaque={opaque} {role}: {ratio:.3} < {floor}"
                    );
                    if at_cap(layers) && !["TokyoNight Night", "Kettle Day"].contains(&name) {
                        capped.push(line);
                    } else {
                        failures.push(line);
                    }
                }
                if rest_track_ratio > REST_TRACK_MAX_CONTRAST {
                    failures.push(format!(
                        "{name} linear={linear} opaque={opaque} rest channel too strong: \
                         {rest_track_ratio:.3}"
                    ));
                }
                if opaque && alphas.iter().any(|&a| a != 1.0) {
                    failures.push(format!("{name} linear={linear}: solid paint is not opaque"));
                }
                if !opaque
                    && alphas
                        .iter()
                        .any(|&a| !(0.0..=MAX_LAYER_ALPHA).contains(&a))
                {
                    failures.push(format!(
                        "{name} linear={linear}: a translucent layer hides what is under it: \
                         {alphas:?}"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} scrollbar colour failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    // One saturated theme today (Hot Dog Stand's active thumb, linear), not a
    // way around the floors.
    assert!(
        capped.len() <= 2,
        "{} capped:\n{}",
        capped.len(),
        capped.join("\n")
    );
}

/// A command tick stands out against the background and against the channel
/// it lies across, on every bundled theme.
#[test]
fn command_ticks_stand_out_on_the_channel_on_every_bundled_theme() {
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            let colors = scrollbar_colors(&theme, linear);
            for (state, track) in [
                (ScrollbarState::Rest, colors.rest_track),
                (ScrollbarState::Active, colors.active_track),
            ] {
                for success in [true, false] {
                    let tick = colors.tick(state, success);
                    let ratio =
                        contrast_ratio(tick, theme.background).min(contrast_ratio(tick, track));
                    if ratio < TICK_CONTRAST {
                        failures.push(format!(
                            "{name} linear={linear} {state:?} success={success}: {ratio:.3}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} tick failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The two defaults by name: Kettle Day's rest thumb was 1.55:1.
#[test]
fn the_default_themes_rest_thumbs_reach_three_to_one() {
    for name in ["TokyoNight Night", "Kettle Day"] {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            let colors = scrollbar_colors(&theme, linear);
            let ratio = contrast_ratio(colors.rest, theme.background);
            assert!(ratio >= 3.0, "{name} linear={linear}: {ratio:.3}");
        }
    }
}

/// Scrolled back, the bar is the hover width, at least 10 logical px, in a
/// channel as wide as the thumb and as tall as the terminal. At rest it keeps
/// its configured width.
#[test]
fn scrolled_back_bar_uses_interacting_width_and_full_channel() {
    let cfg = config(ScrollbarMode::Auto, 6.0);
    for scale in [1.0, 2.0] {
        let (x, y, w, h) = terminal(scale);
        let back = bar(&cfg, scale, HISTORY, SCROLLED_BACK, false);
        let (tx, ty, tw, th) = back.track_rect;
        assert_eq!(tw, 10.0 * scale, "scale {scale}");
        assert_eq!((ty, th), (y, h), "the channel runs the terminal's height");
        assert_eq!(tx, x + w - tw - 2.0 * scale);
        assert!(back.channel);
        assert_eq!(back.state, ScrollbarState::Active);
        let (thumb_x, _, thumb_w, _) = back.thumb_rect.expect("a thumb");
        assert_eq!((thumb_x, thumb_w), (tx, tw), "thumb and channel align");

        let rest = bar(&cfg, scale, HISTORY, 0, false);
        assert_eq!(rest.track_rect.2, 6.0 * scale, "scale {scale}");
        assert_eq!(rest.thumb_rect.unwrap().2, 6.0 * scale);
        assert_eq!(rest.state, ScrollbarState::Rest);
    }
}

/// `auto` with history draws a faint full-height channel under the rest
/// thumb; a fresh pane with no history draws nothing.
#[test]
fn auto_draws_a_rest_channel_only_with_history() {
    let cfg = config(ScrollbarMode::Auto, 6.0);
    for scale in [1.0, 2.0] {
        let rest = bar(&cfg, scale, HISTORY, 0, false);
        assert!(rest.channel, "a channel at rest with history");
        assert_eq!(rest.track_rect.3, terminal(scale).3);
        assert!(rest.thumb_rect.is_some());
        for interacting in [false, true] {
            let fresh = bar(&cfg, scale, 0, 0, interacting);
            assert!(!fresh.channel, "no channel without history");
            assert_eq!(fresh.thumb_rect, None);
            assert_eq!(fresh.state, ScrollbarState::Rest);
        }
    }
}

/// `always` keeps one width while there is history, at rest, hovered and
/// scrolled back, so the bar never jumps. With no history it draws the
/// channel at the configured width, which hovering does not widen.
#[test]
fn always_mode_keeps_a_steady_width() {
    for width in [2.0, 6.0, 12.0, 40.0] {
        let cfg = config(ScrollbarMode::Always, width);
        for scale in [1.0, 2.0] {
            let widths: Vec<f32> = [(0, false), (0, true), (SCROLLED_BACK, false)]
                .into_iter()
                .map(|(off, interacting)| bar(&cfg, scale, HISTORY, off, interacting))
                .inspect(|bar| assert!(bar.channel))
                .map(|bar| bar.track_rect.2)
                .collect();
            assert!(
                widths.iter().all(|&w| w == widths[0]),
                "width {width} scale {scale}: {widths:?}"
            );
            assert_eq!(widths[0], f32::max(width, 10.0) * scale);
            for interacting in [false, true] {
                let empty = bar(&cfg, scale, 0, 0, interacting);
                assert!(empty.channel, "the empty channel is drawn");
                assert_eq!(empty.track_rect.2, width * scale, "no hover-widen");
                assert_eq!(empty.state, ScrollbarState::Rest);
                assert_eq!(empty.thumb_rect, None);
            }
        }
    }
}

/// `never` draws nothing, and its command ticks keep the resting column
/// even scrolled back.
#[test]
fn never_mode_draws_nothing() {
    let cfg = config(ScrollbarMode::Never, 6.0);
    for scale in [1.0, 2.0] {
        for (off, interacting) in [(0, false), (SCROLLED_BACK, true)] {
            let none = bar(&cfg, scale, HISTORY, off, interacting);
            assert!(!none.channel);
            assert_eq!(none.thumb_rect, None);
            assert_eq!(none.track_rect.2, 6.0 * scale);
        }
    }
}

/// With 10,000 lines of history the thumb is 40 logical px, 80 at 2x.
#[test]
fn min_thumb_is_forty_logical_pixels() {
    let cfg = config(ScrollbarMode::Auto, 6.0);
    for scale in [1.0, 2.0] {
        for off in [0, SCROLLED_BACK, HISTORY] {
            let thumb = bar(&cfg, scale, HISTORY, off, false)
                .thumb_rect
                .expect("a thumb");
            assert_eq!(thumb.3, 40.0 * scale, "scale {scale} offset {off}");
        }
    }
}

/// The pointer's strip touches the terminal's right edge, is at least 12
/// logical px wide, runs the full height and covers the bar and its edge,
/// at every width and in every state.
#[test]
fn hit_strip_reaches_the_pane_edge_and_covers_the_bar() {
    for mode in [ScrollbarMode::Auto, ScrollbarMode::Always] {
        for width in 2..=40 {
            let cfg = config(mode, width as f32);
            for scale in [1.0, 2.0] {
                let (x, y, w, h) = terminal(scale);
                for (hist, off, interacting) in [
                    (HISTORY, 0, false),
                    (HISTORY, 0, true),
                    (HISTORY, SCROLLED_BACK, false),
                    (0, 0, true),
                ] {
                    let bar = bar(&cfg, scale, hist, off, interacting);
                    let (hx, hy, hw, hh) = bar.hit_rect;
                    let context = format!("{mode:?} width {width} scale {scale}");
                    assert_eq!(hx + hw, x + w, "{context}: touches the edge");
                    assert_eq!((hy, hh), (y, h), "{context}");
                    assert!(hw >= 12.0 * scale, "{context}");
                    assert!(hx <= bar.track_rect.0, "{context}: covers the bar");
                    assert_ne!(bar.hit_rect, bar.track_rect, "{context}");
                }
            }
        }
    }
}

/// The jump button stays clear of the widened bar at every width.
#[test]
fn pill_clears_the_scrolled_back_bar() {
    let tr = Translator::default();
    for width in 2..=40 {
        let cfg = config(ScrollbarMode::Auto, width as f32);
        for scale in [1.0, 2.0] {
            for interacting in [false, true] {
                let bar = bar(&cfg, scale, HISTORY, SCROLLED_BACK, interacting);
                let pill = scrollback_pill(
                    terminal(scale),
                    SCROLLED_BACK,
                    scale,
                    (8.0 * scale, 16.0 * scale),
                    width as f32,
                    &tr,
                )
                .expect("a pill");
                assert!(
                    pill.rect.0 + pill.rect.2 <= bar.track_rect.0,
                    "width {width} scale {scale}: pill ends at {} past the bar at {}",
                    pill.rect.0 + pill.rect.2,
                    bar.track_rect.0
                );
            }
        }
    }
}

/// Solid colours only where the bar, at its widest and with its edge, fits
/// in `padding-x` and the window is opaque; translucent layers otherwise.
#[test]
fn opaque_only_inside_the_padding_of_an_opaque_window() {
    let with = |padding_x: f32, opacity: f32, width: f32| Config {
        padding_x,
        background_opacity: opacity,
        scrollbar_width: width,
        ..Config::default()
    };
    let opaque = |cfg: &Config, scale: f32| bar(cfg, scale, HISTORY, 0, false).opaque;
    // 10 px wide when scrolled back, plus its 2 px edge, in 12 px.
    assert!(opaque(&with(12.0, 1.0, 6.0), 1.0));
    assert!(!opaque(&with(11.0, 1.0, 6.0), 1.0), "would cover text");
    assert!(!opaque(&with(12.0, 0.99, 6.0), 1.0), "translucent window");
    assert!(!opaque(&with(12.0, 0.86, 6.0), 1.0), "translucent window");
    assert!(!opaque(&with(12.0, 1.0, 6.0), 2.0), "24 physical px at 2x");
    assert!(opaque(&with(24.0, 1.0, 6.0), 2.0));
    assert!(!opaque(&with(12.0, 1.0, 20.0), 1.0), "a wide bar");
    assert!(opaque(&with(22.0, 1.0, 20.0), 1.0));
    // The default window on this platform is translucent.
    assert!(!opaque(&Config::default(), 1.0));
}

/// A pane narrower than the bar, or a short one, keeps the channel, the
/// thumb, the hit strip and the command ticks inside it at 1x and 2x, so
/// nothing spills into the pane beside it, and the ticks stay on the bar.
#[test]
fn narrow_and_short_panes_keep_the_bar_inside() {
    let mark = kettle_core::term::CommandMark {
        row: 50,
        exit_code: 1,
    };
    for mode in [
        ScrollbarMode::Auto,
        ScrollbarMode::Always,
        ScrollbarMode::Never,
    ] {
        for width in [6.0, 40.0] {
            let cfg = config(mode, width);
            for scale in [1.0_f32, 2.0] {
                for (w, h) in [
                    (3.0, 920.0),
                    (8.0, 920.0),
                    (12.0, 920.0),
                    (30.0, 920.0),
                    (90.0, 920.0),
                    (800.0, 12.0),
                    (800.0, 0.5),
                    (5.0, 5.0),
                ] {
                    let terminal = (40.0 * scale, 30.0 * scale, w * scale, h * scale);
                    let (x, y, w, h) = terminal;
                    let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
                        rx >= x
                            && ry >= y
                            && rw >= 0.0
                            && rh >= 0.0
                            && rx + rw <= x + w + 1e-3
                            && ry + rh <= y + h + 1e-3
                    };
                    for (off, interacting) in [(0, false), (0, true), (SCROLLED_BACK, false)] {
                        let bar = pane_scrollbar_geometry(
                            terminal,
                            scale,
                            &cfg,
                            ROWS,
                            HISTORY,
                            off,
                            interacting,
                        );
                        let context =
                            format!("{mode:?} width {width} scale {scale} pane {w}x{h} off {off}");
                        assert!(inside(bar.track_rect), "{context}: {:?}", bar.track_rect);
                        assert!(inside(bar.hit_rect), "{context}: {:?}", bar.hit_rect);
                        assert!(
                            (bar.hit_rect.0 + bar.hit_rect.2 - (x + w)).abs() < 1e-3,
                            "{context}: the strip reaches the edge"
                        );
                        if let Some(thumb) = bar.thumb_rect {
                            assert!(inside(thumb), "{context}: thumb {thumb:?}");
                            assert_eq!((thumb.0, thumb.2), (bar.track_rect.0, bar.track_rect.2));
                        }
                        let column = (bar.track_rect.0, bar.track_rect.2);
                        let [tx, _, tw, _] = kettle_core::scrollbar::command_tick_rect(
                            terminal, column, scale, 0, 100, mark,
                        )
                        .expect("a tick");
                        assert_eq!((tx, tw), column, "{context}: the tick lies on the bar");
                    }
                }
            }
        }
    }
}

/// A pane with no width or height, or no scale, draws no bar and holds no
/// pointer strip.
#[test]
fn an_empty_pane_draws_no_bar() {
    for mode in [ScrollbarMode::Auto, ScrollbarMode::Always] {
        let cfg = config(mode, 6.0);
        for (w, h, scale) in [
            (0.0, 920.0, 1.0),
            (-5.0, 920.0, 2.0),
            (800.0, 0.0, 1.0),
            (800.0, -1.0, 2.0),
            (800.0, 920.0, 0.0),
            (f32::NAN, 920.0, 1.0),
        ] {
            let bar = pane_scrollbar_geometry(
                (40.0, 30.0, w, h),
                scale,
                &cfg,
                ROWS,
                HISTORY,
                SCROLLED_BACK,
                true,
            );
            let context = format!("{mode:?} {w}x{h} scale {scale}");
            assert!(!bar.channel, "{context}");
            assert_eq!(bar.thumb_rect, None, "{context}");
            assert_eq!(
                (bar.track_rect.2, bar.track_rect.3),
                (0.0, 0.0),
                "{context}"
            );
            assert_eq!((bar.hit_rect.2, bar.hit_rect.3), (0.0, 0.0), "{context}");
            assert!(bar.track_rect.0 >= 40.0, "{context}");
        }
    }
}
