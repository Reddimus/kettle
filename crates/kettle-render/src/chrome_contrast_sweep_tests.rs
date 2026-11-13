//! Chrome contrast floors that the 6.0.0 macOS release comparison measured
//! short, swept over every bundled theme.
//!
//! The comparison measured these on TokyoNight Night at the default 0.86
//! opacity: the search bar's "No matches" at 4.45:1, the jump button's label
//! at 4.503:1 and the Settings control outlines at 1.51:1.
//!
//! On screen, a surface left at alpha `a` is the stored pixel plus `1 - a` of
//! whatever is behind the window. The stored pixel is `a * colour` on a plain
//! target, or the sRGB encoding of `a` times its linear light on an sRGB one.
//! The oracle below writes that arithmetic out on its own, over a black and a
//! white backdrop, which bound every backdrop in between. Every element is
//! swept over every bundled theme at the macOS default (0.86), the Linux
//! default (0.99) and an opaque window.

use crate::chrome::{Colors, ICON_CONTRAST, TEXT_CONTRAST};
use crate::color;
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
/// stored forms, each over black and over white. Opaque, it is itself.
fn on_screen(surface: Rgb, alpha: f64) -> Vec<Rgb> {
    if alpha >= 1.0 {
        return vec![surface];
    }
    let mut out = Vec::new();
    for srgb_target in [false, true] {
        for backdrop in [0.0, 255.0] {
            let channel = |c: u8| {
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
                channel(surface.r),
                channel(surface.g),
                channel(surface.b),
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
                let bars: Vec<Rgb> = on_screen(theme.background, window_alpha)
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
        "lettranslucent_window=composed_bg_alpha(cfg)<1.0;",
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
