use super::{
    CellHighlight, MinimumContrastCache, Rgb, attributed_foreground, color,
    resolved_cell_foreground,
};
use alacritty_terminal::vte::ansi::Color as AnsiColor;
use kettle_config::{Config, Theme};

/// `minimum-contrast` must survive `bold-is-bright`.
///
/// If the lift runs first, `bold_is_bright` then replaces the foreground
/// outright with a palette entry, so the guarantee does nothing for bold
/// text under `bold-is-bright`, the common configuration. The bright
/// variant is the LIGHTER one, so the case it discards is exactly the one
/// that needs the lift.
#[test]
fn the_contrast_lift_sees_the_colour_bold_is_bright_actually_produces() {
    let mut theme = Theme {
        background: Rgb::new(0xe8, 0xe8, 0xe8),
        ..Theme::default()
    };
    // A pale background, a low-palette colour that ALREADY meets the
    // ratio against it (9.44:1), and a bright variant that badly does not
    // (1.01:1).
    //
    // The compliant base makes this sensitive to ordering: lifting it
    // before the remap is a no-op and leaves the pale bright variant
    // unreadable.
    theme.palette[2] = Rgb::new(0x20, 0x40, 0x20);
    theme.palette[10] = Rgb::new(0xd8, 0xf0, 0xd8);
    let bg = theme.background;
    let fg = theme.palette[2];
    let ansi_fg = AnsiColor::Indexed(2);

    let cfg = Config {
        bold_is_bright: true,
        minimum_contrast: 4.5,
        ..Config::default()
    };

    // The bright variant on its own is far below the requested ratio —
    // otherwise this fixture would prove nothing.
    let bright = color::resolve(color::bright_for_bold(ansi_fg), &theme, &Default::default());
    assert_eq!(bright, theme.palette[10], "the fixture must actually remap");
    assert!(
        color::contrast_ratio(bright, bg) < 4.5,
        "the bright variant must start unreadable, got {:.2}",
        color::contrast_ratio(bright, bg)
    );

    let drawn = attributed_foreground(ansi_fg, bg, false, true, &cfg, &theme);
    assert!(
        color::contrast_ratio(drawn, bg) >= 4.5 - 1e-6,
        "minimum-contrast must hold for bold text under bold-is-bright, \
             got {:.2} from {drawn:?}",
        color::contrast_ratio(drawn, bg)
    );

    // Non-bold skips the remap, so the compliant base colour is left as
    // it is rather than lifted away from what the theme asked for.
    assert!(
        color::contrast_ratio(fg, bg) >= 4.5,
        "the fixture's base colour must already be compliant, got {:.2}",
        color::contrast_ratio(fg, bg)
    );
    assert_eq!(
        attributed_foreground(ansi_fg, bg, false, false, &cfg, &theme),
        fg,
        "a compliant non-bold colour must be left alone"
    );

    // With the lift off, bold-is-bright still does its job untouched.
    let no_lift = Config {
        bold_is_bright: true,
        ..Config::default()
    };
    assert_eq!(
        attributed_foreground(ansi_fg, bg, false, true, &no_lift, &theme),
        bright,
        "with minimum-contrast off, the bright variant must be what is drawn"
    );

    // Dim composes first, and the lift still applies to its result.
    let dimmed = attributed_foreground(ansi_fg, bg, true, false, &no_lift, &theme);
    assert_eq!(
        dimmed,
        color::dim(fg, bg),
        "dim must apply to the resolved fg"
    );
    assert!(
        color::contrast_ratio(
            attributed_foreground(ansi_fg, bg, true, false, &cfg, &theme),
            bg
        ) >= 4.5 - 1e-6,
        "a dimmed foreground must still be lifted to the configured ratio"
    );
}

#[test]
fn minimum_contrast_uses_the_background_painted_under_highlights() {
    let mut theme = Theme {
        background: Rgb::new(0, 0, 0),
        foreground: Rgb::new(255, 255, 255),
        selection_background: Rgb::new(255, 255, 255),
        selection_foreground: Rgb::new(255, 255, 255),
        ..Theme::default()
    };
    theme.palette[3] = Rgb::new(255, 255, 255);
    let cfg = Config {
        minimum_contrast: 4.5,
        search_background: Some(Rgb::new(255, 255, 255)),
        search_foreground: Some(Rgb::new(255, 255, 255)),
        ..Config::default()
    };
    let base_fg = Rgb::new(255, 255, 255);
    let base_bg = Rgb::new(0, 0, 0);

    let ordinary = resolved_cell_foreground(
        base_fg,
        base_bg,
        CellHighlight::None,
        false,
        false,
        &cfg,
        &theme,
    );
    assert_eq!(ordinary, base_fg);

    for (name, highlight, painted_bg) in [
        (
            "selection",
            CellHighlight::Selection,
            theme.selection_background,
        ),
        (
            "inactive search",
            CellHighlight::Search(false),
            theme.selection_background,
        ),
        (
            "active search",
            CellHighlight::Search(true),
            cfg.search_background.expect("search background"),
        ),
    ] {
        let drawn =
            resolved_cell_foreground(base_fg, base_bg, highlight, false, false, &cfg, &theme);
        assert!(
            color::contrast_ratio(drawn, painted_bg) >= 4.5 - 1e-6,
            "{name} must meet contrast against its final painted background; got {drawn:?}"
        );
    }
}

#[test]
fn search_and_selection_foregrounds_ignore_underlying_dim_and_bold_attributes() {
    let mut theme = Theme {
        selection_background: Rgb::new(10, 20, 30),
        selection_foreground: Rgb::new(90, 100, 110),
        ..Theme::default()
    };
    // The explicit search RGB matches a low palette entry but must keep
    // its own color even when the underlying cell is bold.
    theme.palette[2] = Rgb::new(20, 80, 20);
    theme.palette[10] = Rgb::new(180, 250, 180);
    let cfg = Config {
        bold_is_bright: true,
        search_background: Some(Rgb::new(40, 50, 60)),
        search_foreground: Some(theme.palette[2]),
        ..Config::default()
    };
    let base_fg = Rgb::new(200, 210, 220);
    let base_bg = Rgb::new(1, 2, 3);

    for (name, highlight) in [
        ("active search", CellHighlight::Search(true)),
        ("inactive search", CellHighlight::Search(false)),
        ("selection", CellHighlight::Selection),
    ] {
        let expected =
            resolved_cell_foreground(base_fg, base_bg, highlight, false, false, &cfg, &theme);
        for (dim, bold) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                resolved_cell_foreground(base_fg, base_bg, highlight, dim, bold, &cfg, &theme,),
                expected,
                "{name} must override the cell's DIM={dim} BOLD={bold} attributes"
            );
        }
    }

    assert_ne!(
        color::dim(
            cfg.search_foreground.unwrap(),
            cfg.search_background.unwrap()
        ),
        cfg.search_foreground.unwrap(),
        "the DIM fixture must visibly change the configured search foreground"
    );
    assert_ne!(
        color::resolve(
            color::bright_for_bold(AnsiColor::Indexed(2)),
            &theme,
            &Default::default(),
        ),
        cfg.search_foreground.unwrap(),
        "the bold-is-bright fixture must visibly remap the configured search foreground"
    );
}

#[test]
fn minimum_contrast_cache_computes_each_colour_pair_once_per_frame() {
    let fg = Rgb::new(0x77, 0x77, 0x77);
    let bg = Rgb::new(0x78, 0x78, 0x78);
    let mut cache = MinimumContrastCache::default();

    let first = cache.resolve(fg, bg, 4.5);
    for _ in 0..10_000 {
        assert_eq!(cache.resolve(fg, bg, 4.5), first);
    }

    assert_eq!(cache.misses, 1);
    assert_eq!(cache.len(), 1);
    cache.clear();
    assert_eq!(cache.len(), 0);
}

#[test]
#[ignore = "release-mode diagnostic benchmark"]
fn minimum_contrast_cache_benchmark() {
    let pairs = [
        (Rgb::new(0x77, 0x77, 0x77), Rgb::new(0x78, 0x78, 0x78)),
        (Rgb::new(0x88, 0x88, 0x88), Rgb::new(0x89, 0x89, 0x89)),
        (Rgb::new(0x99, 0x99, 0x99), Rgb::new(0x9a, 0x9a, 0x9a)),
        (Rgb::new(0xaa, 0xaa, 0xaa), Rgb::new(0xab, 0xab, 0xab)),
    ];
    let iterations = 100_000;

    let started = std::time::Instant::now();
    let mut uncached = Rgb::new(0, 0, 0);
    for index in 0..iterations {
        let (fg, bg) = pairs[index % pairs.len()];
        uncached = std::hint::black_box(color::with_min_contrast(fg, bg, 4.5));
    }
    let uncached_elapsed = started.elapsed();

    let started = std::time::Instant::now();
    let mut cache = MinimumContrastCache::default();
    let mut cached = Rgb::new(0, 0, 0);
    for index in 0..iterations {
        let (fg, bg) = pairs[index % pairs.len()];
        cached = std::hint::black_box(cache.resolve(fg, bg, 4.5));
    }
    let cached_elapsed = started.elapsed();

    assert_eq!(cached, uncached);
    eprintln!(
        "minimum-contrast {iterations} cells, {} pairs: uncached={uncached_elapsed:?}, cached={cached_elapsed:?}, misses={}",
        pairs.len(),
        cache.misses,
    );
}

#[test]
fn automatic_highlight_text_is_readable_in_every_bundled_palette() {
    let cfg = Config::default();
    for name in Theme::list() {
        let bundled = Theme::by_name(name);
        // Exercise automatic foregrounds with every bundled palette, including
        // themes that normally supply their own selection foreground.
        let hex = |c: Rgb| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
        let theme = Theme::parse(&format!(
            "background = {}\nforeground = {}\nselection-background = {}\npalette = 3={}\npalette = 4={}\npalette = 8={}\n",
            hex(bundled.background),
            hex(bundled.foreground),
            hex(bundled.selection_background),
            hex(bundled.palette[3]),
            hex(bundled.palette[4]),
            hex(bundled.palette[8]),
        ));
        for (label, fg, dim) in [
            ("default foreground", theme.foreground, false),
            ("dim palette", theme.palette[8], true),
            ("underlined link", theme.palette[4], false),
        ] {
            for (kind, highlight) in [
                ("selection", CellHighlight::Selection),
                ("search match", CellHighlight::Search(false)),
                ("current match", CellHighlight::Search(true)),
            ] {
                let (_, fill) = super::theme::highlight_colors_cached(
                    highlight,
                    &cfg,
                    &theme,
                    fg,
                    theme.background,
                    &mut MinimumContrastCache::default(),
                );
                let drawn = resolved_cell_foreground(
                    fg,
                    theme.background,
                    highlight,
                    dim,
                    false,
                    &cfg,
                    &theme,
                );
                assert!(
                    color::contrast_ratio(drawn, fill) >= 4.5,
                    "{name}: {label} on {kind} has contrast {}",
                    color::contrast_ratio(drawn, fill),
                );
                if label == "underlined link" {
                    let mut quads = Vec::new();
                    super::terminal::push_cell_decorations(
                        &mut quads,
                        (0.0, 0.0, 8.0, 16.0),
                        0,
                        alacritty_terminal::term::cell::Flags::UNDERLINE,
                        drawn,
                        drawn,
                    );
                    assert_eq!(quads.len(), 1);
                    let [r, g, b, _] = quads[0].color;
                    let underline = Rgb::new(
                        (r * 255.0).round() as u8,
                        (g * 255.0).round() as u8,
                        (b * 255.0).round() as u8,
                    );
                    assert!(color::contrast_ratio(underline, fill) >= 4.5);
                }
                if color::contrast_ratio(fg, fill) >= 4.5 {
                    assert_eq!(drawn, fg, "{name}: readable {label} on {kind}");
                } else {
                    let best = [
                        theme.selection_foreground,
                        theme.foreground,
                        theme.background,
                        Rgb::new(255, 255, 255),
                        Rgb::new(0, 0, 0),
                    ]
                    .into_iter()
                    .map(|ink| color::contrast_ratio(ink, fill))
                    .fold(0.0, f64::max);
                    assert_eq!(
                        color::contrast_ratio(drawn, fill),
                        best,
                        "{name}: strongest fallback on {kind}"
                    );
                }
            }
        }
    }
}

#[test]
fn automatic_highlight_text_reaches_contrast_when_theme_neutrals_cannot() {
    let theme = Theme::parse(
        "background = #777777\nforeground = #777777\nselection-background = #777777\n",
    );
    let cfg = Config {
        search_background: Some(theme.background),
        ..Config::default()
    };
    for highlight in [
        CellHighlight::Selection,
        CellHighlight::Search(false),
        CellHighlight::Search(true),
    ] {
        let (_, fill) = super::theme::highlight_colors_cached(
            highlight,
            &cfg,
            &theme,
            theme.foreground,
            theme.background,
            &mut MinimumContrastCache::default(),
        );
        assert!(color::contrast_ratio(theme.background, fill) < 4.5);
        assert!(color::contrast_ratio(theme.foreground, fill) < 4.5);
        let drawn = resolved_cell_foreground(
            theme.foreground,
            theme.background,
            highlight,
            false,
            false,
            &cfg,
            &theme,
        );
        assert!(color::contrast_ratio(drawn, fill) >= 4.5);
    }
}

#[test]
fn highlight_foregrounds_prefer_readable_user_ink_and_strong_fallbacks() {
    // Neither the theme's pale ink nor the cell is readable on white.
    // Choose the strongest fallback for selections and search matches.
    let theme = Theme::parse("selection-background = #ffffff\nselection-foreground = #eeeeee\n");
    let cfg = Config {
        search_background: Some(Rgb::new(255, 255, 255)),
        search_foreground: Some(Rgb::new(240, 240, 240)),
        ..Config::default()
    };
    for highlight in [CellHighlight::Selection, CellHighlight::Search(false)] {
        let fg = resolved_cell_foreground(
            theme.foreground,
            theme.background,
            highlight,
            true,
            true,
            &cfg,
            &theme,
        );
        assert_ne!(fg, theme.selection_foreground);
        assert!(color::contrast_ratio(fg, Rgb::new(255, 255, 255)) >= 4.5);
    }
    let readable = Theme::parse("selection-background = #ffffff\nselection-foreground = #102030\n");
    assert_eq!(
        resolved_cell_foreground(
            readable.foreground,
            readable.background,
            CellHighlight::Selection,
            true,
            true,
            &cfg,
            &readable
        ),
        Rgb::new(0, 0, 0),
        "the strongest fallback wins over a merely readable theme foreground"
    );
    assert_eq!(
        resolved_cell_foreground(
            theme.foreground,
            theme.background,
            CellHighlight::Search(true),
            true,
            true,
            &cfg,
            &theme
        ),
        Rgb::new(0, 0, 0),
    );

    for text in [
        "selection-background = #ffffff\nselection-foreground = #eeeeee\ntheme = Dracula\n",
        "theme = Dracula\nselection-background = #ffffff\nselection-foreground = #eeeeee\n",
    ] {
        let cfg = Config::parse_text(text);
        for theme in [cfg.theme.clone(), cfg.theme_by_name("TokyoNight Day")] {
            for highlight in [CellHighlight::Selection, CellHighlight::Search(false)] {
                assert_eq!(
                    resolved_cell_foreground(
                        theme.foreground,
                        theme.background,
                        highlight,
                        false,
                        false,
                        &cfg,
                        &theme,
                    ),
                    Rgb::new(0, 0, 0),
                    "unreadable configured foreground must fall back even after a theme change",
                );
            }
        }
    }
}

#[test]
fn highlights_choose_maximum_contrast_after_user_and_cell_ink() {
    let cfg = Config::default();
    let mut theme = Theme {
        background: Rgb::new(100, 100, 100),
        foreground: Rgb::new(130, 130, 130),
        selection_background: Rgb::new(180, 180, 180),
        selection_foreground: Rgb::new(120, 120, 120),
        selection_foreground_user: true,
        ..Theme::default()
    };
    for highlight in [
        CellHighlight::Selection,
        CellHighlight::InactiveSelection,
        CellHighlight::Search(false),
        CellHighlight::Search(true),
    ] {
        let (fg, fill) = super::theme::highlight_colors_cached(
            highlight,
            &cfg,
            &theme,
            theme.foreground,
            theme.background,
            &mut MinimumContrastCache::default(),
        );
        let strongest = [
            theme.selection_foreground,
            theme.foreground,
            theme.background,
            Rgb::new(255, 255, 255),
            Rgb::new(0, 0, 0),
        ]
        .into_iter()
        .map(|c| color::contrast_ratio(c, fill))
        .fold(0.0, f64::max);
        assert_eq!(color::contrast_ratio(fg, fill), strongest);
        assert!(strongest >= 4.5);
    }
    theme.selection_foreground = Rgb::new(20, 20, 20);
    let (fg, fill) = super::theme::highlight_colors_cached(
        CellHighlight::Selection,
        &cfg,
        &theme,
        Rgb::new(0, 0, 0),
        theme.background,
        &mut MinimumContrastCache::default(),
    );
    assert!(color::contrast_ratio(theme.selection_foreground, fill) >= 4.5);
    assert_eq!(
        fg, theme.selection_foreground,
        "readable user ink precedes cell ink"
    );
}

#[test]
fn inactive_selection_moves_toward_background_and_keeps_aa_ink() {
    let cfg = Config::default();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let mut cache = MinimumContrastCache::default();
        let active = super::theme::selection_fill(&theme, theme.background, true, &mut cache);
        let inactive = super::theme::selection_fill(&theme, theme.background, false, &mut cache);
        // Plainly visible: at least 2:1 (1.5:1 nearly vanished on dark themes).
        assert!(
            color::contrast_ratio(inactive, theme.background) >= 2.0,
            "{name}"
        );
        assert!(
            color::contrast_ratio(inactive, theme.background)
                < color::contrast_ratio(active, theme.background),
            "{name}"
        );
        for ink in [theme.foreground, theme.palette[4], Rgb::new(150, 100, 120)] {
            let drawn = resolved_cell_foreground(
                ink,
                theme.background,
                CellHighlight::InactiveSelection,
                true,
                false,
                &cfg,
                &theme,
            );
            assert!(color::contrast_ratio(drawn, inactive) >= 4.5, "{name}");
        }
    }
}

/// Hue distance in sextants, the unit `color::hsl` reports.
fn hue_distance(a: f64, b: f64) -> f64 {
    let d = (a - b).abs();
    d.min(6.0 - d)
}

/// Neither default theme's selection colour reaches 3:1 on its background.
/// Blending it toward white or black got there by greying it (TokyoNight
/// Night's `#283457` became a slate `#5d6781`), the muted fill a UX panel
/// found hard to pick out; the fill is now a stronger tint of the same hue.
#[test]
fn default_theme_selections_become_stronger_tints_with_readable_text() {
    let cfg = Config::default();
    for name in [cfg.dark_theme.as_str(), cfg.light_theme.as_str()] {
        let theme = cfg.theme_by_name(name);
        let background = theme.background;
        assert!(
            color::contrast_ratio(theme.selection_background, background) < 3.0,
            "{name}: the fixture must need strengthening"
        );
        let mut cache = MinimumContrastCache::default();
        let fill = super::theme::selection_fill(&theme, background, true, &mut cache);
        assert!(
            color::contrast_ratio(fill, background) >= 3.0,
            "{name}: {fill:?} has {:.2}:1",
            color::contrast_ratio(fill, background)
        );
        let (hue, saturation, _) = color::hsl(theme.selection_background);
        let (fill_hue, fill_saturation, _) = color::hsl(fill);
        assert!(
            hue_distance(hue, fill_hue) < 0.05,
            "{name}: {fill:?} drifted from the theme's hue"
        );
        assert!(
            fill_saturation >= saturation - 0.02,
            "{name}: {fill:?} greyed the tint ({fill_saturation:.2} < {saturation:.2})"
        );
        for ink in [theme.foreground, theme.palette[4], theme.palette[8]] {
            let text = resolved_cell_foreground(
                ink,
                background,
                CellHighlight::Selection,
                false,
                false,
                &cfg,
                &theme,
            );
            assert!(
                color::contrast_ratio(text, fill) >= 4.5,
                "{name}: {text:?} on {fill:?}"
            );
        }
        let inactive = super::theme::selection_fill(&theme, background, false, &mut cache);
        let inactive_contrast = color::contrast_ratio(inactive, background);
        assert!(inactive_contrast >= 2.0, "{name}: {inactive_contrast:.2}");
        assert!(
            inactive_contrast < color::contrast_ratio(fill, background),
            "{name}: the unfocused fill must stay weaker"
        );
    }
}

#[test]
fn every_bundled_selection_keeps_its_hue_or_its_colour() {
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let selection = theme.selection_background;
        let fill = super::theme::selection_fill(
            &theme,
            theme.background,
            true,
            &mut MinimumContrastCache::default(),
        );
        if color::contrast_ratio(selection, theme.background) >= 3.0 {
            assert_eq!(fill, selection, "{name}: a separate colour is kept");
            continue;
        }
        assert!(
            color::contrast_ratio(fill, theme.background) >= 3.0,
            "{name}"
        );
        let channels = [selection.r, selection.g, selection.b];
        // A near-grey's hue is rounding noise.
        if channels.iter().max().unwrap() - channels.iter().min().unwrap() < 16 {
            continue;
        }
        let (hue, saturation, _) = color::hsl(selection);
        let (fill_hue, fill_saturation, _) = color::hsl(fill);
        assert!(hue_distance(hue, fill_hue) < 0.05, "{name}: {fill:?}");
        assert!(fill_saturation >= saturation - 0.02, "{name}: {fill:?}");
    }
}

#[test]
fn selected_ansi_indexed_truecolour_link_and_decoration_ink_meets_aa() {
    let cfg = Config::default();
    let theme = Theme::default();
    for source in [
        AnsiColor::Named(alacritty_terminal::vte::ansi::NamedColor::Foreground),
        AnsiColor::Indexed(4),
        AnsiColor::Indexed(67),
        AnsiColor::Indexed(244),
        AnsiColor::Spec(alacritty_terminal::vte::ansi::Rgb {
            r: 127,
            g: 140,
            b: 160,
        }),
    ] {
        let ink = color::resolve(source, &theme, &Default::default());
        for highlight in [
            CellHighlight::Selection,
            CellHighlight::InactiveSelection,
            CellHighlight::Search(false),
            CellHighlight::Search(true),
        ] {
            let (_, fill) = super::theme::highlight_colors_cached(
                highlight,
                &cfg,
                &theme,
                ink,
                theme.background,
                &mut MinimumContrastCache::default(),
            );
            let drawn = resolved_cell_foreground(
                ink,
                theme.background,
                highlight,
                true,
                true,
                &cfg,
                &theme,
            );
            assert!(color::contrast_ratio(drawn, fill) >= 4.5);
        }
    }
}

/// A focused selection keeps its hue; an unfocused window's selection is a
/// neutral grey, in the row fill and in the per-cell fill a program-set
/// background gets, so losing focus changes its colour as well as its
/// strength, as macOS does.
#[test]
fn unfocused_selection_is_neutral_grey_in_every_bundled_theme() {
    use super::theme::{PaneSelectionFills, resolved_highlight_foreground_cached};
    let cfg = Config::default();
    let grey = |c: Rgb| c.r == c.g && c.g == c.b;
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for alpha in [1.0, 0.86] {
            let mut cache = MinimumContrastCache::default();
            let fills = PaneSelectionFills::new(&theme, theme.background, alpha, &mut cache);
            let row = fills.row(false);
            assert!(grey(row), "{name} at {alpha}: row fill {row:?}");
            let base = color::with_min_contrast_same_hue(
                theme.selection_background,
                theme.background,
                3.0,
            );
            let target = super::theme::selection_contrast_target(theme.background, base, alpha);
            // The focused fill strengthens away from the background as shown
            // and never crosses it: Grass's dark brown once turned orange.
            let side = |c: Rgb| color::relative_luminance(c) >= color::relative_luminance(target);
            assert_eq!(
                side(fills.row(true)),
                side(base),
                "{name} at {alpha}: {:?} crossed {target:?} from {base:?}",
                fills.row(true)
            );
            let ink = resolved_highlight_foreground_cached(
                theme.foreground,
                row,
                CellHighlight::InactiveSelection,
                &cfg,
                &theme,
                &mut cache,
            );
            let text = color::contrast_ratio(ink, row);
            assert!(
                text >= 4.5,
                "{name} at {alpha}: text {text:.2}:1 on {row:?}"
            );
            // 2:1 from the background as shown, unless on the next grey out
            // the selected text would fall below 4.5:1 or switch from light
            // to dark (or back), or the grey would come within 1.3:1 of the
            // focused fill.
            let shown = color::contrast_ratio(row, target);
            let lighter = color::relative_luminance(row) > color::relative_luminance(target);
            let next = if lighter {
                row.r.saturating_add(1)
            } else {
                row.r.saturating_sub(1)
            };
            let next = Rgb::new(next, next, next);
            let (white, black) = (Rgb::new(255, 255, 255), Rgb::new(0, 0, 0));
            let (ink_side, other_side) =
                if color::relative_luminance(ink) > color::relative_luminance(row) {
                    (white, black)
                } else {
                    (black, white)
                };
            let capped = color::contrast_ratio(ink_side, next) < 4.5
                || color::contrast_ratio(other_side, next) > color::contrast_ratio(ink_side, next)
                || color::contrast_ratio(fills.row(true), next) < 1.3;
            assert!(
                shown >= 2.0 || capped,
                "{name} at {alpha}: {row:?} is {shown:.2}:1 from {target:?}"
            );
            let own = color::contrast_ratio(row, theme.background);
            assert!(
                own >= 2.0,
                "{name} at {alpha}: {row:?} is {own:.2}:1 from the theme background"
            );
            for program_bg in [theme.background, theme.palette[1], theme.palette[4], row] {
                let cell = fills.cell(
                    CellHighlight::InactiveSelection,
                    &cfg,
                    &theme,
                    program_bg,
                    &mut cache,
                );
                assert!(
                    grey(cell),
                    "{name} at {alpha}: cell fill {cell:?} on {program_bg:?}"
                );
            }
        }
    }
}

/// TokyoNight Night at the macOS and Windows default opacity of 86%, over a
/// white page: the background shows as #3a3b44, against which the old fills
/// measured 1.95:1 focused and 1.31:1 unfocused. The grey stops short of 2:1
/// there so that it stays 1.3:1 quieter than the focused fill.
#[test]
fn selections_are_measured_against_a_see_through_background_as_shown() {
    use super::theme::{PaneSelectionFills, selection_contrast_target};
    let cfg = Config::default();
    let theme = cfg.theme_by_name("TokyoNight Night");
    let mut cache = MinimumContrastCache::default();
    let fills = PaneSelectionFills::new(&theme, theme.background, 0.86, &mut cache);
    let target = selection_contrast_target(theme.background, fills.row(true), 0.86);
    assert_eq!(target, Rgb::new(58, 59, 68));
    let focused = color::contrast_ratio(fills.row(true), target);
    let unfocused = color::contrast_ratio(fills.row(false), target);
    assert!(
        focused >= 2.4,
        "focused {:?} at {focused:.2}:1",
        fills.row(true)
    );
    assert!(
        unfocused >= 1.8,
        "unfocused {:?} at {unfocused:.2}:1",
        fills.row(false)
    );
    assert_eq!(fills.row(true), Rgb::new(93, 115, 180));
    assert_eq!(fills.row(false), Rgb::new(99, 99, 99));
    let (hue, saturation, _) = color::hsl(theme.selection_background);
    let (fill_hue, fill_saturation, _) = color::hsl(fills.row(true));
    assert!(hue_distance(hue, fill_hue) < 0.05);
    assert!(fill_saturation >= saturation - 0.02);
}

/// Text on any selection fill stays at 4.5:1, see-through or not, and a dark
/// theme's selected text stays light.
#[test]
fn selected_text_stays_readable_on_see_through_and_opaque_fills() {
    use super::theme::{PaneSelectionFills, resolved_highlight_foreground_cached};
    let cfg = Config::default();
    for theme in [
        cfg.theme_by_name("TokyoNight Night"),
        cfg.theme_by_name("Kettle Day"),
        Theme::default(),
    ] {
        for alpha in [1.0, 0.99, 0.86, 0.5] {
            let mut cache = MinimumContrastCache::default();
            let fills = PaneSelectionFills::new(&theme, theme.background, alpha, &mut cache);
            let unfocused = fills.row(false);
            assert!(
                unfocused.r == unfocused.g && unfocused.g == unfocused.b,
                "{unfocused:?} at {alpha}"
            );
            for (highlight, fill) in [
                (CellHighlight::Selection, fills.row(true)),
                (CellHighlight::InactiveSelection, fills.row(false)),
            ] {
                for ink in [theme.foreground, theme.palette[4], theme.palette[8]] {
                    let drawn = resolved_highlight_foreground_cached(
                        ink, fill, highlight, &cfg, &theme, &mut cache,
                    );
                    let ratio = color::contrast_ratio(drawn, fill);
                    assert!(ratio >= 4.5, "{drawn:?} on {fill:?} at {alpha}: {ratio:.2}");
                }
            }
            if theme.is_dark() {
                let ink = resolved_highlight_foreground_cached(
                    theme.foreground,
                    fills.row(true),
                    CellHighlight::Selection,
                    &cfg,
                    &theme,
                    &mut cache,
                );
                assert!(
                    color::relative_luminance(ink) > color::relative_luminance(fills.row(true)),
                    "a dark theme's selected text stays light at {alpha}"
                );
            }
        }
    }
}

/// Where nothing shows behind the window, the selection is the one the theme
/// gives against its own background: at full opacity, under macOS Reduce
/// Transparency, over the starfield and on an opaque surface.
#[test]
fn an_opaque_window_keeps_the_theme_selection() {
    use super::theme::{PaneSelectionFills, selection_backdrop_alpha};
    let mut cfg = Config::default();
    let theme = cfg.theme_by_name("TokyoNight Night");
    let fills = |alpha: f64| {
        PaneSelectionFills::new(
            &theme,
            theme.background,
            alpha,
            &mut MinimumContrastCache::default(),
        )
        .row(true)
    };
    assert_eq!(fills(1.0), Rgb::new(77, 100, 166));
    assert_ne!(
        fills(0.86),
        fills(1.0),
        "a see-through window is measured as shown"
    );
    cfg.background_opacity = 0.86;
    let pre = wgpu::CompositeAlphaMode::PreMultiplied;
    assert_eq!(
        selection_backdrop_alpha(&cfg, pre, false, false),
        0.86_f32 as f64
    );
    for alpha in [
        selection_backdrop_alpha(&cfg, pre, false, true),
        selection_backdrop_alpha(&cfg, wgpu::CompositeAlphaMode::Opaque, false, false),
    ] {
        assert_eq!(alpha, 1.0);
        assert_eq!(fills(alpha), Rgb::new(77, 100, 166));
    }
    cfg.background_type = kettle_config::BackgroundType::Starfield;
    assert_eq!(selection_backdrop_alpha(&cfg, pre, false, false), 1.0);
    cfg.background_type = kettle_config::BackgroundType::Image;
    assert_eq!(selection_backdrop_alpha(&cfg, pre, true, false), 1.0);
    assert!(selection_backdrop_alpha(&cfg, pre, false, false) < 1.0);
}

/// An unfocused selection is a grey at least 1.3:1 quieter than the focused
/// fill, between it and the background, in every bundled theme and at the
/// see-through opacities where the focused fill cannot reach 3:1: at 86%
/// Dracula's grey once matched its focused fill.
#[test]
fn unfocused_selection_stays_quieter_than_focused_in_every_bundled_theme() {
    use super::theme::PaneSelectionFills;
    assert_eq!(
        Theme::default().selection_background,
        Rgb::new(0x58, 0x5b, 0x70)
    );
    let between = |low: Rgb, mid: Rgb, high: Rgb| {
        let (low, mid, high) = (
            color::relative_luminance(low),
            color::relative_luminance(mid),
            color::relative_luminance(high),
        );
        (low < mid && mid < high) || (high < mid && mid < low)
    };
    let themes = Theme::list()
        .into_iter()
        .map(|name| (name.to_string(), Theme::by_name(name)))
        .chain([("default".to_string(), Theme::default())]);
    for (name, theme) in themes {
        for alpha in [1.0, 0.99, 0.86, 0.7, 0.65, 0.6, 0.5, 0.3] {
            let fills = PaneSelectionFills::new(
                &theme,
                theme.background,
                alpha,
                &mut MinimumContrastCache::default(),
            );
            let (focused, unfocused) = (fills.row(true), fills.row(false));
            assert!(
                unfocused.r == unfocused.g && unfocused.g == unfocused.b,
                "{name} at {alpha}: {unfocused:?}"
            );
            let ratio = color::contrast_ratio(focused, unfocused);
            assert!(
                ratio >= 1.3,
                "{name} at {alpha}: {focused:?} and {unfocused:?} are {ratio:.2}:1"
            );
            assert!(
                between(theme.background, unfocused, focused),
                "{name} at {alpha}: {unfocused:?} is not between {:?} and {focused:?}",
                theme.background
            );
        }
    }
}

/// A see-through window never turns a selection toward its background.
/// Over white, a dark theme's background shows lighter than its selection
/// below about 65% opacity; strengthening toward 3:1 against that turned
/// TokyoNight Night's selection a dark navy 1.26:1 from its own background.
#[test]
fn a_see_through_selection_keeps_its_side_of_the_theme_background() {
    use super::theme::PaneSelectionFills;
    let themes = Theme::list()
        .into_iter()
        .map(|name| (name.to_string(), Theme::by_name(name)))
        .chain([("default".to_string(), Theme::default())]);
    for (name, theme) in themes {
        let base =
            color::with_min_contrast_same_hue(theme.selection_background, theme.background, 3.0);
        let lighter =
            |c: Rgb| color::relative_luminance(c) > color::relative_luminance(theme.background);
        for alpha in [0.99, 0.86, 0.7, 0.65, 0.6, 0.5, 0.3] {
            let fills = PaneSelectionFills::new(
                &theme,
                theme.background,
                alpha,
                &mut MinimumContrastCache::default(),
            );
            let focused = fills.row(true);
            assert_eq!(
                lighter(focused),
                lighter(base),
                "{name} at {alpha}: {focused:?} crossed the background from {base:?}"
            );
            let (now, was) = (
                color::contrast_ratio(focused, theme.background),
                color::contrast_ratio(base, theme.background),
            );
            assert!(
                now >= was - 0.01,
                "{name} at {alpha}: {focused:?} is {now:.2}:1 from the background, {base:?} {was:.2}:1"
            );
        }
    }
}
