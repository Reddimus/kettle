use super::{
    Rect4, SettingsControl, SettingsHit, SettingsOverlay, SettingsRow,
    settings_hit_test_with_text_width, settings_layout, settings_panel_geometry,
    settings_value_rect,
};

fn overlay() -> SettingsOverlay {
    SettingsOverlay {
        categories: [
            "Appearance",
            "Background",
            "Behavior",
            "Search",
            "Tabs",
            "Graphics",
            "Agents",
            "Keybinds",
        ]
        .map(str::to_string)
        .to_vec(),
        rows: (0..12)
            .map(|i| SettingsRow {
                label: format!("Setting {i}"),
                value: "14 pt".into(),
                description: "A factual description of this setting.".into(),
                control: SettingsControl::Value,
                ..Default::default()
            })
            .collect(),
        title: "Appearance".into(),
        search_placeholder: "Search settings".into(),
        no_matches: "No settings match".into(),
        hints: "Up/Down field · Left/Right change · Tab category · Esc close".into(),
        ..Default::default()
    }
}

fn inside(parent: Rect4, child: Rect4) {
    let epsilon = 0.1;
    assert!(child.2 >= 0.0 && child.3 >= 0.0, "{child:?}");
    assert!(
        child.0 + epsilon >= parent.0
            && child.1 + epsilon >= parent.1
            && child.0 + child.2 <= parent.0 + parent.2 + epsilon
            && child.1 + child.3 <= parent.1 + parent.3 + epsilon,
        "{child:?} outside {parent:?}"
    );
}
fn disjoint(a: Rect4, b: Rect4) {
    assert!(
        a.0 + a.2 <= b.0 + 0.1
            || b.0 + b.2 <= a.0 + 0.1
            || a.1 + a.3 <= b.1 + 0.1
            || b.1 + b.3 <= a.1 + 0.1,
        "overlap: {a:?}, {b:?}"
    );
}

#[test]
fn settings_window_uses_proportional_clamped_dimensions() {
    let set = overlay();
    for (sw, sh, width, height) in [
        (1280.0, 820.0, 921.6, 639.6),
        (800.0, 600.0, 576.0, 468.0),
        (560.0, 600.0, 528.0, 468.0),
        (2000.0, 1200.0, 980.0, 760.0),
    ] {
        let panel = settings_panel_geometry(&set, 8.0, 16.0, sw, sh).rect;
        assert!((panel.2 - width).abs() < 0.1, "{panel:?}");
        assert!((panel.3 - height).abs() < 0.1, "{panel:?}");
        assert!((panel.0 - (sw - width) / 2.0).abs() < 0.1);
        assert!((panel.1 - (sh - height) / 2.0).abs() < 0.1);
    }
}

#[test]
fn settings_panes_rows_chips_search_and_footer_do_not_overlap() {
    for surface in [(1280.0, 820.0), (800.0, 600.0), (560.0, 600.0)] {
        for focus in 0..12 {
            let mut set = overlay();
            set.focused_row = focus;
            set.footer_note = Some("Active GPU: Test (Integrated, Metal)\nRestart Kettle or open a new window to apply pending changes.".into());
            let layout = settings_layout(&set, (8.0, 16.0), surface, 8.0);
            inside(
                (16.0, 16.0, surface.0 - 32.0, surface.1 - 32.0),
                layout.rect,
            );
            assert_eq!(layout.sidebar_rect.is_some(), surface.0 == 1280.0);
            if let Some(side) = layout.sidebar_rect {
                inside(layout.rect, side);
                disjoint(side, layout.content_rect);
                assert!((side.2 - 239.616).abs() < 0.1);
                inside(side, layout.search_rect);
            } else {
                inside(layout.content_rect, layout.search_rect);
            }
            inside(layout.rect, layout.content_rect);
            inside(layout.content_rect, layout.title_rect);
            let mut controls = vec![layout.search_rect, layout.title_rect];
            controls.extend(layout.category_rects.iter().flatten().copied());
            controls.extend(
                [layout.category_previous, layout.category_next]
                    .into_iter()
                    .flatten()
                    .map(|(_, rect)| rect),
            );
            for row in &layout.rows {
                inside(layout.content_rect, row.rect);
                inside(row.rect, row.label_rect);
                inside(row.rect, row.description_rect);
                inside(row.rect, row.chip_rect);
                disjoint(row.label_rect, row.chip_rect);
                disjoint(row.description_rect, row.chip_rect);
                controls.push(row.rect);
            }
            assert!(layout.rows.iter().any(|row| row.index == focus));
            controls.extend(layout.footer.iter().map(|(_, r)| *r));
            for (i, &a) in controls.iter().enumerate() {
                inside(layout.rect, a);
                for &b in &controls[i + 1..] {
                    disjoint(a, b);
                }
            }
            assert!(layout.footer.last().unwrap().0.contains("changes."));
        }
    }
}

#[test]
fn settings_hit_targets_and_editor_share_the_painted_rectangles() {
    let mut set = overlay();
    for surface in [(1280.0, 820.0), (800.0, 600.0), (560.0, 600.0)] {
        for cw in [4.0, 8.0, 16.0] {
            for active in 0..set.categories.len() {
                set.active_category = active;
                let layout = settings_layout(&set, (cw, 16.0), surface, 8.0);
                let hit = |r: Rect4| {
                    settings_hit_test_with_text_width(
                        &set,
                        (cw, 16.0),
                        surface,
                        (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0),
                        8.0,
                    )
                };
                assert_eq!(hit(layout.search_rect), SettingsHit::Search);
                assert_eq!(
                    hit(layout.category_rects[active].unwrap()),
                    SettingsHit::Category(active)
                );
                for row in &layout.rows {
                    assert_eq!(hit(row.chip_rect), SettingsHit::Field(row.index));
                    assert_eq!(hit(row.label_rect), SettingsHit::Field(row.index));
                    assert_eq!(hit(row.description_rect), SettingsHit::Field(row.index));
                }
                let value = settings_value_rect(&set, cw, 16.0, surface.0, surface.1, 8.0);
                inside(layout.rows[0].chip_rect, value);
                assert!(value.2 > 0.0 && value.3 >= 16.0);
                assert_eq!(hit(layout.title_rect), SettingsHit::Inert);
                assert_eq!(
                    settings_hit_test_with_text_width(&set, (cw, 16.0), surface, (0.0, 0.0), 8.0),
                    SettingsHit::Outside
                );
            }
        }
    }
}

#[test]
fn settings_search_groups_and_empty_state_use_real_layout_slots() {
    let mut set = overlay();
    set.search_query = "setting".into();
    set.rows.truncate(4);
    set.rows[2].category = 3;
    set.rows[3].category = 3;
    let layout = settings_layout(&set, (8.0, 16.0), (1280.0, 820.0), 8.0);
    assert_eq!(
        layout.groups.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        [0, 3]
    );
    for (_, rect) in &layout.groups {
        for row in &layout.rows {
            disjoint(*rect, row.rect);
        }
    }
    set.rows.clear();
    let layout = settings_layout(&set, (8.0, 16.0), (560.0, 600.0), 8.0);
    let items = super::settings::settings_text_items(&set, &layout, 8.0);
    assert!(
        items
            .iter()
            .any(|item| item.text == "No settings match" && item.rect == layout.empty_rect)
    );
    assert!(layout.rows.is_empty() && layout.groups.is_empty());
}

#[test]
fn background_preview_opacity_uses_page_identity_in_every_language() {
    let mut set = overlay();
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        set.categories[0] = tr
            .text(kettle_i18n::Text::SettingsCategoryBackground)
            .into();
        set.background_page = true;
        assert_eq!(set.backdrop_alpha(), 0.30);
        set.background_page = false;
        assert_eq!(set.backdrop_alpha(), 0.55);
    }
}

#[test]
fn settings_choice_chips_keep_both_arrows_when_values_are_shortened() {
    let mut set = overlay();
    set.rows.truncate(1);
    set.rows[0].control = SettingsControl::Choice;
    set.rows[0].value = "A very long theme or GPU name that cannot fit in the chip".into();
    for surface in [(1280.0, 820.0), (560.0, 600.0)] {
        let layout = settings_layout(&set, (8.0, 16.0), surface, 8.0);
        let items = super::settings::settings_text_items(&set, &layout, 8.0);
        let value = items
            .iter()
            .find(|item| item.kind == super::SettingsTextKind::Value(0))
            .unwrap();
        assert!(
            value.text.starts_with("‹ ") && value.text.ends_with(" ›"),
            "{}",
            value.text
        );
        assert!(value.text.contains('…'));
    }
}

#[test]
fn short_settings_windows_keep_the_focused_row_editable() {
    for surface in [
        (800.0, 200.0),
        (800.0, 120.0),
        (360.0, 120.0),
        (1280.0, 120.0),
    ] {
        for query in ["", "setting"] {
            for focus in 0..12 {
                let mut set = overlay();
                set.focused_row = focus;
                set.search_query = query.into();
                let layout = settings_layout(&set, (8.0, 16.0), surface, 8.0);
                let row = layout
                    .rows
                    .iter()
                    .find(|r| r.index == focus)
                    .expect("short windows must retain the focused row");
                inside(layout.rect, row.rect);
                inside(row.rect, row.chip_rect);
                disjoint(layout.search_rect, row.rect);
                for category in layout.category_rects.iter().flatten() {
                    disjoint(*category, row.rect);
                }
                let value = settings_value_rect(&set, 8.0, 16.0, surface.0, surface.1, 8.0);
                inside(row.chip_rect, value);
                assert!(value.2 > 0.0 && value.3 >= 16.0);
                for (_, group) in &layout.groups {
                    disjoint(*group, row.rect);
                }
            }
        }
    }
}

#[test]
fn narrow_settings_categories_are_reachable_through_pointer_transitions() {
    for surface in [(800.0, 600.0), (560.0, 600.0)] {
        let mut set = overlay();
        let mut pending = vec![0];
        let mut visited = vec![false; set.categories.len()];
        while let Some(active) = pending.pop() {
            if visited[active] {
                continue;
            }
            visited[active] = true;
            set.active_category = active;
            let layout = settings_layout(&set, (8.0, 16.0), surface, 8.0);
            assert!(layout.category_rects[active].is_some());
            let category_y = layout.category_rects[active].unwrap().1 + 1.0;
            for x in 0..surface.0 as usize {
                if let SettingsHit::Category(index) = settings_hit_test_with_text_width(
                    &set,
                    (8.0, 16.0),
                    surface,
                    (x as f32, category_y),
                    8.0,
                ) && !visited[index]
                {
                    pending.push(index);
                }
            }
        }
        assert!(
            visited.iter().all(|v| *v),
            "unreachable categories: {visited:?}"
        );
    }
}

#[test]
fn resize_chip_is_hidden_while_settings_or_a_picker_is_open() {
    let mut overlay = super::Overlay {
        resize_overlay: Some((100, 37)),
        ..Default::default()
    };
    assert_eq!(overlay.visible_resize_overlay(), Some((100, 37)));
    overlay.settings = Some(super::SettingsOverlay::default());
    assert_eq!(overlay.visible_resize_overlay(), None);
    overlay.settings = None;
    for kind in 0..4 {
        overlay.palette_query = (kind == 0).then(String::new);
        overlay.ssh_query = (kind == 1).then(String::new);
        overlay.layout_picker_query = (kind == 2).then(String::new);
        overlay.theme_picker_query = (kind == 3).then(String::new);
        assert_eq!(overlay.visible_resize_overlay(), None);
    }
    overlay.theme_picker_query = None;
    assert_eq!(overlay.visible_resize_overlay(), Some((100, 37)));
    overlay.context_menu = Some(super::ContextMenu {
        picker_header_height: 30.0,
        picker_footer: None,
        anchor: (176.0, 78.0),
        rows: Vec::new(),
        highlight: 0,
        scroll_offset: 0,
        panel_w_clamped: 448.0,
        panel_h_clamped: 224.0,
        a11y_label: None,
    });
    assert_eq!(overlay.visible_resize_overlay(), None);
    overlay.context_menu.as_mut().unwrap().picker_header_height = 0.0;
    assert_eq!(overlay.visible_resize_overlay(), Some((100, 37)));
}

/// The chip says what kettle-ui asked for, the font size after a font
/// change, and the bare grid otherwise; hidden, it says nothing.
#[test]
fn resize_chip_text_is_the_caption_or_the_grid() {
    let mut overlay = super::Overlay {
        resize_overlay: Some((80, 24)),
        ..Default::default()
    };
    assert_eq!(overlay.visible_resize_label().as_deref(), Some("80×24"));
    overlay.resize_overlay_text = Some("Font size 14 · 80×24".into());
    assert_eq!(
        overlay.visible_resize_label().as_deref(),
        Some("Font size 14 · 80×24")
    );
    overlay.settings = Some(super::SettingsOverlay::default());
    assert_eq!(overlay.visible_resize_label(), None);
}

#[test]
fn category_arrows_and_wheel_page_forward_and_back_without_hiding_selection() {
    for surface in [(800.0, 600.0), (560.0, 600.0), (360.0, 120.0)] {
        for wheel in [false, true] {
            let mut set = overlay();
            for direction in [1, -1] {
                for _ in 0..set.categories.len() {
                    let layout = settings_layout(&set, (8.0, 16.0), surface, 8.0);
                    let arrow = if direction > 0 {
                        layout.category_next
                    } else {
                        layout.category_previous
                    };
                    let Some((target, rect)) = arrow else {
                        break;
                    };
                    let point = if wheel {
                        let active = layout.category_rects[set.active_category].unwrap();
                        (active.0 + 1.0, active.1 + 1.0)
                    } else {
                        (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0)
                    };
                    assert_eq!(
                        layout.pointer_hit(point, wheel.then_some(-direction)),
                        SettingsHit::Category(target)
                    );
                    assert!(
                        (direction > 0 && target > set.active_category)
                            || (direction < 0 && target < set.active_category)
                    );
                    set.active_category = target;
                    assert!(
                        settings_layout(&set, (8.0, 16.0), surface, 8.0).category_rects[target]
                            .is_some()
                    );
                }
                let layout = settings_layout(&set, (8.0, 16.0), surface, 8.0);
                if direction > 0 {
                    assert!(layout.category_rects.last().unwrap().is_some());
                    assert!(layout.category_next.is_none());
                } else {
                    assert!(layout.category_rects[0].is_some());
                    assert!(layout.category_previous.is_none());
                }
                let active = layout.category_rects[set.active_category].unwrap();
                assert_eq!(
                    layout.pointer_hit((active.0 + 1.0, active.1 + 1.0), Some(-direction)),
                    SettingsHit::Inert
                );
            }
        }
    }
}
