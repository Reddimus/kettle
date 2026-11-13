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
        &[],
        false,
    );
    let menu = picker_context_menu(list, (320.0, 160.0), (8.0, 16.0), 0.0)
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
        &[],
        false,
    );
    let entries = super::palette_entries(None, &es, &[]);
    let ranked = kettle_config::palette::rank_matches("ct", &entries, &es);
    assert_eq!(list.rows.len(), ranked.len());
    for (row, entry) in list.rows.iter().zip(ranked) {
        assert_eq!(row.label, entries[entry.index].0.shown(&es));
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
        &[],
        false,
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
                &[],
                false,
            );
            let menu = picker_context_menu(list, surface, (8.0, 16.0), top)
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

/// A long picker list fills its panel: the first row sits right under the
/// query header, all eight rows are painted and hit, and paint, pointer and
/// accessibility agree on every row. A short window still shows a row.
#[test]
fn overflowing_picker_rows_start_at_the_header_and_fill_the_panel() {
    use super::{PickerPointer, apply_picker_pointer};
    let bindings = kettle_config::keybinds::defaults();
    let profiles: Vec<String> = (0..20).map(|i| format!("profile-{i}")).collect();
    let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, &profiles, false);
    assert!(list.rows.len() >= 150, "{} rows", list.rows.len());
    let cell = (8.0, 16.0);
    let geometry = super::picker_card_geometry((800.0, 600.0), cell, 0.0, list.rows.len()).unwrap();
    let menu = picker_context_menu(list, (800.0, 600.0), cell, 0.0).unwrap();
    let header_bottom = geometry.header.1 + geometry.header.3;
    let painted = kettle_render::context_menu_row_rects(&menu, cell);
    assert_eq!(painted.len(), 8, "eight full rows");
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
        apply_picker_pointer(&mut ws, &menu, cell.1, PickerPointer::Press);
        assert_eq!(ws.palette_input.as_ref().unwrap().1, index, "row {index}");
    }
    assert_eq!(menu.visible_row_rect(8), None);
    // A window with room for one or two rows still paints and hits them.
    for height in [120.0, 150.0] {
        let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, &[], false);
        let menu = picker_context_menu(list, (800.0, height), cell, 0.0).unwrap();
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
        let list = command_picker_list("", 0, &bindings, &tr, None, &[], true);
        let bound = kettle_config::palette::commands()
            .into_iter()
            .filter(|(_, action)| {
                kettle_config::keybinds::ui_hint_label(&bindings, action)
                    .is_some_and(|hint| !hint.is_empty())
            })
            .count();
        assert!(bound >= 10, "{bound} bound commands");
        assert_eq!(list.rows.len(), bound);
        assert!(list.rows.iter().all(|row| row.enabled && !row.hint.is_empty()));
        let full = command_picker_list("", 0, &bindings, &tr, None, &[], false);
        assert!(full.rows.len() > list.rows.len());
        assert!(full.rows.iter().any(|row| row.hint.is_empty()));
        for query in ["", "tab", "split", "font"] {
            let entries = super::palette_view_entries(None, &tr, &[], &bindings, true);
            let ranked = kettle_config::palette::rank_entries(query, &entries, &tr);
            let list = command_picker_list(query, 0, &bindings, &tr, None, &[], true);
            assert_eq!(list.rows.len(), ranked.len().max(1), "{query:?}");
            for (row, index) in list.rows.iter().zip(&ranked) {
                assert_eq!(row.label, entries[*index].0.shown(&tr), "{query:?}");
            }
        }
    }
    // Enter, the pointer and the painted list read the same view entries.
    let source = finding_body(include_str!("modals.rs"), "palette_key");
    assert!(source.contains("palette_view_entries("));
    assert!(source.contains("ws.palette_bound_only"));
    let open = include_str!("pickers.rs")
        .split("pub(super) fn open_picker_list(")
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
    assert!(arm.contains("ws.palette_input = Some("));
}
