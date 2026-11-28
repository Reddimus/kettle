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

/// Every toggle row uses the same fixed capsule size regardless of language,
/// state, value text or unavailability, emits no text inside the track, and
/// places unavailable status text right-aligned outside the track with an 8 px
/// gap.
#[test]
fn switches_use_uniform_capsule_and_place_unavailable_status_outside() {
    use super::{SettingsControl, SettingsTextKind};
    let row =
        |label: &str, value: &str, on: bool, unavailable: bool, value_cols: usize| SettingsRow {
            label: label.into(),
            value: value.into(),
            control: SettingsControl::Toggle(on),
            unavailable,
            value_cols,
            reset: Some(true),
            ..Default::default()
        };
    let cases = [
        ("Cursor blink", "On", true, false, 3),
        ("Cursor blink", "Off", false, false, 3),
        ("Parpadeo del cursor", "Activado", true, false, 11),
        ("Parpadeo del cursor", "Desactivado", false, false, 11),
        ("Window blur", "Unavailable", true, true, 11),
        ("Desenfoque de ventana", "Comprobando…", true, true, 13),
    ];
    let set = SettingsOverlay {
        categories: vec!["Appearance".into()],
        reset_label: "Reset".into(),
        rows: cases
            .iter()
            .map(|(label, text, on, unavailable, cols)| row(label, text, *on, *unavailable, *cols))
            .collect(),
        ..Default::default()
    };
    for scale in [1.0_f32, 2.0] {
        let layout = settings_layout(
            &set,
            (8.4 * scale, 17.5 * scale),
            (1280.0 * scale, 820.0 * scale),
            8.4 * scale,
            scale,
        );
        let control_h = super::settings_control_height(17.5);
        let (expected_w, expected_h) = super::settings::settings_toggle_size(control_h);
        let items = super::settings::settings_text_items(&set, &layout, 8.4 * scale);
        for (index, (_, text, _, unavailable, _)) in cases.iter().enumerate() {
            let geom = &layout.rows[index];
            let chip = geom.chip_rect;
            assert!(
                (chip.2 - expected_w * scale).abs() < 0.01
                    && (chip.3 - expected_h * scale).abs() < 0.01,
                "row {index} {scale}x: {chip:?}"
            );
            let slot_center = geom.rect.1 + 1.0 * scale + control_h * 0.5 * scale;
            assert!(
                (chip.1 + chip.3 * 0.5 - slot_center).abs() < 0.01,
                "row {index} {scale}x not vertically centred"
            );
            let value_item = items
                .iter()
                .find(|item| item.kind == SettingsTextKind::Value(index));
            if *unavailable {
                let status = geom
                    .status_rect
                    .expect("unavailable toggle reserves status_rect");
                assert!(
                    (chip.0 - (status.0 + status.2) - 8.0 * scale).abs() < 0.01,
                    "row {index} {scale}x: 8 px gap before toggle"
                );
                let item = value_item.expect("unavailable toggle emits status text");
                assert_eq!(item.text, *text);
                assert!(
                    (item.rect.0 + item.rect.2 - (status.0 + status.2)).abs() < 0.01,
                    "row {index} {scale}x: status text right-aligned"
                );
                assert!(item.rect.0 >= status.0 - 0.01);
                assert!(item.rect.0 + item.rect.2 <= chip.0 - 8.0 * scale + 0.01);
                assert!(geom.label_rect.0 + geom.label_rect.2 <= status.0 + 0.01);
                if let Some(reset) = geom.reset {
                    assert!(
                        (status.0 - (reset.rect.0 + reset.rect.2) - 4.0 * scale).abs() < 0.01,
                        "row {index} {scale}x: 4 px gap between Reset and status"
                    );
                }
            } else {
                assert_eq!(geom.status_rect, None, "row {index}");
                assert!(
                    value_item.is_none(),
                    "row {index}: no text inside an available switch track"
                );
                if let Some(reset) = geom.reset {
                    assert!(
                        (chip.0 - (reset.rect.0 + reset.rect.2) - 4.0 * scale).abs() < 0.01,
                        "row {index} {scale}x: 4 px gap between Reset and switch"
                    );
                }
            }
        }
    }
}

#[test]
fn fixed_capsule_switches_clamp_cleanly_and_keep_thumbs_inside_track() {
    use super::{SettingsControl, SettingsHit, SettingsTextKind, display_width};
    for language in [Language::Es, Language::En] {
        let tr = Translator::new(language);
        for on in [true, false] {
            for disabled in [false, true] {
                for unavailable in [false, true] {
                    let text = if unavailable {
                        tr.text(Text::SettingsValueUnavailable)
                    } else if on {
                        tr.text(Text::SettingsValueOn)
                    } else {
                        tr.text(Text::SettingsValueOff)
                    };
                    let set = SettingsOverlay {
                        rows: vec![SettingsRow {
                            label: tr.text(Text::SettingsFieldCursorBlink).into(),
                            value: text.into(),
                            control: SettingsControl::Toggle(on),
                            disabled,
                            unavailable,
                            ..Default::default()
                        }],
                        ..Default::default()
                    };
                    for (surface, cell, text_cw) in [
                        ((360.0, 600.0), (24.0, 48.0), 24.0),
                        ((1280.0, 820.0), (8.0, 16.0), 8.0),
                        ((2000.0, 1200.0), (24.0, 48.0), 24.0),
                        ((360.0, 600.0), (48.0, 48.0), 24.0),
                        ((360.0, 600.0), (6.0, 48.0), 24.0),
                        ((720.0, 1200.0), (48.0, 96.0), 48.0),
                        ((120.0, 120.0), (8.0, 16.0), 24.0),
                        ((56.0, 120.0), (8.0, 16.0), 24.0),
                    ] {
                        let layout = settings_layout(&set, cell, surface, text_cw, 1.0);
                        let row = &layout.rows[0];
                        let chip = row.chip_rect;
                        let on_right = super::settings::settings_knob_on_right(&set.rows[0]);
                        let thumb = super::settings::settings_switch_thumb(
                            chip,
                            on_right,
                            unavailable,
                            1.0,
                        );
                        let context = format!(
                            "{language:?} on={on} disabled={disabled} unavailable={unavailable} surface={surface:?} cell={cell:?} text_cw={text_cw}: chip={chip:?} thumb={thumb:?}"
                        );
                        assert!(
                            chip.0 >= row.rect.0 - 0.1
                                && chip.0 + chip.2 <= row.rect.0 + row.rect.2 + 0.1,
                            "{context}"
                        );
                        assert!(
                            thumb.0 >= chip.0 - 0.1
                                && thumb.0 + thumb.2 <= chip.0 + chip.2 + 0.1
                                && thumb.1 >= chip.1 - 0.1
                                && thumb.1 + thumb.3 <= chip.1 + chip.3 + 0.1,
                            "thumb escapes track: {context}"
                        );
                        if !unavailable && !disabled && chip.3 >= 22.0 {
                            let d_on =
                                super::settings::settings_switch_thumb(chip, true, false, 1.0).2;
                            let d_off =
                                super::settings::settings_switch_thumb(chip, false, false, 1.0).2;
                            assert!(d_on > d_off, "ON thumb must exceed OFF thumb: {context}");
                        }
                        let items = super::settings::settings_text_items(&set, &layout, text_cw);
                        let value = items
                            .iter()
                            .find(|item| item.kind == SettingsTextKind::Value(0));
                        if !unavailable {
                            assert!(value.is_none(), "no track text when available: {context}");
                        } else if let Some(status) = row.status_rect {
                            assert!(
                                status.0 >= row.label_rect.0 + row.label_rect.2 - 0.1
                                    && status.0 + status.2 <= chip.0 - 8.0 + 0.1,
                                "status overlaps label or switch: {context}"
                            );
                            if let Some(value) = value {
                                let advance = display_width(&value.text) as f32 * text_cw;
                                assert!(advance <= value.rect.2 + 0.1, "{context}");
                                assert!(
                                    value.rect.0 >= status.0 - 0.1
                                        && value.rect.0 + value.rect.2 <= status.0 + status.2 + 0.1,
                                    "{context}"
                                );
                            }
                        } else {
                            assert!(value.is_none(), "{context}");
                        }
                        for x in [chip.0 + chip.2 * 0.5, thumb.0 + thumb.2 * 0.5] {
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
}

#[test]
fn switch_checkmark_mask_is_non_empty_cached_and_bounded_at_1x_and_2x() {
    use super::settings::{CheckmarkMask, checkmark_mask, push_switch_checkmark};
    let mut cache = CheckmarkMask::default();
    for (scale, side) in [(1.0_f32, 12_u32), (2.0, 24)] {
        let mask = checkmark_mask(side);
        assert_eq!(mask.len(), (side * side) as usize, "{scale}x");
        assert!(
            mask.contains(&255),
            "{scale}x checkmark must have solid interior coverage"
        );
        assert!(
            mask.iter().any(|&c| c > 0 && c < 255),
            "{scale}x checkmark must have supersampled edge coverage"
        );
        // Corners remain clear inside the circular thumb.
        assert_eq!(mask[0], 0, "{scale}x top-left corner");
        assert_eq!(mask[(side - 1) as usize], 0, "{scale}x top-right corner");
        assert_eq!(
            mask[((side - 1) * side) as usize],
            0,
            "{scale}x bottom-left corner"
        );
        assert_eq!(
            mask[(side * side - 1) as usize],
            0,
            "{scale}x bottom-right corner"
        );
        let first = cache.get(side).as_ptr();
        assert_eq!(cache.get(side).as_ptr(), first, "{scale}x cache hit");
        assert_eq!(cache.get(side), mask.as_slice());

        let thumb = (100.0 * scale, 50.0 * scale, 19.0 * scale, 19.0 * scale);
        let mut quads = Vec::new();
        push_switch_checkmark(
            &mut quads,
            thumb,
            kettle_config::Rgb::new(0, 120, 255),
            &mut cache,
        );
        assert!(!quads.is_empty(), "{scale}x checkmark quads");
        for q in &quads {
            assert!(
                q.pos[0] >= thumb.0 - 0.5
                    && q.pos[1] >= thumb.1 - 0.5
                    && q.pos[0] + q.size[0] <= thumb.0 + thumb.2 + 0.5
                    && q.pos[1] + q.size[1] <= thumb.1 + thumb.3 + 0.5,
                "{scale}x quad {:?}/{:?} escaped thumb {thumb:?}",
                q.pos,
                q.size
            );
        }
    }
}
