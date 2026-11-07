use super::dim_blend;
use crate::color::contrast_ratio;
use kettle_config::Rgb;

#[test]
fn dimmed_text_keeps_45_percent_of_the_foreground() {
    let white = Rgb::new(255, 255, 255);
    let black = Rgb::new(0, 0, 0);
    assert_eq!(dim_blend(white, black), Rgb::new(115, 115, 115));
}

/// Disabled rows and shortcut hints must stay readable. On TokyoNight
/// Night the old blend kept a sixth of the foreground: about 1.6:1.
#[test]
fn dimmed_text_stays_readable_on_a_dark_theme() {
    let fg = Rgb::parse("#c0caf5").unwrap();
    let bg = Rgb::parse("#1a1b26").unwrap();
    let ratio = contrast_ratio(dim_blend(fg, bg), bg);
    assert!(ratio >= 3.0, "{ratio:.2}:1");
    assert!(
        ratio < contrast_ratio(fg, bg),
        "still dimmer than normal text"
    );
}

#[test]
fn inactive_pane_text_keeps_a_text_contrast_floor() {
    let theme = kettle_config::Theme::by_name("Builtin Light");
    let dimmed = crate::chrome::Colors::default().inactive(
        theme.foreground,
        theme.background,
        theme.background,
        0.55,
        true,
    );
    let ratio = contrast_ratio(dimmed, theme.background);
    assert!(ratio >= 4.5, "inactive pane: {ratio:.3}:1 < 4.5");
}

#[test]
fn regression_media_layers_are_covered_by_their_pane_dimmer() {
    let source = crate::production_source();
    let pass = source
        .split("fn encode_scene_pass(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    let pane_dim = pass
        .find("self.pane_dimming.draw(")
        .expect("pane dimming must have its own pass after image and card surfaces");
    for layer in ["self.imgs.draw(", "self.card_base.draw(", "posters.draw("] {
        assert!(
            pass.find(layer).unwrap() < pane_dim,
            "{layer} must dim with the pane"
        );
    }
    let lane_dim = pass.find("self.overlay_quads.draw(").unwrap();
    for layer in [
        "self.lane_quads.draw(",
        "lanes.draw(",
        "self.lane_text_renderer",
    ] {
        assert!(
            pass.find(layer).unwrap() < lane_dim,
            "{layer} must dim with the lane"
        );
    }
    let build = source
        .split("        let mut pane_dim")
        .nth(1)
        .expect("separate pane dim draw list");
    assert!(
        build.contains("lane.pane == pv.id"),
        "lane dimming follows its owning pane"
    );
    assert!(
        build.contains("let (x, y, w, h) = lane.rect;"),
        "dimmer must cover the reserved lane outside the terminal"
    );
}

#[test]
fn regression_cell_decorations_paint_after_the_dimmer() {
    let source = crate::production_source();
    let pass = source
        .split("fn encode_scene_pass(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    let dim = pass
        .find("self.pane_dimming.draw(")
        .expect("separate dim pass");
    let decorations = pass
        .find("self.text_decorations.draw(")
        .expect("separate decoration pass");
    assert!(
        decorations > dim,
        "final inactive decoration colours must not be dimmed twice"
    );
    let cells = source.split("    fn build_pane_cells(").nth(1).unwrap();
    assert!(
        cells.contains("let decoration_quads = if !matches!(highlight, CellHighlight::None) {")
    );
    assert!(cells.contains("} else if inactive > 0.0 {"));
    assert!(cells.contains("decorations.extend(highlight_decorations)"));
    assert!(
        cells.contains("&mut *decorations\n            } else {\n                &mut *quads"),
        "active decorations retain their original ordering below the cursor"
    );
    let sink = cells
        .split("push_cell_decorations(")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .trim();
    assert_eq!(
        sink, "decoration_quads",
        "inactive decorations need the foreground draw list"
    );
}

#[test]
fn inactive_bundled_text_retains_contrast_after_raster_coverage() {
    use super::{Attrs, Family, Shaping, StartupFonts, SwashCache, TextBuffer};
    let mut checked = 0;
    for size in [13.0, 14.0, 15.0] {
        let cfg = kettle_config::Config {
            font_size: size,
            ..Default::default()
        };
        let mut fonts = StartupFonts::load(&cfg, 1.0);
        let mut buf = TextBuffer::new(&mut fonts.font_system, super::metrics_for(size, 1.0));
        buf.set_size(Some(400.0), Some(100.0));
        buf.set_text(
            "M",
            &Attrs::new().family(Family::Name(&cfg.font_family)),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut fonts.font_system, false);
        let mut cache = SwashCache::new();
        for run in buf.layout_runs() {
            for glyph in run.glyphs {
                for x in [0.0, 0.25, 0.5, 0.75] {
                    let image = cache
                        .get_image_uncached(
                            &mut fonts.font_system,
                            glyph.physical((x, 0.0), 1.0).cache_key,
                        )
                        .unwrap();
                    assert_eq!(image.content, glyphon::cosmic_text::SwashContent::Mask);
                    let coverage = *image.data.iter().max().unwrap();
                    let black = Rgb::new(0, 0, 0);
                    let white = Rgb::new(255, 255, 255);
                    for (fg, bg) in [(white, black), (black, white)] {
                        for linear in [false, true] {
                            checked += 1;
                            let ink =
                                crate::chrome::Colors::default().inactive(fg, bg, bg, 0.9, linear);
                            let painted =
                                crate::color::over(bg, ink, f64::from(coverage) / 255.0, linear);
                            let ratio = contrast_ratio(painted, bg);
                            assert!(
                                ratio >= 4.5,
                                "{size} px, phase {x}, coverage {coverage}, {bg:?}, linear={linear}: {ratio:.3}:1"
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        checked, 48,
        "all sizes, phases, polarities and blend modes must render"
    );
}
