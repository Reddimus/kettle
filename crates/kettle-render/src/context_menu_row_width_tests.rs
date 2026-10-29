use super::{
    ContextMenu, ContextMenuRow, context_menu_clip_indicators, context_menu_panel_width, menu,
    menu_chrome_quads, menu_row_chars,
};

#[test]
fn menu_width_and_inverse_share_one_formula() {
    assert_eq!(menu::panel_width(20.0, 8.0), 200.0);
    assert_eq!(menu::panel_columns(200.0, 8.0), 20);
    assert_eq!(menu::panel_width(1.0, 8.0), menu::MIN_W);
}

#[test]
fn row_budget_uses_display_columns() {
    let row = ContextMenuRow {
        label: "主题 👩‍💻".to_string(),
        separator: false,
        enabled: true,
        hint: "Ctrl+界".to_string(),
    };
    assert_eq!(
        menu_row_chars(&row),
        unicode_width::UnicodeWidthStr::width(row.label.as_str())
            + unicode_width::UnicodeWidthStr::width(row.hint.as_str())
            + 2
    );
}

#[test]
fn app_clamped_width_is_authoritative_for_every_render_pass() {
    let row = ContextMenuRow {
        label: "short".to_string(),
        separator: false,
        enabled: true,
        hint: String::new(),
    };
    let menu = ContextMenu {
        anchor: (0.0, 0.0),
        rows: vec![row],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 237.5,
        panel_h_clamped: 0.0,
        a11y_label: None,
    };
    assert_eq!(context_menu_panel_width(&menu, 9.6), 237.5);
}

#[test]
fn scroll_indicators_follow_the_remaining_suffix() {
    let rows = [false, false, true, false, false]
        .into_iter()
        .enumerate()
        .map(|(index, separator)| ContextMenuRow {
            label: format!("row {index}"),
            separator,
            enabled: !separator,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    let row_h = 24.0;
    let sep_h = 8.0;
    let panel_h = 48.0;

    assert_eq!(
        context_menu_clip_indicators(&rows, 0, panel_h, row_h, sep_h),
        (false, true),
        "the first window has content below but none above"
    );
    assert_eq!(
        context_menu_clip_indicators(&rows, 2, panel_h, row_h, sep_h),
        (true, true),
        "the middle window has content on both sides"
    );
    assert_eq!(
        context_menu_clip_indicators(&rows, 3, panel_h, row_h, sep_h),
        (true, false),
        "the final two rows fit exactly, so no down indicator remains"
    );
    assert_eq!(
        context_menu_clip_indicators(&rows, rows.len(), panel_h, row_h, sep_h),
        (true, false),
        "an out-of-range offset is clamped to the empty suffix"
    );
}

#[test]
fn tiny_or_invalid_menu_never_emits_negative_or_nonfinite_quads() {
    let menu = ContextMenu {
        anchor: (0.0, 0.0),
        rows: vec![ContextMenuRow {
            label: String::new(),
            separator: true,
            enabled: false,
            hint: String::new(),
        }],
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 1.0,
        panel_h_clamped: 1.0,
        a11y_label: None,
    };
    let theme = kettle_config::Theme::default();
    let quads = menu_chrome_quads(
        &menu,
        &theme,
        kettle_config::Rgb::new(1, 2, 3),
        8.0,
        16.0,
        &mut crate::chrome::Colors::default(),
        true,
    );
    assert!(!quads.is_empty());
    assert!(quads.iter().all(|quad| {
        quad.pos.iter().all(|value| value.is_finite())
            && quad
                .size
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0)
    }));

    let invalid = ContextMenu {
        anchor: (f32::NAN, 0.0),
        ..menu
    };
    assert!(
        menu_chrome_quads(
            &invalid,
            &theme,
            kettle_config::Rgb::new(1, 2, 3),
            8.0,
            16.0,
            &mut crate::chrome::Colors::default(),
            true,
        )
        .is_empty()
    );
}

#[test]
fn cut_menu_reserves_scroll_targets_and_fitting_menu_has_none() {
    let (inset, content) = menu::scroll_content(600.0, 200.0, 28.0);
    assert!(
        inset >= 18.0,
        "cut menu has no scroll target: inset={inset}"
    );
    assert_eq!(content + 2.0 * inset, 200.0);
    assert_eq!(menu::scroll_content(140.0, 140.0, 28.0), (0.0, 140.0));
}

#[test]
fn cut_menu_paints_arrow_inside_reserved_strip() {
    let theme = kettle_config::Theme::by_name("Builtin Light");
    let mut menu = ContextMenu {
        anchor: (10.0, 10.0),
        rows: (0..20)
            .map(|_| ContextMenuRow {
                label: "Command".into(),
                hint: String::new(),
                separator: false,
                enabled: true,
            })
            .collect(),
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 200.0,
        panel_h_clamped: 200.0,
        a11y_label: None,
    };
    let arrows = |menu: &ContextMenu| {
        menu_chrome_quads(
            menu,
            &theme,
            theme.accent,
            8.0,
            16.0,
            &mut crate::chrome::Colors::default(),
            true,
        )
        .into_iter()
        .filter(|quad| quad.size[1] == 2.0 && quad.size[0] < 20.0 && quad.color[3] == 1.0)
        .collect::<Vec<_>>()
    };
    let cut = arrows(&menu);
    assert_eq!(cut.len(), 3);
    assert!(
        cut.iter()
            .all(|quad| quad.pos[1] >= 10.0 + 200.0 - menu::SCROLL_H
                && quad.pos[1] + quad.size[1] <= 210.0)
    );
    menu.panel_h_clamped = 0.0;
    assert!(arrows(&menu).is_empty());
}
