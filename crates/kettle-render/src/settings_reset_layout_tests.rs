use super::settings::{SettingsTextKind, settings_text_items};
use super::{
    Rect4, SettingsControl, SettingsHit, SettingsOverlay, SettingsRow, display_width,
    settings_control_height, settings_layout,
};
use kettle_i18n::{Language, Text, Translator};

fn inside(parent: Rect4, child: Rect4, context: &str) {
    let e = 0.1;
    assert!(
        child.2 >= 0.0
            && child.3 >= 0.0
            && child.0 + e >= parent.0
            && child.1 + e >= parent.1
            && child.0 + child.2 <= parent.0 + parent.2 + e
            && child.1 + child.3 <= parent.1 + parent.3 + e,
        "{context}: {child:?} outside {parent:?}"
    );
}

fn disjoint(a: Rect4, b: Rect4, context: &str) {
    assert!(
        a.2 <= 0.0
            || b.2 <= 0.0
            || a.0 + a.2 <= b.0 + 0.1
            || b.0 + b.2 <= a.0 + 0.1
            || a.1 + a.3 <= b.1 + 0.1
            || b.1 + b.3 <= a.1 + 0.1,
        "{context}: {a:?} overlaps {b:?}"
    );
}

fn center((x, y, w, h): Rect4) -> (f32, f32) {
    (x + w * 0.5, y + h * 0.5)
}

/// The focused row at its default, then changed rows: the longest Settings
/// labels, beside wide values.
fn overlay(tr: &Translator) -> SettingsOverlay {
    let off = tr.text(Text::SettingsValueOff);
    let row = |label: Text, value: &str, control, reset| SettingsRow {
        label: tr.text(label).into(),
        description: tr
            .text(Text::SettingsDescriptionFieldMinimumContrast)
            .into(),
        value: value.into(),
        control,
        reset: Some(reset),
        value_cols: display_width(value),
        ..Default::default()
    };
    SettingsOverlay {
        categories: vec![tr.text(Text::SettingsCategoryAppearance).into()],
        rows: vec![
            row(
                Text::SettingsFieldUpdateCheckInterval,
                "24 h",
                SettingsControl::Value,
                false,
            ),
            row(
                Text::SettingsFieldUpdateCheckInterval,
                "24 h",
                SettingsControl::Value,
                true,
            ),
            row(
                Text::SettingsFieldShowTitlebar,
                off,
                SettingsControl::Toggle(false),
                true,
            ),
            row(
                Text::SettingsFieldChromeBackground,
                tr.text(Text::SettingsValueAutomaticFromWallpaper),
                SettingsControl::Choice,
                true,
            ),
            row(
                Text::SettingsFieldFontSize,
                "16 px",
                SettingsControl::Value,
                true,
            ),
            row(
                Text::SettingsFieldMinimumContrast,
                off,
                SettingsControl::Choice,
                true,
            ),
        ],
        focused_row: 0,
        reset_label: tr.text(Text::SettingsResetButton).into(),
        title: tr.text(Text::SettingsCategoryAppearance).into(),
        ..Default::default()
    }
}

/// As a row narrows, Reset gives way in a fixed order and nothing on the
/// row overlaps or spills, at 1x and 2x, in English, Spanish and the
/// pseudo-locale:
/// 1. The disabled chip (the focused row at its default) shows only beside
///    the whole label.
/// 2. An enabled chip shows its word beside the whole label, else the icon
///    beside the whole label, else the icon while the label keeps twelve
///    columns; past that it is not drawn (Delete still resets).
/// 3. The value chip keeps its minimum; the description never yields.
///
/// A chip is as tall as the value chip, at least 24 points, and only a
/// click on it resets: its centre hits `Reset`, the rest of the row
/// `Field`.
#[test]
fn reset_yields_in_order_without_overlap_at_any_width() {
    for tr in [
        Translator::new(Language::En),
        Translator::new(Language::Es),
        #[cfg(debug_assertions)]
        Translator::pseudo(),
    ] {
        let set = overlay(&tr);
        for (scale, cw, line) in [(1.0_f32, 8.4_f32, 17.5_f32), (2.0, 16.8, 35.0)] {
            let mut states = std::collections::BTreeSet::new();
            for width in (300..=2000).step_by(10) {
                let surface = (width as f32 * scale, 1000.0 * scale);
                let layout = settings_layout(&set, (cw, line), surface, cw, scale);
                let items = settings_text_items(&set, &layout, cw);
                for row in &layout.rows {
                    let data = &set.rows[row.index];
                    let context = format!(
                        "{:?} {scale}x width {width} row {}",
                        tr.language(),
                        row.index
                    );
                    inside(row.rect, row.chip_rect, &context);
                    inside(row.rect, row.label_rect, &context);
                    inside(row.rect, row.description_rect, &context);
                    disjoint(row.label_rect, row.chip_rect, &context);
                    disjoint(row.description_rect, row.chip_rect, &context);
                    let inner = row.rect.2;
                    assert!(
                        row.chip_rect.2 + 0.01 >= (8.0 * cw + 20.0).min(inner * 0.48),
                        "{context}: the value chip lost its minimum"
                    );
                    for item in items.iter().filter(|item| match item.kind {
                        SettingsTextKind::Label(i)
                        | SettingsTextKind::Value(i)
                        | SettingsTextKind::Description(i)
                        | SettingsTextKind::Reset(i) => i == row.index,
                        _ => false,
                    }) {
                        inside(row.rect, item.rect, &context);
                        assert!(
                            display_width(&item.text) as f32 * cw <= item.rect.2 + 0.1,
                            "{context}: {:?} spills out of {:?}",
                            item.text,
                            item.rect
                        );
                    }
                    let label = items
                        .iter()
                        .find(|item| item.kind == SettingsTextKind::Label(row.index))
                        .expect("every row has its label");
                    let whole = label.text == data.label;
                    assert_eq!(
                        layout.pointer_hit(center(row.chip_rect), None),
                        SettingsHit::Field(row.index),
                        "{context}"
                    );
                    let Some(reset) = row.reset else {
                        states.insert(format!("{} none", row.index));
                        continue;
                    };
                    inside(row.rect, reset.rect, &context);
                    disjoint(reset.rect, row.chip_rect, &context);
                    disjoint(reset.rect, row.label_rect, &context);
                    assert_eq!(reset.rect.3, row.chip_rect.3, "{context}");
                    assert!(reset.rect.3 >= 24.0 * scale, "{context}");
                    assert!(reset.rect.2 >= 24.0 * scale, "{context}");
                    assert_eq!(
                        layout.pointer_hit(center(reset.rect), None),
                        SettingsHit::Reset(row.index),
                        "{context}"
                    );
                    assert_eq!(
                        layout.pointer_hit(center(reset.rect), Some(1)),
                        SettingsHit::Field(row.index),
                        "{context}: a wheel over Reset steps the row"
                    );
                    assert_eq!(reset.enabled, data.reset == Some(true), "{context}");
                    if !reset.enabled || !reset.icon {
                        assert!(whole, "{context}: {:?} shortened for Reset", label.text);
                    } else {
                        assert!(
                            whole || display_width(&label.text) >= 12,
                            "{context}: {:?} below twelve columns beside Reset",
                            label.text
                        );
                    }
                    if reset.icon {
                        assert!(
                            !items
                                .iter()
                                .any(|item| item.kind == SettingsTextKind::Reset(row.index)),
                            "{context}: an icon chip has no word"
                        );
                    }
                    states.insert(format!(
                        "{} {}",
                        row.index,
                        if reset.icon { "icon" } else { "word" }
                    ));
                }
            }
            // Logical geometry reaches the same Reset states at either DPI.
            let steps: &[&str] = &["0 word", "0 none", "1 word", "1 icon"];
            for &state in steps {
                assert!(
                    states.contains(state),
                    "{:?} {scale}x never shows {state}: {states:?}",
                    tr.language()
                );
            }
        }
    }
}

/// Every control is one height, at least 24 points at 1x and 2x, and a
/// row's chip keeps its width while its value changes.
#[test]
fn controls_share_one_height_and_keep_their_width() {
    for (scale, cw, line) in [
        (1.0_f32, 7.8_f32, 16.25_f32),
        (1.0, 8.4, 17.5),
        (2.0, 15.6, 32.5),
        (2.0, 16.8, 35.0),
    ] {
        let control_h = settings_control_height(line / scale) * scale;
        assert!(control_h >= 24.0 * scale, "{line}");
        for (control, narrow, wide, cols) in [
            (SettingsControl::Choice, "Light", "Follow system", 13),
            (SettingsControl::Value, "9 px", "99 px", 5),
            (SettingsControl::Toggle(true), "On", "Off", 3),
        ] {
            let set = |value: &str| SettingsOverlay {
                rows: vec![
                    SettingsRow {
                        label: "Appearance".into(),
                        value: value.into(),
                        control,
                        value_cols: cols,
                        ..Default::default()
                    },
                    SettingsRow {
                        label: "Image".into(),
                        control: SettingsControl::Text,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let surface = (1280.0 * scale, 820.0 * scale);
            let first = settings_layout(&set(narrow), (cw, line), surface, cw, scale);
            let second = settings_layout(&set(wide), (cw, line), surface, cw, scale);
            assert_eq!(
                first.rows[0].chip_rect, second.rows[0].chip_rect,
                "{control:?} {line}: the chip moved with its value"
            );
            assert!(first.rows[0].chip_rect.2 >= 8.0 * cw + 20.0, "{control:?}");
            for layout in [&first, &second] {
                assert_eq!(layout.search_rect.3, control_h);
                for row in &layout.rows {
                    assert_eq!(row.chip_rect.3, control_h, "{control:?} {line}");
                }
            }
        }
    }
}

/// A typical Linux window (1600x1000, the 14 px font's 8.4x17.5 cell)
/// still shows the whole twelve-row Appearance page; an 800x600 window
/// shows five rows, as it did before the taller controls (51 px rows then,
/// 53 px now).
#[test]
fn taller_controls_keep_the_appearance_page_in_view() {
    let rows = |count| SettingsOverlay {
        categories: vec!["Appearance".into(); 11],
        rows: (0..count)
            .map(|i| SettingsRow {
                label: format!("Setting {i}"),
                description: "A description.".into(),
                value: "14 px".into(),
                ..Default::default()
            })
            .collect(),
        title: "Appearance".into(),
        hints: vec![
            ("↑↓/jk".into(), "field".into()),
            ("←→/hl".into(), "change".into()),
            ("g/G".into(), "ends".into()),
            ("Del".into(), "reset".into()),
            ("Tab".into(), "category".into()),
            ("Esc".into(), "close".into()),
        ],
        ..Default::default()
    };
    let set = rows(12);
    let wide = settings_layout(&set, (8.4, 17.5), (1600.0, 1000.0), 8.4, 1.0);
    assert_eq!(wide.rows.len(), 12);
    let small = settings_layout(&set, (8.4, 17.5), (800.0, 600.0), 8.4, 1.0);
    assert_eq!(small.rows.len(), 6);
}

/// Free text (the image path) is not centred: it starts where its inline
/// editor starts, on the same line, so opening the editor does not move it.
#[test]
fn text_values_start_where_their_editor_does() {
    for (surface, cell) in [
        ((1280.0, 820.0), (8.4, 17.5)),
        ((640.0, 820.0), (8.4, 17.5)),
        ((2560.0, 1640.0), (16.8, 35.0)),
        ((800.0, 160.0), (8.0, 16.0)),
    ] {
        let set = SettingsOverlay {
            rows: vec![SettingsRow {
                label: "Image path".into(),
                value: "~/Pictures/wallpaper.png".into(),
                control: SettingsControl::Text,
                ..Default::default()
            }],
            ..Default::default()
        };
        let layout = settings_layout(&set, cell, surface, cell.0, 1.0);
        let items = settings_text_items(&set, &layout, cell.0);
        let value = items
            .iter()
            .find(|item| item.kind == SettingsTextKind::Value(0))
            .expect("the path is drawn");
        let editor =
            super::settings_value_rect(&set, cell.0, cell.1, surface.0, surface.1, cell.0, 1.0);
        assert_eq!(
            (value.rect.0, value.rect.1, value.rect.3),
            (editor.0, editor.1, editor.3),
            "{surface:?}"
        );
    }
}
