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

/// A switch's label starts after the knob when it is off (the knob sits at
/// the left) and at the chip's inset when on (the knob is at the right), and
/// it fits beside the knob in both languages' longest words.
#[test]
fn switch_labels_clear_the_knob() {
    use super::{SettingsControl, SettingsTextKind};
    let row = |value: &str, on: bool| SettingsRow {
        label: "Cursor blink".into(),
        value: value.into(),
        control: SettingsControl::Toggle(on),
        ..Default::default()
    };
    let cases = [
        ("On", true),
        ("Off", false),
        ("Desactivado", true),
        ("Desactivado", false),
    ];
    let set = SettingsOverlay {
        categories: vec!["Appearance".into()],
        rows: cases.iter().map(|(text, on)| row(text, *on)).collect(),
        ..Default::default()
    };
    let layout = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0);
    let items = super::settings::settings_text_items(&set, &layout, 8.0);
    for (index, (text, on)) in cases.into_iter().enumerate() {
        let chip = layout.rows[index].chip_rect;
        let knob = super::settings::settings_toggle_knob(chip.3);
        let value = items
            .iter()
            .find(|item| item.kind == SettingsTextKind::Value(index))
            .unwrap()
            .rect;
        let (start, room_end) = if on {
            (chip.0 + 4.0, chip.0 + chip.2 - knob - 3.0)
        } else {
            (chip.0 + 4.0 + knob + 2.0, chip.0 + chip.2)
        };
        assert_eq!(value.0, start, "{text} {on}");
        assert!(
            value.0 + text.len() as f32 * 8.0 <= room_end,
            "{text} {on}: {value:?} in {chip:?}"
        );
    }
}

#[test]
fn translated_switch_labels_fit_beside_painted_knobs_after_width_clamping() {
    use super::{SettingsControl, SettingsHit, SettingsTextKind, display_width};
    for language in [Language::Es, Language::En] {
        let tr = Translator::new(language);
        for on in [true, false] {
            for disabled in [false, true] {
                let text = tr.text(if on {
                    Text::SettingsValueOn
                } else {
                    Text::SettingsValueOff
                });
                let set = SettingsOverlay {
                    rows: vec![SettingsRow {
                        label: tr.text(Text::SettingsFieldCursorBlink).into(),
                        value: text.into(),
                        control: SettingsControl::Toggle(on),
                        disabled,
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                for (surface, cell, text_cw, full_label) in [
                    ((360.0, 600.0), (24.0, 48.0), 24.0, false),
                    ((1280.0, 820.0), (8.0, 16.0), 8.0, true),
                    ((2000.0, 1200.0), (24.0, 48.0), 24.0, true),
                    ((360.0, 600.0), (48.0, 48.0), 24.0, false),
                    ((360.0, 600.0), (6.0, 48.0), 24.0, false),
                    ((720.0, 1200.0), (48.0, 96.0), 48.0, false),
                    ((120.0, 120.0), (8.0, 16.0), 24.0, false),
                    ((56.0, 120.0), (8.0, 16.0), 24.0, false),
                ] {
                    let layout = settings_layout(&set, cell, surface, text_cw);
                    let row = &layout.rows[0];
                    let chip = row.chip_rect;
                    let diameter = super::settings::settings_toggle_knob(chip.3);
                    let items = super::settings::settings_text_items(&set, &layout, text_cw);
                    let value = items
                        .iter()
                        .find(|item| item.kind == SettingsTextKind::Value(0))
                        .unwrap();
                    let context = format!(
                        "{language:?} on={on} disabled={disabled} surface={surface:?} cell={cell:?} text_cw={text_cw}: {:?} {:?} in {chip:?}",
                        value.text, value.rect
                    );
                    assert!(
                        value.rect.2 >= 0.0
                            && value.rect.0 >= chip.0
                            && value.rect.0 + value.rect.2 <= chip.0 + chip.2 + 0.1,
                        "{context}"
                    );
                    let advance = display_width(&value.text) as f32 * text_cw;
                    assert!(advance <= value.rect.2 + 0.1, "{context}");
                    let knob_left = if on && !disabled {
                        chip.0 + chip.2 - diameter - 3.0
                    } else {
                        chip.0 + 3.0
                    };
                    if !value.text.is_empty() {
                        assert!(
                            value.rect.0 + advance <= knob_left
                                || value.rect.0 >= knob_left + diameter,
                            "painted label overlaps knob: {context}"
                        );
                        assert!(
                            value.rect.0 + value.rect.2 <= knob_left
                                || value.rect.0 >= knob_left + diameter,
                            "label bounds overlap knob: {context}"
                        );
                    }
                    if full_label {
                        assert_eq!(value.text, text, "{context}");
                    }
                    if surface.0 == 360.0 && language == Language::Es && on && !disabled {
                        assert!((chip.2 - 145.92).abs() < 0.01);
                        assert_eq!(diameter, 50.0);
                        assert!(
                            !value.text.is_empty(),
                            "a clamped switch must retain a fitted label"
                        );
                    }
                    for x in [chip.0 + chip.2 * 0.5, knob_left + diameter * 0.5] {
                        if x >= chip.0 && x < chip.0 + chip.2 {
                            assert_eq!(
                                layout.pointer_hit((x, chip.1 + chip.3 * 0.5), None),
                                SettingsHit::Field(0)
                            );
                        }
                    }
                }
            }
        }
    }
}
