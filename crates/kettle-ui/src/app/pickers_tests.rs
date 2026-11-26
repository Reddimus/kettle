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
        false,
        kettle_config::BroadcastDefault::default(),
    );
    let menu = picker_context_menu(list, (320.0, 160.0), (8.0, 16.0), (8.0, 16.0), 0.0)
        .expect("a 320x160 surface fits a picker card");

    assert!(
        menu.rows.len() > 20,
        "the projection must retain one row per ranked command instead of flattening matches"
    );
    assert_eq!(kettle_render::menu_row_option(&menu.rows, menu.highlight), Some(20));
    assert!(menu.highlight > 20, "headings above it");
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
        &[],
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(
        ssh.rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Connect to \"gp\"",
            "gpu",
            "gpu-prod",
            "staging-prod",
            "Save \"gp\" as a host…"
        ]
    );
    assert_eq!(ssh.selected, 1);
    let connect = |target: &str| Some(super::SshEnter::Connect(target.to_string()));
    assert_eq!(ssh_row_action("gp", 1, &hosts, &[]), connect("dev@gpu-short"));
    assert_eq!(ssh_row_action("gp", 2, &hosts, &[]), connect("dev@gpu"));
    assert_eq!(
        ssh_row_action("someone@example.test", 0, &hosts, &[]),
        connect("someone@example.test"),
        "an unmatched query must retain direct user-at-host launches"
    );
    assert_eq!(ssh_row_action("", 0, &[], &[]), None);
    assert_eq!(ssh_row_action("gp", 0, &hosts, &[]), connect("gp"));
    assert_eq!(
        ssh_row_action("gp", usize::MAX, &hosts, &[]),
        connect("dev@staging"),
        "dispatch must clamp to the last host row after a host-list reload, never to Save"
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
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    ws.cursor.x = f64::from(menu.anchor.0 + 10.0);
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 0.5);
    ws.accessibility_pending = false;
    assert!(apply_picker_pointer(
        &mut ws,
        &kettle_config::Config::default(),
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
                let geometry = super::picker_card_geometry(surface, cell, top, 2, 1.0).unwrap();
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
        assert!(super::picker_card_geometry(surface, (8.0, 16.0), 0.0, 2, 1.0).is_none());
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
        &kettle_config::Config::default(),
        &menu,
        16.0,
        PickerPointer::Press
    ));
    assert_eq!(ws.layout_picker_input.as_ref().unwrap().1, 1);
    ws.cursor.y = 100.0;
    assert!(!apply_picker_pointer(
        &mut ws,
        &kettle_config::Config::default(),
        &menu,
        16.0,
        PickerPointer::Press
    ));
    assert!(!apply_picker_pointer(
        &mut ws,
        &kettle_config::Config::default(),
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
        false,
        kettle_config::BroadcastDefault::default(),
    );
    let entries = super::palette_entries(None, &es, Some(&[]), kettle_config::BroadcastDefault::default());
    let ranked = kettle_config::palette::rank_matches("ct", &entries, &es);
    assert_eq!(list.rows.len(), ranked.len());
    for (row, entry) in list.rows.iter().zip(ranked) {
        assert_eq!(row.label, entries[entry.index].label.shown(&es));
        assert_eq!(
            row.matched_chars,
            entry.matched_chars.map(|positions| {
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
        ..Default::default()
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
        false,
        kettle_config::BroadcastDefault::default(),
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
                false,
                kettle_config::BroadcastDefault::default(),
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
                &kettle_config::Config::default(),
                &menu,
                16.0,
                super::PickerPointer::Press
            ));
            assert_eq!(ws.palette_input.as_ref().unwrap().1, 20);
            assert_eq!(
                kettle_render::menu_row_option(&menu.rows, menu.highlight),
                Some(20)
            );
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
                        let commands = super::palette_entries(None, &tr, None, kettle_config::BroadcastDefault::default());
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
                            super::open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap(),
                            surface,
                            cell,
                            text_cell,
                            top,
                        )
                        .expect("pending status cannot hide the input and selected action");
                        let card = menu.card_rect();
                        assert!(card.1 >= top && card.1 + card.3 <= surface.1 + 0.01);
                        assert_eq!(menu.picker_header_height, cell.1 + 14.0);
                        // The empty palette paints its commands under group
                        // headings: `selected` counts commands, the highlight
                        // is the row the selected one is painted on.
                        let options = menu.rows.iter().filter(|row| !row.heading).count();
                        assert_eq!(options, ranked.len().max(1));
                        assert_eq!(
                            kettle_render::menu_row_option(&menu.rows, menu.highlight),
                            Some(selected)
                        );
                        let row = menu
                            .visible_row_rect(menu.highlight)
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
                                &cfg,
                                &menu,
                                cell.1,
                                super::PickerPointer::Press,
                            ));
                        }
                        let mut field = kettle_render::TextFieldOverlay {
                            placeholder: String::new(),
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
                                &cfg,
                                &menu,
                                cell.1,
                                super::PickerPointer::Press,
                            ));
                            assert_eq!(ws.palette_input, before);
                            assert_eq!(
                                super::palette_enter_action(&query, selected, &commands, &tr),
                                Some(commands[entry].action.clone())
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
                command_picker_list("", 20, &kettle_config::keybinds::defaults(), &tr, None, None, false, kettle_config::BroadcastDefault::default()),
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
            assert!(menu.visible_row_rect(menu.highlight).is_some());
        }
    }
}

/// A scrolling picker paints whole rows down to its bottom edge: it has no
/// arrow strips, and its height is whole rows, so no blank part-row is left
/// under the selection, and a long list paints all twelve rows. The last
/// painted row takes a click.
#[test]
fn scrolling_pickers_paint_whole_rows_with_no_blank_strip() {
    let names: Vec<String> = (0..30).map(|i| format!("layout {i}")).collect();
    for surface in [(1280.0, 820.0), (800.0, 600.0), (640.0, 360.0), (800.0, 200.0)] {
        for cell in [(8.0, 16.0), (16.0, 34.0)] {
            for total in [3, 8, 12, 13, 30] {
                let menu = picker_context_menu(
                    layout_picker_list("", 0, &names[..total], &EN_TEXT),
                    surface,
                    cell,
                    cell,
                    32.0,
                )
                .unwrap();
                let row_h = kettle_render::menu::row_height(cell.1);
                let (_, content_h) =
                    kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
                let painted = (content_h / row_h).floor();
                let what = format!("{surface:?} {cell:?} {total} rows");
                assert!(
                    content_h - painted * row_h < 0.5,
                    "{what}: {:.1}px of blank part-row",
                    content_h - painted * row_h
                );
                let room = surface.1 - menu.anchor.1 - 16.0;
                if room >= 12.0 * row_h {
                    assert!(painted as usize >= total.min(12), "{what}: {painted} painted");
                }
                let last = menu.scroll_offset + painted as usize - 1;
                let rect = menu.visible_row_rect(last).expect("last painted row");
                let mut ws = WindowState::new(0, Mux::new());
                ws.layout_picker_input = Some((Default::default(), 0));
                ws.cursor.x = f64::from(rect.0 + 10.0);
                ws.cursor.y = f64::from(rect.1 + rect.3 - 2.0);
                assert!(super::apply_picker_pointer(
                    &mut ws,
                    &kettle_config::Config::default(),
                    &menu,
                    cell.1,
                    super::PickerPointer::Press
                ));
                assert_eq!(ws.layout_picker_input.as_ref().unwrap().1, last, "{what}");
            }
        }
    }
}

/// A long picker list fills its panel: the first row sits right under the
/// query header, all twelve rows are painted and hit, and paint, pointer and
/// accessibility agree on every row. A short window still shows a row.
#[test]
fn overflowing_picker_rows_start_at_the_header_and_fill_the_panel() {
    use super::{PickerPointer, apply_picker_pointer};
    let bindings = kettle_config::keybinds::defaults();
    let profiles: Vec<String> = (0..20).map(|i| format!("profile-{i}")).collect();
    let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, Some(&profiles), false, kettle_config::BroadcastDefault::default());
    assert!(list.rows.len() >= 150, "{} rows", list.rows.len());
    let cell = (8.0, 16.0);
    let geometry = super::picker_card_geometry((800.0, 600.0), cell, 0.0, list.rows.len(), 1.0).unwrap();
    let menu = picker_context_menu(list, (800.0, 600.0), cell, cell, 0.0).unwrap();
    let header_bottom = geometry.header.1 + geometry.header.3;
    let painted = kettle_render::context_menu_row_rects(&menu, cell);
    assert_eq!(painted.len(), 12, "twelve full rows");
    assert_eq!(painted[0].1.1, header_bottom, "no gap under the header");
    let row_h = kettle_render::menu::row_height(cell.1);
    assert_eq!(
        painted.last().unwrap().1.1 + row_h,
        menu.anchor.1 + menu.panel_h_clamped
    );
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((Default::default(), 0));
    for &(index, rect) in &painted {
        assert_eq!(menu.visible_row_rect(index), Some(rect), "row {index}");
        ws.cursor.x = f64::from(rect.0 + 10.0);
        ws.cursor.y = f64::from(rect.1 + rect.3 * 0.5);
        // A press selects the command on the row; a heading takes none.
        let before = ws.palette_input.as_ref().unwrap().1;
        apply_picker_pointer(&mut ws, &kettle_config::Config::default(), &menu, cell.1, PickerPointer::Press);
        let expected = kettle_render::menu_row_option(&menu.rows, index).unwrap_or(before);
        assert_eq!(ws.palette_input.as_ref().unwrap().1, expected, "row {index}");
    }
    assert_eq!(menu.visible_row_rect(12), None);
    // A window with room for one or two rows still paints and hits them.
    for height in [120.0, 150.0] {
        let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, Some(&[]), false, kettle_config::BroadcastDefault::default());
        let menu = picker_context_menu(list, (800.0, height), cell, cell, 0.0).unwrap();
        let painted = kettle_render::context_menu_row_rects(&menu, cell);
        assert!(!painted.is_empty(), "height {height}");
        assert_eq!(painted[0].1.1, menu.anchor.1);
        for &(index, rect) in &painted {
            assert_eq!(menu.visible_row_rect(index), Some(rect));
        }
    }
}

/// The keyboard-shortcut list is the palette with only bound commands: every
/// row shows its shortcut, it holds exactly the palette commands that have
/// one, and the row a selection index paints is the one Enter runs, because
/// the painter and the key handler rank the same view entries.
#[test]
fn bound_only_palette_lists_every_bound_command_with_its_shortcut() {
    let bindings = kettle_config::keybinds::defaults();
    for tr in [EN_TEXT, kettle_i18n::Translator::new(kettle_i18n::Language::Es)] {
        let list = command_picker_list("", 0, &bindings, &tr, None, Some(&[]), true, kettle_config::BroadcastDefault::default());
        let bound = kettle_config::palette::commands_for(kettle_config::BroadcastDefault::default())
            .into_iter()
            .filter(|(_, action)| {
                kettle_config::keybinds::ui_hint_label(&bindings, action)
                    .is_some_and(|hint| !hint.is_empty())
            })
            .count();
        assert!(bound >= 10, "{bound} bound commands");
        assert_eq!(list.rows.len(), bound);
        assert!(list.rows.iter().all(|row| row.enabled && !row.hint.is_empty()));
        let full = command_picker_list("", 0, &bindings, &tr, None, Some(&[]), false, kettle_config::BroadcastDefault::default());
        assert!(full.rows.len() > list.rows.len());
        assert!(full.rows.iter().any(|row| row.hint.is_empty()));
        for query in ["", "tab", "split", "font"] {
            let entries = super::palette_view_entries(None, &tr, Some(&[]), kettle_config::BroadcastDefault::default(), &bindings, true);
            let ranked = kettle_config::palette::rank_entries(query, &entries, &tr);
            let list = command_picker_list(query, 0, &bindings, &tr, None, Some(&[]), true, kettle_config::BroadcastDefault::default());
            assert_eq!(list.rows.len(), ranked.len().max(1), "{query:?}");
            for (row, index) in list.rows.iter().zip(&ranked) {
                assert_eq!(row.label, entries[*index].label.shown(&tr), "{query:?}");
            }
        }
    }
    // Enter, the pointer and the painted list read the same view entries;
    // `enter_in_the_shortcut_list_runs_the_selected_bound_row` checks the
    // lookup Enter makes in them.
    let source = finding_body(include_str!("modals.rs"), "palette_key");
    assert!(source.contains("palette_view_entries("));
    assert!(source.contains("ws.palette_bound_only"));
    assert!(source.contains("palette_enter_action(q, *sel, &cmds, &self.ui_text)"));
    // `open_picker_list` and the frame both take the palette from here
    // (`the_painted_palette_takes_the_window_scale_like_presses`).
    let open = include_str!("pickers.rs")
        .split("pub(super) fn palette_picker_list(")
        .nth(1)
        .unwrap();
    assert!(open.split("fn ").next().unwrap().contains("ws.palette_bound_only"));
    // Each open sets the mode: Keyboard Shortcuts on, the palette off.
    let arm = production_source()
        .split("Action::CommandPalette | Action::ShowKeybinds =>")
        .nth(1)
        .unwrap()
        .split("\n            }")
        .next()
        .unwrap()
        .to_string();
    assert!(arm.contains("ws.palette_bound_only = matches!(action, Action::ShowKeybinds);"));
    // The arm opens the palette through the profile snapshot helper, which
    // leaves the mode alone and sets the input.
    assert!(arm.contains("profiles::open_profile_palette("));
    let open_palette = include_str!("profiles.rs")
        .split("pub(super) fn open_profile_palette(")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    assert!(open_palette.contains("ws.palette_input = Some("));
    assert!(!open_palette.contains("palette_bound_only"));
}

/// Enter in the keyboard-shortcut list runs the row painted at the selection,
/// for every selection and query: a command with a shortcut, the label and
/// hint the list shows there. Past the last row it runs nothing.
#[test]
fn enter_in_the_shortcut_list_runs_the_selected_bound_row() {
    let bindings = kettle_config::keybinds::defaults();
    for tr in [EN_TEXT, kettle_i18n::Translator::new(kettle_i18n::Language::Es)] {
        let entries = super::palette_view_entries(None, &tr, Some(&[]), kettle_config::BroadcastDefault::default(), &bindings, true);
        let full = super::palette_view_entries(None, &tr, Some(&[]), kettle_config::BroadcastDefault::default(), &bindings, false);
        for query in ["", "tab", "split", "font", "pane"] {
            let list = command_picker_list(query, 0, &bindings, &tr, None, Some(&[]), true, kettle_config::BroadcastDefault::default());
            let matched = kettle_config::palette::rank_entries(query, &entries, &tr).len();
            assert!(matched > 1, "{query:?} needs several rows");
            for (selected, row) in list.rows.iter().enumerate() {
                let action = super::palette_enter_action(query, selected, &entries, &tr)
                    .unwrap_or_else(|| panic!("{query:?} row {selected}"));
                let hint = kettle_config::keybinds::ui_hint_label(&bindings, &action)
                    .unwrap_or_default();
                assert!(!hint.is_empty(), "{query:?} row {selected}: {action:?}");
                assert_eq!(row.hint, hint, "{query:?} row {selected}");
                let label = entries
                    .iter()
                    .find(|candidate| candidate.action == action)
                    .map(|candidate| candidate.label.shown(&tr));
                assert_eq!(label, Some(row.label.as_str()), "{query:?} row {selected}");
            }
            assert_eq!(list.rows.len(), matched, "{query:?}");
            assert_eq!(
                super::palette_enter_action(query, matched, &entries, &tr),
                None
            );
        }
        // The full palette at the same selections runs commands without a
        // shortcut too; the list above never did.
        let unbound = (0..full.len())
            .filter_map(|selected| super::palette_enter_action("", selected, &full, &tr))
            .find(|action| {
                kettle_config::keybinds::ui_hint_label(&bindings, action)
                    .is_none_or(|hint| hint.is_empty())
            });
        assert!(unbound.is_some());
        assert_eq!(super::palette_enter_action("qqzzxx", 0, &entries, &tr), None);
    }
}

fn ssh_fixture_hosts(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, target)| (name.to_string(), target.to_string()))
        .collect()
}

/// Enter on the open launcher, and the target it connects to, if any.
fn enter_target(ws: &mut WindowState, hosts: &[(String, String)]) -> Option<String> {
    match super::take_ssh_enter(ws, hosts) {
        Some(super::SshEnter::Connect(target)) => Some(target),
        _ => None,
    }
}

fn ssh_launcher_with(query: &str) -> WindowState {
    let mut ws = WindowState::new(0, Mux::new());
    let mut editor = crate::text_editor::TextEditor::default();
    editor.replace_all(query, crate::modal_input::MAX_MODAL_INPUT_BYTES);
    ws.ssh_input = Some((editor, 0));
    ws
}

#[test]
fn ssh_rows_list_configured_hosts_then_imported_aliases_once_each() {
    use super::SshRow::{Configured, Imported};
    let hosts = ssh_fixture_hosts(&[("prod", "me@prod.example"), ("box", "Bastion")]);
    let imported: Vec<String> = ["web", "PROD", "bastion", "db"]
        .map(String::from)
        .into();
    // `PROD` is a configured name and `bastion` a configured target, ignoring
    // ASCII case, so neither shows twice.
    assert_eq!(
        ssh_rows("", &hosts, &imported),
        [Configured(0), Configured(1), Imported(0), Imported(3)]
    );
    assert_eq!(ssh_rows("", &[], &[]), [super::SshRow::Placeholder]);
}

/// A valid typed target that no row's name or alias equals (ignoring ASCII
/// case) is the first row, selected, so Enter connects to exactly what was
/// typed. Fuzzy matches follow it and take a deliberate Down.
#[test]
fn ssh_enter_connects_the_typed_target_unless_a_row_is_exactly_it() {
    use super::SshEnter::Connect;
    use super::SshRow::{ConnectTyped, Imported};
    let imported = vec!["web03".to_string()];
    let rows = ssh_rows("web3", &[], &imported);
    assert_eq!(rows, [ConnectTyped, Imported(0), super::SshRow::SaveTyped]);
    let Some(Connect(target)) = ssh_row_action("web3", 0, &[], &imported) else {
        panic!("Enter on the first row must connect");
    };
    assert_eq!(
        crate::mux::ssh_tab_argv(&target).unwrap(),
        ["ssh", "-t", "--", "web3"]
    );
    let mut ws = ssh_launcher_with("web3");
    ws.ssh_imported = imported.clone();
    assert_eq!(
        enter_target(&mut ws, &[]).as_deref(),
        Some("web3"),
        "Enter with no navigation connects to the typed target"
    );
    let down = super::ssh_step_selection(&rows, 0, true);
    assert_eq!(down, 1);
    assert_eq!(
        ssh_row_action("web3", down, &[], &imported),
        Some(Connect("web03".into())),
        "Down then Enter connects to the fuzzy match"
    );
    let list = ssh_picker_list("web3", 0, &[], &imported, &kettle_i18n::Translator::default());
    assert_eq!(list.rows[0].label, "Connect to \"web3\"");
    assert!(list.rows[0].enabled);
    assert_eq!(list.selected, 0);
    // A prefix of an imported alias is not that alias.
    let imported = vec!["prod-db".to_string()];
    assert_eq!(
        ssh_row_action("prod", 0, &[], &imported),
        Some(Connect("prod".into()))
    );
    // Tab on the Connect row still completes to the best match below it.
    assert_eq!(
        super::ssh_completion("prod", 0, &[], &imported).as_deref(),
        Some("prod-db")
    );
}

/// A host row whose name or alias equals the trimmed query, ignoring ASCII
/// case, is row 0 and there is no Connect row: Enter connects to that host.
#[test]
fn ssh_an_exact_name_or_alias_is_row_zero_with_no_connect_row() {
    use super::SshEnter::Connect;
    use super::SshRow::{Configured, Imported};
    let imported = vec!["prod-db".to_string(), "prod".to_string()];
    // Saving an imported alias pins it, so the Save row still ends the list.
    let save = super::SshRow::SaveTyped;
    assert_eq!(ssh_rows("prod", &[], &imported), [Imported(1), Imported(0), save]);
    assert_eq!(ssh_rows("PROD ", &[], &imported), [Imported(1), Imported(0), save]);
    assert_eq!(
        ssh_row_action("prod", 0, &[], &imported),
        Some(Connect("prod".into()))
    );
    // An exact name wins over an earlier prefix match.
    let hosts = ssh_fixture_hosts(&[("db-old", "me@old"), ("db", "me@db")]);
    assert_eq!(
        ssh_rows("db", &hosts, &[]),
        [Configured(1), Configured(0), super::SshRow::Remove(1)]
    );
    assert_eq!(
        ssh_row_action("db", 0, &hosts, &[]),
        Some(Connect("me@db".into()))
    );
    // An exact imported alias wins over a configured fuzzy match.
    let hosts = ssh_fixture_hosts(&[("web-staging-3", "me@stg3")]);
    let imported = vec!["web3".to_string()];
    assert_eq!(
        ssh_rows("web3", &hosts, &imported),
        [Imported(0), Configured(0), save]
    );
}

#[test]
fn ssh_an_invalid_typed_target_hides_the_hosts_spawns_nothing_and_stays_open() {
    for typed in ["-oProxyCommand=x", "me@h -p 22", "a\u{202E}b"] {
        let mut ws = ssh_launcher_with(typed);
        assert_eq!(ssh_rows(typed, &[], &[]), [super::SshRow::Placeholder]);
        assert_eq!(
            ssh_row_action(typed, 0, &[], &[]),
            Some(super::SshEnter::Invalid)
        );
        assert_eq!(enter_target(&mut ws, &[]), None, "{typed:?}");
        assert!(ws.ssh_input.is_some(), "{typed:?}: the launcher stays open");
        let list = ssh_picker_list(typed, 0, &[], &[], &kettle_i18n::Translator::default());
        assert!(!list.rows[0].enabled);
    }
    let mut ws = ssh_launcher_with("me@box");
    assert_eq!(
        enter_target(&mut ws, &[]).as_deref(),
        Some("me@box")
    );
    assert!(ws.ssh_input.is_none(), "a connection closes the launcher");
    // Nothing typed and no hosts: Enter closes, as it always has.
    let mut ws = ssh_launcher_with("");
    assert_eq!(enter_target(&mut ws, &[]), None);
    assert!(ws.ssh_input.is_none());
    // Host rows that fuzzy-match an invalid query are hidden, so Enter cannot
    // connect to one: only the refusal row shows.
    let hosts = ssh_fixture_hosts(&[("h-1", "me@h1"), ("my box", "me@box")]);
    for typed in ["-", "-oProxyCommand=x", "h -1"] {
        assert_eq!(ssh_rows(typed, &hosts, &[]), [super::SshRow::Placeholder], "{typed:?}");
        let mut ws = ssh_launcher_with(typed);
        assert_eq!(enter_target(&mut ws, &hosts), None, "{typed:?}");
        assert!(ws.ssh_input.is_some(), "{typed:?}");
    }
    // A configured name with a space, typed in full, is still that host, and
    // can be removed.
    assert_eq!(
        ssh_rows("my box", &hosts, &[]),
        [super::SshRow::Configured(1), super::SshRow::Remove(1)]
    );
    let mut ws = ssh_launcher_with("My Box");
    assert_eq!(
        enter_target(&mut ws, &hosts).as_deref(),
        Some("me@box")
    );
    // The key handler goes through it, and keeps the spawn inside `ssh_key`.
    let body = finding_body(include_str!("modals.rs"), "ssh_key");
    assert!(body.contains("Key::Named(NamedKey::Enter) => match take_ssh_enter(ws, &self.cfg.ssh_hosts) {"));
}

/// Arrows skip the disabled placeholder. Down reaches the Save row that ends
/// the list, and wraps from it to the first row. Up from the first row wraps
/// past the Save row onto the last host: an action row takes a deliberate
/// Down, never one Up press.
#[test]
fn ssh_arrows_skip_the_placeholder_and_up_wraps_past_the_action_row() {
    use super::ssh_step_selection as step;
    let hosts = ssh_fixture_hosts(&[("h-1", "me@h1"), ("h-2", "me@h2")]);
    // The Connect row, then hosts, then Save.
    let rows = ssh_rows("h", &hosts, &[]);
    assert_eq!(rows.first(), Some(&super::SshRow::ConnectTyped));
    assert_eq!(rows.last(), Some(&super::SshRow::SaveTyped));
    assert_eq!(step(&rows, 0, true), 1);
    assert_eq!(step(&rows, 1, true), 2);
    assert_eq!(step(&rows, 2, true), 3, "a deliberate Down reaches Save");
    assert_eq!(step(&rows, 3, true), 0, "Down wraps to the Connect row");
    assert_eq!(step(&rows, 3, false), 2);
    assert_eq!(step(&rows, 2, false), 1);
    assert_eq!(step(&rows, 0, false), 2, "Up wraps onto the last host, not Save");
    // The same for Remove after an exact configured name.
    let hosts = ssh_fixture_hosts(&[("db", "me@db"), ("db-old", "me@old")]);
    let rows = ssh_rows("db", &hosts, &[]);
    assert_eq!(rows.last(), Some(&super::SshRow::Remove(0)));
    assert_eq!(step(&rows, 0, false), 1, "Up wraps onto the last host, not Remove");
    assert_eq!(step(&rows, 1, true), 2, "a deliberate Down reaches Remove");
    // Hosts only: Up wraps among them, as before.
    let rows = ssh_rows("", &hosts, &[]);
    assert_eq!(step(&rows, 0, false), 1);
    // A list with only the placeholder has nothing to move to.
    let rows = ssh_rows("", &[], &[]);
    assert_eq!((step(&rows, 0, true), step(&rows, 0, false)), (0, 0));
    let rows = ssh_rows("-", &[], &[]);
    assert_eq!((step(&rows, 0, true), step(&rows, 0, false)), (0, 0));
}

/// The wheel scrolls the launcher's list, and a selection it scrolls past
/// moves to the nearest host in view: the first after scrolling down, the
/// last after scrolling up. It never selects the Save row, which takes a
/// deliberate Down or a click on it, even when Save is all that is in view.
#[test]
fn ssh_wheel_moves_a_scrolled_past_selection_to_a_host_in_view_but_never_save() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_menu};
    let cfg = kettle_config::Config {
        ssh_hosts: (0..40)
            .map(|i| (format!("host-{i}"), format!("me@host-{i}")))
            .collect(),
        ..Default::default()
    };
    let tr = kettle_i18n::Translator::default();
    let cell = (8.0, 16.0);
    let project = |ws: &WindowState, surface: (f32, f32)| {
        open_picker_menu(ws, &cfg, "dark-theme", &tr, None, surface, cell, cell, 0.0)
    };
    let shown = |menu: &kettle_render::ContextMenu| {
        (0..menu.rows.len())
            .filter(|&index| menu.visible_row_rect(index).is_some())
            .count()
    };
    let surface = (320.0, 160.0);
    let mut ws = ssh_launcher_with("host");
    let first = project(&ws, surface).unwrap();
    assert_eq!(first.rows.len(), 42);
    assert_eq!(
        ssh_rows("host", &cfg.ssh_hosts, &[]).last(),
        Some(&super::SshRow::SaveTyped)
    );
    let visible = shown(&first);
    ws.cursor.x = f64::from(first.anchor.0 + first.panel_w_clamped * 0.5);
    ws.cursor.y = f64::from(first.anchor.1 + first.panel_h_clamped * 0.5);
    assert!(apply_picker_pointer(&mut ws, &cfg, &first, cell.1, PickerPointer::Wheel(100)));
    let bottom = project(&ws, surface).unwrap();
    assert_eq!(bottom.scroll_offset, 42 - visible);
    assert!(bottom.visible_row_rect(41).is_some(), "Save is in view");
    let selected = ws.ssh_input.as_ref().unwrap().1;
    assert_eq!(selected, 42 - visible, "the first host in view");
    assert_eq!(
        ssh_row_action("host", selected, &cfg.ssh_hosts, &[]),
        Some(super::SshEnter::Connect(format!("me@host-{}", selected - 1)))
    );
    assert!(apply_picker_pointer(&mut ws, &cfg, &bottom, cell.1, PickerPointer::Wheel(-100)));
    assert_eq!(project(&ws, surface).unwrap().scroll_offset, 0);
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, visible - 1, "the last row in view");
    // A card one row tall: at the end only Save is in view, and the wheel
    // leaves the selection on its host.
    let tiny = (60..400)
        .map(|height| (320.0, height as f32))
        .find(|&surface| project(&ws, surface).is_some_and(|menu| shown(&menu) == 1))
        .expect("a surface one row tall");
    let mut ws = ssh_launcher_with("host");
    let menu = project(&ws, tiny).unwrap();
    ws.cursor.x = f64::from(menu.anchor.0 + menu.panel_w_clamped * 0.5);
    ws.cursor.y = f64::from(menu.anchor.1 + menu.panel_h_clamped * 0.5);
    assert!(apply_picker_pointer(&mut ws, &cfg, &menu, cell.1, PickerPointer::Wheel(100)));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0, "never Save");
}

/// Whatever the picker, the wheel never selects a disabled row: a selection
/// it scrolls past moves to the nearest enabled row in view.
#[test]
fn picker_wheel_never_selects_a_disabled_row() {
    use super::{PickerList, PickerPointer, apply_picker_pointer, picker_context_menu_at};
    let row = |label: String, enabled: bool| ContextMenuRow {
        toggle: None,
        swatches: None,
        matched_chars: None,
        label,
        separator: false,
        enabled,
        hint: String::new(),
        ..Default::default()
    };
    let menu_for = |ws: &WindowState| {
        let rows = (0..41)
            .map(|i| row(format!("row-{i}"), i != 20))
            .collect();
        let list = PickerList {
            status: None,
            card_scale: 1.0,
            rows,
            selected: ws.ssh_input.as_ref().unwrap().1,
            footer: None,
        };
        let surface = (320.0, 160.0);
        picker_context_menu_at(list, surface, (8.0, 16.0), (8.0, 16.0), 0.0, ws.picker_view.scroll)
            .unwrap()
    };
    let mut ws = ssh_launcher_with("");
    ws.ssh_input.as_mut().unwrap().1 = 30;
    let menu = menu_for(&ws);
    let visible = (0..menu.rows.len())
        .filter(|&index| menu.visible_row_rect(index).is_some())
        .count();
    assert_eq!(menu.scroll_offset, 31 - visible);
    ws.cursor.x = f64::from(menu.anchor.0 + menu.panel_w_clamped * 0.5);
    ws.cursor.y = f64::from(menu.anchor.1 + menu.panel_h_clamped * 0.5);
    // Up until the disabled row is the last one in view.
    let delta = (21 - visible) as isize - menu.scroll_offset as isize;
    assert!(apply_picker_pointer(&mut ws, &kettle_config::Config::default(), &menu, 16.0, PickerPointer::Wheel(delta)));
    assert_eq!(menu_for(&ws).scroll_offset, 21 - visible);
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 19);
}

#[test]
fn ssh_pointer_press_on_the_connect_row_matches_enter() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config {
        ssh_hosts: ssh_fixture_hosts(&[("alpha", "me@alpha"), ("beta", "me@beta")]),
        ..Default::default()
    };
    let tr = kettle_i18n::Translator::default();
    let mut ws = ssh_launcher_with("a");
    let menu = picker_context_menu(
        open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap(),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        0.0,
    )
    .unwrap();
    assert_eq!(menu.rows[0].label, "Connect to \"a\"");
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    ws.cursor.x = f64::from(menu.anchor.0 + 10.0);
    // Press the first host row, then the Connect row above it.
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 1.5);
    assert!(apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Press));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 1);
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 0.5);
    assert!(apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Press));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0);
    // A press activates through the same Enter as the keyboard.
    assert_eq!(
        enter_target(&mut ws, &cfg.ssh_hosts).as_deref(),
        Some("a")
    );
}

/// The import is read when the launcher opens, only with `ssh-config-hosts`
/// on, from a scratch fixture, never the user's own file.
#[test]
fn ssh_launcher_reads_the_fixture_only_when_the_key_is_on() {
    let home = crate::test_tempdir();
    let dot_ssh = home.path().join(".ssh");
    std::fs::create_dir(&dot_ssh).unwrap();
    let config = dot_ssh.join("config");
    std::fs::write(&config, "Host fixture-alias\n  HostName fixture.invalid\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dot_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut ws = WindowState::new(0, Mux::new());
    ws.ssh_imported = vec!["from-an-earlier-open".into()];
    let off = kettle_config::Config {
        ssh_config_hosts: false,
        ..Default::default()
    };
    super::open_ssh_launcher(&mut ws, &off, || -> Option<std::path::PathBuf> {
        panic!("with the key off, no path is looked up")
    });
    assert!(ws.ssh_imported.is_empty(), "the key is off: nothing imported");
    assert!(ws.ssh_input.is_some());
    let on = kettle_config::Config::default();
    assert!(on.ssh_config_hosts);
    super::open_ssh_launcher(&mut ws, &on, || Some(config.clone()));
    assert_eq!(ws.ssh_imported, ["fixture-alias"]);
    // Each open replaces the list, so a removed alias does not linger.
    std::fs::write(&config, "Host replaced\n").unwrap();
    super::open_ssh_launcher(&mut ws, &on, || Some(config.clone()));
    assert_eq!(ws.ssh_imported, ["replaced"]);
    // The action reads the real path through the same function.
    let src = production_source();
    let open = src
        .split("Action::OpenSsh =>")
        .nth(1)
        .and_then(|rest| rest.split("Action::").next())
        .unwrap();
    assert!(open.contains("open_ssh_launcher("), "{open}");
    assert!(open.contains("kettle_config::ssh_config::user_ssh_config_path"));
}

/// The row list and the key hint each have one builder, so the painter, the
/// pointer, accessibility and the IME projection cannot disagree.
#[test]
fn ssh_rows_and_hint_have_one_builder_each() {
    let src = production_source();
    let calls = src.matches("ssh_picker_list(").count() - src.matches("fn ssh_picker_list(").count();
    assert_eq!(calls, 1, "only ssh_picker_list_for builds the rows");
    let builder = src
        .split("fn ssh_picker_list_for(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    assert!(builder.contains("ssh_picker_list("));
    assert_eq!(
        src.matches("PickerHintConnect").count(),
        1,
        "only ssh_picker_hint names the connect hint"
    );
    let hint = src
        .split("fn ssh_picker_hint(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    assert!(hint.contains("T::PickerHintConnect"));
    assert_eq!(src.matches("ssh_picker_hint(").count(), 3, "defined once, used by redraw and the text field");
}

/// The Save row ends the list only for a valid target that no configured name
/// or target equals; the Remove row only for an exact configured name, never
/// an imported alias. Both paint and announce what Enter on them does.
#[test]
fn ssh_save_and_remove_rows_show_only_when_they_apply() {
    use super::SshEnter::{Remove, Save};
    use super::SshRow::{Configured, ConnectTyped, Imported, Remove as RemoveRow, SaveTyped};
    let hosts = ssh_fixture_hosts(&[("prod", "me@prod.example"), ("my box", "me@box")]);
    let imported = vec!["web".to_string()];
    // A new valid target: Save last.
    assert_eq!(
        ssh_rows("me@new.example", &hosts, &imported),
        [ConnectTyped, SaveTyped]
    );
    assert_eq!(
        ssh_row_action("me@new.example", 1, &hosts, &imported),
        Some(Save("me@new.example".into()))
    );
    // Equal to a configured target or name, ignoring ASCII case: no Save.
    for typed in ["ME@PROD.EXAMPLE", "me@box"] {
        assert!(
            !ssh_rows(typed, &hosts, &imported).contains(&SaveTyped),
            "{typed}"
        );
    }
    // An exact configured name: Remove last, for that host.
    assert_eq!(ssh_rows("PROD", &hosts, &imported), [Configured(0), RemoveRow(0)]);
    assert_eq!(
        ssh_row_action("PROD", 1, &hosts, &imported),
        Some(Remove {
            name: "prod".into(),
            target: "me@prod.example".into()
        })
    );
    // An imported alias is never removed; saving it pins it.
    assert_eq!(ssh_rows("web", &hosts, &imported), [Imported(0), SaveTyped]);
    // Nothing typed, or a refused target: no action row.
    assert!(ssh_rows("", &hosts, &imported).iter().all(|row| !row.is_action()));
    assert_eq!(
        ssh_rows("-oProxyCommand=x", &hosts, &imported),
        [super::SshRow::Placeholder]
    );
    // Tab never completes to an action row.
    assert_eq!(super::ssh_completion("PROD", 1, &hosts, &imported), None);
    // The painted labels, in both languages.
    for (tr, save, remove) in [
        (
            kettle_i18n::Translator::new(kettle_i18n::Language::En),
            "Save \"me@new.example\" as a host…",
            "Remove \"prod\" from Kettle…",
        ),
        (
            kettle_i18n::Translator::new(kettle_i18n::Language::Es),
            "Guardar \"me@new.example\" como host…",
            "Quitar \"prod\" de Kettle…",
        ),
    ] {
        let list = ssh_picker_list("me@new.example", 0, &hosts, &imported, &tr);
        assert_eq!(list.rows.last().unwrap().label, save);
        assert!(list.rows.last().unwrap().enabled);
        let list = ssh_picker_list("prod", 0, &hosts, &imported, &tr);
        assert_eq!(list.rows.last().unwrap().label, remove);
    }
}

/// Selection safety: a hover over Save never selects it, and the wheel stops
/// above it; a press on the row itself selects it, and that press is the
/// Enter that opens the name editor.
#[test]
fn ssh_action_rows_take_a_click_on_the_row_itself() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config {
        ssh_hosts: ssh_fixture_hosts(&[("alpha", "me@alpha"), ("beta", "me@beta")]),
        ..Default::default()
    };
    let tr = kettle_i18n::Translator::default();
    let mut ws = ssh_launcher_with("a");
    let menu = picker_context_menu(
        open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap(),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        0.0,
    )
    .unwrap();
    let save = menu.rows.len() - 1;
    assert_eq!(menu.rows[save].label, "Save \"a\" as a host…");
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    ws.cursor.x = f64::from(menu.anchor.0 + 10.0);
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * (save as f32 + 0.5));
    assert!(!apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Hover));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0, "a hover selects nothing");
    assert!(apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Wheel(3)));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0, "nothing to scroll");
    assert!(apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Press));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, save);
    assert_eq!(
        super::take_ssh_enter(&mut ws, &cfg.ssh_hosts),
        Some(super::SshEnter::Save("a".into()))
    );
    assert!(ws.ssh_input.is_none(), "Save closes the launcher");
    // The key handler opens the editor and the confirmation from Enter.
    let body = finding_body(include_str!("modals.rs"), "ssh_key");
    assert!(body.contains("Some(SshEnter::Save(target)) => self.begin_ssh_host_save(ws, &target),"));
    assert!(body.contains("self.confirm_ssh_host_removal(ws, name, target);"));
}

/// The name editor's Enter: an unusable name stays in the editor, a new name
/// is written expecting no host by that name, and a name saved with another
/// target asks before replacing it.
#[test]
fn ssh_save_plan_asks_before_replacing_a_saved_name() {
    use super::SshSavePlan::{Invalid, Replace, Write};
    use kettle_config::ssh_config::HostExpect;
    let hosts = ssh_fixture_hosts(&[("db", "me@db-old")]);
    for name in ["", "  ", "a=b", "x\u{7}", "a\u{202E}b"] {
        assert_eq!(super::ssh_save_plan(&hosts, name, "me@new"), Invalid, "{name:?}");
    }
    assert_eq!(
        super::ssh_save_plan(&hosts, " web ", "me@web"),
        Write {
            name: "web".into(),
            expect: HostExpect::Absent
        }
    );
    assert_eq!(
        super::ssh_save_plan(&hosts, "DB", "me@new"),
        Replace {
            name: "DB".into(),
            old_target: "me@db-old".into()
        }
    );
    assert_eq!(
        super::ssh_save_plan(&hosts, "db", "me@db-old"),
        Write {
            name: "db".into(),
            expect: HostExpect::Target("me@db-old".into())
        }
    );
}

/// Remove is destructive and Replace is not; both default to Cancel. The
/// Remove body names the OpenSSH file by the platform's display path, in
/// words a screen reader reads as written.
#[test]
fn ssh_remove_and_replace_confirmations_default_to_cancel() {
    use super::{ConfirmAction, ConfirmButton};
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let remove = super::ssh_remove_dialog(&tr, "db".into(), "me@db".into());
        assert_eq!(remove.focus_idx, 0);
        assert!(matches!(remove.buttons[0], ConfirmButton::Cancel));
        assert!(matches!(
            remove.buttons[1],
            ConfirmButton::Confirm {
                destructive: true,
                ..
            }
        ));
        assert!(matches!(
            &remove.on_confirm,
            ConfirmAction::RemoveSshHost { name, target } if name == "db" && target == "me@db"
        ));
        assert!(remove.heading.contains("\"db\""), "{}", remove.heading);
        assert!(remove.body.contains("me@db"), "{}", remove.body);
        assert!(!remove.body.contains('→') && !remove.heading.contains('→'));
        assert!(remove
            .body
            .contains(kettle_config::ssh_config::SSH_CONFIG_DISPLAY_PATH));
        if cfg!(windows) {
            assert!(!remove.body.contains("~/.ssh"), "{}", remove.body);
        }
        let replace = super::ssh_replace_dialog(&tr, "db".into(), "me@old".into(), "me@new".into());
        assert_eq!(replace.focus_idx, 0);
        assert!(matches!(replace.buttons[0], ConfirmButton::Cancel));
        assert!(matches!(
            replace.buttons[1],
            ConfirmButton::Confirm {
                destructive: false,
                ..
            }
        ));
        assert!(replace.body.contains("me@old") && replace.body.contains("me@new"));
        // Neither question is about a pane, so neither samples one.
        let ws = WindowState::new(0, Mux::new());
        for action in [&remove.on_confirm, &replace.on_confirm] {
            assert_eq!(super::lone_pane_in_scope(&ws, 1, action), None);
        }
    }
}

/// Log lines this thread emitted from the pickers module while `run` ran.
fn picker_log_lines(run: impl FnOnce()) -> Vec<String> {
    kettle_test_support::lines_logged(|target| target.ends_with("::pickers"), run).1
}

/// After a save the in-memory hosts change at once, the launcher reopens with
/// the saved host as its exact row 0, and the status line is painted in the
/// hint lane and announced by a polite status node. A host added by hand
/// since the last reload is never overwritten, and the failure is logged by
/// its kind alone.
#[test]
fn ssh_save_and_remove_update_hosts_and_reopen_with_a_status_line() {
    use kettle_config::ssh_config::HostExpect;
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    std::fs::write(&path, "ssh-host = db-old=me@old\n").unwrap();
    let mut cfg = kettle_config::Config::parse_text(&std::fs::read_to_string(&path).unwrap());
    super::save_ssh_host_to(Some(&path), &mut cfg.ssh_hosts, "db", "me@db", &HostExpect::Absent)
        .unwrap();
    assert_eq!(
        cfg.ssh_hosts,
        ssh_fixture_hosts(&[("db-old", "me@old"), ("db", "me@db")])
    );
    assert_eq!(
        kettle_config::Config::parse_text(&std::fs::read_to_string(&path).unwrap()).ssh_hosts,
        cfg.ssh_hosts,
        "memory and file agree"
    );
    let mut ws = WindowState::new(0, Mux::new());
    let status = tr.ssh_status_saved("db");
    super::reopen_ssh_launcher(
        &mut ws,
        &cfg,
        || None,
        "db",
        Some(("db", "me@db")),
        status.clone(),
    );
    let (query, selected) = ws.ssh_input.as_ref().unwrap();
    assert_eq!(query.text(), "db");
    assert_eq!(*selected, 0, "the saved host is the exact row 0");
    assert_eq!(
        ssh_rows("db", &cfg.ssh_hosts, &[])[0],
        super::SshRow::Configured(1)
    );
    assert_eq!(
        ssh_row_action("db", *selected, &cfg.ssh_hosts, &[]),
        Some(super::SshEnter::Connect("me@db".into()))
    );
    assert_eq!(status, "Saved \"db\"");
    assert_eq!(super::ssh_status_line(&ws), Some(status.as_str()));
    let lane = super::ssh_picker_hint(super::ssh_status_line(&ws), &tr);
    assert_eq!(lane, status, "the hint lane paints the status");
    // The projection announces the painted status politely, over the lane
    // that paints it.
    let field = kettle_render::TextFieldOverlay {
        picker_header: true,
        placeholder: String::new(),
        text: "db".into(),
        cursor_byte: 2,
        selection: None,
        horizontal_scroll: 0,
        rect: (100.0, 20.0, 600.0, 24.0),
        prefix: "  ssh \u{276f} ".into(),
        hint: lane.clone(),
        accent_palette: 4,
    };
    let cw = kettle_render::text_field_cell_width(&field, 8.0, 16.0);
    let painted = kettle_render::text_field_rects(&field, cw)[2];
    assert!(painted.2 > 0.0, "the lane paints the status");
    let overlay = kettle_render::Overlay {
        ssh_query: Some("db".into()),
        ssh_status: super::ssh_status_line(&ws).map(str::to_string),
        text_field: Some(field),
        context_menu: super::picker_context_menu(
            super::ssh_picker_list_for(&ws, &cfg, &tr).unwrap(),
            (800.0, 600.0),
            (8.0, 16.0),
            (8.0, 16.0),
            0.0,
        ),
        ..kettle_render::Overlay::default()
    };
    let projection =
        super::modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &tr);
    let node = projection
        .nodes
        .iter()
        .map(|(_, node)| node)
        .find(|node| node.role() == accesskit::Role::Status)
        .expect("a status node");
    assert_eq!(node.label(), Some(lane.as_str()));
    assert_eq!(node.live(), Some(accesskit::Live::Polite));
    let (x, y, w, h) = painted;
    assert_eq!(
        node.bounds(),
        Some(accesskit::Rect::new(
            x as f64,
            y as f64,
            (x + w) as f64,
            (y + h) as f64
        ))
    );
    // The next edit brings the key hint back.
    ws.ssh_input
        .as_mut()
        .unwrap()
        .0
        .replace_all("db2", crate::modal_input::MAX_MODAL_INPUT_BYTES);
    assert_eq!(super::ssh_status_line(&ws), None);
    assert_ne!(super::ssh_picker_hint(None, &tr), status);

    // A hand edit since the last reload: the file has `web`, memory does not.
    let before = std::fs::read_to_string(&path).unwrap() + "ssh-host = web=me@hand\n";
    std::fs::write(&path, &before).unwrap();
    let hosts = cfg.ssh_hosts.clone();
    let lines = picker_log_lines(|| {
        assert_eq!(
            super::save_ssh_host_to(
                Some(&path),
                &mut cfg.ssh_hosts,
                "web",
                "me@web",
                &HostExpect::Absent
            ),
            Err(std::io::ErrorKind::WouldBlock)
        );
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_eq!(cfg.ssh_hosts, hosts);
    assert_eq!(lines, ["ssh host not saved: WouldBlock"]);

    // Remove, then reopen empty with its status.
    super::remove_ssh_host_from(Some(&path), &mut cfg.ssh_hosts, "db", "me@db").unwrap();
    assert_eq!(cfg.ssh_hosts, ssh_fixture_hosts(&[("db-old", "me@old")]));
    let lines = picker_log_lines(|| {
        assert_eq!(
            super::remove_ssh_host_from(Some(&path), &mut cfg.ssh_hosts, "db", "me@db"),
            Err(std::io::ErrorKind::NotFound)
        );
    });
    assert_eq!(lines, ["ssh host not removed: NotFound"]);
    super::reopen_ssh_launcher(&mut ws, &cfg, || None, "", None, tr.ssh_status_removed("db"));
    assert_eq!(ws.ssh_input.as_ref().unwrap().0.text(), "");
    assert_eq!(super::ssh_status_line(&ws), Some("Removed \"db\""));
    // Opening the launcher afresh clears it.
    super::open_ssh_launcher(&mut ws, &cfg, || None);
    assert_eq!(super::ssh_status_line(&ws), None);
}

/// A host list that changes under an open launcher (a reload, or a Save or
/// Remove in another window, since `cfg` is shared) sends its selection back
/// to row 0. Without that, an index on the last host slides onto the Save or
/// Remove row that now ends the shorter list.
#[test]
fn ssh_a_host_list_change_moves_the_launcher_back_to_row_zero() {
    use super::SshEnter::{Connect, Remove, Save};
    let open = |query: &str, cfg: &kettle_config::Config| {
        let mut ws = WindowState::new(0, Mux::new());
        super::open_ssh_launcher(&mut ws, cfg, || None);
        let (editor, _) = ws.ssh_input.as_mut().unwrap();
        let _ = editor.replace_all(query, crate::modal_input::MAX_MODAL_INPUT_BYTES);
        ws
    };
    let enter = |ws: &WindowState, hosts: &[(String, String)]| {
        let (query, selected) = ws.ssh_input.as_ref().unwrap();
        ssh_row_action(query, *selected, hosts, &ws.ssh_imported)
    };
    // Save: [Connect, alpha, beta, Save] with beta selected, then alpha goes.
    let mut cfg = kettle_config::Config {
        ssh_hosts: ssh_fixture_hosts(&[("alpha", "me@alpha"), ("beta", "me@beta")]),
        ..Default::default()
    };
    let mut ws = open("a", &cfg);
    assert_eq!(ssh_rows("a", &cfg.ssh_hosts, &[]).len(), 4);
    ws.ssh_input.as_mut().unwrap().1 = 2;
    assert_eq!(enter(&ws, &cfg.ssh_hosts), Some(Connect("me@beta".into())));
    // Unchanged hosts keep the selection.
    super::sync_ssh_hosts(&mut ws, &cfg.ssh_hosts);
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 2);
    cfg.ssh_hosts.remove(0);
    assert_eq!(
        enter(&ws, &cfg.ssh_hosts),
        Some(Save("a".into())),
        "the stale index now names Save"
    );
    super::sync_ssh_hosts(&mut ws, &cfg.ssh_hosts);
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0);
    assert_eq!(enter(&ws, &cfg.ssh_hosts), Some(Connect("a".into())));
    // Remove: [db, db-a, db-b, Remove(db)] with db-b selected, then db-a goes.
    let mut cfg = kettle_config::Config {
        ssh_hosts: ssh_fixture_hosts(&[("db", "me@db"), ("db-a", "me@a"), ("db-b", "me@b")]),
        ..Default::default()
    };
    let mut ws = open("db", &cfg);
    ws.ssh_input.as_mut().unwrap().1 = 2;
    assert_eq!(enter(&ws, &cfg.ssh_hosts), Some(Connect("me@b".into())));
    cfg.ssh_hosts.remove(1);
    assert_eq!(
        enter(&ws, &cfg.ssh_hosts),
        Some(Remove {
            name: "db".into(),
            target: "me@db".into()
        }),
        "the stale index now names Remove"
    );
    super::sync_ssh_hosts(&mut ws, &cfg.ssh_hosts);
    assert_eq!(enter(&ws, &cfg.ssh_hosts), Some(Connect("me@db".into())));
    // A reload runs it for every window, and a write for every other one.
    let src = production_source();
    let reload = src
        .split("fn apply_reloaded_config(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap();
    assert!(reload.contains("sync_ssh_hosts(ws, &self.cfg.ssh_hosts);"));
    let finish = src
        .split("fn finish_ssh_host_effect(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap();
    assert!(finish.contains("for sibling in self.windows.values_mut() {"));
    assert!(finish.contains("sync_ssh_hosts(sibling, &self.cfg.ssh_hosts);"));
}

/// A scratch config file holding `text`, and the config parsed from it.
fn ssh_scratch_config(text: &str) -> (kettle_test_support::PrivateTempDir, std::path::PathBuf, kettle_config::Config) {
    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    std::fs::write(&path, text).unwrap();
    let cfg = kettle_config::Config::parse_text(text);
    (dir, path, cfg)
}

/// The name editor for `target` with `name` typed.
fn ssh_name_editor(target: &str, name: &str) -> super::TitleEditState {
    let mut state = super::begin_ssh_host_name_edit(target).unwrap();
    let _ = state
        .input
        .replace_all(name, crate::modal_input::MAX_MODAL_INPUT_BYTES);
    state
}

/// Enter in the name editor: an unusable name and a refused write keep the
/// editor and what was typed; a name saved with another target asks; a
/// write reopens the launcher on the saved host.
#[test]
fn ssh_name_editor_enter_keeps_the_editor_on_failure() {
    use super::SshHostEffect;
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let (_dir, path, mut cfg) = ssh_scratch_config("ssh-host = db=me@old\n");
    let mut ws = WindowState::new(0, Mux::new());
    let submit = |ws: &mut WindowState, cfg: &mut kettle_config::Config, state| {
        super::submit_ssh_host_name(ws, cfg, Some(&path), || None, state, &tr)
    };
    // An unusable name.
    let effect = submit(&mut ws, &mut cfg, ssh_name_editor("me@web", "a=b"));
    assert!(matches!(effect, SshHostEffect::NotSaved), "{effect:?}");
    assert_eq!(ws.editing_title.take().unwrap().input.text(), "a=b");
    // A host added by hand since the last reload: refused, editor kept.
    let hand = "ssh-host = db=me@old\nssh-host = web=me@hand\n";
    std::fs::write(&path, hand).unwrap();
    let effect = submit(&mut ws, &mut cfg, ssh_name_editor("me@web", "web"));
    assert!(matches!(effect, SshHostEffect::NotSaved), "{effect:?}");
    let kept = ws.editing_title.take().expect("the editor stays open");
    assert_eq!(kept.input.text(), "web");
    assert_eq!(kept.target, super::TitleEditTarget::SshHost("me@web".into()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), hand);
    assert!(ws.ssh_input.is_none());
    // A name saved with another target asks, Cancel first.
    let effect = submit(&mut ws, &mut cfg, ssh_name_editor("me@new", "db"));
    let SshHostEffect::Ask(dialog) = effect else {
        panic!("{effect:?}");
    };
    assert!(matches!(
        &dialog.on_confirm,
        super::ConfirmAction::ReplaceSshHost { name, old_target, new_target }
            if name == "db" && old_target == "me@old" && new_target == "me@new"
    ));
    // A new name is written and the launcher reopens on it.
    let effect = submit(&mut ws, &mut cfg, ssh_name_editor("me@box", "box"));
    assert!(matches!(effect, SshHostEffect::Changed), "{effect:?}");
    assert!(ws.editing_title.is_none());
    assert_eq!(ws.ssh_input.as_ref().unwrap().0.text(), "box");
    assert_eq!(super::ssh_status_line(&ws), Some("Saved \"box\""));
    assert!(std::fs::read_to_string(&path).unwrap().contains("ssh-host = box=me@box"));
}

/// A confirmed Replace writes only over the target the question showed, and
/// a refused Replace or Remove reopens the launcher on what was typed with a
/// status line, so the failure shows even with notifications off.
#[test]
fn ssh_confirmed_replace_and_remove_report_refusals_in_the_launcher() {
    use super::{ConfirmAction, SshHostEffect};
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let (_dir, path, mut cfg) = ssh_scratch_config("ssh-host = db=me@old\n");
    let mut ws = WindowState::new(0, Mux::new());
    let confirm = |ws: &mut WindowState, cfg: &mut kettle_config::Config, action| {
        super::confirm_ssh_host_change(ws, cfg, Some(&path), || None, action, &tr)
    };
    let replace = |old: &str| ConfirmAction::ReplaceSshHost {
        name: "db".into(),
        old_target: old.into(),
        new_target: "me@new".into(),
    };
    // The file was changed by hand after the question went up.
    std::fs::write(&path, "ssh-host = db=me@hand\n").unwrap();
    let effect = confirm(&mut ws, &mut cfg, replace("me@old"));
    assert!(matches!(effect, SshHostEffect::NotSaved), "{effect:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ssh-host = db=me@hand\n");
    assert_eq!(cfg.ssh_hosts, ssh_fixture_hosts(&[("db", "me@old")]));
    assert_eq!(ws.ssh_input.as_ref().unwrap().0.text(), "me@new");
    assert_eq!(
        super::ssh_status_line(&ws),
        Some(tr.ssh_status_not_saved("db").as_str())
    );
    // Unchanged since the question: the Replace writes over the old target.
    std::fs::write(&path, "ssh-host = db=me@old\n").unwrap();
    let effect = confirm(&mut ws, &mut cfg, replace("me@old"));
    assert!(matches!(effect, SshHostEffect::Changed), "{effect:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ssh-host = db=me@new\n");
    assert_eq!(cfg.ssh_hosts, ssh_fixture_hosts(&[("db", "me@new")]));
    let (query, selected) = ws.ssh_input.as_ref().unwrap();
    assert_eq!(query.text(), "db");
    assert_eq!(
        ssh_row_action(query, *selected, &cfg.ssh_hosts, &[]),
        Some(super::SshEnter::Connect("me@new".into()))
    );
    // A Remove for a target the file no longer has.
    let remove = |target: &str| ConfirmAction::RemoveSshHost {
        name: "db".into(),
        target: target.into(),
    };
    let effect = confirm(&mut ws, &mut cfg, remove("me@old"));
    assert!(matches!(effect, SshHostEffect::NotRemoved), "{effect:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ssh-host = db=me@new\n");
    assert_eq!(ws.ssh_input.as_ref().unwrap().0.text(), "db");
    assert_eq!(
        super::ssh_status_line(&ws),
        Some(tr.ssh_status_not_removed("db").as_str())
    );
    let effect = confirm(&mut ws, &mut cfg, remove("me@new"));
    assert!(matches!(effect, SshHostEffect::Changed), "{effect:?}");
    assert!(cfg.ssh_hosts.is_empty());
    assert_eq!(ws.ssh_input.as_ref().unwrap().0.text(), "");
    assert_eq!(super::ssh_status_line(&ws), Some("Removed \"db\""));
    // The dispatch arms go through this function.
    let dispatch = finding_body(include_str!("modals.rs"), "dispatch_confirm_action_arms");
    assert!(dispatch.contains("self.run_ssh_host_confirm(ws, action);"));
}

/// The palette shows twelve rows at the 1280x820 window the panel captures,
/// at the default 13 px font, under a 32 px tab bar: the Tabs and Splits
/// headings and ten commands. The card ends at y = 95.04 + 30.25 + 12 x
/// 28.25, and the painter, a press, the accessibility tree and
/// `ctl ui_geometry` agree on every command row, numbering commands past the
/// headings, which none of them selects.
#[test]
fn palette_shows_twelve_rows_at_1280x820() {
    use super::{PickerPointer, apply_picker_pointer};
    let bindings = kettle_config::keybinds::defaults();
    let cell = (7.8, 16.25);
    let surface = (1280.0, 820.0);
    let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, Some(&[]), false, kettle_config::BroadcastDefault::default());
    let menu = picker_context_menu(list, surface, cell, cell, 32.0).unwrap();
    let (_, y, _, h) = menu.card_rect();
    assert!((y - 95.04).abs() < 0.01, "card top {y}");
    assert!((y + h - (95.04 + 30.25 + 12.0 * 28.25)).abs() < 0.01, "card bottom {}", y + h);
    let painted = kettle_render::context_menu_row_rects(&menu, cell);
    assert_eq!(painted.len(), 12, "twelve painted rows");
    let headings: Vec<usize> = painted.iter().map(|(index, _)| *index).filter(|&index| menu.rows[index].heading).collect();
    assert_eq!(headings, [0, 10], "Tabs, then Splits");
    let overlay = kettle_render::Overlay {
        palette_query: Some(String::new()),
        context_menu: Some(menu),
        ..Default::default()
    };
    let menu = overlay.context_menu.as_ref().unwrap();
    let projection = super::modal_accessibility_projection(&overlay, surface, cell, 1.0, &EN_TEXT);
    let diagnostic = super::ctl::picker_diagnostic_geometry(&overlay).unwrap();
    let ctl_rows = diagnostic["rows"].as_array().unwrap();
    assert_eq!(ctl_rows.len(), 10, "ctl lists the ten commands");
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((Default::default(), 0));
    let mut ctl_rows = ctl_rows.iter();
    for &(index, rect) in &painted {
        assert_eq!(menu.visible_row_rect(index), Some(rect), "row {index}");
        let (x, y, w, h) = rect;
        ws.cursor.x = f64::from(x + 10.0);
        ws.cursor.y = f64::from(y + h * 0.5);
        let before = ws.palette_input.as_ref().unwrap().1;
        let pressed = apply_picker_pointer(&mut ws, &kettle_config::Config::default(), menu, cell.1, PickerPointer::Press);
        let Some(option) = kettle_render::menu_row_option(&menu.rows, index) else {
            assert!(!pressed, "a heading takes no press");
            assert_eq!(ws.palette_input.as_ref().unwrap().1, before);
            continue;
        };
        assert!(pressed);
        assert_eq!(ws.palette_input.as_ref().unwrap().1, option, "press on row {index}");
        let node = projection
            .nodes
            .iter()
            .find(|(id, _)| Some(*id) == super::accessibility_picker_option_ids(super::MODAL_KIND_PALETTE, &menu.rows)[index])
            .unwrap();
        assert_eq!(node.1.label(), Some(menu.rows[index].label.as_str()));
        assert_eq!(
            node.1.bounds(),
            Some(accesskit::Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64)),
            "accessibility row {index}"
        );
        let ctl_row = ctl_rows.next().unwrap();
        assert_eq!(ctl_row["index"], index);
        assert_eq!(ctl_row["option"], option, "ctl names the command index too");
        assert_eq!(ctl_row["rect"]["y"].as_f64().unwrap() as f32, y, "ctl row {index}");
    }
    assert_eq!(menu.visible_row_rect(12), None);
}

/// On a 2x display the command palette is 360 to 720 points wide, as at 1x:
/// a 1280x820-point window (2560x1640 pixels) gets the same 0.56 share of
/// its width and the same columns as at 1x, where it was held to 720 pixels,
/// 360 points. The header shares the card. The layout picker, like the theme
/// and SSH pickers, keeps its pixel limits.
#[test]
fn palette_card_is_sized_in_points_and_other_pickers_keep_pixels() {
    let cfg = kettle_config::Config::default();
    let surface = (2560.0, 1640.0);
    let cell = (15.6, 32.5);
    let mut ws = WindowState::new(0, Mux::new());
    ws.startup_scale = 2.0;
    ws.palette_input = Some((Default::default(), 0));
    let list = super::open_picker_list(&ws, &cfg, "TokyoNight Night", &EN_TEXT, None).unwrap();
    assert_eq!(list.card_scale, 2.0);
    let menu = picker_context_menu(list, surface, cell, cell, 64.0).unwrap();
    assert!((menu.panel_w_clamped - 1433.6).abs() < 0.01, "{}", menu.panel_w_clamped);
    assert_eq!(kettle_render::menu::panel_columns(menu.panel_w_clamped, cell.0), 89);
    let header = super::picker_card_geometry(surface, cell, 64.0, 1, 2.0).unwrap().header;
    assert_eq!((header.0, header.2), (menu.anchor.0, menu.panel_w_clamped));
    // The same window at 1x is unchanged.
    ws.startup_scale = 1.0;
    let list = super::open_picker_list(&ws, &cfg, "TokyoNight Night", &EN_TEXT, None).unwrap();
    let menu = picker_context_menu(list, (1280.0, 820.0), (7.8, 16.25), (7.8, 16.25), 32.0).unwrap();
    assert!((menu.panel_w_clamped - 716.8).abs() < 0.01);
    // Another picker at 2x keeps its 720-pixel limit.
    ws.startup_scale = 2.0;
    ws.palette_input = None;
    ws.layout_picker_input = Some((Default::default(), 0));
    ws.layout_picker_entries = vec!["one".into(), "two".into()];
    let list = super::open_picker_list(&ws, &cfg, "TokyoNight Night", &EN_TEXT, None).unwrap();
    assert_eq!(list.card_scale, 1.0);
    let menu = picker_context_menu(list, surface, cell, cell, 64.0).unwrap();
    assert_eq!(menu.panel_w_clamped, 720.0);
}

/// At 2x, the open palette's one layout, the one the frame paints and
/// presses, the header field, accessibility and ctl measure, gives a card
/// and header 1440 pixels (720 points) wide where 1x caps it at 720 pixels,
/// header and card share their x and width with no gap between them, the
/// described rows in view show their descriptions whole, and the theme
/// picker's card keeps its pixel limit.
#[test]
fn the_open_palette_is_sized_in_points_at_2x() {
    let cfg = kettle_config::Config::default();
    let surface = (2713.0, 1635.0);
    let cell = kettle_render::bundled_cell_size(kettle_config::DEFAULT_FONT_SIZE, 2.0);
    let mut ws = WindowState::new(0, Mux::new());
    // Profiles have loaded (none), so no pending band sits between the
    // header and the rows.
    ws.palette_profiles = Some(Vec::new());
    ws.palette_input = Some((Default::default(), 0));
    let menu_at = |ws: &WindowState| {
        super::open_picker_menu(ws, &cfg, "TokyoNight Night", &EN_TEXT, None, surface, cell, cell, 64.0).unwrap()
    };
    ws.startup_scale = 2.0;
    let menu = menu_at(&ws);
    let (x, y, w, h) = menu.picker_header_rect();
    assert!(menu.panel_w_clamped >= 1400.0, "card {} px", menu.panel_w_clamped);
    assert_eq!((x, w), (menu.anchor.0, menu.panel_w_clamped));
    assert_eq!(y + h, menu.anchor.1, "the header sits on the rows");
    assert_eq!(menu.card_rect().1, y);
    let painted = kettle_render::context_menu_row_rects(&menu, cell);
    let described: Vec<&str> = painted
        .iter()
        .map(|&(index, _)| &menu.rows[index])
        .filter(|row| !row.description.is_empty())
        .map(|row| {
            assert!(
                kettle_render::menu_row_description_left(row, menu.anchor.0, menu.panel_w_clamped, cell.0).is_some(),
                "{} hides its description at 2x",
                row.label
            );
            row.label.as_str()
        })
        .collect();
    assert!(described.len() >= 3, "{described:?}");
    ws.startup_scale = 1.0;
    assert_eq!(menu_at(&ws).panel_w_clamped, 720.0, "1x caps at 720 pixels");
    ws.startup_scale = 2.0;
    ws.palette_input = None;
    ws.theme_picker_input = Some(crate::window_state::ThemePickerState {
        query: Default::default(),
        selected: 0,
        opened_on: "TokyoNight Night".into(),
        slot: None,
    });
    assert_eq!(menu_at(&ws).panel_w_clamped, 720.0, "the theme picker keeps its pixels");
}

/// One-row wheel steps scroll the whole empty palette, headings and all, one
/// row a step down to the end and back to the top. No step selects a
/// heading, and the selection is always in view.
#[test]
fn wheel_steps_scroll_past_headings() {
    use super::{PickerPointer, apply_picker_pointer, picker_context_menu_at};
    let bindings = kettle_config::keybinds::defaults();
    let cfg = kettle_config::Config::default();
    let cell = (7.8, 16.25);
    let surface = (1280.0, 820.0);
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((Default::default(), 0));
    let menu_for = |ws: &WindowState| {
        let selected = ws.palette_input.as_ref().unwrap().1;
        let list = command_picker_list("", selected, &bindings, &EN_TEXT, None, Some(&[]), false, kettle_config::BroadcastDefault::default());
        picker_context_menu_at(list, surface, cell, cell, 32.0, ws.picker_view.scroll).unwrap()
    };
    let first = menu_for(&ws);
    let max_offset = first.rows.len() - 12;
    ws.cursor.x = f64::from(first.anchor.0 + 20.0);
    ws.cursor.y = f64::from(first.anchor.1 + 40.0);
    for (delta, end) in [(1, max_offset), (-1, 0)] {
        let mut offset = menu_for(&ws).scroll_offset;
        let mut steps = 0;
        while offset != end {
            let menu = menu_for(&ws);
            assert!(apply_picker_pointer(&mut ws, &cfg, &menu, cell.1, PickerPointer::Wheel(delta)));
            let next = menu_for(&ws);
            assert!(!next.rows[next.highlight].heading, "a wheel step selected a heading");
            assert_eq!(next.scroll_offset, offset.saturating_add_signed(delta), "one row a step");
            assert!(
                (next.scroll_offset..next.scroll_offset + 12).contains(&next.highlight),
                "the selection is in view at offset {}",
                next.scroll_offset
            );
            offset = next.scroll_offset;
            steps += 1;
            assert!(steps <= first.rows.len(), "the wheel never reached {end}");
        }
    }
}

/// An open picker keeps its query header even with no row to list: the
/// header is laid out from the card's own layout, at least one row tall.
#[test]
fn an_empty_picker_list_keeps_its_header() {
    let list = super::PickerList { status: None, rows: Vec::new(), selected: 0, footer: None, card_scale: 1.0 };
    let menu = picker_context_menu(list, (1280.0, 820.0), (7.8, 16.25), (7.8, 16.25), 32.0).expect("a header and card");
    let (_, _, w, h) = menu.picker_header_rect();
    assert!(w > 0.0 && h > 0.0);
}

/// Hover prelight, the quick-pick rule: a pointer that moves selects the row
/// under it, and one that has not moved since the picker opened, took a key
/// or scrolled keeps the selection it has. Moving again, even within the
/// same row, selects that row once more. A hover never takes the motion.
#[test]
fn a_moving_pointer_selects_the_row_under_it_and_a_still_one_does_not() {
    use super::{
        PickerPointer, apply_picker_pointer, note_picker_keys, open_picker_list,
        picker_context_menu, reset_picker_view,
    };
    let cfg = kettle_config::Config::default();
    let tr = kettle_i18n::Translator::default();
    let mut ws = WindowState::new(0, Mux::new());
    ws.layout_picker_entries = (0..40).map(|i| format!("layout-{i}")).collect();
    ws.layout_picker_input = Some((Default::default(), 0));
    let menu = picker_context_menu(
        open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap(),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        0.0,
    )
    .unwrap();
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    let over = |ws: &mut WindowState, row: usize, dy: f32| {
        ws.cursor.x = f64::from(menu.anchor.0 + 20.0);
        ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * (row as f32 + 0.5) + dy);
    };
    let selected = |ws: &WindowState| ws.layout_picker_input.as_ref().unwrap().1;
    let hover = |ws: &mut WindowState| {
        apply_picker_pointer(ws, &cfg, &menu, 16.0, PickerPointer::Hover)
    };
    // The picker opens under a pointer resting on row 3.
    over(&mut ws, 3, 0.0);
    reset_picker_view(&mut ws);
    assert!(!hover(&mut ws));
    assert_eq!(selected(&ws), 0, "a pointer that did not move selects nothing");
    over(&mut ws, 5, 0.0);
    assert!(!hover(&mut ws), "a hover never takes the motion");
    assert_eq!(selected(&ws), 5, "a moving pointer selects the row under it");
    // Down, then the same position again: the keyboard's selection holds.
    ws.layout_picker_input.as_mut().unwrap().1 = 6;
    note_picker_keys(&mut ws);
    hover(&mut ws);
    assert_eq!(selected(&ws), 6, "the keyboard wins until the pointer moves");
    // A real move within row 5 selects it again.
    over(&mut ws, 5, 1.0);
    hover(&mut ws);
    assert_eq!(selected(&ws), 5);
    // The wheel moves the rows under the pointer, which keeps what it has.
    assert!(apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Wheel(3)));
    let after_wheel = selected(&ws);
    hover(&mut ws);
    assert_eq!(selected(&ws), after_wheel, "rows that scroll under a still pointer");
    // Off the card, nothing is selected.
    ws.cursor.x = f64::from(menu.anchor.0 - 5.0);
    hover(&mut ws);
    assert_eq!(selected(&ws), after_wheel);
}

/// The SSH launcher's Save row is lit, not selected, by a hover, and only
/// while the physical pointer is in the window; a key or a move elsewhere
/// puts the light out.
#[test]
fn a_hover_lights_the_ssh_save_row_without_selecting_it() {
    use super::{
        PickerPointer, apply_picker_pointer, note_picker_keys, open_picker_menu,
        reset_picker_view,
    };
    let cfg = kettle_config::Config {
        ssh_hosts: ssh_fixture_hosts(&[("alpha", "me@alpha"), ("beta", "me@beta")]),
        ..Default::default()
    };
    let tr = kettle_i18n::Translator::default();
    let mut ws = ssh_launcher_with("a");
    reset_picker_view(&mut ws);
    let project = |ws: &WindowState| {
        open_picker_menu(
            ws,
            &cfg,
            "dark-theme",
            &tr,
            None,
            (800.0, 600.0),
            (8.0, 16.0),
            (8.0, 16.0),
            0.0,
        )
        .unwrap()
    };
    let menu = project(&ws);
    let save = menu.rows.len() - 1;
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    let at = |row: usize| {
        winit::dpi::PhysicalPosition::new(
            f64::from(menu.anchor.0 + 10.0),
            f64::from(menu.anchor.1 + inset + row_h * (row as f32 + 0.5)),
        )
    };
    ws.cursor = at(save);
    ws.native_pointer = Some(ws.cursor);
    assert!(!apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Hover));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0, "a hover never selects Save");
    let lit = project(&ws);
    assert!(lit.rows[save].prelit, "a hover lights it");
    assert_eq!(lit.rows.iter().filter(|row| row.prelit).count(), 1);
    ws.native_pointer = None;
    assert!(!project(&ws).rows[save].prelit, "nor after the pointer left");
    ws.native_pointer = Some(ws.cursor);
    note_picker_keys(&mut ws);
    assert!(!project(&ws).rows[save].prelit, "a key puts the light out");
    ws.cursor = at(save);
    ws.cursor.y += 1.0;
    apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Hover);
    assert!(project(&ws).rows[save].prelit);
    ws.cursor = at(0);
    apply_picker_pointer(&mut ws, &cfg, &menu, 16.0, PickerPointer::Hover);
    assert!(!project(&ws).rows[save].prelit, "a move to a host row");
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0);
}

/// The keyboard previews the theme it selects at once. A theme the pointer
/// selects previews only once the pointer has rested 150 ms, a wake the
/// scheduler waits for, so sweeping across rows does not flash themes; a
/// key in between previews at once, and a picker that closes drops the wait.
#[test]
fn a_hovered_theme_previews_only_after_the_pointer_rests() {
    use super::{
        PickerPointer, ThemeHoverPreview, apply_picker_pointer, begin_theme_picker,
        note_picker_keys, picker_context_menu, poll_theme_hover_preview, theme_picker_list,
        theme_picker_selection, theme_preview_change,
    };
    use crate::window_state::THEME_HOVER_PREVIEW_DELAY;
    use std::time::{Duration, Instant};
    let tr = kettle_i18n::Translator::default();
    let cfg = kettle_config::Config {
        theme_name: "Dracula".into(),
        ..Default::default()
    };
    let mut ws = WindowState::new(0, Mux::new());
    begin_theme_picker(&mut ws, ("Dracula".into(), cfg.theme.clone()));
    let state = ws.theme_picker_input.clone().unwrap();
    let menu = picker_context_menu(
        theme_picker_list(&state.query, state.selected, &state.opened_on, &tr),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        0.0,
    )
    .unwrap();
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    let hover_row = |ws: &mut WindowState, row: usize| {
        ws.cursor.x = f64::from(menu.anchor.0 + 20.0);
        ws.cursor.y =
            f64::from(menu.anchor.1 + inset + row_h * ((row - menu.scroll_offset) as f32 + 0.5));
        apply_picker_pointer(ws, &cfg, &menu, 16.0, PickerPointer::Hover);
    };
    let name = |ws: &WindowState| theme_picker_selection(ws.theme_picker_input.as_ref().unwrap(), &tr);
    let (first, second) = (menu.scroll_offset, menu.scroll_offset + 1);
    let target = if menu.highlight == first { second } else { first };
    let before = Instant::now();
    hover_row(&mut ws, target);
    assert_eq!(ws.theme_picker_input.as_ref().unwrap().selected, target);
    let hovered = name(&ws).unwrap();
    assert_ne!(hovered, "Dracula");
    assert!(theme_preview_change(&mut ws, &cfg, &tr).is_none(), "no preview yet");
    let crate::window_state::PointerPreview::At(at) = ws.picker_view.pointer_preview else {
        panic!("a wake for the rest");
    };
    assert!(at >= before + THEME_HOVER_PREVIEW_DELAY);
    assert!(at <= Instant::now() + THEME_HOVER_PREVIEW_DELAY);
    assert_eq!(
        poll_theme_hover_preview(&mut ws, at - Duration::from_millis(40)),
        ThemeHoverPreview::Wait(Duration::from_millis(40))
    );
    assert!(theme_preview_change(&mut ws, &cfg, &tr).is_none(), "still moving");
    assert_eq!(poll_theme_hover_preview(&mut ws, at), ThemeHoverPreview::Due);
    assert_eq!(ws.picker_view.pointer_preview, crate::window_state::PointerPreview::None);
    let (preview, _) = theme_preview_change(&mut ws, &cfg, &tr).expect("the rest previews it");
    assert_eq!(preview, hovered);
    assert_eq!(poll_theme_hover_preview(&mut ws, at), ThemeHoverPreview::Idle);
    // A key during the wait previews the selection at once.
    let other = if target == first { second } else { first };
    hover_row(&mut ws, other);
    assert!(theme_preview_change(&mut ws, &cfg, &tr).is_none());
    note_picker_keys(&mut ws);
    let (preview, _) = theme_preview_change(&mut ws, &cfg, &tr).expect("a key previews at once");
    assert_eq!(Some(preview.as_str()), name(&ws));
    // A picker that closes drops the wait.
    hover_row(&mut ws, target);
    assert!(matches!(ws.picker_view.pointer_preview, crate::window_state::PointerPreview::At(_)));
    ws.theme_picker_input = None;
    assert_eq!(poll_theme_hover_preview(&mut ws, Instant::now()), ThemeHoverPreview::Idle);
    assert_eq!(ws.picker_view.pointer_preview, crate::window_state::PointerPreview::None);
}

/// The scheduler waits for the pointer's theme preview like any other
/// timed repaint, and each picker's keys hand the selection back to the
/// keyboard first.
#[test]
fn the_hover_preview_wakes_the_loop_and_keys_take_the_selection_back() {
    let src = production_source();
    let wait = src
        .split("fn about_to_wait_inner(")
        .nth(1)
        .expect("about_to_wait_inner");
    assert!(wait.contains("let theme_preview_wait = self.settle_theme_hover_preview(ws, now);"));
    assert!(wait.contains("if let Some(next) = theme_preview_wait {"));
    for key in ["fn palette_key(", "fn theme_picker_key(", "fn layout_picker_key(", "fn ssh_key("] {
        let body = src.split(key).nth(1).expect(key);
        let start = &body[..body.find("modal_editor_key").expect("editor keys")];
        assert!(start.contains("note_picker_keys(ws);"), "{key}");
    }
}

/// Review D10: the wheel over a picker scrolls its list and keeps the
/// selection while it stays in view; a selection the list scrolls past moves
/// to the nearest row in view. Keys then move the list from where the last
/// frame drew it, only as far as keeps the selection in view, and a theme
/// the wheel selects previews once the pointer rests.
#[test]
fn the_wheel_scrolls_a_picker_and_keeps_its_selection_in_view() {
    use super::{
        PickerPointer, apply_picker_pointer, begin_theme_picker, open_picker_menu,
        remember_picker_scroll, reset_picker_view,
    };
    let cfg = kettle_config::Config::default();
    let tr = kettle_i18n::Translator::default();
    let project = |ws: &WindowState| {
        open_picker_menu(
            ws,
            &cfg,
            "dark-theme",
            &tr,
            None,
            (1280.0, 820.0),
            (8.0, 16.0),
            (8.0, 16.0),
            0.0,
        )
        .unwrap()
    };
    let wheel = |ws: &mut WindowState, delta: isize| {
        let menu = project(ws);
        ws.cursor.x = f64::from(menu.anchor.0 + menu.panel_w_clamped * 0.5);
        ws.cursor.y = f64::from(menu.anchor.1 + menu.panel_h_clamped * 0.5);
        assert!(apply_picker_pointer(ws, &cfg, &menu, 16.0, PickerPointer::Wheel(delta)));
        project(ws)
    };
    let mut ws = WindowState::new(0, Mux::new());
    ws.layout_picker_entries = (0..40).map(|i| format!("layout-{i}")).collect();
    ws.layout_picker_input = Some((Default::default(), 0));
    reset_picker_view(&mut ws);
    let selected = |ws: &WindowState| ws.layout_picker_input.as_ref().unwrap().1;
    let select = |ws: &mut WindowState, row: usize| ws.layout_picker_input.as_mut().unwrap().1 = row;
    let first = project(&ws);
    let visible = (0..first.rows.len())
        .filter(|&index| first.visible_row_rect(index).is_some())
        .count();
    assert_eq!(visible, 12);
    select(&mut ws, 5);
    let menu = wheel(&mut ws, 3);
    assert_eq!(menu.scroll_offset, 3, "the wheel scrolls the list");
    assert_eq!(selected(&ws), 5, "and keeps a selection in view");
    assert_eq!(menu.highlight, 5);
    let menu = wheel(&mut ws, 5);
    assert_eq!(menu.scroll_offset, 8);
    assert_eq!(selected(&ws), 8, "one it scrolls past moves to the first row in view");
    let menu = wheel(&mut ws, 100);
    assert_eq!(menu.scroll_offset, 40 - visible);
    assert_eq!(selected(&ws), 40 - visible);
    let menu = wheel(&mut ws, -100);
    assert_eq!(menu.scroll_offset, 0);
    assert_eq!(selected(&ws), visible - 1, "or to the last row in view");
    // Keys move the list from where it was drawn.
    let menu = wheel(&mut ws, 10);
    assert_eq!((menu.scroll_offset, selected(&ws)), (10, 11));
    remember_picker_scroll(&mut ws, Some(&menu));
    select(&mut ws, 12);
    let menu = project(&ws);
    assert_eq!(menu.scroll_offset, 10, "a key inside the view moves nothing");
    remember_picker_scroll(&mut ws, Some(&menu));
    select(&mut ws, 22);
    let menu = project(&ws);
    assert_eq!(menu.scroll_offset, 11, "one row past the bottom moves one row");
    remember_picker_scroll(&mut ws, Some(&menu));
    select(&mut ws, 21);
    assert_eq!(project(&ws).scroll_offset, 11, "Up inside the view moves nothing");
    select(&mut ws, 4);
    assert_eq!(project(&ws).scroll_offset, 4, "above the view: just far enough");
    // A theme the wheel selects waits for the pointer to rest.
    let mut ws = WindowState::new(0, Mux::new());
    begin_theme_picker(&mut ws, ("Dracula".into(), cfg.theme.clone()));
    let before = ws.theme_picker_input.as_ref().unwrap().selected;
    let far = if before < project(&ws).rows.len() / 2 { 1000 } else { -1000 };
    wheel(&mut ws, far);
    assert_ne!(ws.theme_picker_input.as_ref().unwrap().selected, before);
    assert!(matches!(ws.picker_view.pointer_preview, crate::window_state::PointerPreview::At(_)));
    // The frame records where it drew the list.
    let src = production_source();
    let redraw = src.split("fn redraw(").nth(1).expect("redraw");
    let overlay = redraw.find("let overlay = self.overlay(ws, reuse_pane_snapshots);").unwrap();
    let remember = redraw
        .find("remember_picker_scroll(ws, overlay.context_menu.as_ref());")
        .expect("the frame remembers the picker's scroll");
    assert!(overlay < remember);
}

/// Review: a palette with room for one row wheels past group headings. With
/// the last command of a group selected, one detent down shows only the next
/// heading; the selection moves to the first command past it, so the list
/// moves on instead of snapping back, and one detent up returns. Detent by
/// detent the list reaches its end and its top again.
#[test]
fn a_one_row_palette_wheels_past_group_headings() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_menu};
    let cfg = kettle_config::Config::default();
    let cell = (8.0, 16.0);
    let mut ws = WindowState::new(0, Mux::new());
    super::profiles::open_profile_palette(&mut ws, Some(&[]));
    let project = |ws: &WindowState, surface: (f32, f32)| {
        open_picker_menu(ws, &cfg, "dark-theme", &EN_TEXT, None, surface, cell, cell, 0.0)
    };
    let shown = |menu: &kettle_render::ContextMenu| {
        (0..menu.rows.len())
            .filter(|&index| menu.visible_row_rect(index).is_some())
            .count()
    };
    let surface = (40..400)
        .map(|height| (800.0, height as f32))
        .find(|&surface| project(&ws, surface).is_some_and(|menu| shown(&menu) == 1))
        .expect("a palette one row tall");
    let first = project(&ws, surface).unwrap();
    let heading = (1..first.rows.len())
        .find(|&row| first.rows[row].heading)
        .expect("a second group");
    let last = heading - 1;
    ws.palette_input.as_mut().unwrap().1 = kettle_render::menu_row_option(&first.rows, last).unwrap();
    let menu = project(&ws, surface).unwrap();
    assert_eq!((menu.scroll_offset, menu.highlight), (last, last));
    ws.cursor.x = f64::from(menu.anchor.0 + menu.panel_w_clamped * 0.5);
    ws.cursor.y = f64::from(menu.anchor.1 + menu.panel_h_clamped * 0.5);
    let wheel = |ws: &mut WindowState, delta: isize| {
        let menu = project(ws, surface).unwrap();
        assert!(apply_picker_pointer(ws, &cfg, &menu, cell.1, PickerPointer::Wheel(delta)));
        project(ws, surface).unwrap()
    };
    let down = wheel(&mut ws, 1);
    assert_eq!(down.highlight, heading + 1, "the first command past the heading");
    assert_eq!(down.scroll_offset, heading + 1, "the list moved on");
    let up = wheel(&mut ws, -1);
    assert_eq!((up.scroll_offset, up.highlight), (last, last), "and back");
    for (delta, end) in [(1, first.rows.len() - 1), (-1, 1)] {
        let mut offset = project(&ws, surface).unwrap().scroll_offset;
        let mut steps = 0;
        while offset != end {
            let next = wheel(&mut ws, delta);
            assert!(!next.rows[next.highlight].heading);
            assert_eq!(next.scroll_offset, next.highlight, "the selection is the row shown");
            assert!(
                if delta > 0 { next.scroll_offset > offset } else { next.scroll_offset < offset },
                "a detent {delta} at row {offset} did not move the list"
            );
            offset = next.scroll_offset;
            steps += 1;
            assert!(steps <= first.rows.len());
        }
    }
}

/// Review: the theme hover preview waits for the pointer to rest on the
/// selected row, not for the selection to stop changing. Motion within the
/// row starts the wait again, so a sweep that stays on a row for longer than
/// the wait previews nothing mid-motion. Leaving the row (for the footer) or
/// the window drops the wait: the window keeps its theme until the pointer
/// rests on a row again or a key previews at once.
#[test]
fn the_hover_preview_waits_for_rest_and_drops_when_the_pointer_leaves_the_row() {
    use super::{
        PickerPointer, ThemeHoverPreview, apply_picker_pointer, begin_theme_picker,
        note_picker_keys, picker_context_menu, picker_pointer_left, poll_theme_hover_preview,
        theme_picker_list, theme_picker_selection, theme_preview_change,
    };
    use crate::window_state::PointerPreview;
    use std::time::{Duration, Instant};
    let tr = kettle_i18n::Translator::default();
    let cfg = kettle_config::Config {
        theme_name: "Dracula".into(),
        ..Default::default()
    };
    let mut ws = WindowState::new(0, Mux::new());
    begin_theme_picker(&mut ws, ("Dracula".into(), cfg.theme.clone()));
    let state = ws.theme_picker_input.clone().unwrap();
    let menu = picker_context_menu(
        theme_picker_list(&state.query, state.selected, &state.opened_on, &tr),
        (800.0, 600.0),
        (8.0, 16.0),
        (8.0, 16.0),
        0.0,
    )
    .unwrap();
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    let move_to = |ws: &mut WindowState, x: f32, y: f32| {
        ws.cursor.x = f64::from(x);
        ws.cursor.y = f64::from(y);
        apply_picker_pointer(ws, &cfg, &menu, 16.0, PickerPointer::Hover);
    };
    let row_y = |row: usize| menu.anchor.1 + inset + row_h * ((row - menu.scroll_offset) as f32 + 0.5);
    let waiting = |ws: &WindowState| match ws.picker_view.pointer_preview {
        PointerPreview::At(at) => at,
        other => panic!("no wait: {other:?}"),
    };
    let target = if menu.highlight == menu.scroll_offset { menu.scroll_offset + 1 } else { menu.scroll_offset };
    move_to(&mut ws, menu.anchor.0 + 20.0, row_y(target));
    let first = waiting(&ws);
    // Still moving within the row: the wait starts again.
    std::thread::sleep(Duration::from_millis(5));
    move_to(&mut ws, menu.anchor.0 + 60.0, row_y(target) + 1.0);
    let again = waiting(&ws);
    assert!(again > first, "motion within the row restarts the wait");
    assert_eq!(
        poll_theme_hover_preview(&mut ws, first),
        ThemeHoverPreview::Wait(again - first),
        "the first wait's end previews nothing"
    );
    assert!(theme_preview_change(&mut ws, &cfg, &tr).is_none());
    // Onto the footer, under the rows: the wait is dropped, and no wake or
    // later poll previews the row.
    move_to(&mut ws, menu.anchor.0 + 20.0, menu.anchor.1 + menu.panel_h_clamped + 4.0);
    assert_eq!(ws.picker_view.pointer_preview, PointerPreview::Held);
    let later = Instant::now() + Duration::from_secs(1);
    assert_eq!(poll_theme_hover_preview(&mut ws, later), ThemeHoverPreview::Idle);
    assert!(theme_preview_change(&mut ws, &cfg, &tr).is_none(), "the window keeps its theme");
    // Back onto the selected row, resting: it previews.
    move_to(&mut ws, menu.anchor.0 + 30.0, row_y(target));
    let at = waiting(&ws);
    assert_eq!(poll_theme_hover_preview(&mut ws, at), ThemeHoverPreview::Due);
    let (preview, _) = theme_preview_change(&mut ws, &cfg, &tr).expect("a rest previews it");
    assert_eq!(Some(preview.as_str()), theme_picker_selection(ws.theme_picker_input.as_ref().unwrap(), &tr));
    // A wiggle on a row already previewed starts nothing.
    move_to(&mut ws, menu.anchor.0 + 31.0, row_y(target));
    assert_eq!(ws.picker_view.pointer_preview, PointerPreview::None);
    // Out of the window: the wait is dropped too.
    let other = if target == menu.scroll_offset { target + 1 } else { menu.scroll_offset };
    move_to(&mut ws, menu.anchor.0 + 20.0, row_y(other));
    waiting(&ws);
    picker_pointer_left(&mut ws);
    assert_eq!(ws.picker_view.pointer_preview, PointerPreview::Held);
    assert_eq!(poll_theme_hover_preview(&mut ws, later), ThemeHoverPreview::Idle);
    // A key previews at once.
    note_picker_keys(&mut ws);
    assert!(theme_preview_change(&mut ws, &cfg, &tr).is_some(), "a key previews at once");
    let src = production_source();
    let left = src
        .split("WindowEvent::CursorLeft { .. } => {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::CursorEntered").next())
        .expect("the leave handler");
    assert!(left.contains("picker_pointer_left(ws);"));
}
