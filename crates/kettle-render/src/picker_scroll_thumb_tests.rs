use super::overlays::{
    MENU_PANEL_RADIUS, MENU_SELECTION_INSET, menu_hint_left, menu_level_surfaces,
    menu_row_swatch_rects, picker_scroll_thumb, picker_scroll_thumb_color,
};
use super::{ContextMenu, ContextMenuRow, chrome::Colors, context_menu_row_rects, menu};
use kettle_config::{Rgb, Theme};

const CELL: (f32, f32) = (8.0, 16.0);

fn row(index: usize) -> ContextMenuRow {
    ContextMenuRow {
        toggle: None,
        swatches: index
            .is_multiple_of(2)
            .then_some([Rgb::new(1, 2, 3); menu::SWATCH_COUNT]),
        matched_chars: None,
        label: format!("Command {index}"),
        separator: false,
        enabled: true,
        hint: if index.is_multiple_of(3) {
            "Ctrl+Shift+K".into()
        } else {
            String::new()
        },
    }
}

/// A picker as the UI builds it: `rows` rows, eight of them visible.
fn picker(rows: usize, scroll_offset: usize) -> ContextMenu {
    let row_h = menu::row_height(CELL.1);
    ContextMenu {
        picker_header_height: CELL.1 + menu::PICKER_HEADER_PAD,
        anchor: (176.0, 108.0),
        rows: (0..rows).map(row).collect(),
        highlight: scroll_offset,
        scroll_offset,
        panel_w_clamped: 448.0,
        panel_h_clamped: rows.min(8) as f32 * row_h,
        a11y_label: None,
    }
}

/// An overflowing picker's thumb sits in the gap between the selection's
/// inset and the panel border at every scale, clear of hints, swatches and
/// the rounded bottom corner, and travels the track with the scroll offset.
#[test]
fn picker_scroll_thumb_stays_in_the_right_gap_and_tracks_the_offset() {
    for scale in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
        let menu = picker(150, 0);
        let right = menu.anchor.0 + menu.panel_w_clamped;
        let bottom = menu.anchor.1 + menu.panel_h_clamped;
        let inset_x = (MENU_SELECTION_INSET.0 * scale).min(10.0);
        let (x, y, w, h) = picker_scroll_thumb(&menu, CELL, scale).expect("overflowing picker");
        assert!(
            x >= right - inset_x && x + w <= right - 1.0,
            "scale {scale}"
        );
        assert!(w >= 2.0, "scale {scale}: {w}");
        assert!(y >= menu.anchor.1, "scale {scale}");
        assert!(
            y + h <= bottom - MENU_PANEL_RADIUS * scale + 0.001,
            "scale {scale}"
        );
        // The visible share of the rows, but at least one row tall.
        assert!(h >= menu::row_height(CELL.1) - 0.001, "scale {scale}: {h}");
        for (index, rect) in context_menu_row_rects(&menu, CELL) {
            let row = &menu.rows[index];
            let hint_w = unicode_width::UnicodeWidthStr::width(row.hint.as_str()) as f32 * CELL.0;
            if !row.hint.is_empty() {
                let hint_right =
                    menu_hint_left(menu.anchor.0, menu.panel_w_clamped, hint_w) + hint_w;
                assert!(hint_right < x, "hint {index} at scale {scale}");
            }
            if let Some(swatches) = menu_row_swatch_rects(
                row,
                (menu.anchor.0, menu.panel_w_clamped),
                (rect.1, rect.3),
                CELL.0,
            ) {
                for (sx, _, sw, _) in swatches {
                    assert!(sx + sw < x, "swatch {index} at scale {scale}");
                }
            }
        }
        let last = picker(150, 142);
        let (_, last_y, _, last_h) = picker_scroll_thumb(&last, CELL, scale).unwrap();
        assert!(last_y > y);
        assert!((last_y + last_h - (bottom - MENU_PANEL_RADIUS * scale)).abs() < 0.01);
        let middle = picker(150, 71);
        let (_, middle_y, _, _) = picker_scroll_thumb(&middle, CELL, scale).unwrap();
        assert!(y < middle_y && middle_y < last_y);
    }
    // A list that fits, and a context menu, have no thumb.
    assert_eq!(picker_scroll_thumb(&picker(8, 0), CELL, 1.0), None);
    let mut context = picker(150, 0);
    context.picker_header_height = 0.0;
    assert_eq!(picker_scroll_thumb(&context, CELL, 1.0), None);
}

/// The thumb is the level's last rounded surface, drawn after the selection,
/// and reads at the 3:1 non-text minimum on the panel in every bundled theme.
#[test]
fn picker_scroll_thumb_is_drawn_last_at_three_to_one() {
    let menu = picker(150, 0);
    let mut failures = Vec::new();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for linear in [false, true] {
            let mut colors = Colors::default();
            let surfaces =
                menu_level_surfaces(&menu, &theme, theme.accent, CELL, 2.0, &mut colors, linear);
            let thumb = picker_scroll_thumb(&menu, CELL, 2.0).unwrap();
            let last = surfaces.last().expect("thumb surface");
            assert_eq!(
                (last.pos, last.size),
                ([thumb.0, thumb.1], [thumb.2, thumb.3])
            );
            let color = picker_scroll_thumb_color(&theme, &mut colors);
            let ratio = super::color::contrast_ratio(color, theme.background);
            if ratio < super::chrome::ICON_CONTRAST {
                failures.push(format!("{name}: thumb {color:?} is {ratio:.2}:1"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Without overflow the selection stays the last surface.
    let fits = picker(8, 0);
    let theme = Theme::by_name("TokyoNight Night");
    let surfaces = menu_level_surfaces(
        &fits,
        &theme,
        theme.accent,
        CELL,
        2.0,
        &mut Colors::default(),
        true,
    );
    assert_eq!(
        surfaces.len(),
        super::overlays::MENU_SHADOW_LAYERS.len() + 2
    );
}
