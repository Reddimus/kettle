use super::*;

/// Terminator parity (`layoutlauncher.py`): rank saved
/// layouts against the user-typed query. Empty query returns
/// every layout in original (alphabetical) order; non-empty query
/// keeps only entries whose lower-cased name contains every
/// lower-cased query token. Same shape as
/// `kettle_config::palette::rank` but layouts have only a name
/// field (no description), so the inner predicate is simpler.
/// Pure and separate from `layout_picker_key` so a drift guard
/// can exercise it without touching App state.
pub(crate) fn rank_layouts(q: &str, layouts: &[String]) -> Vec<usize> {
    let q = q.trim().to_ascii_lowercase();
    if q.is_empty() {
        return (0..layouts.len()).collect();
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    layouts
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            let lower = name.to_ascii_lowercase();
            tokens.iter().all(|t| lower.contains(t))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Indices into configured SSH hosts, best match first, with config order breaking ties.
pub(super) fn rank_ssh_hosts(query: &str, hosts: &[(String, String)]) -> Vec<usize> {
    let query = query.trim();
    let mut ranked: Vec<(usize, i32)> = hosts
        .iter()
        .enumerate()
        .filter_map(|(index, (name, _))| {
            kettle_config::fuzzy::score(query, name).map(|score| (index, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.into_iter().map(|(index, _)| index).collect()
}

pub(super) fn selected_ssh_target(
    query: &str,
    selected: usize,
    hosts: &[(String, String)],
) -> Option<String> {
    let ranked = rank_ssh_hosts(query, hosts);
    let selected = selected.min(ranked.len().saturating_sub(1));
    ranked
        .get(selected)
        .map(|&index| hosts[index].1.clone())
        .or_else(|| {
            let query = query.trim();
            (!query.is_empty()).then(|| query.to_string())
        })
}

pub(super) struct PickerList {
    pub(super) rows: Vec<ContextMenuRow>,
    pub(super) selected: usize,
}

/// A picker's key hint: each key followed by what it does. The keys are the
/// same in every language.
pub(super) fn picker_hint(
    tr: &kettle_i18n::Translator,
    hints: &[(&str, kettle_i18n::Text)],
) -> String {
    let parts: Vec<String> = hints
        .iter()
        .map(|(keys, what)| format!("{keys} {}", tr.text(*what)))
        .collect();
    format!("({})", parts.join(" · "))
}

const PICKER_MAX_VISIBLE_ROWS: usize = 8;

/// Profile discovery is shared with `--list-profiles`; names remain config data.
pub(crate) fn profile_launch_entries(
    names: &[String],
    tr: &kettle_i18n::Translator,
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    use kettle_config::palette::PaletteLabel;
    if names.is_empty() {
        return vec![(
            PaletteLabel::Text(kettle_i18n::Text::MenuNoProfiles),
            Action::OpenProfilesDirectory,
        )];
    }
    let en = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    names
        .iter()
        .map(|name| {
            (
                PaletteLabel::Dynamic {
                    shown: tr.palette_launch_profile(name),
                    english: en.palette_launch_profile(name),
                },
                Action::LaunchProfile(name.clone()),
            )
        })
        .collect()
}

/// Commands and the profiles captured when the palette opened, followed by
/// release notes and Dismiss while an update notice is active.
pub(super) fn palette_entries(
    notice: Option<&crate::update_notice::Notice>,
    tr: &kettle_i18n::Translator,
    profiles: &[String],
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    use kettle_config::palette::PaletteLabel;
    let mut entries: Vec<_> = kettle_config::palette::commands()
        .into_iter()
        .map(|(label, action)| (PaletteLabel::Text(label), action))
        .collect();
    entries.extend(profile_launch_entries(profiles, tr));
    if let Some(notice) = notice {
        let english = kettle_i18n::Translator::new(kettle_i18n::Language::En);
        entries.push((
            PaletteLabel::Dynamic {
                shown: tr.palette_open_update(&notice.tag),
                english: english.palette_open_update(&notice.tag),
            },
            Action::OpenUpdate,
        ));
        entries.push((
            PaletteLabel::Text(kettle_i18n::Text::PaletteDismissUpdate),
            Action::DismissUpdate,
        ));
    }
    entries
}

pub(super) fn command_picker_list(
    query: &str,
    selected: usize,
    bindings: &Bindings,
    tr: &kettle_i18n::Translator,
    notice: Option<&crate::update_notice::Notice>,
    profiles: &[String],
) -> PickerList {
    let commands = palette_entries(notice, tr, profiles);
    let ranked = kettle_config::palette::rank_entries(query, &commands, tr);
    let rows = ranked
        .iter()
        .map(|&index| {
            let (label, action) = &commands[index];
            ContextMenuRow {
                swatches: None,
                matched_chars: kettle_config::palette::matched_char_positions(
                    query,
                    label.shown(tr),
                )
                .map(|positions| {
                    positions
                        .into_iter()
                        .map(|index| index..index + 1)
                        .collect()
                }),
                label: label.shown(tr).to_string(),
                separator: false,
                enabled: true,
                hint: kettle_config::keybinds::ui_hint_label(bindings, action).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        PickerList {
            rows: vec![ContextMenuRow {
                swatches: None,
                matched_chars: None,
                label: tr
                    .text(kettle_i18n::Text::PickerNoMatchingCommand)
                    .to_string(),
                separator: false,
                enabled: false,
                hint: String::new(),
            }],
            selected: 0,
        }
    } else {
        PickerList {
            selected: selected.min(rows.len() - 1),
            rows,
        }
    }
}

pub(super) fn layout_picker_list(
    query: &str,
    selected: usize,
    layouts: &[String],
    tr: &kettle_i18n::Translator,
) -> PickerList {
    let ranked = rank_layouts(query, layouts);
    let rows = ranked
        .iter()
        .map(|&index| ContextMenuRow {
            swatches: None,
            matched_chars: None,
            label: layouts[index].clone(),
            separator: false,
            enabled: true,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        let label = if layouts.is_empty() {
            kettle_i18n::Text::PickerNoSavedLayouts
        } else {
            kettle_i18n::Text::PickerNoMatchingLayout
        };
        PickerList {
            rows: vec![ContextMenuRow {
                swatches: None,
                matched_chars: None,
                label: tr.text(label).to_string(),
                separator: false,
                enabled: false,
                hint: if layouts.is_empty() {
                    tr.text(kettle_i18n::Text::PickerCreateLayoutHint)
                        .to_string()
                } else {
                    String::new()
                },
            }],
            selected: 0,
        }
    } else {
        PickerList {
            selected: selected.min(rows.len() - 1),
            rows,
        }
    }
}

/// The themes the theme picker lists for `query`, each with whether it is
/// dark. An empty query lists every theme of `opened_on`'s appearance in look
/// order, then the other appearance, so the arrows step between similar
/// palettes and reach the other appearance only at the end. A query ranks by
/// fuzzy match, ties keeping that order, except that a theme named exactly by
/// the query comes first: `dracula` scores the same against "Dracula+".
pub(super) fn theme_picker_matches(
    query: &str,
    opened_on: &str,
    tr: &kettle_i18n::Translator,
) -> Vec<(&'static str, bool)> {
    let opened_dark = kettle_config::Theme::by_name(opened_on).is_dark();
    let order = [opened_dark, !opened_dark].into_iter().flat_map(|dark| {
        kettle_config::Theme::look_order(dark)
            .iter()
            .map(move |name| (*name, dark))
    });
    let query = query.trim();
    if query.is_empty() {
        return order.collect();
    }
    let mut ranked: Vec<(bool, i32, usize, (&'static str, bool))> = order
        .enumerate()
        .filter_map(|(index, theme)| {
            let label = tr.theme_name(theme.0);
            let exact = theme.0.eq_ignore_ascii_case(query) || label.eq_ignore_ascii_case(query);
            kettle_config::fuzzy::score(query, theme.0)
                .into_iter()
                .chain(kettle_config::fuzzy::score(query, label))
                .max()
                .map(|score| (exact, score, index, theme))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    ranked.into_iter().map(|(_, _, _, theme)| theme).collect()
}

pub(super) fn theme_picker_appearance_prefix(key: &str, tr: &kettle_i18n::Translator) -> String {
    use kettle_i18n::Text;
    let label = match key {
        "light-theme" => Text::PickerThemeForLightAppearance,
        "dark-theme" => Text::PickerThemeForDarkAppearance,
        _ => return kettle_render::theme_picker_prefix(tr),
    };
    format!("  ◐ {} ", tr.text(label))
}

type ThemeSwatches = [kettle_config::Rgb; kettle_render::menu::SWATCH_COUNT];

/// Picker swatches of every bundled theme the picker has listed, by name.
/// Each theme is parsed once per process: the picker rebuilds all of its rows
/// on every frame, and the list holds hundreds of themes.
fn theme_swatch_cache()
-> std::sync::MutexGuard<'static, std::collections::HashMap<&'static str, ThemeSwatches>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, ThemeSwatches>>,
    > = std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A bundled theme's picker swatches: its background and foreground, then
/// ANSI red, green, yellow, blue, magenta and cyan (palette 1 to 6). These are
/// the theme's own colours, before any colour overrides in the user's config.
pub(super) fn theme_swatches(theme: &kettle_config::Theme) -> ThemeSwatches {
    let p = theme.palette;
    [
        theme.background,
        theme.foreground,
        p[1],
        p[2],
        p[3],
        p[4],
        p[5],
        p[6],
    ]
}

/// The theme picker's rows. The theme it opened on is ticked, each row
/// says whether it is dark or light, and each shows the theme's colours.
pub(super) fn theme_picker_list(
    query: &str,
    selected: usize,
    opened_on: &str,
    tr: &kettle_i18n::Translator,
) -> PickerList {
    use kettle_i18n::Text;
    let mut swatches = theme_swatch_cache();
    let rows: Vec<ContextMenuRow> = theme_picker_matches(query, opened_on, tr)
        .into_iter()
        .map(|(name, dark)| {
            let appearance = tr.text(if dark {
                Text::PickerThemeDark
            } else {
                Text::PickerThemeLight
            });
            let tick = if name.eq_ignore_ascii_case(opened_on.trim()) {
                "✓ "
            } else {
                ""
            };
            ContextMenuRow {
                swatches: Some(
                    *swatches
                        .entry(name)
                        .or_insert_with(|| theme_swatches(&kettle_config::Theme::by_name(name))),
                ),
                matched_chars: None,
                label: tr.theme_name(name).to_string(),
                separator: false,
                enabled: true,
                hint: format!("{tick}{appearance}"),
            }
        })
        .collect();
    if rows.is_empty() {
        return PickerList {
            rows: vec![ContextMenuRow {
                swatches: None,
                matched_chars: None,
                label: tr.text(Text::PickerNoMatchingTheme).to_string(),
                separator: false,
                enabled: false,
                hint: String::new(),
            }],
            selected: 0,
        };
    }
    let selected = selected.min(rows.len() - 1);
    PickerList { rows, selected }
}

/// The theme the picker's highlighted row names, if any row matches. Clamped
/// like the painted highlight, so the preview and Enter use the row shown.
pub(super) fn theme_picker_selection(
    state: &crate::window_state::ThemePickerState,
    tr: &kettle_i18n::Translator,
) -> Option<&'static str> {
    let matches = theme_picker_matches(&state.query, &state.opened_on, tr);
    let last = matches.len().checked_sub(1)?;
    Some(matches[state.selected.min(last)].0)
}

/// A theme the window can wear: its name and colours.
type ThemeState = (String, kettle_config::Theme);

/// Open the theme picker on `live`, the theme the window wears, and keep it as
/// the baseline Esc restores. A preview still running from an earlier picker
/// is cancelled first, so the new picker opens on, ticks and restores the
/// theme from before that preview. Returns the theme to put back, if any.
pub(super) fn begin_theme_picker(ws: &mut WindowState, live: ThemeState) -> Option<ThemeState> {
    let restore = ws.theme_preview.take();
    let opened = restore.clone().unwrap_or(live);
    ws.theme_picker_input = Some(crate::window_state::ThemePickerState {
        query: Default::default(),
        selected: theme_picker_start(&opened.0),
        opened_on: opened.0.clone(),
    });
    ws.theme_preview = Some(opened);
    restore
}

pub(super) fn begin_theme_picker_session(
    ws: &mut WindowState,
    windows: &mut std::collections::BTreeMap<u64, WindowState>,
    live: ThemeState,
) -> Option<ThemeState> {
    for other in windows.values_mut() {
        if let Some(baseline) = other.theme_preview.take() {
            ws.theme_preview = Some(baseline);
        }
        if other.theme_picker_input.take().is_some() {
            end_modal_composition(other);
            if let Some(window) = &other.window {
                window.request_redraw();
            }
        }
    }
    begin_theme_picker(ws, live)
}

/// The theme the window must switch to after an event, if any, while
/// `live_name` is the one it wears. An open picker shows its selection, or its
/// baseline while nothing matches the query; the baseline stays until the
/// picker closes. A picker that closed without keeping a theme (a kept one
/// clears the baseline first) puts the baseline back.
pub(super) fn theme_preview_change(
    ws: &mut WindowState,
    cfg: &Config,
    tr: &kettle_i18n::Translator,
) -> Option<ThemeState> {
    let live_name = cfg.theme_name.as_str();
    let Some(state) = ws.theme_picker_input.as_ref() else {
        return ws.theme_preview.take();
    };
    match theme_picker_selection(state, tr) {
        Some(name) if name == live_name => None,
        Some(name) => Some((name.to_string(), cfg.theme_by_name(name))),
        None => ws
            .theme_preview
            .as_ref()
            .filter(|(name, _)| name != live_name)
            .cloned(),
    }
}

/// Escape from a picker must restore the new system side after a live appearance change.
pub(super) fn rebase_theme_preview(ws: &mut WindowState, baseline: &ThemeState) {
    if ws.theme_preview.is_some() {
        ws.theme_preview = Some(baseline.clone());
    }
}

pub(super) fn cancel_theme_preview(ws: &mut WindowState) -> Option<(String, kettle_config::Theme)> {
    ws.theme_picker_input = None;
    ws.theme_preview.take()
}

/// End an input-method composition when the modal that owned it closes (see
/// `remember_closing_key`). A late Commit no longer matches the focus
/// generation, so it is dropped instead of typed into whatever the modal
/// covered.
pub(super) fn end_modal_composition(ws: &mut WindowState) {
    ws.ime_focus_generation = ws.ime_focus_generation.wrapping_add(1);
    if let Some(session) = ws.ime_preedit_owner.as_mut() {
        ws.ime_preedit = None;
        session.ended.get_or_insert(ws.ime_focus_generation);
    }
}

/// Where the theme picker opens: on the current theme, so the first arrow
/// press steps to its nearest look.
pub(super) fn theme_picker_start(current: &str) -> usize {
    theme_picker_matches("", current, &kettle_i18n::Translator::default())
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case(current.trim()))
        .unwrap_or(0)
}

pub(super) fn ssh_picker_list(
    query: &str,
    selected: usize,
    hosts: &[(String, String)],
    tr: &kettle_i18n::Translator,
) -> PickerList {
    let ranked = rank_ssh_hosts(query, hosts);
    let rows = ranked
        .iter()
        .map(|&index| ContextMenuRow {
            swatches: None,
            matched_chars: None,
            label: hosts[index].0.clone(),
            separator: false,
            enabled: true,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        PickerList {
            rows: vec![ContextMenuRow {
                swatches: None,
                matched_chars: None,
                label: tr
                    .text(if hosts.is_empty() {
                        kettle_i18n::Text::PickerNoConfiguredHosts
                    } else {
                        kettle_i18n::Text::PickerNoMatchingHost
                    })
                    .to_string(),
                separator: false,
                enabled: false,
                hint: tr.text(kettle_i18n::Text::PickerTypedTarget).to_string(),
            }],
            selected: 0,
        }
    } else {
        PickerList {
            selected: selected.min(rows.len() - 1),
            rows,
        }
    }
}

/// Keep the selected picker row inside the visible suffix. Picker rows have no
/// separators, so this is the context-menu scroll rule reduced to equal-height
/// rows and can be recomputed from selection on each frame.
pub(super) fn picker_scroll_offset(
    row_count: usize,
    selected: usize,
    panel_height: f32,
    row_height: f32,
) -> usize {
    if row_count == 0
        || !panel_height.is_finite()
        || !row_height.is_finite()
        || panel_height <= 0.0
        || row_height <= 0.0
    {
        return 0;
    }
    let visible = ((panel_height / row_height).floor() as usize)
        .max(1)
        .min(row_count);
    let selected = selected.min(row_count - 1);
    selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(row_count - visible)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PickerGeometry {
    pub(super) header: (f32, f32, f32, f32),
    pub(super) rows: (f32, f32, f32, f32),
}

/// Physical menu pixels: fixed padding follows the menu painter, while cell
/// metrics and top chrome already include the window's scale.
pub(super) fn picker_card_geometry(
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
    row_count: usize,
) -> Option<PickerGeometry> {
    let (sw, sh) = surface;
    let (_, ch) = cell;
    if ![sw, sh, cell.0, ch, top_chrome]
        .into_iter()
        .all(f32::is_finite)
        || sw <= 32.0
        || ch <= 0.0
        || cell.0 <= 0.0
        || row_count == 0
    {
        return None;
    }
    let max_width = (sw - 32.0).max(0.0);
    let width = (sw * 0.56).clamp(360.0_f32.min(max_width), 720.0_f32.min(max_width));
    let x = (sw - width) * 0.5;
    let remaining = (sh - top_chrome).max(0.0);
    let header_h = ch + kettle_render::menu::PICKER_HEADER_PAD;
    let row_h = ch + kettle_render::menu::ROW_PAD;
    let spacing = remaining - header_h - row_h;
    if spacing < 0.0 {
        return None;
    }
    let bottom = 16.0_f32.min(spacing * 0.5);
    let top = 24.0_f32.max(remaining * 0.08).min(spacing - bottom);
    let y = top_chrome + top;
    let available = sh - bottom - y - header_h;
    let rows_h = (row_count.min(PICKER_MAX_VISIBLE_ROWS) as f32 * row_h).min(available);
    Some(PickerGeometry {
        header: (x, y, width, header_h),
        rows: (x, y + header_h, width, rows_h),
    })
}

/// The results rectangle shares its x and width with the query header.
pub(super) fn picker_context_menu(
    mut list: PickerList,
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
) -> Option<ContextMenu> {
    let geometry = picker_card_geometry(surface, cell, top_chrome, list.rows.len())?;
    let (x, y, width, height) = geometry.rows;
    let row_height = cell.1 + kettle_render::menu::ROW_PAD;
    let max_columns = kettle_render::menu::panel_columns(width, cell.0);
    for row in &mut list.rows {
        fit_context_menu_row(row, max_columns);
    }
    let (_, content_h) = kettle_render::menu::scroll_content(
        row_height * list.rows.len() as f32,
        height,
        row_height,
    );
    let scroll_offset = picker_scroll_offset(list.rows.len(), list.selected, content_h, row_height);
    Some(ContextMenu {
        anchor: (x, y),
        rows: list.rows,
        highlight: list.selected,
        scroll_offset,
        panel_w_clamped: width,
        panel_h_clamped: height,
        a11y_label: None,
        picker_header_height: geometry.header.3,
    })
}

/// The open picker's rows as a menu panel: the first of `pickers` that is
/// open, in key-routing order.
pub(super) fn picker_overlay_context_menu(
    pickers: [Option<PickerList>; 4],
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
) -> Option<ContextMenu> {
    pickers
        .into_iter()
        .flatten()
        .next()
        .and_then(|list| picker_context_menu(list, surface, cell, top_chrome))
}

#[derive(Clone, Copy)]
pub(super) enum PickerPointer {
    Hover,
    Press,
    Wheel(isize),
}

pub(super) fn open_picker_list(
    ws: &WindowState,
    cfg: &Config,
    tr: &kettle_i18n::Translator,
    notice: Option<&crate::update_notice::Notice>,
) -> Option<PickerList> {
    if let Some((query, selected)) = &ws.palette_input {
        Some(command_picker_list(
            query,
            *selected,
            &cfg.keybinds,
            tr,
            notice,
            &ws.palette_profiles,
        ))
    } else if let Some(state) = &ws.theme_picker_input {
        Some(theme_picker_list(
            &state.query,
            state.selected,
            &state.opened_on,
            tr,
        ))
    } else if let Some((query, selected)) = &ws.layout_picker_input {
        Some(layout_picker_list(
            query,
            *selected,
            &ws.layout_picker_entries,
            tr,
        ))
    } else {
        ws.ssh_input
            .as_ref()
            .map(|(query, selected)| ssh_picker_list(query, *selected, &cfg.ssh_hosts, tr))
    }
}

fn picker_result_row(menu: &ContextMenu, cell_h: f32, cursor: (f64, f64)) -> Option<usize> {
    let (px, py) = (cursor.0 as f32, cursor.1 as f32);
    let (x, y) = menu.anchor;
    let row_h = cell_h + kettle_render::menu::ROW_PAD;
    let (inset, content_h) = kettle_render::menu::scroll_content(
        menu.rows.len() as f32 * row_h,
        menu.panel_h_clamped,
        row_h,
    );
    if px < x || px >= x + menu.panel_w_clamped || py < y + inset || py >= y + inset + content_h {
        return None;
    }
    let row = ((py - y - inset) / row_h).floor() as usize;
    if (row + 1) as f32 * row_h > content_h {
        return None;
    }
    let index = menu.scroll_offset.checked_add(row)?;
    menu.rows
        .get(index)
        .filter(|row| row.enabled && !row.separator)
        .map(|_| index)
}

pub(super) fn apply_picker_pointer(
    ws: &mut WindowState,
    menu: &ContextMenu,
    cell_h: f32,
    event: PickerPointer,
) -> bool {
    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
    let (x, y) = menu.anchor;
    let (w, h) = (menu.panel_w_clamped, menu.panel_h_clamped);
    if px < x || px >= x + w || py < y || py >= y + h {
        return false;
    }
    let row_h = cell_h + kettle_render::menu::ROW_PAD;
    let (inset, content_h) =
        kettle_render::menu::scroll_content(menu.rows.len() as f32 * row_h, h, row_h);
    let selected = if matches!(event, PickerPointer::Press)
        && let Some(row) = picker_result_row(menu, cell_h, (ws.cursor.x, ws.cursor.y))
    {
        row
    } else {
        let delta = match event {
            PickerPointer::Wheel(delta) => delta,
            _ if inset > 0.0 && py < y + inset => -1,
            _ if inset > 0.0 && py >= y + inset + content_h => 1,
            _ => return false,
        };
        let visible = ((content_h / row_h).floor() as usize)
            .max(1)
            .min(menu.rows.len());
        let max_offset = menu.rows.len().saturating_sub(visible);
        let offset = menu
            .scroll_offset
            .saturating_add_signed(delta)
            .min(max_offset);
        // The picker viewport follows selection. Crossing the cut edge selects
        // the newly exposed row and keeps keyboard/preview routing in sync.
        if offset == menu.scroll_offset {
            menu.highlight
        } else {
            offset + visible - 1
        }
    };
    let selection = if let Some((_, selected)) = ws.palette_input.as_mut() {
        selected
    } else if let Some(state) = ws.theme_picker_input.as_mut() {
        &mut state.selected
    } else if let Some((_, selected)) = ws.layout_picker_input.as_mut() {
        selected
    } else if let Some((_, selected)) = ws.ssh_input.as_mut() {
        selected
    } else {
        return false;
    };
    if *selection != selected {
        *selection = selected;
        ws.accessibility_pending = true;
        ws.reuse_pane_snapshots_once = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }
    true
}

impl App {
    pub(super) fn picker_top_chrome(&self, ws: &WindowState) -> f32 {
        let tabs = if self.cfg.tab_bar_pos == kettle_config::TabBarPos::Top {
            self.tab_bar_h(ws)
        } else {
            0.0
        };
        tabs + if self.cfg.status_bar == kettle_config::StatusBarMode::Top {
            self.status_bar_h(ws)
        } else {
            0.0
        }
    }

    pub(super) fn picker_pointer(
        &mut self,
        ws: &mut WindowState,
        event: PickerPointer,
        event_loop: Option<&ActiveEventLoop>,
    ) -> bool {
        if ws.context_menu.is_some() {
            return false;
        }
        let Some(list) =
            open_picker_list(ws, &self.cfg, &self.ui_text, self.update_notice().as_ref())
        else {
            return false;
        };
        let surface = ws
            .renderer
            .as_ref()
            .map(|renderer| {
                let (w, h) = renderer.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let cell = self.menu_cell(ws);
        let Some(menu) = picker_context_menu(list, surface, cell, self.picker_top_chrome(ws))
        else {
            return false;
        };
        let activate = matches!(event, PickerPointer::Press)
            && picker_result_row(&menu, cell.1, (ws.cursor.x, ws.cursor.y)).is_some();
        let handled = apply_picker_pointer(ws, &menu, cell.1, event);
        if handled
            && activate
            && let Some(event_loop) = event_loop
        {
            end_modal_composition(ws);
            let mods = std::mem::replace(&mut ws.mods, ModifiersState::empty());
            let key = Key::Named(NamedKey::Enter);
            if ws.palette_input.is_some() {
                self.palette_key(ws, &key, None, event_loop);
            } else if ws.theme_picker_input.is_some() {
                self.theme_picker_key(ws, &key, None);
            } else if ws.layout_picker_input.is_some() {
                self.layout_picker_key(ws, &key, None);
            } else if ws.ssh_input.is_some() {
                self.ssh_key(ws, &key, None);
            }
            ws.mods = mods;
        }
        handled || matches!(event, PickerPointer::Wheel(_))
    }
}
