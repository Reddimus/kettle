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
    );
    let menu = picker_context_menu(list, (320.0, 160.0), (8.0, 16.0))
        .expect("a 320x160 surface fits a picker row above its input lane");

    assert!(
        menu.rows.len() > 20,
        "the projection must retain one row per ranked command instead of flattening matches"
    );
    assert_eq!(menu.highlight, 20);
    assert!(menu.scroll_offset > 0);
    let visible_rows =
        (menu.panel_h_clamped / (16.0 + kettle_render::menu::ROW_PAD)).floor() as usize;
    assert!(
        (menu.scroll_offset..menu.scroll_offset + visible_rows).contains(&menu.highlight),
        "the command Enter will run must remain inside the visible row window"
    );
    assert!(
        menu.rows.iter().any(|row| !row.hint.is_empty()),
        "command rows must carry live keybind hints in the menu hint column"
    );
    assert!(
        menu.anchor.1 + menu.panel_h_clamped <= 160.0 - (16.0 + 10.0),
        "the vertical list must stay above the bottom input lane"
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

