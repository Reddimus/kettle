#[test]
fn layout_picker_reads_the_directory_only_when_it_opens() {
    let src = production_source();
    let overlay = src
        .split("fn overlay(")
        .nth(1)
        .and_then(|rest| rest.split("fn redraw(").next())
        .expect("overlay body");
    assert!(!overlay.contains("Session::list_layouts()"));
    assert!(src.contains("ws.layout_picker_entries = crate::session::Session::list_layouts();"));
}

/// Drift guard. `rank_layouts` filters layout names by
/// every lower-cased query token; empty query returns identity.
#[test]
fn rank_layouts_filters_by_tokens_case_insensitive() {
    let layouts: Vec<String> = ["dev", "work", "dev-rust", "Notes", "ssh prod"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Empty query — identity, every layout shows.
    assert_eq!(rank_layouts("", &layouts), vec![0, 1, 2, 3, 4]);
    // Whitespace-only query — still identity (trim).
    assert_eq!(rank_layouts("   ", &layouts), vec![0, 1, 2, 3, 4]);
    // Single token, case-insensitive.
    assert_eq!(rank_layouts("dev", &layouts), vec![0, 2]);
    assert_eq!(rank_layouts("DEV", &layouts), vec![0, 2]);
    // Multi-token AND (every token must match).
    assert_eq!(rank_layouts("dev rust", &layouts), vec![2]);
    assert_eq!(rank_layouts("ssh prod", &layouts), vec![4]);
    // Lower-cased target matches mixed-case layouts.
    assert_eq!(rank_layouts("notes", &layouts), vec![3]);
    // No matches → empty (not a panic).
    assert_eq!(rank_layouts("xyz", &layouts), Vec::<usize>::new());
    // Empty layouts → empty (defensive).
    assert_eq!(rank_layouts("dev", &[]), Vec::<usize>::new());
}

#[test]
fn picker_candidates_are_vertical_rows_and_selection_scrolls_into_view() {
    let list = command_picker_list(
        "",
        20,
        &kettle_config::keybinds::defaults(),
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
        None,
        Some(&[]),
    );
    let menu = picker_context_menu(list, (320.0, 160.0), (8.0, 16.0), (8.0, 16.0), 0.0)
        .expect("a 320x160 surface fits a picker card");

    assert!(
        menu.rows.len() > 20,
        "the projection must retain one row per ranked command instead of flattening matches"
    );
    assert_eq!(menu.highlight, 20);
    assert!(menu.scroll_offset > 0);
    let visible_rows =
        (menu.panel_h_clamped / kettle_render::menu::row_height(16.0)).floor() as usize;
    assert!(
        (menu.scroll_offset..menu.scroll_offset + visible_rows).contains(&menu.highlight),
        "the command Enter will run must remain inside the visible row window"
    );
    assert!(
        menu.rows.iter().any(|row| !row.hint.is_empty()),
        "command rows must carry live keybind hints in the menu hint column"
    );
    assert!(
        menu.anchor.1 + menu.panel_h_clamped <= 144.0,
        "the card must stay inside the surface's bottom margin"
    );
}

#[test]
fn layout_and_ssh_pickers_keep_ranked_candidates_on_separate_rows() {
    let layouts = ["dev", "dev-rust", "notes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let layout = layout_picker_list(
        "dev",
        1,
        &layouts,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(
        layout
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["dev", "dev-rust"]
    );
    assert_eq!(layout.selected, 1);

    let hosts = [
        ("staging-prod".to_string(), "dev@staging".to_string()),
        ("gpu-prod".to_string(), "dev@gpu".to_string()),
        ("gpu".to_string(), "dev@gpu-short".to_string()),
    ];
    assert_eq!(rank_ssh_hosts("gp", &hosts), vec![2, 1, 0]);
    let ssh = ssh_picker_list(
        "gp",
        1,
        &hosts,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(
        ssh.rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["gpu", "gpu-prod", "staging-prod"]
    );
    assert_eq!(ssh.selected, 1);
    assert_eq!(
        selected_ssh_target("gp", 1, &hosts).as_deref(),
        Some("dev@gpu")
    );
    assert_eq!(
        selected_ssh_target("someone@example.test", 0, &hosts).as_deref(),
        Some("someone@example.test"),
        "an unmatched query must retain direct user-at-host launches"
    );
    assert_eq!(selected_ssh_target("", 0, &[]), None);
    assert_eq!(
        selected_ssh_target("gp", usize::MAX, &hosts).as_deref(),
        Some("dev@staging"),
        "dispatch must clamp to the same last visible row after a host-list reload"
    );
}

#[test]
fn picker_scroll_offset_handles_tiny_and_invalid_geometry() {
    assert_eq!(picker_scroll_offset(12, 11, 72.0, 24.0), 9);
    assert_eq!(picker_scroll_offset(12, 0, 72.0, 24.0), 0);
    assert_eq!(picker_scroll_offset(0, 0, 72.0, 24.0), 0);
    assert_eq!(picker_scroll_offset(12, 11, f32::NAN, 24.0), 0);
    assert_eq!(picker_scroll_offset(12, 11, 72.0, 0.0), 0);
}

#[test]
fn finding_layout_picker_reaps_its_children() {
    let body = finding_body(include_str!("modals.rs"), "layout_picker_key");
    assert!(
        body.contains("child.wait()"),
        "layout launches need a background wait"
    );
    assert!(body.contains("std::thread::Builder::new()"));
}

#[test]
fn review_activating_the_selected_layout_requests_a_frame() {
    use super::{PickerPointer, apply_picker_pointer};
    let mut ws = WindowState::new(0, Mux::new());
    ws.layout_picker_entries = vec!["alpha".into()];
    ws.layout_picker_input = Some((Default::default(), 0));
    let menu = picker_context_menu(
        layout_picker_list("", 0, &ws.layout_picker_entries, &EN_TEXT),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        0.0,
    )
    .unwrap();
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::scroll_content(
        menu.rows.len() as f32 * row_h,
        menu.panel_h_clamped,
        row_h,
    );
    ws.cursor.x = f64::from(menu.anchor.0 + 10.0);
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 0.5);
    ws.accessibility_pending = false;
    assert!(apply_picker_pointer(
        &mut ws,
        &menu,
        16.0,
        PickerPointer::Press
    ));
    assert!(
        !ws.accessibility_pending,
        "clicking the selected row does not repaint selection"
    );
    let body = finding_body(include_str!("modals.rs"), "layout_picker_key");
    let enter = body
        .split("Key::Named(NamedKey::Enter) => {")
        .nth(1)
        .unwrap();
    let dismissed = enter
        .split("ws.layout_picker_input = None;")
        .nth(1)
        .unwrap();
    let before_launch = dismissed.split("if let Some(name) = name").next().unwrap();
    assert!(
        before_launch.contains("request_redraw()"),
        "dismissal must request a frame even if the layout launch fails"
    );
}

#[test]
fn picker_card_geometry_is_centered_below_chrome() {
    for (surface, expected_width) in [
        ((1280.0, 820.0), 716.8),
        ((640.0, 820.0), 360.0),
        ((360.0, 600.0), 328.0),
    ] {
        let menu = picker_context_menu(
            layout_picker_list("", 0, &["one".into(), "two".into()], &EN_TEXT),
            surface,
            (8.0, 16.0),
            (8.0, 16.0),
            0.0,
        )
        .unwrap();
        assert!((menu.panel_w_clamped - expected_width).abs() < 0.01);
        assert!((menu.anchor.0 * 2.0 + menu.panel_w_clamped - surface.0).abs() < 0.01);
        assert!((menu.anchor.1 - (surface.1 * 0.08 + 30.0)).abs() < 0.01);
        assert_eq!(menu.panel_h_clamped, 56.0);
    }
}

#[test]
fn picker_header_and_rows_share_geometry_below_top_chrome() {
    for surface in [
        (1280.0, 820.0),
        (640.0, 820.0),
        (360.0, 600.0),
        (1920.0, 1080.0),
    ] {
        for top in [0.0, 46.0] {
            for cell in [(8.0, 16.0), (16.0, 32.0)] {
                let geometry = super::picker_card_geometry(surface, cell, top, 2).unwrap();
                let (x, y, w, h) = geometry.header;
                assert_eq!(geometry.rows, (x, y + h, w, 2.0 * kettle_render::menu::row_height(cell.1)));
                assert!((2.0 * x + w - surface.0).abs() < 0.01);
                assert!(x >= 16.0 && w <= 720.0 && w >= 360.0_f32.min(surface.0 - 32.0));
                assert!((y - top - 24.0_f32.max((surface.1 - top) * 0.08)).abs() < 0.01);
                assert_eq!(h, cell.1 + 14.0);
            }
        }
    }
    for surface in [(f32::NAN, 600.0), (800.0, 40.0), (20.0, 600.0)] {
        assert!(super::picker_card_geometry(surface, (8.0, 16.0), 0.0, 2).is_none());
    }
}

#[test]
fn picker_press_and_diagnostics_use_the_card_rows() {
    use super::{PickerPointer, apply_picker_pointer};
    let mut ws = WindowState::new(0, Mux::new());
    ws.layout_picker_input = Some((Default::default(), 0));
    let menu = picker_context_menu(
        layout_picker_list("", 0, &["alpha".into(), "beta".into()], &EN_TEXT),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        46.0,
    )
    .unwrap();
    assert_eq!(menu.anchor.0, 176.0);
    assert!((menu.anchor.1 - 120.32).abs() < 0.01);
    ws.cursor.x = 186.0;
    ws.cursor.y = 162.32;
    assert!(apply_picker_pointer(
        &mut ws,
        &menu,
        16.0,
        PickerPointer::Press
    ));
    assert_eq!(ws.layout_picker_input.as_ref().unwrap().1, 1);
    ws.cursor.y = 100.0;
    assert!(!apply_picker_pointer(
        &mut ws,
        &menu,
        16.0,
        PickerPointer::Press
    ));
    assert!(!apply_picker_pointer(
        &mut ws,
        &menu,
        16.0,
        PickerPointer::Wheel(1)
    ));
    let overlay = kettle_render::Overlay {
        context_menu: Some(menu),
        ..Default::default()
    };
    let diagnostic = super::ctl::picker_diagnostic_geometry(&overlay).unwrap();
    assert_eq!(diagnostic["rect"]["width"], 448.0);
    assert_eq!(diagnostic["rect"]["height"], 86.0);
    assert!((diagnostic["rect"]["y"].as_f64().unwrap() - 90.32).abs() < 0.01);
    assert_eq!(diagnostic["rows"][1]["index"], 1);
    assert!((diagnostic["rows"][1]["rect"]["y"].as_f64().unwrap() - 148.32).abs() < 0.01);
    assert!(!diagnostic.to_string().contains("alpha"));
}

#[test]
fn palette_ranges_follow_translated_labels_and_truncation() {
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    let list = command_picker_list(
        "ct",
        0,
        &kettle_config::keybinds::defaults(),
        &es,
        None,
        Some(&[]),
    );
    for row in &list.rows {
        assert_eq!(
            row.matched_chars,
            kettle_config::palette::matched_char_positions("ct", &row.label).map(|positions| {
                positions
                    .into_iter()
                    .map(|index| index..index + 1)
                    .collect()
            })
        );
    }
    let mut row = ContextMenuRow {
        toggle: None,
        swatches: None,
        label: "año abcdefghijklmnop".into(),
        matched_chars: Some(vec![0..3, 6..19]),
        hint: String::new(),
        enabled: true,
        separator: false,
    };
    fit_context_menu_row(&mut row, 6);
    assert_eq!(row.label, "año a…");
    assert_eq!(row.matched_chars, Some(std::iter::once(0..3).collect()));
    let empty = command_picker_list(
        "zzzzzzzzz",
        0,
        &kettle_config::keybinds::defaults(),
        &es,
        None,
        Some(&[]),
    );
    assert_eq!(empty.rows.len(), 1);
    assert!(!empty.rows[0].enabled);
    assert!(empty.rows[0].matched_chars.is_none());
}

#[test]
fn short_picker_windows_keep_header_and_selected_result_visible() {
    for top in [0.0, 24.0, 46.0] {
        for surface in [(360.0, 120.0), (800.0, 120.0), (800.0, 200.0)] {
            let list = command_picker_list(
                "",
                20,
                &kettle_config::keybinds::defaults(),
                &EN_TEXT,
                None,
                Some(&[]),
            );
            let menu = picker_context_menu(list, surface, (8.0, 16.0), (8.0, 16.0), top)
                .expect("a restored 120px window must retain its picker");
            let card = menu.card_rect();
            assert!(card.1 >= top);
            assert!(card.1 + card.3 <= surface.1);
            assert!(menu.picker_header_height >= 16.0);
            let row = menu
                .visible_row_rect(menu.highlight)
                .expect("selected result is visible");
            assert!(row.1 >= menu.anchor.1);
            assert!(row.1 + row.3 <= menu.anchor.1 + menu.panel_h_clamped + 0.01);
            let mut ws = WindowState::new(0, Mux::new());
            ws.palette_input = Some((Default::default(), 20));
            ws.cursor.x = (row.0 + 1.0) as f64;
            ws.cursor.y = (row.1 + 1.0) as f64;
            assert!(super::apply_picker_pointer(
                &mut ws,
                &menu,
                16.0,
                super::PickerPointer::Press
            ));
            assert_eq!(ws.palette_input.as_ref().unwrap().1, menu.highlight);
        }
    }
}

#[test]
fn short_pending_palette_preserves_input_action_and_accessibility_with_scaled_cells() {
    use kettle_i18n::{Language, Text, Translator};
    use unicode_width::UnicodeWidthStr as _;

    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let cfg = kettle_config::Config::default();
        for (cell, text_cell) in [
            ((8.0, 16.0), (8.0, 16.0)),
            ((4.0, 16.0), (8.0, 16.0)),
            ((8.0, 8.0), (8.0, 16.0)),
            ((4.5, 9.0), (9.0, 18.0)),
            ((8.0, 16.0), (16.0, 32.0)),
        ] {
            for top in [0.0, 24.0, 46.0] {
                for surface in [(360.0, 120.0), (800.0, 120.0), (800.0, 200.0)] {
                    for query in [String::new(), "split".into(), "query".repeat(40)] {
                        let commands = super::palette_entries(None, &tr, None);
                        let ranked = kettle_config::palette::rank_entries(&query, &commands, &tr);
                        let selected = 20.min(ranked.len().saturating_sub(1));
                        let mut ws = WindowState::new(1, Mux::new());
                        super::profiles::open_profile_palette(&mut ws, None);
                        let mut editor = crate::text_editor::TextEditor::from(query.as_str());
                        editor.move_left(true, false);
                        ws.palette_input = Some((editor, selected));
                        ws.ime_preedit = Some(("composing".into(), Some((2, 5))));
                        let before = ws.palette_input.clone();
                        let preedit = ws.ime_preedit.clone();
                        let (text, cursor_byte, selection) = super::project_modal_editor(
                            &ws.palette_input.as_ref().unwrap().0,
                            ws.ime_preedit.as_ref(),
                        );
                        let menu = picker_context_menu(
                            super::open_picker_list(&ws, &cfg, &tr, None).unwrap(),
                            surface,
                            cell,
                            text_cell,
                            top,
                        )
                        .expect("pending status cannot hide the input and selected action");
                        let card = menu.card_rect();
                        assert!(card.1 >= top && card.1 + card.3 <= surface.1 + 0.01);
                        assert_eq!(menu.picker_header_height, cell.1 + 14.0);
                        assert_eq!(menu.rows.len(), ranked.len().max(1));
                        assert_eq!(menu.highlight, selected);
                        let row = menu
                            .visible_row_rect(selected)
                            .expect("selected row is visible");
                        assert!(row.1 + row.3 <= surface.1 + 0.01);
                        if let Some(status) = &menu.status {
                            let band = menu.status_rect().unwrap();
                            for (index, line) in status.lines.iter().enumerate() {
                                let slot = status.line_rect(band, index);
                                assert!(line.width() as f32 * text_cell.0 <= slot.2);
                                assert!(slot.3 >= text_cell.1);
                                assert!(slot.1 + slot.3 <= band.1 + band.3 + 0.01);
                            }
                            ws.cursor.x = (band.0 + 1.0) as f64;
                            ws.cursor.y = (band.1 + 1.0) as f64;
                            assert!(!super::apply_picker_pointer(
                                &mut ws,
                                &menu,
                                cell.1,
                                super::PickerPointer::Press,
                            ));
                        }
                        let mut field = kettle_render::TextFieldOverlay {
                            picker_header: true,
                            text,
                            cursor_byte,
                            selection,
                            horizontal_scroll: 3,
                            rect: (0.0, 0.0, 0.0, 0.0),
                            prefix: "  \u{2318} ".into(),
                            hint: super::picker_hint(&tr, &[("Esc", Text::PickerHintCancel)]),
                            accent_palette: 5,
                        };
                        let original = field.clone();
                        super::place_picker_header(&mut field, &menu, true);
                        assert_eq!(field.rect, menu.header_rect());
                        assert_eq!(field.text, original.text);
                        assert_eq!(field.cursor_byte, original.cursor_byte);
                        assert_eq!(field.selection, original.selection);
                        assert_eq!(field.horizontal_scroll, original.horizontal_scroll);
                        let cw = kettle_render::text_field_cell_width(
                            &field, text_cell.0, text_cell.1,
                        );
                        let slots = kettle_render::text_field_rects(&field, cw);
                        assert!(
                            slots[1].2 > cw,
                            "query and caret retain a visible input slot"
                        );
                        if menu.status.is_none() {
                            assert!(
                                kettle_render::text_field_labels(&field, cw)
                                    .0
                                    .contains("Esc")
                            );
                        }
                        assert_eq!(ws.palette_input, before);
                        assert_eq!(ws.ime_preedit, preedit);
                        if let Some(&entry) = ranked.get(selected) {
                            ws.cursor.x = (row.0 + 1.0) as f64;
                            ws.cursor.y = (row.1 + 1.0) as f64;
                            assert!(super::apply_picker_pointer(
                                &mut ws,
                                &menu,
                                cell.1,
                                super::PickerPointer::Press,
                            ));
                            assert_eq!(ws.palette_input, before);
                            assert_eq!(
                                super::selected_palette_action(&query, selected, &commands, &tr),
                                Some(commands[entry].1.clone())
                            );
                        } else {
                            assert!(!menu.rows[0].enabled);
                        }
                        let overlay = kettle_render::Overlay {
                            tr,
                            palette_query: Some(query.clone()),
                            context_menu: Some(menu),
                            text_field: Some(field),
                            ..Default::default()
                        };
                        let a11y = super::modal_accessibility_projection_for_window(
                            &overlay, &ws, surface, text_cell, 1.0, &tr,
                        );
                        let input = a11y
                            .nodes
                            .iter()
                            .find(|(_, node)| node.role() == accesskit::Role::TextInput)
                            .unwrap();
                        assert_eq!(
                            input.1.description(),
                            Some(tr.text(Text::MenuProfilesPending))
                        );
                        let status = a11y
                            .nodes
                            .iter()
                            .find(|(_, node)| node.role() == accesskit::Role::Status)
                            .unwrap();
                        assert_eq!(
                            status.1.label(),
                            Some(tr.text(Text::MenuProfilesPending))
                        );
                        assert_eq!(status.1.live(), Some(accesskit::Live::Polite));
                    }
                }
            }
        }
    }
}

#[test]
fn short_pending_palette_degrades_guidance_then_explanation_before_omitting_the_band() {
    use kettle_i18n::{Language, Text, Translator};
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        for top in [0.0, 24.0, 46.0] {
            let menu = picker_context_menu(
                command_picker_list(
                    "", 20, &kettle_config::keybinds::defaults(), &tr, None, None,
                ),
                (360.0, 120.0),
                (8.0, 16.0),
                (8.0, 16.0),
                top,
            )
            .unwrap();
            let shown = menu.status.as_ref().map(|status| status.lines.join(" "));
            let expected = match top {
                0.0 => Some(tr.text(Text::MenuProfilesPending)),
                24.0 => Some(tr.text(Text::PickerProfilesPending)),
                _ => None,
            };
            assert_eq!(shown.as_deref(), expected);
            assert!(menu.visible_row_rect(20).is_some());
        }
    }
}
