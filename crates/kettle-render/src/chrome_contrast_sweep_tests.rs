//! Chrome contrast floors that the 6.0.0 macOS release comparison measured
//! short, swept over every bundled theme.
//!
//! The comparison measured these on TokyoNight Night: the jump button's label
//! at 4.503:1, passing by a rounding step, and the Settings control outlines
//! at 1.51:1.

use crate::chrome::{Colors, ICON_CONTRAST, TEXT_CONTRAST};
use crate::color;
use crate::settings;
use kettle_config::Theme;

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
