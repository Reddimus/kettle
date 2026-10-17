use super::{PreparedFonts, StartupFonts, production_source};
use kettle_config::Config;

/// A window's first renderer takes over the fonts its first pane was
/// sized with. Loading them again would enumerate the system fonts twice
/// at startup and could measure a different cell than the pane got. Fonts
/// measured for another scale are measured again from the same system
/// fonts; only a renderer given no fonts loads them.
#[test]
fn the_renderer_takes_over_matching_startup_fonts() {
    let src = production_source();
    // Every constructor, windowed or headless, ends in `with_gpu`.
    let body = src
        .split_once("fn with_gpu(")
        .expect("with_gpu")
        .1
        .split_once("\n    }\n")
        .expect("end of with_gpu")
        .0;
    assert!(body.contains("Some(fonts) if fonts.matches(cfg, scale) => fonts,"));
    assert!(body.contains("Some(fonts) => fonts.remeasure(cfg, scale),"));
    assert!(body.contains("None => StartupFonts::load(cfg, scale),"));
    assert_eq!(body.matches("StartupFonts::load(").count(), 1);
    assert!(!body.contains("FontSystem::new()"));
}

/// Fonts prepared before the scale is known measure the cell a direct
/// load measures, so the first pane's grid does not depend on which path
/// loaded them. The preload warms the compiled-in family while it waits
/// for the config, then the configured one. A family the system lacks
/// falls back the same way on both paths, and warming one family never
/// changes what another measures.
#[test]
fn prepared_fonts_measure_what_load_measures() {
    const MISSING: &str = "Kettle Test Family That Is Not Installed";
    let compiled_in = kettle_config::font::FAMILY;
    let default = Config::default();
    let missing = Config {
        font_family: MISSING.to_owned(),
        font_size: 17.0,
        ..Config::default()
    };
    for (cfg, warmed) in [
        (&default, &[compiled_in][..]),
        (&missing, &[compiled_in, MISSING][..]),
        (&default, &[MISSING][..]),
    ] {
        for scale in [1.0, 2.0] {
            let mut prepared = PreparedFonts::enumerate();
            for family in warmed {
                prepared.warm_family(family);
            }
            let fonts = prepared.measure(cfg, scale);
            let direct = StartupFonts::load(cfg, scale);
            assert!(fonts.matches(cfg, scale));
            assert_eq!(
                fonts.cell, direct.cell,
                "{:?} at {scale}x, {warmed:?} warmed",
                cfg.font_family
            );
            assert_eq!(fonts.text_symbol_family, direct.text_symbol_family);
        }
    }
}

/// The preload warms the compiled-in family before the config arrives,
/// and then the configured family, which is usually the same one. The
/// second call must not shape it again.
#[test]
fn warming_a_family_again_does_nothing() {
    let family = kettle_config::font::FAMILY;
    let mut prepared = PreparedFonts::enumerate();
    prepared.warm_family(family);
    prepared.warm_family(family);
    assert_eq!(prepared.warm, [family]);
}

/// A window that opens at another scale than its fonts were measured for
/// gets the cell a fresh load at that scale would measure.
#[test]
fn remeasure_matches_a_fresh_load_at_the_new_scale() {
    let cfg = Config::default();
    let fonts = StartupFonts::load(&cfg, 1.0).remeasure(&cfg, 2.0);
    assert!(fonts.matches(&cfg, 2.0));
    assert!(!fonts.matches(&cfg, 1.0));
    assert_eq!(fonts.cell, StartupFonts::load(&cfg, 2.0).cell);

    let mut larger = cfg.clone();
    larger.font_size += 3.0;
    let fonts = fonts.remeasure(&larger, 2.0);
    assert!(fonts.matches(&larger, 2.0));
    assert_eq!(fonts.cell, StartupFonts::load(&larger, 2.0).cell);
}

/// Only enumerating builds a font system; measuring again reuses it.
#[test]
fn remeasuring_never_enumerates_the_system_fonts() {
    fn body<'a>(src: &'a str, start: &str) -> &'a str {
        src.split_once(start)
            .unwrap_or_else(|| panic!("missing {start}"))
            .1
            .split_once("\n    }\n")
            .expect("end of fn")
            .0
    }
    let src = production_source();
    for start in [
        "pub fn remeasure(",
        "pub fn measure(",
        "pub fn warm_family(",
    ] {
        let body = body(&src, start);
        assert!(!body.contains("FontSystem::new"), "{start}");
        assert!(!body.contains("enumerate("), "{start}");
        assert!(!body.contains("StartupFonts::load("), "{start}");
    }
    assert!(body(&src, "pub fn enumerate(").contains("FontSystem::new()"));
}

/// Fonts measured for one scale or size are not reused for another.
#[test]
fn startup_fonts_match_only_their_own_font_and_scale() {
    let cfg = Config::default();
    let fonts = StartupFonts::load(&cfg, 2.0);
    assert!(fonts.matches(&cfg, 2.0));
    assert!(!fonts.matches(&cfg, 1.0));
    let mut larger = cfg.clone();
    larger.font_size += 2.0;
    assert!(!fonts.matches(&larger, 2.0));

    let one = StartupFonts::load(&cfg, 1.0);
    assert!(one.cell.0 > 0.0 && one.cell.1 > 0.0);
    assert!(
        (fonts.cell.1 - 2.0 * one.cell.1).abs() < 0.01,
        "the cell is measured in physical pixels: {:?} at 2x, {:?} at 1x",
        fonts.cell,
        one.cell
    );
}
