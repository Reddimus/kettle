use super::{GColor, Rgb, Style, Weight, font_features, run_attrs};
use glyphon::Family;
use kettle_config::Config;

/// `run_attrs` must map the SGR bits: color from the resolved fg, BOLD →
/// `Weight::BOLD`, ITALIC → `Style::Italic`, and the family routed through
/// `cfg.family_for` so configured bold/italic font variants keep working.
#[test]
fn run_attrs_maps_color_weight_style_and_family() {
    let cfg = Config::default();
    let ff = font_features(&cfg);

    let plain = run_attrs(&cfg, &ff, Rgb::new(10, 20, 30), false, false);
    assert_eq!(plain.color_opt, Some(GColor::rgb(10, 20, 30)));
    assert_eq!(plain.weight, Weight::NORMAL);
    assert_eq!(plain.style, Style::Normal);
    assert_eq!(plain.family, Family::Name(cfg.family_for(false, false)));

    let bold_italic = run_attrs(&cfg, &ff, Rgb::new(40, 50, 60), true, true);
    assert_eq!(bold_italic.color_opt, Some(GColor::rgb(40, 50, 60)));
    assert_eq!(bold_italic.weight, Weight::BOLD);
    assert_eq!(bold_italic.style, Style::Italic);
    assert_eq!(bold_italic.family, Family::Name(cfg.family_for(true, true)));
}

/// Drift guard: identical run tuples must produce EQUAL `Attrs`, and
/// differing tuples UNEQUAL ones. The per-line cache's `set_text` second
/// guard compares `AttrsList`s, so per-call variation would defeat the
/// cache and re-shape every row every frame, and false equality would keep
/// stale styling.
#[test]
fn run_attrs_is_deterministic_and_distinguishes_runs() {
    let cfg = Config::default();
    let ff = font_features(&cfg);

    let a = run_attrs(&cfg, &ff, Rgb::new(1, 2, 3), true, false);
    let b = run_attrs(&cfg, &ff, Rgb::new(1, 2, 3), true, false);
    assert_eq!(a, b);

    let other_color = run_attrs(&cfg, &ff, Rgb::new(9, 2, 3), true, false);
    assert_ne!(a, other_color);
    let other_weight = run_attrs(&cfg, &ff, Rgb::new(1, 2, 3), false, false);
    assert_ne!(a, other_weight);
}
