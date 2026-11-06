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
