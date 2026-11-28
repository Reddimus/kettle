//! Settings switches: one fixed track for every boolean row, state shown by
//! the thumb's side and an on/off symbol rather than words, and every part
//! readable against what it sits on in every bundled theme.

use super::chrome::{self, ICON_CONTRAST, TEXT_CONTRAST};
use super::color::contrast_ratio as ratio;
use super::settings::{
    SettingsRowFill, SettingsSurface, SettingsTextKind, settings_colors, settings_pressable,
    settings_row_surfaces, settings_switch_stroke, settings_switch_symbol, settings_switch_thumb,
    settings_text_color, settings_text_items, switch_on_colors,
};
use super::{
    Rect4, SettingsControl, SettingsHover, SettingsOverlay, SettingsRow, settings_control_height,
    settings_layout, settings_switch_size,
};
use kettle_config::{Rgb, Theme};
use kettle_i18n::{Language, Text, Translator};

fn center((x, y, w, h): Rect4) -> (f32, f32) {
    (x + w / 2.0, y + h / 2.0)
}

/// Every boolean row a Settings page can show, in `tr`'s language: short
/// and long labels, each state's word, and Window blur's unavailable and
/// checking states with their wider words.
fn switches(tr: &Translator) -> SettingsOverlay {
    let row = |label: Text, value: Text, on: bool, unavailable: bool, reset: bool| SettingsRow {
        label: tr.text(label).into(),
        value: tr.text(value).into(),
        description: tr.text(Text::SettingsDescriptionFieldLanguage).into(),
        control: SettingsControl::Toggle(on),
        unavailable,
        reset: Some(reset),
        value_cols: super::display_width(tr.text(value)),
        ..Default::default()
    };
    SettingsOverlay {
        categories: vec![tr.text(Text::SettingsCategoryAppearance).into()],
        rows: vec![
            row(
                Text::SettingsFieldCursorBlink,
                Text::SettingsValueOn,
                true,
                false,
                false,
            ),
            row(
                Text::SettingsFieldShowTitlebar,
                Text::SettingsValueOff,
                false,
                false,
                true,
            ),
            row(
                Text::SettingsFieldCursorBlink,
                Text::SettingsValueUnavailable,
                true,
                true,
                false,
            ),
            row(
                Text::SettingsFieldChromeBackground,
                Text::SettingsValueChecking,
                true,
                true,
                true,
            ),
            row(
                Text::SettingsFieldMinimumContrast,
                Text::SettingsValueOff,
                false,
                false,
                false,
            ),
        ],
        reset_label: tr.text(Text::SettingsResetButton).into(),
        title: tr.text(Text::SettingsCategoryAppearance).into(),
        ..Default::default()
    }
}

/// Every switch is one size, from the control height alone: the same in
/// English and Spanish, on and off, available or not, beside short and long
/// labels, at 1x and 2x and at every panel width, its right edge in line
/// with the others and its centre on the control slot's.
#[test]
fn every_switch_shares_one_fixed_track() {
    for tr in [Translator::new(Language::En), Translator::new(Language::Es)] {
        let set = switches(&tr);
        for (scale, cw, line) in [(1.0_f32, 8.4_f32, 17.5_f32), (2.0, 16.8, 35.0)] {
            let control_h = settings_control_height(line / scale);
            let (track_w, track_h) = settings_switch_size(control_h);
            let mut seen = Vec::new();
            for width in (300..=2000).step_by(20) {
                let surface = (width as f32 * scale, 1000.0 * scale);
                let layout = settings_layout(&set, (cw, line), surface, cw, scale);
                assert_eq!(layout.rows.len(), set.rows.len());
                let right = layout.rows[0].chip_rect.0 + layout.rows[0].chip_rect.2;
                for row in &layout.rows {
                    let context = format!(
                        "{:?} {scale}x width {width} row {}",
                        tr.language(),
                        row.index
                    );
                    let chip = row.chip_rect;
                    let expected_w = (track_w * scale).min(row.rect.2);
                    assert!((chip.2 - expected_w).abs() < 0.01, "{context}: {chip:?}");
                    assert!(
                        (chip.3 - track_h * scale).abs() < 0.01,
                        "{context}: {chip:?}"
                    );
                    assert!(
                        (chip.0 + chip.2 - right).abs() < 0.01,
                        "{context}: edges differ"
                    );
                    let slot = row.rect.1 + scale;
                    assert!(
                        (chip.1 + chip.3 / 2.0 - slot - control_h * scale / 2.0).abs() < 0.01,
                        "{context}: not centred on the slot"
                    );
                    seen.push((chip.2, chip.3));
                }
            }
            seen.dedup();
            assert_eq!(seen.len(), 1, "{:?} {scale}x: {seen:?}", tr.language());
        }
    }
}

/// At the default 14 px font (an 8.4x17.5 cell) a switch is 46x24 points,
/// and it grows with the font and with the backing scale.
#[test]
fn the_default_switch_is_46_by_24_points_and_follows_the_font() {
    assert_eq!(
        settings_switch_size(settings_control_height(17.5)),
        (46.0, 24.0)
    );
    let mut previous = (0.0, 0.0);
    for line in [12.5_f32, 16.25, 17.5, 22.5, 30.0, 45.0] {
        let control_h = settings_control_height(line);
        let (w, h) = settings_switch_size(control_h);
        assert!(h >= 22.0 && h <= control_h, "{line}: {h}");
        assert!((w - (h * 1.92).round()).abs() < 0.01, "{line}");
        assert!(w >= previous.0 && h >= previous.1, "{line}");
        previous = (w, h);
    }
    let set = SettingsOverlay {
        rows: vec![SettingsRow {
            label: "Cursor blink".into(),
            value: "On".into(),
            control: SettingsControl::Toggle(true),
            ..Default::default()
        }],
        ..Default::default()
    };
    for scale in [1.0_f32, 1.5, 2.0] {
        let layout = settings_layout(
            &set,
            (8.4 * scale, 17.5 * scale),
            (1280.0 * scale, 820.0 * scale),
            8.4 * scale,
            scale,
        );
        let chip = layout.rows[0].chip_rect;
        assert!((chip.2 - 46.0 * scale).abs() < 0.01, "{scale}: {chip:?}");
        assert!((chip.3 - 24.0 * scale).abs() < 0.01, "{scale}: {chip:?}");
    }
}

fn find(surfaces: &[SettingsSurface], what: impl Fn(&SettingsSurface) -> bool) -> Vec<Rect4> {
    surfaces
        .iter()
        .filter(|surface| what(surface))
        .map(|surface| surface.rect)
        .collect()
}

/// Off: an outlined well, the thumb at the left and an open ring centred
/// where the thumb goes when on. On: a solid track without an outline, a
/// white thumb at the right and a bar where the thumb was. Unavailable: an
/// outline over the panel and a hollow thumb, without either symbol.
#[test]
fn switches_show_their_state_with_a_thumb_and_a_symbol() {
    let theme = Theme::by_name("TokyoNight Night");
    let mut colors = chrome::Colors::default();
    let palette = settings_colors(&theme, theme.accent, true, &mut colors);
    let bg = theme.background;
    for scale in [1.0_f32, 2.0] {
        for (on, unavailable) in [(false, false), (true, false), (true, true), (false, true)] {
            let set = SettingsOverlay {
                rows: vec![SettingsRow {
                    label: "Window blur".into(),
                    value: if unavailable { "Unavailable" } else { "On" }.into(),
                    control: SettingsControl::Toggle(on),
                    unavailable,
                    ..Default::default()
                }],
                ..Default::default()
            };
            let layout = settings_layout(
                &set,
                (8.4 * scale, 17.5 * scale),
                (1280.0 * scale, 820.0 * scale),
                8.4 * scale,
                scale,
            );
            let row = &layout.rows[0];
            let (x, y, w, h) = row.chip_rect;
            let surfaces = settings_row_surfaces(
                row,
                &set.rows[0],
                &palette,
                bg,
                (SettingsRowFill::Panel, None),
                scale,
            );
            let context = format!("on={on} unavailable={unavailable} {scale}x");
            let track = &surfaces[0];
            assert_eq!(track.rect, row.chip_rect, "{context}");
            assert!((track.radius - h / 2.0).abs() < 0.01, "{context}");
            let d = settings_switch_thumb(h, scale);
            assert!((d - (h - 6.0 * scale)).abs() < 0.01, "{context}");
            let thumbs = find(&surfaces, |s| (s.rect.2 - d).abs() < 0.01 && s.rect.3 == d);
            let thumb = *thumbs.last().expect("a thumb");
            let middle = y + h / 2.0;
            assert!((thumb.1 + d / 2.0 - middle).abs() < 0.01, "{context}");
            let left = x + 3.0 * scale;
            let right = x + w - 3.0 * scale - d;
            assert!(
                (thumb.0 - if on { right } else { left }).abs() < 0.01,
                "{context}: thumb at {thumb:?}"
            );
            let stroke = settings_switch_stroke(scale);
            let size = settings_switch_symbol(h);
            assert!((size - (0.42 * h).round()).abs() < 0.01);
            let rings = find(&surfaces, |s| {
                s.rect.2 == size && s.rect.3 == size && s.width == stroke && s.rect.2 < d
            });
            let bars = find(&surfaces, |s| {
                s.rect.2 == stroke && s.rect.3 == size && s.width == 0.0
            });
            // A symbol sits where the thumb goes in the other state.
            let symbol_centre = if on { left } else { right } + d / 2.0;
            if unavailable {
                assert_eq!(track.fill, bg, "{context}");
                assert_eq!(track.stroke, palette.unavailable_control, "{context}");
                assert_eq!(track.width, scale, "{context}");
                let hollow = surfaces.iter().find(|s| s.rect == thumb).unwrap();
                assert_eq!(hollow.fill, bg, "{context}");
                assert!(hollow.width > 0.0, "{context}");
                assert!(
                    rings.is_empty() && bars.is_empty(),
                    "{context}: {surfaces:?}"
                );
            } else if on {
                assert_eq!(track.fill, palette.switch_on, "{context}");
                assert_eq!(track.width, 0.0, "{context}: no outline on an on track");
                let solid = surfaces.iter().rfind(|s| s.rect == thumb).unwrap();
                assert_eq!(solid.fill, palette.switch_thumb, "{context}");
                assert!(rings.is_empty(), "{context}");
                assert_eq!(bars.len(), 1, "{context}: {surfaces:?}");
                let bar = bars[0];
                assert!(
                    (center(bar).0 - symbol_centre).abs() <= 0.5 + 0.01,
                    "{context}"
                );
                assert!((center(bar).1 - middle).abs() <= 0.5 + 0.01, "{context}");
                assert!(
                    bar.0 + bar.2 <= thumb.0,
                    "{context}: the bar is clear of the thumb"
                );
            } else {
                assert_eq!(track.fill, palette.well, "{context}");
                assert_eq!(track.width, scale, "{context}");
                let solid = surfaces.iter().find(|s| s.rect == thumb).unwrap();
                assert_eq!(solid.fill, palette.switch_off_ink, "{context}");
                assert!(bars.is_empty(), "{context}");
                assert_eq!(rings.len(), 1, "{context}: {surfaces:?}");
                let ring = surfaces.iter().find(|s| s.rect == rings[0]).unwrap();
                assert_eq!(ring.fill, track.fill, "{context}: the ring is open");
                assert_eq!(ring.stroke, palette.switch_off_ink, "{context}");
                assert!((ring.radius - size / 2.0).abs() < 0.01, "{context}");
                assert!((center(ring.rect).0 - symbol_centre).abs() <= 0.5 + 0.01);
                assert!((center(ring.rect).1 - middle).abs() <= 0.5 + 0.01);
                assert!(
                    ring.rect.0 >= thumb.0 + d,
                    "{context}: the ring is clear of the thumb"
                );
            }
            for surface in &surfaces[1..] {
                let (sx, sy, sw, sh) = surface.rect;
                assert!(
                    sx >= x - 0.01 && sy >= y - 0.01 && sx + sw <= x + w + 0.01,
                    "{context}: {surface:?} leaves the track"
                );
                assert!(sy + sh <= y + h + 0.01, "{context}: {surface:?}");
            }
            let items = settings_text_items(&set, &layout, 8.4 * scale);
            assert_eq!(
                items
                    .iter()
                    .any(|item| item.kind == SettingsTextKind::Value(0)),
                unavailable,
                "{context}: only an unavailable switch says its state"
            );
        }
    }
}

/// An unavailable switch's state word sits 8 points before its track,
/// right-aligned, in the description ink of the row's fill; Reset keeps its
/// 4 points before the word, or before the track where there is no word.
/// Its value stays whole on the row, for the accessibility tree.
#[test]
fn an_unavailable_switch_says_its_state_beside_the_track() {
    let theme = Theme::by_name("Kettle Day");
    let mut colors = chrome::Colors::default();
    let palette = settings_colors(&theme, theme.accent, true, &mut colors);
    for tr in [Translator::new(Language::En), Translator::new(Language::Es)] {
        let mut set = switches(&tr);
        for scale in [1.0_f32, 2.0] {
            for focused in 0..set.rows.len() {
                set.focused_row = focused;
                let layout = settings_layout(
                    &set,
                    (8.4 * scale, 17.5 * scale),
                    (1280.0 * scale, 820.0 * scale),
                    8.4 * scale,
                    scale,
                );
                let items = settings_text_items(&set, &layout, 8.4 * scale);
                for row in &layout.rows {
                    let data = &set.rows[row.index];
                    let context = format!("{:?} {scale}x row {}", tr.language(), row.index);
                    let anchor = row.status_rect.map_or(row.chip_rect.0, |status| status.0);
                    if let Some(reset) = row.reset {
                        let gap = (anchor - reset.rect.0 - reset.rect.2) / scale;
                        assert!((gap - 4.0).abs() < 0.01, "{context}: Reset gap {gap}");
                    }
                    assert!(row.label_rect.0 + row.label_rect.2 <= anchor, "{context}");
                    let Some(status) = row.status_rect else {
                        assert!(!data.unavailable, "{context}");
                        continue;
                    };
                    assert!(data.unavailable, "{context}");
                    let gap = (row.chip_rect.0 - status.0 - status.2) / scale;
                    assert!((gap - 8.0).abs() < 0.01, "{context}: status gap {gap}");
                    let word = items
                        .iter()
                        .find(|item| item.kind == SettingsTextKind::Value(row.index))
                        .expect("the state word");
                    assert_eq!(word.text, data.value, "{context}: whole at 1280 points");
                    assert!(
                        (word.rect.0 + word.rect.2 - status.0 - status.2).abs() < 0.01,
                        "{context}: right-aligned"
                    );
                    let ink = settings_text_color(
                        &set,
                        SettingsTextKind::Value(row.index),
                        &palette,
                        &theme,
                        &mut colors,
                    );
                    let (expected, surface) = if row.index == focused {
                        (palette.description_selected, palette.selected)
                    } else {
                        (palette.description, theme.background)
                    };
                    assert_eq!(ink, expected, "{context}");
                    assert!(ratio(ink, surface) >= TEXT_CONTRAST, "{context}");
                    // The pointer over the word lights the row, not the switch.
                    assert_eq!(
                        layout.hover_at(&set, center(status)),
                        Some(SettingsHover::Row(row.index)),
                        "{context}"
                    );
                    assert_eq!(
                        layout.hover_at(&set, center(row.chip_rect)),
                        Some(SettingsHover::Switch(row.index)),
                        "{context}"
                    );
                }
            }
        }
        // The rows keep their words for the accessibility tree.
        let fresh = switches(&tr);
        assert_eq!(fresh.rows[0].value, tr.text(Text::SettingsValueOn));
        assert_eq!(fresh.rows[2].value, tr.text(Text::SettingsValueUnavailable));
    }
}

/// On a narrow panel the state word gives way first, to at most half the
/// room the label leaves, and nothing on the row overlaps.
#[test]
fn a_narrow_panel_fits_the_state_word_beside_the_label() {
    let tr = Translator::new(Language::Es);
    let set = switches(&tr);
    for width in (200..=700).step_by(10) {
        let layout = settings_layout(&set, (8.4, 17.5), (width as f32, 820.0), 8.4, 1.0);
        for row in &layout.rows {
            let context = format!("width {width} row {}", row.index);
            let mut parts = vec![row.label_rect, row.chip_rect];
            parts.extend(row.status_rect);
            parts.extend(row.reset.map(|reset| reset.rect));
            for (i, a) in parts.iter().enumerate() {
                assert!(a.0 >= row.rect.0 - 0.01, "{context}: {a:?} leaves the row");
                assert!(a.0 + a.2 <= row.rect.0 + row.rect.2 + 0.01, "{context}");
                for b in &parts[i + 1..] {
                    assert!(
                        a.2 <= 0.0
                            || b.2 <= 0.0
                            || a.0 + a.2 <= b.0 + 0.01
                            || b.0 + b.2 <= a.0 + 0.01,
                        "{context}: {a:?} overlaps {b:?}"
                    );
                }
            }
        }
    }
}

/// The focused switch under the pointer moves its own track, on, off and
/// unavailable, while the row keeps its focus fill; another row's hover
/// leaves it at rest.
#[test]
fn the_focused_switch_prelights_under_the_pointer() {
    let theme = Theme::by_name("Kettle Day");
    let mut colors = chrome::Colors::default();
    let palette = settings_colors(&theme, theme.accent, true, &mut colors);
    let bg = theme.background;
    let tr = Translator::new(Language::En);
    let set = switches(&tr);
    let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
    for row in &layout.rows {
        let data = &set.rows[row.index];
        let resting = if data.unavailable {
            bg
        } else if data.control == SettingsControl::Toggle(true) {
            palette.switch_on
        } else {
            palette.well
        };
        let track = |hover| {
            settings_row_surfaces(
                row,
                data,
                &palette,
                bg,
                (SettingsRowFill::Focused, hover),
                1.0,
            )[0]
            .fill
        };
        assert_eq!(track(None), resting, "row {}", row.index);
        assert_eq!(
            track(Some(SettingsHover::Switch(row.index))),
            settings_pressable(resting),
            "row {}",
            row.index
        );
        assert_ne!(settings_pressable(resting), resting, "row {}", row.index);
        assert_eq!(
            track(Some(SettingsHover::Switch(row.index + 1))),
            resting,
            "row {}",
            row.index
        );
    }
}

/// Every bundled theme, both blend modes, the theme's own accent, its blue
/// and a desktop's purple: the on track keeps 3:1 against the panel, the
/// thumb and bar 3:1 against the track at rest and under the pointer, the
/// off ring and thumb 3:1 against the well at rest and under the pointer,
/// and every outline 3:1 against the surfaces it divides. The thumb is
/// white wherever a white thumb can reach those floors.
#[test]
fn switch_parts_reach_three_to_one_in_every_theme() {
    let mut colors = chrome::Colors::default();
    let mut failures = Vec::new();
    let (white, black) = (Rgb::new(255, 255, 255), Rgb::new(0, 0, 0));
    let mut white_thumbs = 0;
    let mut cases = 0;
    let tr = Translator::new(Language::En);
    let set = switches(&tr);
    let layout = settings_layout(&set, (8.4, 17.5), (1280.0, 820.0), 8.4, 1.0);
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let bg = theme.background;
        for linear in [true, false] {
            for accent in [theme.accent, theme.palette[4], Rgb::new(0xbb, 0x99, 0xf8)] {
                let at = format!("{name} linear={linear} accent={accent:?}");
                let (track, thumb) = switch_on_colors(bg, accent, linear);
                let palette = settings_colors(&theme, accent, linear, &mut colors);
                assert_eq!((palette.switch_on, palette.switch_thumb), (track, thumb));
                cases += 1;
                if thumb == white {
                    white_thumbs += 1;
                } else {
                    // A black thumb only where no white one reaches.
                    let reachable = (0..=256).any(|step| {
                        [black, white].into_iter().any(|target| {
                            let t = super::color::over(accent, target, step as f64 / 256.0, linear);
                            let hovered = settings_pressable(t);
                            ratio(t, bg).min(ratio(hovered, bg)) >= 3.0
                                && ratio(white, t).min(ratio(white, hovered)) >= 3.0
                        })
                    });
                    if reachable {
                        failures.push(format!("{at}: black thumb where white reaches"));
                    }
                }
                // Every ink meets 3:1 on each surface it touches, wherever
                // black or white could.
                let mut check_on = |role: &str, ink: Rgb, surfaces: &[Rgb]| {
                    let reachable = [black, white].into_iter().any(|ink| {
                        surfaces
                            .iter()
                            .all(|surface| ratio(ink, *surface) >= ICON_CONTRAST)
                    });
                    for surface in surfaces.iter().filter(|_| reachable) {
                        if ratio(ink, *surface) + 0.005 < ICON_CONTRAST {
                            failures.push(format!("{at}: {role} {:.3}", ratio(ink, *surface)));
                        }
                    }
                };
                check_on("on track/panel", track, &[bg]);
                check_on("thumb/track", thumb, &[track]);
                check_on("thumb/hovered track", thumb, &[settings_pressable(track)]);
                check_on("off ink/well", palette.switch_off_ink, &[palette.well]);
                check_on(
                    "off ink/hovered well",
                    palette.switch_off_ink,
                    &[settings_pressable(palette.well)],
                );
                // The painted surfaces, on every row fill: each symbol and
                // thumb against the track, each outline against the track
                // inside it and the row around it.
                for (fill, row_surface) in [
                    (SettingsRowFill::Panel, bg),
                    (SettingsRowFill::Focused, palette.selected),
                    (SettingsRowFill::Hovered, palette.hover),
                ] {
                    for row in &layout.rows {
                        let data = &set.rows[row.index];
                        for hover in [None, Some(SettingsHover::Switch(row.index))] {
                            let surfaces =
                                settings_row_surfaces(row, data, &palette, bg, (fill, hover), 1.0);
                            let track = &surfaces[0];
                            let role = format!("row {} {fill:?} {hover:?}", row.index);
                            if track.width > 0.0 {
                                check_on(
                                    &format!("{role} outline"),
                                    track.stroke,
                                    &[track.fill, row_surface],
                                );
                            } else {
                                check_on(&format!("{role} track/panel"), track.fill, &[bg]);
                            }
                            for part in &surfaces[1..] {
                                let inside = part.rect.0 >= track.rect.0 - 0.01
                                    && part.rect.0 + part.rect.2
                                        <= track.rect.0 + track.rect.2 + 0.01;
                                if !inside {
                                    continue; // Reset, before the track
                                }
                                let ink = if part.width > 0.0 {
                                    part.stroke
                                } else {
                                    part.fill
                                };
                                // The thumb's shadow only lifts it.
                                if part.width == 0.0
                                    && part.fill != palette.switch_thumb
                                    && part.fill != palette.switch_off_ink
                                {
                                    continue;
                                }
                                check_on(
                                    &format!("{role} part {:?}", part.rect),
                                    ink,
                                    &[track.fill],
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        white_thumbs * 10 >= cases * 9,
        "white thumbs in only {white_thumbs} of {cases}"
    );
    // The panels the switch was designed against keep a white thumb.
    for name in ["TokyoNight Night", "Kettle Day"] {
        let theme = Theme::by_name(name);
        let (_, thumb) = switch_on_colors(theme.background, theme.accent, true);
        assert_eq!(thumb, white, "{name}");
    }
}

/// The track keeps the accent where it already reaches every floor, so a
/// theme's own switch colour is unchanged wherever it can be.
#[test]
fn a_reaching_accent_keeps_its_own_colour() {
    let white = Rgb::new(255, 255, 255);
    let bg = Rgb::new(0x1a, 0x1b, 0x26);
    let accent = Rgb::new(0x5a, 0x7f, 0xd6);
    let hovered = settings_pressable(accent);
    assert!(ratio(accent, bg).min(ratio(hovered, bg)) >= 3.0);
    assert!(ratio(white, accent).min(ratio(white, hovered)) >= 3.0);
    assert_eq!(switch_on_colors(bg, accent, true), (accent, white));
    assert_eq!(switch_on_colors(bg, accent, false), (accent, white));
}
