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

/// The palette's rows: every command, then, while an update notice is
/// active, its release notes and Dismiss.
pub(super) fn palette_entries(
    notice: Option<&crate::update_notice::Notice>,
    tr: &kettle_i18n::Translator,
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    use kettle_config::palette::PaletteLabel;
    let mut entries: Vec<_> = kettle_config::palette::commands()
        .into_iter()
        .map(|(label, action)| (PaletteLabel::Text(label), action))
        .collect();
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
) -> PickerList {
    let commands = palette_entries(notice, tr);
    let ranked = kettle_config::palette::rank_entries(query, &commands, tr);
    let rows = ranked
        .iter()
        .map(|&index| {
            let (label, action) = &commands[index];
            ContextMenuRow {
                label: label.shown(tr).to_string(),
                separator: false,
                enabled: true,
                hint: kettle_config::keybinds::hint_label(bindings, action).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        PickerList {
            rows: vec![ContextMenuRow {
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

/// The theme picker's rows. The theme it opened on is ticked, and each row
/// says whether it is dark or light.
pub(super) fn theme_picker_list(
    query: &str,
    selected: usize,
    opened_on: &str,
    tr: &kettle_i18n::Translator,
) -> PickerList {
    use kettle_i18n::Text;
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
            label: hosts[index].0.clone(),
            separator: false,
            enabled: true,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        PickerList {
            rows: vec![ContextMenuRow {
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

/// Place a picker list above its bottom input lane using the same rows, width
/// budget, clipping, selection rail, and overflow cues as the context menu.
pub(super) fn picker_context_menu(
    mut list: PickerList,
    surface: (f32, f32),
    cell: (f32, f32),
) -> Option<ContextMenu> {
    let (surface_w, surface_h) = surface;
    let (cell_w, cell_h) = cell;
    let input_height = cell_h + 10.0;
    let list_surface = (surface_w, (surface_h - input_height - 4.0).max(0.0));
    if !context_menu_surface_can_fit_row(list_surface, cell) || list.rows.is_empty() {
        return None;
    }

    let row_height = cell_h + kettle_render::menu::ROW_PAD;
    let max_columns = list
        .rows
        .iter()
        .map(kettle_render::menu_row_chars)
        .max()
        .unwrap_or(0) as f32;
    let natural_width = kettle_render::menu::panel_width(max_columns, cell_w);
    let natural_height = row_height * list.rows.len().min(PICKER_MAX_VISIBLE_ROWS) as f32;
    let panel = clamp_context_menu_panel((natural_width, natural_height), list_surface);
    let max_columns = kettle_render::menu::panel_columns(panel.0, cell_w);
    for row in &mut list.rows {
        fit_context_menu_row(row, max_columns);
    }
    let (_, content_h) = kettle_render::menu::scroll_content(
        row_height * list.rows.len() as f32,
        panel.1,
        row_height,
    );
    let scroll_offset = picker_scroll_offset(list.rows.len(), list.selected, content_h, row_height);
    let requested_anchor = (
        (surface_w - panel.0) * 0.5,
        (list_surface.1 - panel.1 - 4.0).max(4.0),
    );
    let anchor = clamp_context_menu_anchor(requested_anchor, panel, list_surface);
    Some(ContextMenu {
        anchor,
        rows: list.rows,
        highlight: list.selected,
        scroll_offset,
        panel_w_clamped: panel.0,
        panel_h_clamped: panel.1,
        a11y_label: None,
    })
}

/// The open picker's rows as a menu panel: the first of `pickers` that is
/// open, in key-routing order.
pub(super) fn picker_overlay_context_menu(
    pickers: [Option<PickerList>; 4],
    surface: (f32, f32),
    cell: (f32, f32),
) -> Option<ContextMenu> {
    pickers
        .into_iter()
        .flatten()
        .next()
        .and_then(|list| picker_context_menu(list, surface, cell))
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
    let selected = if offset == menu.scroll_offset {
        menu.highlight
    } else {
        offset + visible - 1
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
    pub(super) fn picker_pointer(&self, ws: &mut WindowState, event: PickerPointer) -> bool {
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
        let Some(menu) = picker_context_menu(list, surface, cell) else {
            return false;
        };
        let handled = apply_picker_pointer(ws, &menu, cell.1, event);
        handled || matches!(event, PickerPointer::Wheel(_))
    }
}
