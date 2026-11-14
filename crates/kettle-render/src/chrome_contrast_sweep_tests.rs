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
//! whatever is behind the window. The stored pixel is `a * colour` with
//! straight alpha or on a plain target, or the sRGB encoding of `a` times its
//! linear light when premultiplied on an sRGB one. The oracle below writes
//! that arithmetic out on its own, over a black and a white backdrop (on
//! macOS both lifted as measured), which bound every backdrop in between.
//! Every element is swept over every bundled theme at the opacities in
//! `OPACITIES`.
//!
//! Chrome text on a surface the backdrop shows through is floored against
//! both extremes. Where no ink can reach its floor that way, the surface is
//! drawn more opaque: the 98% search bar fully, the active tab by at most
//! `MAX_TEXT_SURFACE_RAISE` and only over a solid background. Past that the
//! text is best effort.

use crate::chrome::{self, Colors, ICON_CONTRAST, TEXT_CONTRAST};
use crate::color::{self, Backing};
use crate::{SEARCH_FOCUSED_FILL_ALPHA, SearchControl, search, settings};
use kettle_config::{Rgb, Theme};

/// The macOS default, a lower setting, 0.86 at `background-darkness`'s
/// default 0.5 (a wallpaper's tint on macOS), the Linux default and opaque.
const OPACITIES: [f64; 5] = [0.86, 0.7, 0.43, 0.99, 1.0];

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
/// stored forms, each over a black and a white backdrop raised by `lift`.
/// Opaque, it is itself.
fn on_screen(surface: Rgb, alpha: f64, lift: [f64; 3]) -> Vec<Rgb> {
    if alpha >= 1.0 {
        return vec![surface];
    }
    let mut out = Vec::new();
    for srgb_target in [false, true] {
        for white in [0.0, 255.0] {
            let channel = |c: u8, lift: f64| {
                let stored = if srgb_target {
                    encode(alpha * to_linear(c))
                } else {
                    alpha * f64::from(c)
                };
                (stored + (1.0 - alpha) * (white + lift))
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            out.push(Rgb::new(
                channel(surface.r, lift[0]),
                channel(surface.g, lift[1]),
                channel(surface.b, lift[2]),
            ));
        }
    }
    out
}

/// `colours`, each also one 8-bit level darker and lighter.
fn with_rounding(colours: &[Rgb]) -> Vec<Rgb> {
    colours
        .iter()
        .flat_map(|c| {
            let step = |d: i16| {
                let f = |v: u8| (i16::from(v) + d).clamp(0, 255) as u8;
                Rgb::new(f(c.r), f(c.g), f(c.b))
            };
            [step(-1), *c, step(1)]
        })
        .collect()
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

/// Whether white or black clears `floor` over `colours`.
fn reachable(colours: &[Rgb], floor: f64) -> bool {
    [Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]
        .into_iter()
        .any(|end| weakest(end, colours) >= floor)
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
    let span = on_screen(bg, 0.86, color::NO_BACKDROP_LIFT);
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

/// macOS shows a translucent window about +(3, 5, 5) brighter than the plain
/// composite at both ends. Over the capture's black bar the TokyoNight Night
/// terminal measured (25, 28, 38), and over its white bar (61, 64, 74) most
/// often and (60, 63, 73) next; without the lift the model predicts
/// (22, 23, 33) and at most (59, 60, 71), so inks floored against it read
/// about 4.4:1 over white. With the lift the model reproduces the black-bar
/// pixel and reaches the white-bar one, and the tab's inks, as the painter
/// resolves them on macOS, clear their floors against all three pixels.
#[test]
fn tab_inks_clear_the_measured_macos_pixels() {
    let theme = Theme::by_name("TokyoNight Night");
    let (bg, fg) = (theme.background, theme.foreground);
    let measured = [
        Rgb::new(25, 28, 38),
        Rgb::new(61, 64, 74),
        Rgb::new(60, 63, 73),
    ];
    let ends = |lift| {
        let span = on_screen(bg, 0.86, lift);
        let key = |c: &Rgb| (c.r, c.g, c.b);
        (
            *span.iter().min_by_key(|c| key(c)).unwrap(),
            *span.iter().max_by_key(|c| key(c)).unwrap(),
        )
    };
    assert_eq!(
        ends(color::NO_BACKDROP_LIFT),
        (Rgb::new(22, 23, 33), Rgb::new(59, 60, 71)),
        "without the lift the model undershoots both bars"
    );
    let (dark, light) = ends(color::MACOS_BACKDROP_LIFT);
    assert_eq!(dark, measured[0], "the lift reproduces the black bar");
    assert!(
        light.r >= 61 && light.g >= 64 && light.b >= 74,
        "the lift must reach the brightest white-bar pixel: {light:?}"
    );
    assert_eq!(
        color::PLATFORM_BACKDROP_LIFT,
        if cfg!(target_os = "macos") {
            color::MACOS_BACKDROP_LIFT
        } else {
            color::NO_BACKDROP_LIFT
        }
    );
    let mut colors = Colors::default();
    let alpha = chrome::text_surface_alpha(bg, 0.86, color::MACOS_BACKDROP_LIFT, true);
    assert_eq!(alpha, 0.86);
    let backing = Backing::translucent(bg, alpha, color::MACOS_BACKDROP_LIFT);
    // The painter's model spans the same: the black bar at its dark end, the
    // white bar inside its light end.
    assert_eq!(backing.over_black, measured[0]);
    let light = backing.over_white;
    assert!(
        light.r >= 61 && light.g >= 64 && light.b >= 74,
        "the painter's light end must reach the white bar: {light:?}"
    );
    let rest = colors.secondary(fg, bg);
    let cases = [
        (
            "title",
            colors.tab_title(fg, bg, backing, true, true),
            TEXT_CONTRAST,
        ),
        ("hint", colors.tab_hint(fg, bg, backing), TEXT_CONTRAST),
        (
            "close",
            colors.tab_close(rest, bg, backing, true),
            TEXT_CONTRAST,
        ),
        (
            "unfocused title",
            colors.tab_title(fg, bg, backing, true, false),
            TEXT_CONTRAST,
        ),
        (
            "unfocused close",
            colors.tab_close(rest, bg, backing, false),
            ICON_CONTRAST,
        ),
    ];
    for (role, ink, floor) in cases {
        for pixel in measured {
            let ratio = color::contrast_ratio(ink, pixel);
            assert!(
                ratio >= floor,
                "{role} {ink:?} reads {ratio:.3}:1 on {pixel:?}"
            );
        }
    }
}

/// The active tab's title, Cmd+N hint and close glyph, and every other tab's,
/// in focused and unfocused windows, with and without the macOS lift, over a
/// solid background and over Kettle's own wallpapers. An unfocused window
/// still fades titles, but they name the tab, so they keep the text floor; it
/// paints no hints. Where no ink can reach a floor, even with the tab raised
/// as far as allowed, the ink goes as far as it can: to white or black.
#[test]
fn tab_labels_keep_their_floors_over_any_backdrop() {
    let mut colors = Colors::default();
    let mut failures = Vec::new();
    let mut raised = Vec::new();
    let mut best_effort = 0;
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let (bg, fg) = (theme.background, theme.foreground);
        // The strip the other tabs sit on, as the default config resolves it.
        let strip = crate::resolve_chrome_bg(&kettle_config::Config::default(), &theme, None);
        for window_alpha in OPACITIES {
            for lift in [color::NO_BACKDROP_LIFT, color::MACOS_BACKDROP_LIFT] {
                for solid in [true, false] {
                    // As the painter does: the active horizontal tab is the
                    // terminal surface, raised a little only over a solid
                    // background and only where no ink could pass.
                    let alpha = chrome::text_surface_alpha(bg, window_alpha, lift, solid);
                    assert!(
                        alpha <= window_alpha + chrome::MAX_TEXT_SURFACE_RAISE + 1e-9,
                        "{name} @{window_alpha}: raised to {alpha}"
                    );
                    if !solid {
                        assert_eq!(alpha, window_alpha, "{name}: raised over a wallpaper");
                    }
                    if alpha > window_alpha {
                        raised.push(format!("{name}@{window_alpha}/{lift:?}->{alpha:.3}"));
                    }
                    let backing = Backing::translucent(bg, alpha, lift);
                    let screen = on_screen(bg, alpha, lift);
                    for focused in [true, false] {
                        let rest = colors.secondary(fg, bg);
                        let mut cases = vec![
                            (
                                "active title",
                                colors.tab_title(fg, bg, backing, true, focused),
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
                        ];
                        if focused {
                            cases.push((
                                "active hint",
                                colors.tab_hint(fg, bg, backing),
                                screen.clone(),
                                TEXT_CONTRAST,
                            ));
                            cases.push((
                                "background hint",
                                colors.tab_hint(fg, strip, Backing::opaque(strip)),
                                vec![strip],
                                TEXT_CONTRAST,
                            ));
                        }
                        for (role, ink, behind, floor) in cases {
                            let Some(ratio) = shortfall(ink, &behind, floor) else {
                                continue;
                            };
                            let at_an_end =
                                ink == Rgb::new(0, 0, 0) || ink == Rgb::new(255, 255, 255);
                            if !reachable(&behind, floor) && at_an_end {
                                best_effort += 1;
                                continue;
                            }
                            failures.push(format!(
                                "{name} @{window_alpha} lift={lift:?} solid={solid} \
                                 focused={focused} {role}: {ratio:.3}"
                            ));
                        }
                    }
                }
            }
        }
    }
    report(&failures, "tab label");
    // The default 0.86 raises only a few mid-tone themes.
    let at_default = raised.iter().filter(|r| r.contains("@0.86/")).count();
    assert!(at_default <= 8, "{raised:?}");
    eprintln!(
        "raised: {} ({at_default} at 0.86); best effort: {best_effort}",
        raised.len()
    );
}

/// An ink moved for a translucent surface clears its floor with an 8-bit
/// level to spare at each end, wherever its side of the surface can: the GPU
/// and the window server each round the composite.
#[test]
fn inks_moved_for_the_backdrop_keep_a_rounding_step_to_spare() {
    let mut colors = Colors::default();
    let mut failures = Vec::new();
    let mut checked = 0;
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let (bg, fg) = (theme.background, theme.foreground);
        for window_alpha in [0.86, 0.99] {
            let alpha = chrome::text_surface_alpha(bg, window_alpha, color::NO_BACKDROP_LIFT, true);
            let backing = Backing::translucent(bg, alpha, color::NO_BACKDROP_LIFT);
            let screen = on_screen(bg, alpha, color::NO_BACKDROP_LIFT);
            let rounded = with_rounding(&screen);
            let before = colors.muted(fg, bg);
            let hint = colors.tab_hint(fg, bg, backing);
            if hint == before {
                continue;
            }
            let side = if color::relative_luminance(hint) > color::relative_luminance(bg) {
                Rgb::new(255, 255, 255)
            } else {
                Rgb::new(0, 0, 0)
            };
            if weakest(side, &rounded) < TEXT_CONTRAST {
                continue;
            }
            checked += 1;
            if let Some(ratio) = shortfall(hint, &rounded, TEXT_CONTRAST) {
                failures.push(format!("{name} @{window_alpha}: {ratio:.3}"));
            }
        }
    }
    assert!(checked > 500, "only {checked} moved inks checked");
    report(&failures, "rounding spare");
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
            let alpha = chrome::text_surface_alpha(bg, window_alpha, color::NO_BACKDROP_LIFT, true);
            let backing = Backing::translucent(bg, alpha, color::NO_BACKDROP_LIFT);
            let rest = colors.secondary(fg, bg);
            let after = [
                colors.tab_title(fg, bg, backing, true, true),
                colors.tab_hint(fg, bg, backing),
                colors.tab_close(rest, bg, backing, true),
            ];
            let screen = on_screen(bg, alpha, color::NO_BACKDROP_LIFT);
            let screen = if alpha < 1.0 {
                with_rounding(&screen)
            } else {
                screen
            };
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
                    on_screen(theme.background, window_alpha, color::MACOS_BACKDROP_LIFT)
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

/// The jump button's colours take two bisections, so the painter resolves
/// them once per theme, not every frame: a later frame reads what is saved.
#[test]
fn the_jump_button_colours_are_resolved_once_per_theme() {
    let mut colors = Colors::default();
    let night = Theme::by_name("TokyoNight Night");
    let resolved = crate::scrollback::scrollback_pill_colors(&night, true);
    assert_eq!(colors.scrollback_pill(&night, true), resolved);
    // Mark what was saved; only a frame that reads it back returns the mark.
    let marked = (Rgb::new(1, 2, 3), Rgb::new(4, 5, 6));
    let (key, _) = colors.pill.expect("the first frame saves its colours");
    colors.pill = Some((key, marked));
    assert_eq!(colors.scrollback_pill(&night, true), marked);
    // Another theme or blend mode resolves afresh.
    let day = Theme::by_name("TokyoNight Day");
    assert_eq!(
        colors.scrollback_pill(&day, true),
        crate::scrollback::scrollback_pill_colors(&day, true)
    );
    assert_eq!(
        colors.scrollback_pill(&day, false),
        crate::scrollback::scrollback_pill_colors(&day, false)
    );
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
        "letactive_tab_alpha=chrome::text_surface_alpha(default_bg,composed_bg_alpha(cfg),color::PLATFORM_BACKDROP_LIFT,cfg.background_type==kettle_config::BackgroundType::Solid,);",
        "letactive_tab_backing=color::Backing::translucent(default_bg,active_tab_alpha,color::PLATFORM_BACKDROP_LIFT,);",
        "ifoverlay.window_focused&&letSome((hint_rect,_))=&s.hint",
        "(default_bg,active_tab_alpha asf32),",
        "lettranslucent_window=composed_bg_alpha(cfg)<1.0;",
        "lettab_backing=ifpainted_active&&!cfg.tab_bar_pos.is_vertical(){active_tab_backing}else{color::Backing::opaque(tab_background)};",
        "lettitle_fg=self.chrome_colors.tab_title(",
        "let(bar_fill,bar_alpha)=search::search_bar_layer(theme,translucent_window,self.config.format.is_srgb(),&mutself.chrome_colors,);",
        "geometry.rect.3,bar_fill,bar_alpha asf32,",
        "letbackground=search::search_segment_fill(",
        "let(fill,ink)=self.chrome_colors.scrollback_pill(theme,self.config.format.is_srgb());",
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
