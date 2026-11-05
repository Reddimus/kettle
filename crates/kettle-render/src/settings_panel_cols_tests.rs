use super::{SettingsOverlay, SettingsRow, settings_layout};
use kettle_i18n::{Language, Text, Translator};

#[test]
fn contextual_notes_do_not_move_the_panel() {
    let mut set = SettingsOverlay::default();
    let before = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0);
    set.footer_note = Some("A very long active GPU note that must fit without moving the panel across category switches".into());
    let after = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0);
    assert_eq!(before.rect, after.rect);
    assert_eq!(before.content_rect, after.content_rect);
    assert_eq!(before.search_rect, after.search_rect);
}

#[test]
fn settings_slots_fit_english_spanish_and_measured_pseudo_text() {
    for tr in [
        Translator::new(Language::En),
        Translator::new(Language::Es),
        Translator::pseudo(),
    ] {
        let set = SettingsOverlay {
            categories: [
                Text::SettingsCategoryAppearance,
                Text::SettingsCategoryBackground,
                Text::SettingsCategoryBehavior,
                Text::SettingsCategorySearch,
                Text::SettingsCategoryTabs,
                Text::SettingsCategoryGraphics,
                Text::SettingsCategoryAgents,
                Text::SettingsCategoryKeybinds,
            ]
            .map(|key| tr.text(key).to_string())
            .to_vec(),
            rows: vec![SettingsRow {
                label: tr.text(Text::SettingsFieldLanguage).into(),
                description: tr.text(Text::SettingsDescriptionFieldLanguage).into(),
                value: tr.text(Text::SettingsValueAutomatic).into(),
                ..Default::default()
            }],
            search_placeholder: tr.text(Text::SettingsSearchPlaceholder).into(),
            ..Default::default()
        };
        for surface in [(1280.0, 820.0), (800.0, 600.0), (560.0, 600.0)] {
            let layout = settings_layout(&set, (9.6, 20.0), surface, 9.6);
            for item in super::settings::settings_text_items(&set, &layout, 9.6) {
                assert!(!item.text.contains('\n'));
                let factor = if item.kind == super::SettingsTextKind::Title {
                    1.2
                } else {
                    1.0
                };
                assert!(
                    super::display_width(&item.text) as f32 * 9.6 * factor <= item.rect.2 + 0.1,
                    "{}",
                    item.text
                );
            }
            assert!(layout.rows[0].label_rect.2 > 100.0);
        }
    }
}
