use super::{SettingsOverlay, SettingsRow, settings_layout};
use kettle_i18n::{Language, Text, Translator};

#[test]
fn contextual_notes_do_not_move_the_panel() {
    let mut set = SettingsOverlay::default();
    let before = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0, 1.0);
    set.footer_note = Some("A very long active GPU note that must fit without moving the panel across category switches".into());
    let after = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0, 1.0);
    assert_eq!(before.rect, after.rect);
    assert_eq!(before.content_rect, after.content_rect);
    assert_eq!(before.search_rect, after.search_rect);
}

#[test]
fn settings_slots_fit_english_spanish_and_measured_pseudo_text() {
    for tr in [
        Translator::new(Language::En),
        Translator::new(Language::Es),
        #[cfg(debug_assertions)]
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
            let layout = settings_layout(&set, (9.6, 20.0), surface, 9.6, 1.0);
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

/// A switch's track holds no words in either language: On, Off and their
/// translations are left to the accessibility tree. Only an unavailable
/// switch says its state, whole where the row has room, fitted before the
/// track where it does not, and never over the label, the track or Reset.
#[test]
fn switch_tracks_hold_no_words_and_their_state_words_fit_beside_them() {
    use super::{SettingsControl, SettingsHit, SettingsTextKind, display_width};
    for language in [Language::Es, Language::En] {
        let tr = Translator::new(language);
        let states = [
            (true, false, Text::SettingsValueOn),
            (false, false, Text::SettingsValueOff),
            (true, true, Text::SettingsValueUnavailable),
            (true, true, Text::SettingsValueChecking),
        ];
        for (on, unavailable, value) in states {
            for disabled in [false, true] {
                let text = tr.text(value);
                let set = SettingsOverlay {
                    rows: vec![SettingsRow {
                        label: tr.text(Text::SettingsFieldCursorBlink).into(),
                        value: text.into(),
                        control: SettingsControl::Toggle(on),
                        disabled,
                        unavailable,
                        reset: Some(true),
                        value_cols: display_width(tr.text(Text::SettingsValueUnavailable)),
                        ..Default::default()
                    }],
                    reset_label: tr.text(Text::SettingsResetButton).into(),
                    ..Default::default()
                };
                for (surface, cell, text_cw, whole) in [
                    ((1280.0, 820.0), (8.0, 16.0), 8.0, true),
                    ((2000.0, 1200.0), (24.0, 48.0), 24.0, false),
                    ((360.0, 600.0), (24.0, 48.0), 24.0, false),
                    ((360.0, 600.0), (6.0, 48.0), 24.0, false),
                    ((720.0, 1200.0), (48.0, 96.0), 48.0, false),
                    ((120.0, 120.0), (8.0, 16.0), 24.0, false),
                    ((56.0, 120.0), (8.0, 16.0), 24.0, false),
                ] {
                    let layout = settings_layout(&set, cell, surface, text_cw, 1.0);
                    let row = &layout.rows[0];
                    let chip = row.chip_rect;
                    let items = super::settings::settings_text_items(&set, &layout, text_cw);
                    let context = format!(
                        "{language:?} {text} disabled={disabled} surface={surface:?} cell={cell:?}"
                    );
                    let value = items
                        .iter()
                        .find(|item| item.kind == SettingsTextKind::Value(0));
                    assert_eq!(row.status_rect.is_some(), unavailable, "{context}");
                    match (value, row.status_rect) {
                        (Some(value), Some(status)) => {
                            let advance = display_width(&value.text) as f32 * text_cw;
                            assert!(advance <= value.rect.2 + 0.1, "{context}");
                            assert!(
                                value.rect.0 >= status.0 - 0.01
                                    && (value.rect.0 + value.rect.2 - status.0 - status.2).abs()
                                        < 0.01,
                                "{context}: {:?} not right-aligned in {status:?}",
                                value.rect
                            );
                            assert!(value.rect.0 + value.rect.2 <= chip.0 + 0.01, "{context}");
                            if whole {
                                assert_eq!(value.text, text, "{context}");
                                assert!(
                                    (chip.0 - status.0 - status.2 - 8.0).abs() < 0.01,
                                    "{context}: the state word keeps 8 points before the track"
                                );
                            }
                        }
                        (None, None) => {}
                        (value, status) => {
                            panic!("{context}: {:?} {status:?}", value.map(|value| &value.text))
                        }
                    }
                    for item in &items {
                        if let SettingsTextKind::Label(0) | SettingsTextKind::Reset(0) = item.kind
                            && item.rect.2 > 0.0
                        {
                            assert!(
                                item.rect.0 + item.rect.2 <= chip.0 + 0.01,
                                "{context}: {:?} runs into the track",
                                item.text
                            );
                        }
                    }
                    assert_eq!(
                        layout.pointer_hit((chip.0 + chip.2 * 0.5, chip.1 + chip.3 * 0.5), None),
                        SettingsHit::Field(0),
                        "{context}"
                    );
                }
            }
        }
    }
}
