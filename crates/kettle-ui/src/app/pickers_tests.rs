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
        &[], false, kettle_config::BroadcastDefault::default(),
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
        &[],
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(
        ssh.rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["Connect to \"gp\"", "gpu", "gpu-prod", "staging-prod"]
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
        &[], false, kettle_config::BroadcastDefault::default(),
    );
    let entries = super::palette_entries(None, &es, &[], kettle_config::BroadcastDefault::default());
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
        &[], false, kettle_config::BroadcastDefault::default(),
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
                &[], false, kettle_config::BroadcastDefault::default(),
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

/// A scrolling picker paints whole rows down to its bottom edge: it has no
/// arrow strips, and its height is whole rows, so no blank part-row is left
/// under the selection, and a long list paints all eight rows. The last
/// painted row takes a click.
#[test]
fn scrolling_pickers_paint_whole_rows_with_no_blank_strip() {
    let names: Vec<String> = (0..30).map(|i| format!("layout {i}")).collect();
    for surface in [(1280.0, 820.0), (800.0, 600.0), (640.0, 360.0), (800.0, 200.0)] {
        for cell in [(8.0, 16.0), (16.0, 34.0)] {
            for total in [3, 8, 9, 30] {
                let menu = picker_context_menu(
                    layout_picker_list("", 0, &names[..total], &EN_TEXT),
                    surface,
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
                if room >= 8.0 * row_h {
                    assert!(painted as usize >= total.min(8), "{what}: {painted} painted");
                }
                let last = menu.scroll_offset + painted as usize - 1;
                let rect = menu.visible_row_rect(last).expect("last painted row");
                let mut ws = WindowState::new(0, Mux::new());
                ws.layout_picker_input = Some((Default::default(), 0));
                ws.cursor.x = f64::from(rect.0 + 10.0);
                ws.cursor.y = f64::from(rect.1 + rect.3 - 2.0);
                assert!(super::apply_picker_pointer(
                    &mut ws,
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
/// query header, all eight rows are painted and hit, and paint, pointer and
/// accessibility agree on every row. A short window still shows a row.
#[test]
fn overflowing_picker_rows_start_at_the_header_and_fill_the_panel() {
    use super::{PickerPointer, apply_picker_pointer};
    let bindings = kettle_config::keybinds::defaults();
    let profiles: Vec<String> = (0..20).map(|i| format!("profile-{i}")).collect();
    let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, &profiles, false, kettle_config::BroadcastDefault::default());
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
        let list = command_picker_list("", 0, &bindings, &EN_TEXT, None, &[], false, kettle_config::BroadcastDefault::default());
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
        let list = command_picker_list("", 0, &bindings, &tr, None, &[], true, kettle_config::BroadcastDefault::default());
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
        let full = command_picker_list("", 0, &bindings, &tr, None, &[], false, kettle_config::BroadcastDefault::default());
        assert!(full.rows.len() > list.rows.len());
        assert!(full.rows.iter().any(|row| row.hint.is_empty()));
        for query in ["", "tab", "split", "font"] {
            let entries = super::palette_view_entries(None, &tr, &[], kettle_config::BroadcastDefault::default(), &bindings, true);
            let ranked = kettle_config::palette::rank_entries(query, &entries, &tr);
            let list = command_picker_list(query, 0, &bindings, &tr, None, &[], true, kettle_config::BroadcastDefault::default());
            assert_eq!(list.rows.len(), ranked.len().max(1), "{query:?}");
            for (row, index) in list.rows.iter().zip(&ranked) {
                assert_eq!(row.label, entries[*index].0.shown(&tr), "{query:?}");
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

/// Enter in the keyboard-shortcut list runs the row painted at the selection,
/// for every selection and query: a command with a shortcut, the label and
/// hint the list shows there. Past the last row it runs nothing.
#[test]
fn enter_in_the_shortcut_list_runs_the_selected_bound_row() {
    let bindings = kettle_config::keybinds::defaults();
    for tr in [EN_TEXT, kettle_i18n::Translator::new(kettle_i18n::Language::Es)] {
        let entries = super::palette_view_entries(None, &tr, &[], kettle_config::BroadcastDefault::default(), &bindings, true);
        let full = super::palette_view_entries(None, &tr, &[], kettle_config::BroadcastDefault::default(), &bindings, false);
        for query in ["", "tab", "split", "font", "pane"] {
            let list = command_picker_list(query, 0, &bindings, &tr, None, &[], true, kettle_config::BroadcastDefault::default());
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
                    .find(|(_, candidate)| *candidate == action)
                    .map(|(label, _)| label.shown(&tr));
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
    assert_eq!(rows, [ConnectTyped, Imported(0)]);
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
        super::take_ssh_enter_target(&mut ws, &[]).as_deref(),
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
    assert_eq!(ssh_rows("prod", &[], &imported), [Imported(1), Imported(0)]);
    assert_eq!(ssh_rows("PROD ", &[], &imported), [Imported(1), Imported(0)]);
    assert_eq!(
        ssh_row_action("prod", 0, &[], &imported),
        Some(Connect("prod".into()))
    );
    // An exact name wins over an earlier prefix match.
    let hosts = ssh_fixture_hosts(&[("db-old", "me@old"), ("db", "me@db")]);
    assert_eq!(ssh_rows("db", &hosts, &[]), [Configured(1), Configured(0)]);
    assert_eq!(
        ssh_row_action("db", 0, &hosts, &[]),
        Some(Connect("me@db".into()))
    );
    // An exact imported alias wins over a configured fuzzy match.
    let hosts = ssh_fixture_hosts(&[("web-staging-3", "me@stg3")]);
    let imported = vec!["web3".to_string()];
    assert_eq!(ssh_rows("web3", &hosts, &imported), [Imported(0), Configured(0)]);
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
        assert_eq!(super::take_ssh_enter_target(&mut ws, &[]), None, "{typed:?}");
        assert!(ws.ssh_input.is_some(), "{typed:?}: the launcher stays open");
        let list = ssh_picker_list(typed, 0, &[], &[], &kettle_i18n::Translator::default());
        assert!(!list.rows[0].enabled);
    }
    let mut ws = ssh_launcher_with("me@box");
    assert_eq!(
        super::take_ssh_enter_target(&mut ws, &[]).as_deref(),
        Some("me@box")
    );
    assert!(ws.ssh_input.is_none(), "a connection closes the launcher");
    // Nothing typed and no hosts: Enter closes, as it always has.
    let mut ws = ssh_launcher_with("");
    assert_eq!(super::take_ssh_enter_target(&mut ws, &[]), None);
    assert!(ws.ssh_input.is_none());
    // Host rows that fuzzy-match an invalid query are hidden, so Enter cannot
    // connect to one: only the refusal row shows.
    let hosts = ssh_fixture_hosts(&[("h-1", "me@h1"), ("my box", "me@box")]);
    for typed in ["-", "-oProxyCommand=x", "h -1"] {
        assert_eq!(ssh_rows(typed, &hosts, &[]), [super::SshRow::Placeholder], "{typed:?}");
        let mut ws = ssh_launcher_with(typed);
        assert_eq!(super::take_ssh_enter_target(&mut ws, &hosts), None, "{typed:?}");
        assert!(ws.ssh_input.is_some(), "{typed:?}");
    }
    // A configured name with a space, typed in full, is still that host.
    assert_eq!(ssh_rows("my box", &hosts, &[]), [super::SshRow::Configured(1)]);
    let mut ws = ssh_launcher_with("My Box");
    assert_eq!(
        super::take_ssh_enter_target(&mut ws, &hosts).as_deref(),
        Some("me@box")
    );
    // The key handler goes through it, and keeps the spawn inside `ssh_key`.
    let body = finding_body(include_str!("modals.rs"), "ssh_key");
    assert!(body.contains("if let Some(target) = take_ssh_enter_target(ws, &self.cfg.ssh_hosts)"));
}

/// Arrows skip the disabled placeholder. Down wraps from the last row to the
/// first. Up from the first row wraps onto the last row, which is a host.
#[test]
fn ssh_arrows_skip_the_placeholder_and_up_wraps_only_onto_a_host_row() {
    use super::ssh_step_selection as step;
    let hosts = ssh_fixture_hosts(&[("h-1", "me@h1"), ("h-2", "me@h2")]);
    // The Connect row, then hosts.
    let rows = ssh_rows("h", &hosts, &[]);
    assert_eq!(rows.first(), Some(&super::SshRow::ConnectTyped));
    assert_eq!(step(&rows, 0, true), 1);
    assert_eq!(step(&rows, 1, true), 2);
    assert_eq!(step(&rows, 2, true), 0, "Down wraps to the Connect row");
    assert_eq!(step(&rows, 2, false), 1);
    assert_eq!(step(&rows, 0, false), 2, "Up wraps onto the last host");
    // Hosts only: Up wraps among them, as before.
    let rows = ssh_rows("", &hosts, &[]);
    assert_eq!(step(&rows, 0, false), 1);
    // A list with only the placeholder has nothing to move to.
    let rows = ssh_rows("", &[], &[]);
    assert_eq!((step(&rows, 0, true), step(&rows, 0, false)), (0, 0));
    let rows = ssh_rows("-", &[], &[]);
    assert_eq!((step(&rows, 0, true), step(&rows, 0, false)), (0, 0));
}

/// The wheel selects the row it exposes, here the last host below the
/// Connect row.
#[test]
fn ssh_wheel_reaches_the_last_host_row() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config {
        ssh_hosts: (0..40)
            .map(|i| (format!("host-{i}"), format!("me@host-{i}")))
            .collect(),
        ..Default::default()
    };
    let tr = kettle_i18n::Translator::default();
    let (surface, cell) = ((320.0, 160.0), (8.0, 16.0));
    let mut ws = ssh_launcher_with("host");
    let first = picker_context_menu(
        open_picker_list(&ws, &cfg, "dark-theme", &tr, None).unwrap(),
        surface,
        cell,
        0.0,
    )
    .unwrap();
    assert_eq!(first.rows.len(), 41);
    ws.cursor.x = f64::from(first.anchor.0 + first.panel_w_clamped * 0.5);
    ws.cursor.y = f64::from(first.anchor.1 + first.panel_h_clamped * 0.5);
    assert!(apply_picker_pointer(&mut ws, &first, cell.1, PickerPointer::Wheel(100)));
    let selected = ws.ssh_input.as_ref().unwrap().1;
    assert_eq!(selected, 40);
    assert_eq!(
        ssh_row_action("host", selected, &cfg.ssh_hosts, &[]),
        Some(super::SshEnter::Connect("me@host-39".into()))
    );
}

/// Whatever the picker, the wheel never selects a disabled row: it takes the
/// nearest enabled row above it in view.
#[test]
fn picker_wheel_never_selects_a_disabled_row() {
    use super::{PickerList, PickerPointer, apply_picker_pointer, picker_context_menu};
    let row = |label: String, enabled: bool| ContextMenuRow {
        toggle: None,
        swatches: None,
        matched_chars: None,
        label,
        separator: false,
        enabled,
        hint: String::new(),
    };
    let mut rows: Vec<ContextMenuRow> = (0..40).map(|i| row(format!("row-{i}"), true)).collect();
    rows.push(row("disabled".into(), false));
    let list = PickerList {
        rows,
        selected: 0,
        footer: None,
    };
    let menu = picker_context_menu(list, (320.0, 160.0), (8.0, 16.0), 0.0).unwrap();
    let mut ws = ssh_launcher_with("");
    ws.cursor.x = f64::from(menu.anchor.0 + menu.panel_w_clamped * 0.5);
    ws.cursor.y = f64::from(menu.anchor.1 + menu.panel_h_clamped * 0.5);
    assert!(apply_picker_pointer(&mut ws, &menu, 16.0, PickerPointer::Wheel(100)));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 39);
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
        0.0,
    )
    .unwrap();
    assert_eq!(menu.rows[0].label, "Connect to \"a\"");
    let row_h = kettle_render::menu::row_height(16.0);
    let (inset, _) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
    ws.cursor.x = f64::from(menu.anchor.0 + 10.0);
    // Press the first host row, then the Connect row above it.
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 1.5);
    assert!(apply_picker_pointer(&mut ws, &menu, 16.0, PickerPointer::Press));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 1);
    ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 0.5);
    assert!(apply_picker_pointer(&mut ws, &menu, 16.0, PickerPointer::Press));
    assert_eq!(ws.ssh_input.as_ref().unwrap().1, 0);
    // A press activates through the same Enter as the keyboard.
    assert_eq!(
        super::take_ssh_enter_target(&mut ws, &cfg.ssh_hosts).as_deref(),
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
