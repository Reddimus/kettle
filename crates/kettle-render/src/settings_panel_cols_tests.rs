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

/// Every boolean switch uses an identical fixed capsule size derived from
/// `control_h` across languages and states, with no words painted inside the
/// track, and external right-aligned status text only when unavailable.
#[test]
fn switch_labels_clear_the_knob() {
    use super::{
        SettingsControl, SettingsTextKind, settings_control_height, settings_toggle_track_size,
    };
    let row = |label: &str, value: &str, on: bool, unavailable: bool| SettingsRow {
        label: label.into(),
        value: value.into(),
        control: SettingsControl::Toggle(on),
        unavailable,
        value_cols: super::display_width(value),
        ..Default::default()
    };
    let cases = [
        ("Cursor blink", "On", true, false),
        ("Cursor blink", "Off", false, false),
        ("Parpadeo del cursor", "Activado", true, false),
        ("Parpadeo del cursor", "Desactivado", false, false),
        ("Window blur", "Unavailable", true, true),
        ("Desenfoque de ventana", "Comprobando…", true, true),
    ];
    let set = SettingsOverlay {
        categories: vec!["Appearance".into()],
        rows: cases
            .iter()
            .map(|(label, value, on, unavailable)| row(label, value, *on, *unavailable))
            .collect(),
        ..Default::default()
    };
    let layout = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0, 1.0);
    let items = super::settings::settings_text_items(&set, &layout, 8.0);
    let control_h = settings_control_height(16.0);
    let (track_w, track_h) = settings_toggle_track_size(control_h);
    let first_chip = layout.rows[0].chip_rect;
    assert_eq!((first_chip.2, first_chip.3), (track_w, track_h));
    for (index, (_, text, _, unavailable)) in cases.into_iter().enumerate() {
        let row = &layout.rows[index];
        let chip = row.chip_rect;
        assert_eq!(chip.0, first_chip.0, "{text}: x drifted");
        assert_eq!((chip.2, chip.3), (track_w, track_h), "{text}: size drifted");
        let value = items
            .iter()
            .find(|item| item.kind == SettingsTextKind::Value(index));
        if unavailable {
            let status = row
                .status_rect
                .expect("unavailable toggle reserves status_rect");
            assert!(
                (chip.0 - (status.0 + status.2) - 8.0).abs() < 0.01,
                "{text}"
            );
            assert!(
                row.label_rect.0 + row.label_rect.2 <= status.0 + 0.01,
                "{text}"
            );
            let value = value.expect("unavailable toggle emits external status text");
            assert_eq!(value.text, text);
            assert!(
                (value.rect.0 + value.rect.2 - (status.0 + status.2)).abs() < 0.01,
                "{text}: not right-aligned in {status:?}: {:?}",
                value.rect
            );
            assert!(value.rect.0 + value.rect.2 <= chip.0 - 8.0 + 0.01, "{text}");
        } else {
            assert!(
                row.status_rect.is_none(),
                "{text}: no status_rect when available"
            );
            assert!(value.is_none(), "{text}: no inside-track text on toggle");
        }
    }
}

#[test]
fn translated_switch_labels_fit_beside_painted_knobs_after_width_clamping() {
    use super::{
        SettingsControl, SettingsHit, SettingsTextKind, display_width, settings_control_height,
        settings_toggle_track_size,
    };
    for language in [Language::Es, Language::En] {
        let tr = Translator::new(language);
        for (on, unavailable) in [(true, false), (false, false), (true, true)] {
            for disabled in [false, true] {
                let text = tr.text(if unavailable {
                    Text::SettingsValueUnavailable
                } else if on {
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
                        unavailable,
                        value_cols: display_width(text),
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                for (surface, cell, text_cw, full_status) in [
                    ((360.0, 600.0), (24.0, 48.0), 24.0, false),
                    ((1280.0, 820.0), (8.0, 16.0), 8.0, true),
                    ((2000.0, 1200.0), (24.0, 48.0), 24.0, false),
                    ((360.0, 600.0), (48.0, 48.0), 24.0, false),
                    ((360.0, 600.0), (6.0, 48.0), 24.0, false),
                    ((720.0, 1200.0), (48.0, 96.0), 48.0, false),
                    ((120.0, 120.0), (8.0, 16.0), 24.0, false),
                    ((56.0, 120.0), (8.0, 16.0), 24.0, false),
                ] {
                    let layout = settings_layout(&set, cell, surface, text_cw, 1.0);
                    let row = &layout.rows[0];
                    let chip = row.chip_rect;
                    let control_h = settings_control_height(layout.line);
                    let (raw_w, track_h) = settings_toggle_track_size(control_h);
                    let expected_w = raw_w.min(row.rect.2);
                    assert!(
                        (chip.2 - expected_w).abs() < 0.01 && (chip.3 - track_h).abs() < 0.01,
                        "{language:?} on={on} unavailable={unavailable} surface={surface:?}: {chip:?} != ({expected_w}, {track_h})"
                    );
                    let diameter = super::settings::settings_toggle_knob(chip.3, 1.0);
                    assert!(
                        (diameter - (chip.3 - 6.0).max(6.0)).abs() < 0.01,
                        "{diameter} in {chip:?}"
                    );
                    let items = super::settings::settings_text_items(&set, &layout, text_cw);
                    let value = items
                        .iter()
                        .find(|item| item.kind == SettingsTextKind::Value(0));
                    if !unavailable {
                        assert!(row.status_rect.is_none());
                        assert!(value.is_none(), "no inside-track text for available switch");
                    } else if let Some(status) = row.status_rect {
                        assert!(status.0 >= row.rect.0 - 0.01);
                        assert!(status.0 + status.2 <= chip.0 - 8.0 + 0.01);
                        assert!(row.label_rect.0 + row.label_rect.2 <= status.0 + 0.01);
                        if let Some(value) = value {
                            let advance = display_width(&value.text) as f32 * text_cw;
                            assert!(advance <= value.rect.2 + 0.1);
                            assert!(value.rect.0 >= status.0 - 0.01);
                            assert!(value.rect.0 + value.rect.2 <= status.0 + status.2 + 0.01);
                            assert!(value.rect.0 + value.rect.2 <= chip.0 - 8.0 + 0.01);
                            if full_status {
                                assert_eq!(value.text, text);
                            }
                        }
                    } else {
                        assert!(value.is_none());
                    }
                    let knob_left = if on && !disabled {
                        chip.0 + chip.2 - diameter - 3.0
                    } else {
                        chip.0 + 3.0
                    };
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
