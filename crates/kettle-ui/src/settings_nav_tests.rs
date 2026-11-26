//! Settings row keys: where each one moves the focused row and what it does
//! to its value.

use super::*;
use kettle_i18n::Language;

const EN: Translator = Translator::new(Language::En);

fn category(cats: &[Category], name: Text) -> usize {
    cats.iter().position(|cat| cat.name == name).unwrap()
}

fn focused(cats: &[Category], nav: &SettingsNav) -> &'static str {
    cats[nav.category].fields[nav.field].key
}

/// `G`, `Ctrl+D` and `Ctrl+U` skip rows that do not apply, as ↑/↓ do:
/// "Stop blinking after" while the cursor does not blink, and the wallpaper
/// rows behind a solid background.
#[test]
fn jumps_skip_rows_that_do_not_apply() {
    let cats = categories(&[]);
    let cfg = Config {
        cursor_blink: false,
        ..Config::default()
    };
    let appearance = category(&cats, Text::SettingsCategoryAppearance);
    assert!(field_disabled(&cfg, "cursor-blink-timeout"));
    let mut nav = SettingsNav {
        category: appearance,
        ..Default::default()
    };
    nav.move_focus(&cats, &cfg, RowMove::Last);
    assert_eq!(focused(&cats, &nav), "cursor-blink");
    nav.move_focus(&cats, &cfg, RowMove::First);
    assert_eq!(nav.field, 0);
    for to in [RowMove::PageDown, RowMove::PageUp] {
        for _ in 0..cats[appearance].fields.len() {
            nav.move_focus(&cats, &cfg, to);
            assert!(
                !field_disabled(&cfg, focused(&cats, &nav)),
                "{to:?} focused {}",
                focused(&cats, &nav)
            );
        }
    }
    assert_eq!(nav.field, 0);
    nav.move_focus(&cats, &cfg, RowMove::PageDown);
    nav.move_focus(&cats, &cfg, RowMove::PageDown);
    assert_eq!(focused(&cats, &nav), "cursor-blink");

    let cfg = Config::default();
    assert_eq!(cfg.background_type, kettle_config::BackgroundType::Solid);
    let background = category(&cats, Text::SettingsCategoryBackground);
    for to in [
        RowMove::Last,
        RowMove::PageDown,
        RowMove::PageUp,
        RowMove::First,
    ] {
        let mut nav = SettingsNav {
            category: background,
            ..Default::default()
        };
        nav.move_focus(&cats, &cfg, to);
        assert_eq!(focused(&cats, &nav), "background-type", "{to:?}");
    }
    // A click on the category keeps the row number it can, not a row that
    // does not apply.
    let mut nav = SettingsNav {
        category: appearance,
        field: 3,
        ..Default::default()
    };
    nav.apply_hit(
        &cats,
        &cfg,
        &EN,
        kettle_render::SettingsHit::Category(background),
    );
    assert_eq!(nav.category, background);
    assert_eq!(focused(&cats, &nav), "background-type");
}

/// Tab opens a category on its first row that applies.
#[test]
fn a_new_category_opens_on_its_first_row_that_applies() {
    let cats = vec![
        Category {
            name: Text::SettingsCategoryAppearance,
            fields: vec![toggle(Text::SettingsFieldCursorBlink, "cursor-blink")],
            groups: Vec::new(),
        },
        Category {
            name: Text::SettingsCategoryBackground,
            fields: vec![
                text(
                    Text::SettingsFieldBackgroundImage,
                    "background-image",
                    Text::SettingsValueImagePlaceholder,
                ),
                toggle(Text::SettingsFieldShowTitlebar, "show-titlebar"),
            ],
            groups: Vec::new(),
        },
    ];
    let cfg = Config::default();
    assert!(field_disabled(&cfg, "background-image"));
    for next in [true, false] {
        let mut nav = SettingsNav::default();
        nav.switch_category(&cats, &cfg, next);
        assert_eq!((nav.category, nav.field), (1, 1));
    }
}
