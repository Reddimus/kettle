//! How the Settings panel paints its state: the category it marks as open,
//! the focused row and the row under the pointer.

use super::settings::{SettingsTextKind, settings_colors, settings_text_color};
use super::*;

fn panel(query: &str) -> SettingsOverlay {
    SettingsOverlay {
        categories: (0..4).map(|i| format!("Category {i}")).collect(),
        active_category: 2,
        title: "Category 2".into(),
        search_query: query.into(),
        reset_label: "Reset".into(),
        rows: (0..4)
            .map(|i| SettingsRow {
                label: format!("Setting {i}"),
                value: "14 px".into(),
                description: "Description".into(),
                category: i % 4,
                reset: Some(true),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// While a search lists results from every category, no category is
/// marked as open, in its fill or in its ink.
#[test]
fn a_search_marks_no_category_as_open() {
    assert_eq!(panel("").highlighted_category(), Some(2));
    assert_eq!(panel("font").highlighted_category(), None);
    let theme = kettle_config::Theme::by_name("Kettle Day");
    let mut colors = chrome::Colors::default();
    let palette = settings_colors(&theme, theme.accent, true, &mut colors);
    let ink = |set: &SettingsOverlay, index, colors: &mut chrome::Colors| {
        settings_text_color(
            set,
            SettingsTextKind::Category(index),
            &palette,
            (&theme, theme.accent, theme.foreground),
            colors,
        )
    };
    let browsing = panel("");
    let searching = panel("font");
    assert_eq!(
        ink(&browsing, 2, &mut colors),
        colors.text(theme.foreground, palette.selected)
    );
    assert_eq!(
        ink(&searching, 2, &mut colors),
        ink(&searching, 1, &mut colors)
    );
    // The painter fills only the highlighted category.
    let source: String = crate::production_source().split_whitespace().collect();
    assert!(source.contains(
        "ifletSome(Some(r))=set.highlighted_category().and_then(|index|layout.category_rects.get(index))"
    ));
    assert!(!source.contains("layout.category_rects.get(set.active_category)"));
}

/// The focused row's fill, and the open category's, stand out from the
/// panel by 1.3:1 in every bundled theme, light and dark, in both blend
/// modes and with the theme's, a palette and a GNOME desktop's accent, so
/// the fill marks focus along with its bar; Kettle Day's was 1.07:1. The
/// row's label keeps 4.5:1 on it, its description the text floor, and the
/// bar 3:1 against the panel and the fill. A fill that already stood out
/// keeps the light tint it had.
#[test]
fn the_focused_row_fill_stands_out_in_every_theme() {
    use super::chrome::{ICON_CONTRAST, TEXT_CONTRAST};
    use super::color::contrast_ratio as ratio;
    use super::settings::{SETTINGS_SELECTED_ALPHA, SETTINGS_SELECTED_CONTRAST};
    let mut colors = chrome::Colors::default();
    let mut failures = Vec::new();
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        let bg = theme.background;
        for linear in [true, false] {
            for accent in [
                theme.accent,
                theme.palette[4],
                kettle_config::Rgb::new(0xbb, 0x99, 0xf8),
            ] {
                let palette = settings_colors(&theme, accent, linear, &mut colors);
                let at = format!("{name} linear={linear} accent={accent:?}");
                let fill = ratio(palette.selected, bg);
                if fill + 0.005 < SETTINGS_SELECTED_CONTRAST {
                    failures.push(format!("{at}: fill {fill:.3}"));
                }
                let light = color::over(bg, accent, SETTINGS_SELECTED_ALPHA, linear);
                if ratio(light, bg) >= SETTINGS_SELECTED_CONTRAST && palette.selected != light {
                    failures.push(format!("{at}: a fill that stood out changed"));
                }
                let label = colors.text(theme.foreground, palette.selected);
                for (role, ink, floor) in [
                    ("label", label, TEXT_CONTRAST),
                    ("description", palette.description_selected, TEXT_CONTRAST),
                    ("bar on the fill", palette.focus, ICON_CONTRAST),
                ] {
                    if ratio(ink, palette.selected) + 0.005 < floor {
                        failures.push(format!("{at}: {role} {:.3}", ratio(ink, palette.selected)));
                    }
                }
                if ratio(palette.focus, bg) + 0.005 < ICON_CONTRAST {
                    failures.push(format!("{at}: bar {:.3}", ratio(palette.focus, bg)));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let day = kettle_config::Theme::by_name("Kettle Day");
    let palette = settings_colors(&day, day.accent, true, &mut colors);
    assert!(ratio(palette.selected, day.background) >= SETTINGS_SELECTED_CONTRAST - 0.005);
}
