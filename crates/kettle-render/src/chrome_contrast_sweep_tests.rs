//! Chrome contrast floors that the 6.0.0 macOS release comparison measured
//! short, swept over every bundled theme.
//!
//! The comparison measured these on TokyoNight Night at the default 0.86
//! opacity: the active tab's Cmd+2 hint at 3.15:1, its close glyph at 4.88:1,
//! its title at 7.35:1, an unfocused window's title at 2.54:1, the search
//! bar's "No matches" at 4.45:1, the jump button's label at 4.503:1 and the
//! Settings control outlines at 1.51:1.
//!
//! On screen, a surface left at alpha `a` is the stored pixel plus `1 - a` of
//! whatever is behind the window. The stored pixel is `a * colour` on a plain
//! target, or the sRGB encoding of `a` times its linear light on an sRGB one.
//! The oracle below writes that arithmetic out on its own, over a black and a
//! white backdrop, which bound every backdrop in between. Every element is
//! swept over every bundled theme at the macOS default (0.86), the Linux
//! default (0.99) and an opaque window.
//!
//! Chrome text on a surface the backdrop shows through is floored against
//! both extremes; a surface on which no ink could reach its floor that way is
//! drawn more opaque: the 98% search bar opaque, the active tab only on the
//! few mid-tone themes that need it.

use crate::chrome::{self, Colors, ICON_CONTRAST, TEXT_CONTRAST};
use crate::color::{self, Backing};
use crate::{SEARCH_FOCUSED_FILL_ALPHA, SearchControl, search, settings};
use kettle_config::{Rgb, Theme};

const OPACITIES: [f64; 3] = [0.86, 0.99, 1.0];

fn to_linear(value: u8) -> f64 {
    let c = f64::from(value) / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(light: f64) -> f64 {
    255.0
        * if light <= 0.0031308 {
            light * 12.92
        } else {
            1.055 * light.powf(1.0 / 2.4) - 0.055
        }
}

/// Every colour `surface` at `alpha` can take on screen at the extremes: both
/// stored forms, each over black and over `white`. Opaque, it is itself.
fn on_screen(surface: Rgb, alpha: f64, white: [f64; 3]) -> Vec<Rgb> {
    if alpha >= 1.0 {
        return vec![surface];
    }
    let mut out = Vec::new();
    for srgb_target in [false, true] {
        for backdrop in [[0.0; 3], white] {
            let channel = |c: u8, backdrop: f64| {
                let stored = if srgb_target {
                    encode(alpha * to_linear(c))
                } else {
                    alpha * f64::from(c)
                };
                (stored + (1.0 - alpha) * backdrop)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            out.push(Rgb::new(
                channel(surface.r, backdrop[0]),
                channel(surface.g, backdrop[1]),
                channel(surface.b, backdrop[2]),
            ));
        }
    }
    out
}

/// The weakest contrast `ink` has against any of `colours` or anything
/// between them in luminance.
fn weakest(ink: Rgb, colours: &[Rgb]) -> f64 {
    let luminance: Vec<f64> = colours
        .iter()
        .map(|c| color::relative_luminance(*c))
        .collect();
    let low = luminance.iter().copied().fold(f64::INFINITY, f64::min);
    let high = luminance.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let ink_luminance = color::relative_luminance(ink);
    if ink_luminance > low && ink_luminance < high {
        return 1.0;
    }
    colours
        .iter()
        .map(|c| color::contrast_ratio(ink, *c))
        .fold(f64::INFINITY, f64::min)
}

/// The measured ratio when `ink` misses `floor` over `colours`.
fn shortfall(ink: Rgb, colours: &[Rgb], floor: f64) -> Option<f64> {
    let ratio = weakest(ink, colours);
    (ratio < floor).then_some(ratio)
}

fn report(failures: &[String], what: &str) {
    assert!(
        failures.is_empty(),
        "{} {what} failures, first: {}",
        failures.len(),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
}

/// The oracle reproduces the comparison's measurements from the inks 6.0.0
/// painted, so the sweeps below measure what the capture measured.
#[test]
fn the_oracle_reproduces_the_macos_capture() {
    let theme = Theme::by_name("TokyoNight Night");
    let (bg, fg) = (theme.background, theme.foreground);
    let mut colors = Colors::default();
    let title = colors.text(fg, bg);
    let hint = colors.muted(fg, bg);
    let hint = colors.tab_text(hint, bg, true);
    let close = colors.secondary(fg, bg);
    let close = colors.tab_text(close, bg, true);
    let unfocused = colors.tab_text(title, bg, false);
    // The translucent tab as captured, over a light desktop.
    let captured = Rgb::new(52, 55, 65);
    for (role, ink, measured) in [
        ("title", title, 7.35),
        ("hint", hint, 3.15),
        ("close", close, 4.88),
        ("unfocused title", unfocused, 2.54),
    ] {
        let ratio = color::contrast_ratio(ink, captured);
        assert!((ratio - measured).abs() < 0.01, "{role}: {ratio:.3}");
    }
    // The captured backing lies inside the modelled span at 0.86.
    let span = on_screen(bg, 0.86, color::PLAIN_WHITE);
    let luminance = color::relative_luminance(captured);
    assert!(
        span.iter()
            .any(|c| color::relative_luminance(*c) >= luminance)
    );
    assert!(
        span.iter()
            .any(|c| color::relative_luminance(*c) <= luminance)
    );
}

/// On macOS the window blur brightens a white backdrop past white. Over a
/// pure white bar the capture measured the TokyoNight Night tab at (61, 64, 74)
/// (most often) and (60, 63, 73), where plain white predicts (59, 60, 71), so
/// inks floored against plain white read about 4.4:1 there. The calibrated
/// white reproduces the brightest measured pixel, and the tab's inks, as the
/// painter resolves them on macOS, clear their floors against both pixels.
#[test]
fn tab_inks_clear_the_measured_over_white_pixel_on_macos() {
    let theme = Theme::by_name("TokyoNight Night");
    let (bg, fg) = (theme.background, theme.foreground);
    let measured = [Rgb::new(61, 64, 74), Rgb::new(60, 63, 73)];
    let brightest = |white| {
        on_screen(bg, 0.86, white)
            .into_iter()
            .max_by_key(|c| (c.r, c.g, c.b))
            .unwrap()
    };
    let plain = brightest(color::PLAIN_WHITE);
    assert_eq!(
        plain,
        Rgb::new(59, 60, 71),
        "plain white undershoots the capture"
    );
    let blurred = brightest(color::MACOS_BLURRED_WHITE);
    assert!(
        blurred.r >= 61 && blurred.g >= 64 && blurred.b >= 74,
        "the calibrated white must reach the brightest measured pixel: {blurred:?}"
    );
    assert_eq!(
        color::PLATFORM_WHITE,
        if cfg!(target_os = "macos") {
            color::MACOS_BLURRED_WHITE
        } else {
            color::PLAIN_WHITE
        }
    );
    let mut colors = Colors::default();
    let alpha = chrome::text_surface_alpha(bg, 0.86, color::MACOS_BLURRED_WHITE);
    assert_eq!(alpha, 0.86);
    let backing = Backing::translucent(bg, alpha, color::MACOS_BLURRED_WHITE);
    let rest = colors.secondary(fg, bg);
    for focused in [true, false] {
        for (role, ink, floor) in [
            (
                "title",
                colors.tab_title(fg, bg, backing, true, focused),
                TEXT_CONTRAST,
            ),
            (
                "hint",
                colors.tab_hint(fg, bg, backing, focused),
                TEXT_CONTRAST,
            ),
            (
                "close",
                colors.tab_close(rest, bg, backing, focused),
                if focused {
                    TEXT_CONTRAST
                } else {
                    ICON_CONTRAST
                },
            ),
        ] {
            for pixel in measured {
                let ratio = color::contrast_ratio(ink, pixel);
                assert!(
                    ratio >= floor,
                    "focused={focused} {role} {ink:?} reads {ratio:.3}:1 on {pixel:?}"
                );
            }
        }
    }
}

/// The active tab's title, Cmd+N hint and close glyph, and every other tab's,
/// in focused and unfocused windows. An unfocused window still fades them,
/// but a title and a hint name the tab, so they keep the text floor.
#[test]
fn tab_labels_keep_their_floors_over_any_backdrop() {
    let mut colors = Colors::default();
    let mut failures = Vec::new();
    let mut raised = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let (bg, fg) = (theme.background, theme.foreground);
        // The strip the other tabs sit on, as the default config resolves it.
        let strip = crate::resolve_chrome_bg(&kettle_config::Config::default(), &theme, None);
        for (window_alpha, white) in OPACITIES.into_iter().flat_map(|alpha| {
            [
                (alpha, color::PLAIN_WHITE),
                (alpha, color::MACOS_BLURRED_WHITE),
            ]
        }) {
            // As the painter does: the active horizontal tab is the terminal
            // surface, made more opaque only where no ink could pass.
            let alpha = chrome::text_surface_alpha(bg, window_alpha, white);
            if alpha > window_alpha {
                raised.push(format!("{name}@{window_alpha}/{white:?}->{alpha:.3}"));
            }
            let backing = Backing::translucent(bg, alpha, white);
            let screen = on_screen(bg, alpha, white);
            for focused in [true, false] {
                let rest = colors.secondary(fg, bg);
                let cases = [
                    (
                        "active title",
                        colors.tab_title(fg, bg, backing, true, focused),
                        screen.clone(),
                        TEXT_CONTRAST,
                    ),
                    (
                        "active hint",
                        colors.tab_hint(fg, bg, backing, focused),
                        screen.clone(),
                        TEXT_CONTRAST,
                    ),
                    (
                        "active close",
                        colors.tab_close(rest, bg, backing, focused),
                        screen.clone(),
                        if focused {
                            TEXT_CONTRAST
                        } else {
                            ICON_CONTRAST
                        },
                    ),
                    (
                        "background title",
                        colors.tab_title(fg, strip, Backing::opaque(strip), false, focused),
                        vec![strip],
                        TEXT_CONTRAST,
                    ),
                    (
                        "background hint",
                        colors.tab_hint(fg, strip, Backing::opaque(strip), focused),
                        vec![strip],
                        TEXT_CONTRAST,
                    ),
                ];
                for (role, ink, behind, floor) in cases {
                    if let Some(ratio) = shortfall(ink, &behind, floor) {
                        failures.push(format!(
                            "{name} @{window_alpha} white={white:?} focused={focused} \
                             {role}: {ratio:.3}"
                        ));
                    }
                }
            }
        }
    }
    report(&failures, "tab label");
    // Only mid-tone themes need the more opaque tab, and only at 0.86.
    assert!(raised.len() <= 8, "{raised:?}");
    assert!(raised.iter().all(|r| r.contains("@0.86/")), "{raised:?}");
}

/// What already passes stays as it was: a focused window's tab inks do not
/// move on an opaque window, and at 0.99 they move only where the old ink
/// fell short over some backdrop, judged with a rounding step to spare.
#[test]
fn tab_labels_that_already_pass_are_unchanged() {
    let mut colors = Colors::default();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let (bg, fg) = (theme.background, theme.foreground);
        let before = {
            let title = colors.text(fg, bg);
            let hint = colors.muted(fg, bg);
            let close = colors.secondary(fg, bg);
            [
                colors.tab_text(title, bg, true),
                colors.tab_text(hint, bg, true),
                colors.tab_text(close, bg, true),
            ]
        };
        for window_alpha in [0.99, 1.0] {
            let alpha = chrome::text_surface_alpha(bg, window_alpha, color::PLAIN_WHITE);
            let backing = Backing::translucent(bg, alpha, color::PLAIN_WHITE);
            let rest = colors.secondary(fg, bg);
            let after = [
                colors.tab_title(fg, bg, backing, true, true),
                colors.tab_hint(fg, bg, backing, true),
                colors.tab_close(rest, bg, backing, true),
            ];
            let screen: Vec<Rgb> = on_screen(bg, alpha, color::PLAIN_WHITE)
                .into_iter()
                .flat_map(|c| {
                    let step = |d: i16| {
                        let f = |v: u8| (i16::from(v) + d).clamp(0, 255) as u8;
                        Rgb::new(f(c.r), f(c.g), f(c.b))
                    };
                    if alpha < 1.0 {
                        vec![step(-1), c, step(1)]
                    } else {
                        vec![c]
                    }
                })
                .collect();
            for (old, new) in before.into_iter().zip(after) {
                if shortfall(old, &screen, TEXT_CONTRAST).is_none() {
                    assert_eq!(old, new, "{name} @{window_alpha}");
                }
            }
        }
    }
}

/// Every search-bar label on every segment fill. Over a translucent terminal
/// the bar is painted opaque, so what is behind the window cannot reach the
/// labels; over an opaque one it is the theme grey at 0.98, as before.
#[test]
fn search_bar_text_keeps_the_floor_over_any_backdrop() {
    let mut colors = Colors::default();
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let accent = theme.palette[3];
        for window_alpha in OPACITIES {
            for linear in [true, false] {
                let (bar, bar_alpha) =
                    search::search_bar_layer(&theme, window_alpha < 1.0, linear, &mut colors);
                // The bar as the GPU blends it over each extreme of the
                // terminal on screen.
                let bars: Vec<Rgb> =
                    on_screen(theme.background, window_alpha, color::MACOS_BLURRED_WHITE)
                        .into_iter()
                        .map(|base| color::over(base, bar, bar_alpha, linear))
                        .collect();
                for control in [None, Some(SearchControl::Editor), Some(SearchControl::Next)] {
                    for focused in [SearchControl::Editor, SearchControl::Next] {
                        for tint in [
                            None,
                            Some((theme.foreground, 0.14)),
                            Some((theme.background, 0.45)),
                        ] {
                            let painted = search::search_segment_fill(
                                &theme,
                                accent,
                                control,
                                focused,
                                tint,
                                linear,
                                &mut colors,
                            );
                            let mut layers = Vec::new();
                            match control {
                                Some(SearchControl::Editor) => {
                                    layers.push((theme.background, 0.72))
                                }
                                Some(control) => {
                                    layers.push(if focused == control {
                                        (accent, f64::from(SEARCH_FOCUSED_FILL_ALPHA))
                                    } else {
                                        (theme.background, 0.28)
                                    });
                                    if let Some((tint, alpha)) = tint {
                                        layers.push((tint, f64::from(alpha)));
                                    }
                                }
                                None => {}
                            }
                            let screen: Vec<Rgb> = bars
                                .iter()
                                .map(|bar| {
                                    layers.iter().fold(*bar, |under, (top, alpha)| {
                                        color::over(under, *top, *alpha, linear)
                                    })
                                })
                                .collect();
                            // The match count and status ("No matches") are
                            // red on the bar itself; buttons use the theme
                            // text, or the cursor text on the focused one.
                            let inks = match control {
                                None => vec![theme.foreground, theme.palette[1]],
                                Some(control)
                                    if control == focused && control != SearchControl::Editor =>
                                {
                                    vec![theme.cursor_text]
                                }
                                Some(_) => vec![theme.foreground],
                            };
                            for ink in inks {
                                let ink = colors.text(ink, painted);
                                if let Some(ratio) = shortfall(ink, &screen, TEXT_CONTRAST) {
                                    failures.push(format!(
                                        "{name} @{window_alpha} linear={linear} \
                                         {control:?}/{focused:?} {tint:?}: {ratio:.3}"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    report(&failures, "search label");
}

/// On an opaque window the bar is the theme grey at 0.98, as it always was,
/// and each segment's fill is the composite the labels were resolved on.
#[test]
fn an_opaque_window_keeps_its_search_bar() {
    let mut colors = Colors::default();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            assert_eq!(
                search::search_bar_layer(&theme, false, linear, &mut colors),
                (theme.palette[8], 0.98)
            );
            let painted = search::search_segment_fill(
                &theme,
                theme.palette[3],
                Some(SearchControl::Editor),
                SearchControl::Editor,
                None,
                linear,
                &mut colors,
            );
            let bar = color::over(theme.background, theme.palette[8], 0.98, linear);
            assert_eq!(painted, color::over(bar, theme.background, 0.72, linear));
        }
    }
}

/// The jump-to-bottom label clears the text floor at the painted stem
/// pixel, so it does not pass by a rounding step. The pill is opaque, so the
/// window's opacity does not reach it.
#[test]
fn the_jump_button_label_clears_the_floor_at_its_stems() {
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            let (fill, ink) = crate::scrollback::scrollback_pill_colors(&theme, linear);
            let stem = color::over(fill, ink, 248.0 / 255.0, linear);
            let ratio = color::contrast_ratio(stem, fill);
            if ratio < TEXT_CONTRAST {
                failures.push(format!("{name} linear={linear}: {ratio:.3}"));
            }
        }
    }
    report(&failures, "jump button");
}

/// Field, choice and switch outlines, and an off switch's knob, are at least
/// 3:1 against the panel, and on the selected row against its fill as well.
/// The panel is opaque, so the window's opacity does not reach them.
#[test]
fn settings_control_outlines_reach_three_to_one() {
    let mut colors = Colors::default();
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for linear in [true, false] {
            for accent in [theme.accent, theme.palette[4]] {
                let palette = settings::settings_colors(&theme, accent, linear, &mut colors);
                let selected = color::over(theme.background, accent, 0.18, linear);
                for (role, outline, surface) in [
                    ("outline on the panel", palette.control, theme.background),
                    (
                        "selected-row outline inside",
                        palette.selected_control,
                        theme.background,
                    ),
                    (
                        "selected-row outline around",
                        palette.selected_control,
                        selected,
                    ),
                ] {
                    let ratio = color::contrast_ratio(outline, surface);
                    if ratio < ICON_CONTRAST {
                        failures.push(format!("{name} linear={linear} {role}: {ratio:.3}"));
                    }
                }
                // The panel's own outline and divider stay as they were.
                assert_eq!(
                    palette.border,
                    color::over(theme.background, theme.palette[8], 0.65, linear)
                );
            }
        }
    }
    report(&failures, "settings outline");
}

/// The painter takes these colours from the helpers the sweeps test.
#[test]
fn the_painter_uses_the_swept_helpers() {
    let source: String = crate::production_source().split_whitespace().collect();
    for needle in [
        "letactive_tab_alpha=chrome::text_surface_alpha(default_bg,composed_bg_alpha(cfg),color::PLATFORM_WHITE);",
        "letactive_tab_backing=color::Backing::translucent(default_bg,active_tab_alpha,color::PLATFORM_WHITE);",
        "(default_bg,active_tab_alpha asf32),",
        "lettranslucent_window=composed_bg_alpha(cfg)<1.0;",
        "lettab_backing=ifs.active&&!cfg.tab_bar_pos.is_vertical(){active_tab_backing}else{color::Backing::opaque(tab_background)};",
        "lettitle_fg=self.chrome_colors.tab_title(",
        "let(bar_fill,bar_alpha)=search::search_bar_layer(theme,translucent_window,self.config.format.is_srgb(),&mutself.chrome_colors,);",
        "geometry.rect.3,bar_fill,bar_alpha asf32,",
        "letbackground=search::search_segment_fill(",
        "scrollback::scrollback_pill_colors(theme,self.config.format.is_srgb());",
        "}=settings::settings_colors(",
        "letfill=ifon{on_ink}else{control};",
        "letcontrol=ifrow_selected{selected_control}else{control};",
    ] {
        let needle: String = needle.split_whitespace().collect();
        assert!(source.contains(&needle), "missing: {needle}");
    }
    // Control outlines use the floored stroke; only the panel and the
    // sidebar divider keep the quiet border.
    assert_eq!(source.matches("border,1.0,6.0);").count(), 1);
    assert!(!source.contains("{acc}else{border}"));
}
