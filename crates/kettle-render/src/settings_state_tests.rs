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
            &theme,
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

fn hover_panel() -> SettingsOverlay {
    let mut set = panel("");
    set.categories = (0..11).map(|i| format!("Category {i}")).collect();
    set.active_category = 2;
    for (index, (control, reset, disabled)) in [
        (SettingsControl::Choice, Some(true), false),
        (SettingsControl::Toggle(true), None, false),
        (SettingsControl::Value, Some(true), false),
        (SettingsControl::Choice, Some(true), true),
    ]
    .into_iter()
    .enumerate()
    {
        let row = &mut set.rows[index];
        (row.control, row.reset, row.disabled) = (control, reset, disabled);
    }
    set.rows[0].value = "Bar".into();
    set.focused_row = 1;
    set
}

fn center((x, y, w, h): Rect4) -> (f32, f32) {
    (x + w / 2.0, y + h / 2.0)
}

/// The pointer's hover target: a row that applies, its choice or an
/// enabled Reset, a category other than the open one, a strip arrow, and
/// nothing over the search field, a row that does not apply or outside.
#[test]
fn hover_finds_rows_controls_and_categories() {
    let set = hover_panel();
    for surface in [(1280.0, 820.0), (600.0, 820.0)] {
        let layout = settings_layout(&set, (8.4, 17.5), surface, 8.4, 1.0);
        let row = |index: usize| layout.rows.iter().find(|row| row.index == index).unwrap();
        let at = |point| layout.hover_at(&set, point);
        assert_eq!(at(center(row(1).label_rect)), Some(SettingsHover::Row(1)));
        assert_eq!(
            at(center(row(1).chip_rect)),
            Some(SettingsHover::Switch(1)),
            "the focused row's switch"
        );
        assert_eq!(at(center(row(0).chip_rect)), Some(SettingsHover::Choice(0)));
        assert_eq!(at(center(row(0).label_rect)), Some(SettingsHover::Row(0)));
        let reset = row(2).reset.expect("a changed row shows Reset");
        assert_eq!(at(center(reset.rect)), Some(SettingsHover::Reset(2)));
        assert_eq!(at(center(row(3).rect)), None, "a row that does not apply");
        assert_eq!(at(center(layout.search_rect)), None);
        assert_eq!(at((1.0, 1.0)), None);
        let open = layout.category_rects[2].expect("the open category shows");
        assert_eq!(at(center(open)), None);
        let (other, rect) = layout
            .category_rects
            .iter()
            .enumerate()
            .find_map(|(index, rect)| rect.filter(|_| index != 2).map(|rect| (index, rect)))
            .unwrap();
        assert_eq!(at(center(rect)), Some(SettingsHover::Category(other)));
        if surface.0 < 640.0 {
            let (_, next) = layout.category_next.expect("the strip overflows");
            assert_eq!(at(center(next)), Some(SettingsHover::Arrow(true)));
        }
        // While a search lists results, every category takes the prelight.
        let mut searching = set.clone();
        searching.search_query = "x".into();
        assert_eq!(
            layout.hover_at(&searching, center(open)),
            Some(SettingsHover::Category(2))
        );
    }
    // The Reset a focused row at its default shows dimmed acts on the row.
    let mut set = hover_panel();
    set.rows[2].reset = Some(false);
    set.focused_row = 2;
    let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
    let row = layout.rows.iter().find(|row| row.index == 2).unwrap();
    let reset = row.reset.expect("the focused row shows a dimmed Reset");
    assert!(!reset.enabled);
    assert_eq!(
        layout.hover_at(&set, center(reset.rect)),
        Some(SettingsHover::Row(2))
    );
}

/// The prelight is the theme's selection colour at 40% over the panel,
/// with no focus bar; the focused row keeps its own fill under the pointer,
/// and a row that does not apply takes none. Reset and a choice under the
/// pointer take a dialog button's 0.09 hover mix.
#[test]
fn hover_prelight_stays_apart_from_the_keyboard_focus() {
    use super::settings::{
        SETTINGS_HOVER_ALPHA, SettingsRowFill, settings_pressable, settings_row_fill,
        settings_row_surfaces,
    };
    let mut set = hover_panel();
    for (hover, row, fill) in [
        (SettingsHover::Row(0), 0, SettingsRowFill::Hovered),
        (SettingsHover::Choice(0), 0, SettingsRowFill::Hovered),
        (SettingsHover::Reset(2), 2, SettingsRowFill::Hovered),
        (SettingsHover::Row(1), 1, SettingsRowFill::Focused),
        (SettingsHover::Row(3), 3, SettingsRowFill::Panel),
        (SettingsHover::Category(0), 0, SettingsRowFill::Panel),
    ] {
        set.hover = Some(hover);
        assert_eq!(settings_row_fill(&set, row), fill, "{hover:?}");
        assert_eq!(set.focused_row, 1, "hover never moves the focus");
    }
    let theme = kettle_config::Theme::by_name("TokyoNight Night");
    let bg = theme.background;
    let mut colors = chrome::Colors::default();
    let palette = settings_colors(&theme, theme.accent, true, &mut colors);
    assert_eq!(
        palette.hover,
        color::over(bg, theme.selection_background, SETTINGS_HOVER_ALPHA, true)
    );
    assert_ne!(palette.hover, palette.selected);
    assert_eq!(settings_pressable(bg), super::confirm::button_mix(bg, 0.09));
    assert_ne!(settings_pressable(bg), bg);
    let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
    let row = |index: usize| layout.rows.iter().find(|row| row.index == index).unwrap();
    let surfaces = |index: usize, hover| {
        settings_row_surfaces(
            row(index),
            &set.rows[index],
            &palette,
            bg,
            (SettingsRowFill::Hovered, hover),
            1.0,
        )
    };
    // A choice joined to its Reset: the outline's fill is the choice's, and
    // the Reset's inside is painted over it.
    let choice = surfaces(0, Some(SettingsHover::Choice(0)));
    assert_eq!(choice[0].fill, settings_pressable(bg));
    assert_eq!(choice[1].fill, bg);
    let reset = surfaces(0, Some(SettingsHover::Reset(0)));
    assert_eq!(reset[0].fill, bg);
    assert_eq!(reset[1].fill, settings_pressable(bg));
    let resting = surfaces(0, Some(SettingsHover::Row(0)));
    assert_eq!((resting[0].fill, resting[1].fill), (bg, bg));
    assert_eq!(resting[0].stroke, palette.hover_control);
    // The painter fills a hovered row with the prelight and no bar, and
    // the strip's arrows and the categories under the pointer too.
    let source: String = crate::production_source().split_whitespace().collect();
    for needle in [
        "settings::SettingsRowFill::Hovered=>{shape(row.rect,hover,1.0,hover,0.0,4.0);}",
        "ifletSome(SettingsHover::Category(index))=set.hover&&letSome(Some(r))=layout.category_rects.get(index){shape(*r,hover,1.0,hover,0.0,4.0);}",
        "letfill=ifset.hover==Some(SettingsHover::Arrow(next)){settings::settings_pressable(theme.background)}else{theme.background};",
    ] {
        assert!(source.contains(needle), "missing: {needle}");
    }
}

/// On the prelight, a row's label keeps 4.5:1, its description the text
/// floor, and its outlines 3:1 against the prelight and the panel; on a
/// hovered choice or Reset the value and the word keep 4.5:1. Every bundled
/// theme, both blend modes, two accents.
#[test]
fn hover_inks_meet_their_floors() {
    use super::chrome::{ICON_CONTRAST, TEXT_CONTRAST};
    use super::color::contrast_ratio as ratio;
    let mut colors = chrome::Colors::default();
    let mut failures = Vec::new();
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        let bg = theme.background;
        for linear in [true, false] {
            for accent in [theme.accent, kettle_config::Rgb::new(0xbb, 0x99, 0xf8)] {
                let palette = settings_colors(&theme, accent, linear, &mut colors);
                let at = format!("{name} linear={linear} accent={accent:?}");
                let hover = palette.hover;
                // Some ink reaches `floor` on every one of `surfaces`.
                let reachable = |surfaces: &[kettle_config::Rgb], floor: f64| {
                    [
                        kettle_config::Rgb::new(0, 0, 0),
                        kettle_config::Rgb::new(255, 255, 255),
                    ]
                    .into_iter()
                    .any(|ink| surfaces.iter().all(|surface| ratio(ink, *surface) >= floor))
                };
                let mut check = |role: &str, ink, surfaces: &[kettle_config::Rgb], floor: f64| {
                    if !reachable(surfaces, floor) {
                        return;
                    }
                    for surface in surfaces {
                        if ratio(ink, *surface) + 0.005 < floor {
                            failures.push(format!("{at}: {role} {:.3}", ratio(ink, *surface)));
                        }
                    }
                };
                let label = colors.text(theme.foreground, hover);
                check("label", label, &[hover], TEXT_CONTRAST);
                check(
                    "description",
                    palette.description_hover,
                    &[hover],
                    TEXT_CONTRAST,
                );
                check(
                    "outline",
                    palette.hover_control,
                    &[bg, hover],
                    ICON_CONTRAST,
                );
                check(
                    "unavailable",
                    palette.hover_unavailable_control,
                    &[bg, hover],
                    ICON_CONTRAST,
                );
                check(
                    "well outline",
                    palette.hover_well_control,
                    &[bg, hover, palette.well],
                    ICON_CONTRAST,
                );
                let mut set = hover_panel();
                for (hover, kind) in [
                    (SettingsHover::Choice(0), SettingsTextKind::Value(0)),
                    (SettingsHover::Reset(2), SettingsTextKind::Reset(2)),
                ] {
                    set.hover = Some(hover);
                    let ink = settings_text_color(&set, kind, &palette, &theme, &mut colors);
                    if ratio(ink, super::settings::settings_pressable(bg)) + 0.005 < TEXT_CONTRAST {
                        failures.push(format!("{at}: {hover:?} text"));
                    }
                }
                // Active switch track/thumb contrast and unavailable external
                // status contrast across all row fills.
                if ratio(palette.switch_on_thumb, palette.switch_on_track) + 0.005 < ICON_CONTRAST {
                    failures.push(format!(
                        "{at}: switch thumb/track {:.3}",
                        ratio(palette.switch_on_thumb, palette.switch_on_track)
                    ));
                }
                if ratio(palette.switch_on_track, bg) + 0.005 < ICON_CONTRAST {
                    failures.push(format!(
                        "{at}: switch track/bg {:.3}",
                        ratio(palette.switch_on_track, bg)
                    ));
                }
                set.rows[0].control = SettingsControl::Toggle(true);
                set.rows[0].unavailable = true;
                set.hover = Some(SettingsHover::Switch(0));
                let status_ink = settings_text_color(
                    &set,
                    SettingsTextKind::Value(0),
                    &palette,
                    &theme,
                    &mut colors,
                );
                if ratio(status_ink, hover) + 0.005 < TEXT_CONTRAST {
                    failures.push(format!("{at}: hovered unavailable switch status"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The focused row's switch answers the pointer: the row keeps its focus
/// fill and bar, and the switch's own fill takes a button's hover mix, on
/// or off. Active switches use a solid accent track with a larger elevated
/// thumb; inactive switches use a bordered well track with a smaller thumb.
#[test]
fn a_switch_under_the_pointer_answers_it_on_the_focused_row() {
    use super::color::contrast_ratio as ratio;
    use super::settings::{
        SettingsRowFill, settings_pressable, settings_row_fill, settings_row_surfaces,
    };
    let theme = kettle_config::Theme::by_name("TokyoNight Night");
    let (bg, accent) = (theme.background, theme.accent);
    let mut colors = chrome::Colors::default();
    let palette = settings_colors(&theme, accent, true, &mut colors);
    let mut thumb_diameters = [0.0_f32; 2];
    for (slot, on) in [false, true].into_iter().enumerate() {
        let mut set = hover_panel();
        set.rows[1].control = SettingsControl::Toggle(on);
        set.hover = Some(SettingsHover::Switch(1));
        assert_eq!(settings_row_fill(&set, 1), SettingsRowFill::Focused);
        let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
        let row = layout.rows.iter().find(|row| row.index == 1).unwrap();
        let surfaces = |hover| {
            settings_row_surfaces(
                row,
                &set.rows[1],
                &palette,
                bg,
                (SettingsRowFill::Focused, hover),
                1.0,
            )
        };
        let resting = if on {
            palette.switch_on_track
        } else {
            palette.well
        };
        let rest_shapes = surfaces(None);
        assert_eq!(rest_shapes[0].fill, resting);
        assert_eq!(
            rest_shapes[0].width,
            if on { 0.0 } else { 2.0 },
            "on={on} border width"
        );
        thumb_diameters[slot] = rest_shapes[1].rect.2;
        assert_eq!(
            surfaces(Some(SettingsHover::Switch(1)))[0].fill,
            settings_pressable(resting)
        );
        assert_eq!(surfaces(Some(SettingsHover::Switch(0)))[0].fill, resting);
        if on {
            assert_eq!(rest_shapes[1].fill, palette.switch_on_thumb);
            assert!(ratio(palette.switch_on_thumb, palette.switch_on_track) >= 3.0);
            assert!(ratio(palette.switch_on_track, bg) >= 3.0);
        }
    }
    assert!(
        thumb_diameters[1] > thumb_diameters[0],
        "ON thumb ({}) must be larger than OFF thumb ({})",
        thumb_diameters[1],
        thumb_diameters[0]
    );
}
