use super::{
    color, display_width, fit_pane_titlebar_title, fit_single_line_label, fit_tab_path,
    fit_tab_segment_title, fit_tab_title, middle_ellipsis, overlay_label_cols, production_source,
};

/// Every bar that asks for input keeps the theme foreground readable on
/// its fill, and its accent edge visible against it, in every bundled
/// theme and for every accent slot a bar uses.
/// The update chip's text reads at 4.5:1 and its edge at 3:1 against
/// its opaque fill, in every bundled theme and for both tones.
#[test]
fn update_chip_is_readable_in_every_bundled_theme() {
    let mut checked = 0;
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        for tone in [super::UpdateChipTone::Notice, super::UpdateChipTone::Failed] {
            let (fill, edge) = super::update_chip_colors(&theme, tone);
            let text = color::contrast_ratio(theme.foreground, fill);
            assert!(
                text + 1e-6 >= 4.5,
                "{name:?} {tone:?}: chip text is {text:.2}:1"
            );
            let edge_ratio = color::contrast_ratio(edge, fill);
            assert!(
                edge_ratio + 1e-6 >= 3.0,
                "{name:?} {tone:?}: chip edge is {edge_ratio:.2}:1"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 2 * 500,
        "only {checked} theme and tone pairs checked"
    );
}

#[test]
fn input_bars_are_readable_in_every_bundled_theme() {
    let mut checked = 0;
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        for slot in [2, 3, 4, 5, 6] {
            let (fill, edge) = super::input_bar_colors(&theme, theme.palette[slot]);
            let text = color::contrast_ratio(theme.foreground, fill);
            assert!(
                text + 1e-6 >= 4.5,
                "input bar text in {name:?} with accent palette[{slot}] is {text:.2}:1"
            );
            let edge_ratio = color::contrast_ratio(edge, fill);
            assert!(
                edge_ratio + 1e-6 >= 3.0,
                "input bar edge in {name:?} with accent palette[{slot}] is {edge_ratio:.2}:1"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 5 * 500,
        "the bundled theme set must be checked: {checked}"
    );
}

/// The reported case: Dracula's foreground on its yellow, the title
/// editor's old fill, is unreadable; on the shared fill it is not.
#[test]
fn the_title_editor_reads_in_dracula() {
    let theme = kettle_config::Theme::by_name("Dracula");
    assert!(color::contrast_ratio(theme.foreground, theme.palette[3]) < 2.0);
    let (fill, _) = super::input_bar_colors(&theme, theme.palette[3]);
    assert!(color::contrast_ratio(theme.foreground, fill) + 1e-6 >= 4.5);
}

/// Chrome must use its contrast helpers rather than raw accent fills.
#[test]
fn the_renderer_actually_consumes_the_contrast_and_bell_helpers() {
    let src = production_source();
    for (bar, wiring) in [
        (
            "palette and theme picker",
            "push_input_bar(&mut quads, search_rect, theme, theme.palette[5]);",
        ),
        (
            "layout picker",
            "push_input_bar(&mut quads, search_rect, theme, theme.palette[6]);",
        ),
        (
            "ssh picker",
            "push_input_bar(&mut quads, search_rect, theme, theme.palette[4]);",
        ),
        ("update chip", "push_update_chip(&mut quads, chip, theme);"),
        (
            "title editor",
            "push_input_bar(&mut quads, edit.rect, theme, theme.palette[3]);",
        ),
    ] {
        assert!(
            src.contains(wiring),
            "the {bar} must paint through push_input_bar"
        );
    }
    assert_eq!(
        src.matches("push_input_bar(&mut quads, search_rect, theme, theme.palette[5]);")
            .count(),
        2,
        "the command palette and the theme picker both paint the shared bar"
    );
    for slot in 2..=6 {
        assert!(
            !src.contains(&format!("theme.palette[{slot}], 0.96")),
            "no input bar may go back to a translucent raw palette[{slot}] fill"
        );
    }

    assert!(
        src.contains("let alpha = bell_flash_alpha(theme, cfg, pv.bell_flash);")
            && src.contains("quads.push(rect(rx, ry, rw, rh, theme.foreground, alpha));"),
        "the visual bell must wash the ringing pane through the shared perceptual helper"
    );
    assert!(
        src.contains("cfg.bell_flash_intensity.clamp(0.0, 1.0) * ramp.clamp(0.0, 1.0)")
            && src
                .contains("color::perceptual_wash_alpha(theme.background, theme.foreground, step)"),
        "the visual bell peak must come from the config key as an L* step, not an alpha literal"
    );
}

/// The confirmation background is menu chrome, which renders after the
/// ordinary text pass. Its label must travel through the matching menu
/// text pass or the opaque bar covers the prompt and button labels.
#[test]
fn confirm_text_renders_above_its_menu_chrome_background() {
    let src = production_source();
    let scene = src
        .split_once("fn encode_scene_pass(")
        .unwrap()
        .1
        .split("fn create_screenshot_target")
        .next()
        .unwrap();
    let menu = scene.find("self.menu_text_renderer").unwrap();
    let surfaces = scene.find("self.confirmation_surfaces.draw").unwrap();
    let text = scene.find("self.confirmation_text_renderer").unwrap();
    assert!(
        menu < surfaces && surfaces < text,
        "final modal geometry and text must cover retained Settings text"
    );
}

#[test]
fn single_line_overlay_labels_fit_without_wrapping() {
    assert_eq!(overlay_label_cols(800.0, 8.0), 99);
    assert_eq!(overlay_label_cols(0.0, 8.0), 0);
    assert_eq!(overlay_label_cols(800.0, 0.0), 0);

    let fitted = fit_single_line_label("  ⌘ query_ ▸ 中文 command and controls", 20);
    assert_eq!(display_width(&fitted), 20);
    assert!(fitted.starts_with("  ⌘ query_"));
    assert!(fitted.ends_with('…'));
    assert_eq!(fit_single_line_label("abc", 3), "abc");
    assert_eq!(fit_single_line_label("abc", 1), "…");
    assert_eq!(fit_single_line_label("abc", 0), "");
}

#[test]
fn middle_ellipsis_fits_and_keeps_both_ends() {
    // Fits unchanged.
    assert_eq!(middle_ellipsis("hello", 10), "hello");
    assert_eq!(middle_ellipsis("hello", 5), "hello");
    // A path keeps the LEAF (program name) whole + the drive root.
    let p = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
    let cut = middle_ellipsis(p, 16);
    assert!(cut.contains('…'), "must ellipsize: {cut}");
    assert!(cut.ends_with("pwsh.exe"), "leaf must survive: {cut}");
    assert!(cut.starts_with("C:\\"), "drive root must survive: {cut}");
    assert!(display_width(&cut) <= 16, "must fit the budget: {cut}");
}

#[test]
fn middle_ellipsis_falls_back_to_symmetric_for_non_paths() {
    let s = "abcdefghijklmnopqrstuvwxyz"; // 26 cols, no separators
    let cut = middle_ellipsis(s, 11);
    assert!(cut.contains('…'));
    assert!(cut.starts_with('a'), "front kept: {cut}");
    assert!(cut.ends_with('z'), "back kept: {cut}");
    assert!(display_width(&cut) <= 11);
}

#[test]
fn middle_ellipsis_respects_display_columns_not_chars() {
    // CJK: each char is 2 cells. "中文中文" = 8 cells; budget 8 fits.
    assert_eq!(middle_ellipsis("中文中文", 8), "中文中文");
    // Overflow never exceeds the column budget.
    for n in 0..=8 {
        assert!(display_width(&middle_ellipsis("中文中文路径", n)) <= n);
    }
    assert_eq!(middle_ellipsis("a", 0), "");
    assert_eq!(middle_ellipsis("abc", 1), "…");
}

#[test]
fn tab_title_keeps_full_path_when_budget_fits() {
    let title = "~/Repos/SPT-1/flight-event-line-server-go";
    assert_eq!(fit_tab_title(title, display_width(title)), title);
    assert_eq!(fit_tab_title(title, display_width(title) + 10), title);
}

#[test]
fn tab_title_preserves_posix_path_tail_with_ascii_ellipsis() {
    let title = "~/Repos/SPT-1/flight-event-line-server-go";
    let cut = fit_tab_title(title, 30);
    assert_eq!(cut, "...flight-event-line-server-go");
    assert!(cut.ends_with("flight-event-line-server-go"));
    assert!(cut.starts_with("..."));
    assert!(
        !cut.contains('…'),
        "tab path truncation uses ASCII dots: {cut}"
    );
    assert!(display_width(&cut) <= 30);
}

#[test]
fn tab_title_preserves_windows_path_tail_with_ascii_ellipsis() {
    let title = r"C:\src\proj\flight-event-line-server-go";
    let cut = fit_tab_title(title, 30);
    assert_eq!(cut, r"...flight-event-line-server-go");
    assert!(cut.ends_with(r"flight-event-line-server-go"));
    assert!(cut.starts_with("..."));
    assert!(display_width(&cut) <= 30);
}

#[test]
fn tab_title_uses_tail_only_for_tiny_path_budget() {
    let title = "~/Repos/SPT-1/flight-event-line-server-go";
    assert_eq!(fit_tab_title(title, 2), "go");
    assert_eq!(fit_tab_title(title, 1), "o");
    assert_eq!(fit_tab_title(title, 0), "");
}

#[test]
fn tab_title_keeps_middle_ellipsis_for_non_paths() {
    let title = "abcdefghijklmnopqrstuvwxyz";
    let cut = fit_tab_title(title, 11);
    assert!(cut.contains('…'));
    assert!(cut.starts_with('a'), "front kept: {cut}");
    assert!(cut.ends_with('z'), "back kept: {cut}");
    assert!(display_width(&cut) <= 11);
}

#[test]
fn fit_tab_path_tiers_full_then_leaf_then_tail() {
    let path = "~/Repos/kettle/crates/kettle-ui";
    // Tier 1: the whole path when it fits.
    assert_eq!(fit_tab_path(path, display_width(path)), path);
    assert_eq!(fit_tab_path(path, display_width(path) + 5), path);
    // Tier 2: the leaf dir name alone when the full path doesn't fit.
    assert_eq!(fit_tab_path(path, 12), "kettle-ui");
    // Tier 3: the tail of the leaf with a leading ellipsis when even the leaf
    // overflows.
    let cut = fit_tab_path(path, 6);
    assert!(cut.starts_with('…'), "tail marked: {cut}");
    assert!(cut.ends_with("ui"), "tail kept: {cut}");
    assert!(display_width(&cut) <= 6);
    // Degenerate widths.
    assert_eq!(fit_tab_path(path, 1), "…");
    assert_eq!(fit_tab_path(path, 0), "");
    // Backslash separators tier the same way.
    assert_eq!(fit_tab_path("~\\Repos\\kettle", 8), "kettle");
}

#[test]
fn fit_tab_path_keeps_long_leaf_when_it_fits() {
    let path = "~/Repos/SPI-1/flight-event-line-server-go";
    let leaf = "flight-event-line-server-go";
    assert_eq!(fit_tab_path(path, display_width(leaf)), leaf);
    assert!(
        !fit_tab_path(path, display_width(leaf)).starts_with('…'),
        "leaf fits, so no leading ellipsis is needed"
    );
}

#[test]
fn fit_tab_segment_title_uses_render_budget_and_path_tiers() {
    let fmt = "{n}: {title}";
    let path = "~/Repos/SPI-1/platform";
    let leaf = "platform";
    let cell = 10.0;
    let lane_for_cols = |cols: usize| cols as f32 * cell;
    let fixed = display_width("1: ");

    assert_eq!(
        fit_tab_segment_title(
            leaf,
            Some(path),
            0,
            fmt,
            lane_for_cols(fixed + display_width(path)),
            cell
        ),
        path
    );
    assert_eq!(
        fit_tab_segment_title(
            leaf,
            Some(path),
            0,
            fmt,
            lane_for_cols(fixed + display_width(leaf)),
            cell
        ),
        leaf
    );
    assert_eq!(
        fit_tab_segment_title(leaf, Some(path), 0, fmt, lane_for_cols(fixed + 5), cell),
        "…form"
    );
}

#[test]
fn pane_title_sheds_size_then_group_then_ellipsizes() {
    let long = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
    // Wide budget: everything shows, including size + group.
    let wide = fit_pane_titlebar_title(Some("fleet"), "", long, None, Some("120x60"), None, 120);
    assert!(wide.contains("[fleet]") && wide.contains("120x60") && wide.contains(long));
    // Medium: size text is dropped first, group + full title still fit.
    let med_budget = display_width("  [fleet]  ") + long.chars().count() + 1;
    let med = fit_pane_titlebar_title(
        Some("fleet"),
        "",
        long,
        None,
        Some("120x60"),
        None,
        med_budget,
    );
    assert!(med.contains(long), "title intact: {med}");
    assert!(!med.contains("120x60"), "size shed first: {med}");
    // Narrow: group dropped too, and the title middle-ellipsized to the leaf.
    let narrow = fit_pane_titlebar_title(Some("fleet"), "", long, None, Some("120x60"), None, 18);
    assert!(!narrow.contains("[fleet]"), "group shed: {narrow}");
    assert!(narrow.ends_with("pwsh.exe"), "leaf survives: {narrow}");
    assert!(display_width(&narrow) <= 18, "fits budget: {narrow}");
}

#[test]
fn pane_title_keeps_the_bell_through_shedding() {
    let long = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell\\pwsh.exe";
    let narrow = fit_pane_titlebar_title(
        Some("fleet"),
        "",
        long,
        None,
        Some("120x60"),
        Some("\u{1F514}"),
        20,
    );
    assert!(narrow.contains('\u{1F514}'), "bell must survive: {narrow}");
}

#[test]
fn pane_title_uses_path_tiers_after_metadata_sheds() {
    let raw = "..ine-server-go";
    let path = "~/Repos/SPI-1/flight-event-line-server-go";
    let leaf = "flight-event-line-server-go";
    let wide_budget = display_width("  [RO] ") + display_width(path);
    let wide = fit_pane_titlebar_title(
        None,
        "[RO] ",
        raw,
        Some(path),
        Some("131x30"),
        None,
        wide_budget,
    );
    assert_eq!(wide, format!("  [RO] {path}"));

    let leaf_budget = display_width("  [RO] ") + display_width(leaf);
    let leaf_fit = fit_pane_titlebar_title(
        None,
        "[RO] ",
        raw,
        Some(path),
        Some("131x30"),
        None,
        leaf_budget,
    );
    assert_eq!(leaf_fit, format!("  [RO] {leaf}"));

    let tail = fit_pane_titlebar_title(None, "", raw, Some(path), None, None, 12);
    assert_eq!(tail, "  …server-go");
}

#[test]
fn confirmation_panel_text_and_outline_are_readable_in_every_bundled_theme() {
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        assert!(
            color::contrast_ratio(super::confirm_dialog_text_color(&theme), theme.background)
                + 1e-6
                >= 4.5,
            "{name}"
        );
        assert!(
            color::contrast_ratio(super::confirm_dialog_border_color(&theme), theme.background)
                + 1e-6
                >= 1.5,
            "{name}"
        );
    }
}
