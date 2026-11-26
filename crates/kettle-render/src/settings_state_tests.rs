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
