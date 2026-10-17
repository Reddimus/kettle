use super::{
    CONFIRM_BAR_MIN_CONTRAST, ConfirmDialogButton, color, compose_confirm_bar_label,
    confirm_bar_buttons, confirm_bar_spans, confirm_bar_text_color, display_width,
    fit_pane_titlebar_title, fit_single_line_label, fit_tab_path, fit_tab_segment_title,
    fit_tab_title, middle_ellipsis, overlay_label_cols, production_source,
};

fn button(label: &str, destructive: bool) -> ConfirmDialogButton {
    ConfirmDialogButton {
        label: label.to_string(),
        destructive,
    }
}

#[test]
fn confirmation_geometry_keeps_fractional_chrome_metrics() {
    let geometry = super::ConfirmBarGeometry::new((800.0, 600.0), (8.25, 16.5));
    assert_eq!(geometry.columns, 95);
    assert_eq!(
        geometry.button_rects(&[10, 9]),
        [(610.5, 573.5, 82.5, 26.5), (709.5, 573.5, 74.25, 26.5)]
    );
    assert!(geometry.button_rects(&[100]).is_empty());
}

/// Destructive buttons (Close, Delete) draw their label in bold. The whole
/// bar is already `palette[1]`, so a red accent on one button could not
/// show. Only the label is bold; the brackets and focus marker stay
/// regular, and every byte of the bar lands in exactly one span.
#[test]
fn confirm_bar_bolds_only_destructive_labels() {
    let buttons = [button("Cancel", false), button("Close", true)];
    let (row, bold) = confirm_bar_buttons(&buttons, 0);
    assert_eq!(row, "[▶ Cancel]  [  Close]");

    let label = compose_confirm_bar_label("  ⚠ Close this pane?", "  Tab", &row, 60);
    let spans = confirm_bar_spans(&label, &row, &bold);
    assert_eq!(
        spans.iter().map(|(text, _)| *text).collect::<String>(),
        label
    );
    let bold_text: Vec<&str> = spans
        .iter()
        .filter(|(_, bold)| *bold)
        .map(|(text, _)| *text)
        .collect();
    assert_eq!(bold_text, ["Close"]);
}

/// A bar too narrow for its buttons paints nothing, and a dialog without a
/// destructive button stays regular weight.
#[test]
fn confirm_bar_spans_handle_narrow_bars_and_safe_dialogs() {
    let (row, bold) = confirm_bar_buttons(&[button("Delete", true)], 0);
    let empty = compose_confirm_bar_label("  ⚠ Delete it?", "", &row, 3);
    assert!(confirm_bar_spans(&empty, &row, &bold).is_empty());

    let (row, bold) = confirm_bar_buttons(&[button("OK", false)], 0);
    let label = compose_confirm_bar_label("  ⚠ Apply?", "", &row, 40);
    let spans = confirm_bar_spans(&label, &row, &bold);
    assert!(!spans.is_empty());
    assert!(spans.iter().all(|(_, bold)| !bold));

    // No buttons at all still shows the prompt.
    let (row, bold) = confirm_bar_buttons(&[], 0);
    let label = compose_confirm_bar_label("  ⚠ Working...", "", &row, 40);
    assert_eq!(
        confirm_bar_spans(&label, &row, &bold),
        [(label.as_str(), false)]
    );
}

/// The span helpers prove nothing unless the bar is drawn from them.
#[test]
fn the_confirm_bar_draws_destructive_labels_bold() {
    let src = production_source();
    assert!(
        src.contains(
            "let (buttons_label, bold_labels) = confirm_bar_buttons(&dlg.buttons, dlg.focus_idx);"
        ),
        "the confirm bar must build its button row with confirm_bar_buttons"
    );
    assert!(
        src.contains("let spans = confirm_bar_spans(&label, &buttons_label, &bold_labels);")
            && src.contains("let bold = regular.clone().weight(Weight::BOLD);"),
        "the confirm bar must draw its spans, with destructive labels bold"
    );
}

/// A destructive confirmation has to be readable in EVERY bundled theme.
///
/// The bar paints `palette[1]`, but the theme's ordinary foreground is
/// picked to contrast with the theme BACKGROUND. On the shipped TokyoNight
/// Night default, that foreground on the bar is `#c0caf5` on `#f7768e`,
/// about 1.6:1. It would hide the prompt, its buttons, and its focus
/// marker, so the question could not be answered and the window would not
/// close. The failure is per-theme, so iterate the whole bundled set
/// instead of spot-checking one.
#[test]
fn confirm_bar_text_is_readable_in_every_bundled_theme() {
    let mut worst: Option<(&str, f64)> = None;
    for name in kettle_config::Theme::list() {
        let theme = kettle_config::Theme::by_name(name);
        let fg = confirm_bar_text_color(&theme);
        let ratio = color::contrast_ratio(fg, theme.palette[1]);
        assert!(
            ratio >= CONFIRM_BAR_MIN_CONTRAST,
            "confirm bar text is unreadable in theme {name:?}: {ratio:.2}:1 \
                 (fg {fg:?} on palette[1] {:?})",
            theme.palette[1]
        );
        if worst.is_none_or(|(_, w)| ratio < w) {
            worst = Some((name, ratio));
        }
    }
    let (name, ratio) = worst.expect("the bundled theme set must not be empty");
    assert!(
        ratio >= CONFIRM_BAR_MIN_CONTRAST,
        "worst bundled theme {name:?} at {ratio:.2}:1"
    );
}

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

/// On the exact shipped default, not whatever `Theme::default()` happens
/// to be, the raw theme foreground fails on the confirm bar and the
/// helper's output passes.
#[test]
fn tokyonight_night_confirm_bar_was_unreadable_before_the_lift() {
    let theme = kettle_config::Theme::by_name("TokyoNight Night");
    let raw = color::contrast_ratio(theme.foreground, theme.palette[1]);
    assert!(
        raw < CONFIRM_BAR_MIN_CONTRAST,
        "expected the shipped default's raw foreground to fail on palette[1], got {raw:.2}:1"
    );
    let lifted = color::contrast_ratio(confirm_bar_text_color(&theme), theme.palette[1]);
    assert!(
        lifted >= CONFIRM_BAR_MIN_CONTRAST,
        "confirm_bar_text_color did not reach AA on the shipped default: {lifted:.2}:1"
    );
}

/// Wiring guards. Value tests prove `confirm_bar_text_color` and
/// `bell-flash-intensity` compute the right values; neither proves the
/// renderer USES them. Reverting the bar's text color to `theme.foreground`
/// or hard-coding the bell peak back to a literal would leave every value
/// test green.
///
/// The bar's opacity is pinned here too, because the AA guarantee is
/// computed against opaque `palette[1]`: a translucent bar composites over
/// live terminal content and the real ratio drifts with the scrollback.
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
        src.contains("let bar_fg = confirm_bar_text_color(theme);"),
        "the confirm bar must take its text color from confirm_bar_text_color"
    );
    assert!(
        src.contains("search_text_color = Some(GColor::rgb(bar_fg.r, bar_fg.g, bar_fg.b));"),
        "the computed bar color must reach the shared bottom-bar text area"
    );
    assert!(
        src.contains("search_text_color.unwrap_or(GColor::rgb(fg.r, fg.g, fg.b))"),
        "the text area must prefer the per-overlay color over the theme foreground"
    );
    assert!(
        src.contains("sh - bar_h, sw, bar_h, theme.palette[1], 1.0)"),
        "the confirm bar must be painted opaque; the AA guarantee is \
             computed against palette[1] itself"
    );
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
    assert!(
        src.contains("search_text_on_menu = true;"),
        "the confirmation arm must select the topmost text pass"
    );
    assert!(
        src.contains("if search_text_on_menu {\n                menu_areas.push(area);"),
        "confirmation text must be queued after menu chrome, not under it"
    );
}

/// The confirm bar must never clip its own button row.
///
/// Compose it to the budget it is fitted to, `overlay_label_cols(sw, cw)`
/// = `floor(sw/cw) - 1`. Composing to `floor(sw/cw)` overflows by one
/// column at every window size, so `fit_single_line_label` drops two
/// columns for an ellipsis, the rightmost button renders as `[  Clos…`,
/// and the click target from `confirm_dialog_button_hit` extends past the
/// last painted glyph.
#[test]
fn confirm_bar_never_clips_its_button_row() {
    let buttons = "[▶ Cancel]  [  Close]";
    // Real geometries: 800/8.0, 1512/7.8 (this laptop), 1920/9.0, 640/8.0.
    for (sw, cw) in [
        (800.0f32, 8.0f32),
        (1512.0, 7.8),
        (1920.0, 9.0),
        (640.0, 8.0),
    ] {
        let cols = overlay_label_cols(sw, cw);
        let label = compose_confirm_bar_label(
            "  ⚠ Close this pane?",
            "  Tab/←→ · Enter · Esc",
            buttons,
            cols,
        );
        assert!(
            label.ends_with(buttons),
            "confirm bar clipped its buttons at {sw}x{cw} (cols={cols}): {label:?}"
        );
        assert!(
            !label.contains('…'),
            "confirm bar was ellipsised at {sw}x{cw}: {label:?}"
        );
        assert!(
            display_width(&label) <= cols,
            "confirm bar overflowed its budget at {sw}x{cw}: {} > {cols}",
            display_width(&label)
        );
    }
}

/// A genuinely narrow window must keep the interactive row intact or paint
/// none of it. A clipped button row would disagree with the App hit-test.
#[test]
fn confirm_bar_still_fits_when_the_window_cannot_hold_the_buttons() {
    for buttons in ["[▶ Cancel]  [  Close]", "[▶ OK]"] {
        let buttons_cols = display_width(buttons);
        for cols in 0..buttons_cols {
            assert_eq!(
                compose_confirm_bar_label("  ⚠ Close this pane?", "  Tab", buttons, cols),
                "",
                "a {cols}-column bar must not paint a partial {buttons_cols}-column button row"
            );
        }
        for cols in buttons_cols..=buttons_cols + 2 {
            let label = compose_confirm_bar_label("  ⚠ Close this pane?", "  Tab", buttons, cols);
            assert!(
                label.ends_with(buttons),
                "the complete button row must win at cols={cols}: {label:?}"
            );
            assert_eq!(
                display_width(&label),
                cols,
                "the intact button row must remain right-aligned"
            );
        }
    }
}

#[test]
fn confirm_bar_uses_display_columns_for_wide_button_labels() {
    let prompt = "P";
    let buttons = "[▶ Cancel]  [  界]";
    let cols = display_width(prompt) + 2 + display_width(buttons);
    let label = compose_confirm_bar_label(prompt, "", buttons, cols);

    assert_eq!(display_width(&label), cols);
    assert!(
        label.ends_with(buttons),
        "a wide label must not make the fitter clip the button row: {label:?}"
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
