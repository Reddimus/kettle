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

/// Indices of `labels` that fuzzy-match `query`: a label equal to the trimmed
/// query (ignoring ASCII case) first, then by score, then in input order.
fn rank_ssh_labels<'a>(query: &str, labels: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let query = query.trim();
    let mut ranked: Vec<(usize, bool, i32)> = labels
        .enumerate()
        .filter_map(|(index, label)| {
            let exact = !query.is_empty() && label.eq_ignore_ascii_case(query);
            kettle_config::fuzzy::score(query, label).map(|score| (index, exact, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    ranked.into_iter().map(|(index, _, _)| index).collect()
}

/// Indices into configured SSH hosts: an exact name first, then best match,
/// with config order breaking ties.
pub(super) fn rank_ssh_hosts(query: &str, hosts: &[(String, String)]) -> Vec<usize> {
    rank_ssh_labels(query, hosts.iter().map(|(name, _)| name.as_str()))
}

/// One SSH launcher row. Painting, keys, Tab, Enter, the pointer and
/// accessibility all read this one list, from [`ssh_rows`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SshRow {
    /// `cfg.ssh_hosts[i]`.
    Configured(usize),
    /// `ws.ssh_imported[i]`, a `Host` alias from `~/.ssh/config`.
    Imported(usize),
    /// Connect to the trimmed query itself. Shown, first and selected, when
    /// the query is a valid target that no host row's name or alias equals
    /// (ignoring ASCII case), so Enter never connects a typed `prod` to a
    /// fuzzy match such as `prod-db`.
    ConnectTyped,
    /// Disabled and never selected: no hosts and nothing typed, or a typed
    /// target ssh must not receive.
    Placeholder,
    /// Save the trimmed query as a host. Last, and shown only when the query
    /// is a valid target that no configured name or target equals (ignoring
    /// ASCII case). Opens the name editor.
    SaveTyped,
    /// Remove `cfg.ssh_hosts[i]`. Last, and shown only when the trimmed query
    /// is that configured host's name (ignoring ASCII case), never for an
    /// imported alias: Kettle does not write `~/.ssh/config`. Opens a
    /// confirmation.
    Remove(usize),
}

impl SshRow {
    fn selectable(self) -> bool {
        !matches!(self, Self::Placeholder)
    }

    /// Save and Remove change the config, so only a deliberate Down or a
    /// click on the row itself selects one: never the wheel, an Up wrap, a
    /// hover, or a selection clamped after the list shrank.
    pub(super) fn is_action(self) -> bool {
        matches!(self, Self::SaveTyped | Self::Remove(_))
    }

    /// The host name or alias a host row shows; `None` for other rows.
    fn host_label<'a>(
        self,
        hosts: &'a [(String, String)],
        imported: &'a [String],
    ) -> Option<&'a str> {
        match self {
            Self::Configured(index) => hosts.get(index).map(|(name, _)| name.as_str()),
            Self::Imported(index) => imported.get(index).map(String::as_str),
            Self::ConnectTyped | Self::Placeholder | Self::SaveTyped | Self::Remove(_) => None,
        }
    }
}

/// The launcher's rows for `query`, in display order. A host row whose name or
/// alias equals the trimmed query (ignoring ASCII case) comes first. Otherwise
/// a valid typed target comes first as `ConnectTyped`, so it is what Enter
/// connects to. The fuzzy matches follow: configured hosts, then imported
/// aliases, each ranked. A query that is not a valid target shows only the
/// `Placeholder` refusal, or the one host it names exactly. An imported
/// alias equal to a configured name or target, ignoring ASCII case, is not
/// shown twice. At most one action row ends the list: `Remove` when row 0 is
/// the configured host the query names, else `SaveTyped` when the query is a
/// valid target no configured name or target equals.
pub(super) fn ssh_rows(
    query: &str,
    hosts: &[(String, String)],
    imported: &[String],
) -> Vec<SshRow> {
    let query = query.trim();
    let shown: Vec<usize> = (0..imported.len())
        .filter(|&index| {
            let alias = &imported[index];
            !hosts.iter().any(|(name, target)| {
                name.eq_ignore_ascii_case(alias) || target.eq_ignore_ascii_case(alias)
            })
        })
        .collect();
    let mut rows: Vec<SshRow> = rank_ssh_hosts(query, hosts)
        .into_iter()
        .map(SshRow::Configured)
        .collect();
    rows.extend(
        rank_ssh_labels(query, shown.iter().map(|&index| imported[index].as_str()))
            .into_iter()
            .map(|rank| SshRow::Imported(shown[rank])),
    );
    let exact = |row: &SshRow| {
        row.host_label(hosts, imported)
            .is_some_and(|label| label.eq_ignore_ascii_case(query))
    };
    if query.is_empty() {
        if rows.is_empty() {
            rows.push(SshRow::Placeholder);
        }
        return rows;
    }
    let exact = rows.iter().position(exact);
    let mut rows = if !kettle_config::ssh_config::is_valid_ssh_target(query) {
        // No fuzzy match is offered for text ssh must not receive, so Enter
        // cannot reach one. A configured name with a space can still be
        // typed in full: it is the only host row then.
        vec![exact.map_or(SshRow::Placeholder, |position| rows[position])]
    } else if let Some(position) = exact {
        let row = rows.remove(position);
        rows.insert(0, row);
        rows
    } else {
        rows.insert(0, SshRow::ConnectTyped);
        rows
    };
    if let (Some(_), SshRow::Configured(index)) = (exact, rows[0]) {
        rows.push(SshRow::Remove(index));
    } else if kettle_config::ssh_config::is_valid_ssh_target(query)
        && !hosts.iter().any(|(name, target)| {
            name.eq_ignore_ascii_case(query) || target.eq_ignore_ascii_case(query)
        })
    {
        rows.push(SshRow::SaveTyped);
    }
    rows
}

/// `selected` moved onto a selectable row: itself, else the nearest one above,
/// else below. A list with no selectable row keeps 0. An index past the end,
/// left by a list that shrank under it (a config reload), lands on the last
/// row that is not an action row: Save and Remove are never reached by a
/// clamp.
pub(super) fn ssh_clamp_selection(rows: &[SshRow], selected: usize) -> usize {
    let Some(last) = rows.len().checked_sub(1) else {
        return 0;
    };
    let past_end = selected > last;
    let selected = selected.min(last);
    (0..=selected)
        .rev()
        .chain(selected + 1..rows.len())
        .find(|&index| rows[index].selectable() && !(past_end && rows[index].is_action()))
        .unwrap_or(selected)
}

/// The selection after one arrow (or Ctrl+J/K/N/P) step. Disabled rows are
/// skipped. Down moves onto the action row that ends the list, and wraps
/// from it to the first row. Up from the first row wraps past the action
/// row onto the last other row, so Save and Remove take a deliberate Down,
/// never one Up press.
pub(super) fn ssh_step_selection(rows: &[SshRow], selected: usize, down: bool) -> usize {
    let current = ssh_clamp_selection(rows, selected);
    let selectable: Vec<usize> = (0..rows.len())
        .filter(|&index| rows[index].selectable())
        .collect();
    let Some(&first) = selectable.first() else {
        return current;
    };
    let position = selectable.iter().position(|&index| index == current);
    match (down, position) {
        (true, Some(position)) => selectable.get(position + 1).copied().unwrap_or(first),
        (true, None) => first,
        (false, Some(position)) if position > 0 => selectable[position - 1],
        (false, _) => selectable
            .iter()
            .rev()
            .copied()
            .find(|&index| !rows[index].is_action())
            .unwrap_or(current),
    }
}

/// The index of the action row, before which the wheel may select: only a
/// deliberate Down or a click on it selects Save or Remove. `None` without
/// one.
pub(super) fn ssh_wheel_limit(rows: &[SshRow]) -> Option<usize> {
    rows.iter().position(|row| row.is_action())
}

/// What Enter (or a click, which is a synthetic Enter) does on the launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SshEnter {
    /// Spawn `ssh -t -- <target>`.
    Connect(String),
    /// Open the name editor to save this target as a host.
    Save(String),
    /// Ask before removing this configured host.
    Remove { name: String, target: String },
    /// The typed target is not one ssh may receive; nothing is spawned and
    /// the launcher stays open on the row that says why.
    Invalid,
}

/// The action for the selected row; `None` when nothing is typed and there is
/// no host to connect to.
pub(super) fn ssh_row_action(
    query: &str,
    selected: usize,
    hosts: &[(String, String)],
    imported: &[String],
) -> Option<SshEnter> {
    let rows = ssh_rows(query, hosts, imported);
    let row = *rows.get(ssh_clamp_selection(&rows, selected))?;
    let connect = |target: &str| {
        Some(if kettle_config::ssh_config::is_valid_ssh_target(target) {
            SshEnter::Connect(target.to_string())
        } else {
            SshEnter::Invalid
        })
    };
    match row {
        SshRow::Configured(index) => connect(&hosts.get(index)?.1),
        SshRow::Imported(index) => connect(imported.get(index)?),
        SshRow::ConnectTyped => connect(query.trim()),
        SshRow::Placeholder => (!query.trim().is_empty()).then_some(SshEnter::Invalid),
        SshRow::SaveTyped => Some(match connect(query.trim())? {
            SshEnter::Connect(target) => SshEnter::Save(target),
            _ => SshEnter::Invalid,
        }),
        SshRow::Remove(index) => {
            let (name, target) = hosts.get(index)?;
            Some(SshEnter::Remove {
                name: name.clone(),
                target: target.clone(),
            })
        }
    }
}

/// Resolve Enter on the open launcher. A connection, a Save or a Remove
/// closes it and is returned; an invalid typed target leaves it open and
/// returns `Invalid`; an empty launcher closes as it always has.
pub(super) fn take_ssh_enter(ws: &mut WindowState, hosts: &[(String, String)]) -> Option<SshEnter> {
    let (query, selected) = ws.ssh_input.as_ref()?;
    let action = ssh_row_action(query, *selected, hosts, &ws.ssh_imported);
    if action != Some(SshEnter::Invalid) {
        ws.ssh_input = None;
    }
    action
}

/// The text Tab completes to: the selected host row's name or alias, or, on
/// the Connect row, the best fuzzy match below it.
pub(super) fn ssh_completion(
    query: &str,
    selected: usize,
    hosts: &[(String, String)],
    imported: &[String],
) -> Option<String> {
    let rows = ssh_rows(query, hosts, imported);
    let row = *rows.get(ssh_clamp_selection(&rows, selected))?;
    let row = match row {
        SshRow::ConnectTyped => *rows.get(1)?,
        row => row,
    };
    row.host_label(hosts, imported).map(str::to_string)
}

/// Open the SSH launcher. With `ssh-config-hosts` on, the aliases in the file
/// `ssh_config` names are read now, so the list is fresh on every open; with
/// it off, no path is looked up and no file is touched.
pub(super) fn open_ssh_launcher(
    ws: &mut WindowState,
    cfg: &Config,
    ssh_config: impl FnOnce() -> Option<std::path::PathBuf>,
) {
    ws.ssh_imported = if cfg.ssh_config_hosts {
        ssh_config()
            .map(|path| kettle_config::ssh_config::user_ssh_host_aliases(&path).aliases)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    ws.ssh_input = Some((Default::default(), 0));
    ws.ssh_hosts_shown = cfg.ssh_hosts.clone();
    ws.ssh_status = None;
}

/// The launcher's status line, while the query it was set for is unchanged.
pub(super) fn ssh_status_line(ws: &WindowState) -> Option<&str> {
    let (query, _) = ws.ssh_input.as_ref()?;
    ws.ssh_status
        .as_ref()
        .filter(|status| status.query == query.text())
        .map(|status| status.text.as_str())
}

pub(super) struct PickerList {
    pub(super) status: Option<PickerStatus>,
    pub(super) rows: Vec<ContextMenuRow>,
    pub(super) selected: usize,
    /// The theme picker's footer; `None` for every other picker.
    pub(super) footer: Option<kettle_render::ThemePickerFooter>,
}

pub(super) struct PickerStatus {
    message: &'static str,
    compact: &'static str,
    guidance: String,
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

/// Rows a picker paints whole, at most.
const PICKER_MAX_VISIBLE_ROWS: usize = 8;

/// The height of a picker list of `total` rows that paints as many whole rows
/// as fit in `available`, at most `cap`, and always at least one. Pickers have
/// no arrow strips at their cut edges, so the list is whole rows with no
/// slack: a part-row of room at the bottom would be blank panel that neither
/// paints nor takes a press.
pub(super) fn picker_rows_height(total: usize, cap: usize, available: f32, row_h: f32) -> f32 {
    let fit = ((available / row_h).floor() as usize).max(1);
    total.min(cap).min(fit) as f32 * row_h
}

/// Profile discovery is shared with `--list-profiles`; names remain config data.
pub(crate) fn profile_launch_entries(
    names: Option<&[String]>,
    tr: &kettle_i18n::Translator,
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    use kettle_config::palette::PaletteLabel;
    let Some(names) = names else {
        return Vec::new();
    };
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

/// Commands, with the broadcast rows `broadcast-default` picks, and the
/// palette's opening profile snapshot, followed by release notes and Dismiss
/// while an update notice is active.
pub(super) fn palette_entries(
    notice: Option<&crate::update_notice::Notice>,
    tr: &kettle_i18n::Translator,
    profiles: Option<&[String]>,
    broadcast_default: kettle_config::BroadcastDefault,
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    use kettle_config::palette::PaletteLabel;
    let mut entries: Vec<_> = kettle_config::palette::commands_for(broadcast_default)
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

/// The rows the open palette ranks: every entry, or with `bound_only` (the
/// keyboard-shortcut list) only those that have a shortcut to show. The
/// painter, key handling, pointer, accessibility and control paths all take
/// their rows from here, so a selection index means the same row in each.
pub(super) fn palette_view_entries(
    notice: Option<&crate::update_notice::Notice>,
    tr: &kettle_i18n::Translator,
    profiles: Option<&[String]>,
    broadcast_default: kettle_config::BroadcastDefault,
    bindings: &Bindings,
    bound_only: bool,
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    let mut entries = palette_entries(notice, tr, profiles, broadcast_default);
    if bound_only {
        entries.retain(|(_, action)| {
            kettle_config::keybinds::ui_hint_label(bindings, action)
                .is_some_and(|hint| !hint.is_empty())
        });
    }
    entries
}

/// The command Enter runs: the one ranked at `selected` among `entries`, which
/// is the row the painter shows there. `None` past the last match.
pub(super) fn palette_enter_action(
    query: &str,
    selected: usize,
    entries: &[(kettle_config::palette::PaletteLabel, Action)],
    tr: &kettle_i18n::Translator,
) -> Option<Action> {
    kettle_config::palette::rank_entries(query, entries, tr)
        .get(selected)
        .map(|&index| entries[index].1.clone())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn command_picker_list(
    query: &str,
    selected: usize,
    bindings: &Bindings,
    tr: &kettle_i18n::Translator,
    notice: Option<&crate::update_notice::Notice>,
    profiles: Option<&[String]>,
    bound_only: bool,
    broadcast_default: kettle_config::BroadcastDefault,
) -> PickerList {
    let commands = palette_view_entries(
        notice,
        tr,
        profiles,
        broadcast_default,
        bindings,
        bound_only,
    );
    let ranked = kettle_config::palette::rank_matches(query, &commands, tr);
    let rows = ranked
        .into_iter()
        .map(|entry| {
            let (label, action) = &commands[entry.index];
            ContextMenuRow {
                toggle: None,
                swatches: None,
                matched_chars: entry.matched_chars.map(|positions| {
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
    let status = profiles::palette_profile_status(profiles, tr).map(|message| PickerStatus {
        message,
        compact: tr.text(kettle_i18n::Text::PickerProfilesPending),
        guidance: picker_hint(
            tr,
            &[
                ("Enter", kettle_i18n::Text::PickerHintRun),
                ("Tab/↑↓", kettle_i18n::Text::PickerHintSelect),
                ("Esc", kettle_i18n::Text::PickerHintCancel),
            ],
        ),
    });
    if rows.is_empty() {
        PickerList {
            status,
            rows: vec![ContextMenuRow {
                toggle: None,
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
            footer: None,
        }
    } else {
        PickerList {
            status,
            selected: selected.min(rows.len() - 1),
            rows,
            footer: None,
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
            toggle: None,
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
            status: None,
            rows: vec![ContextMenuRow {
                toggle: None,
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
            footer: None,
        }
    } else {
        PickerList {
            status: None,
            selected: selected.min(rows.len() - 1),
            rows,
            footer: None,
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
    let mut rows: Vec<ContextMenuRow> = theme_picker_matches(query, opened_on, tr)
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
                toggle: None,
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
    // Each strip sits just left of its row's hint, and the hints differ in
    // width ("dark", "✓ light", Spanish "oscuro"): pad them to one width so
    // the strips form a single column. Measuring and painting both read the
    // padded hint, so they stay in step.
    use unicode_width::UnicodeWidthStr as _;
    let hint_width = rows.iter().map(|row| row.hint.width()).max().unwrap_or(0);
    for row in &mut rows {
        let pad = hint_width - row.hint.width();
        row.hint.insert_str(0, &" ".repeat(pad));
    }
    if rows.is_empty() {
        return PickerList {
            status: None,
            rows: vec![ContextMenuRow {
                toggle: None,
                swatches: None,
                matched_chars: None,
                label: tr.text(Text::PickerNoMatchingTheme).to_string(),
                separator: false,
                enabled: false,
                hint: String::new(),
            }],
            selected: 0,
            footer: None,
        };
    }
    let selected = selected.min(rows.len() - 1);
    PickerList {
        status: None,
        rows,
        selected,
        footer: None,
    }
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
        slot: None,
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

/// The config key that holds `slot`'s theme.
pub(super) fn theme_slot_key(slot: kettle_render::ThemeSlot) -> &'static str {
    match slot {
        kettle_render::ThemeSlot::Dark => "dark-theme",
        kettle_render::ThemeSlot::Light => "light-theme",
    }
}

/// The key the open picker `state` saves a pick to, while `active` (from
/// `theme_pick_key`) is the side the window wears: the side chosen on the
/// status line, else the active one. A fixed theme has no sides.
pub(super) fn theme_picker_save_key(
    state: Option<&crate::window_state::ThemePickerState>,
    active: &'static str,
) -> &'static str {
    match state.and_then(|state| state.slot) {
        Some(slot) if active != "theme" => theme_slot_key(slot),
        _ => active,
    }
}

/// What Enter in the theme picker does with its pick: the key it saves to,
/// and whether the window goes back to the theme it wore, because the pick
/// is for the side the window is not on now. `active_dark` is the side the
/// schedule or the system calls for, as `theme_pick_key` takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ThemeCommitPlan {
    pub(super) key: &'static str,
    pub(super) restore_active: bool,
}

pub(super) fn theme_commit_plan(
    state: &crate::window_state::ThemePickerState,
    cfg: &Config,
    active_dark: Option<bool>,
) -> ThemeCommitPlan {
    let active = super::theme_pick_key(cfg, active_dark);
    let key = theme_picker_save_key(Some(state), active);
    ThemeCommitPlan {
        key,
        restore_active: key != active,
    }
}

/// Edit `slot`'s theme in the open picker, while `active` (from
/// `theme_pick_key`) is the side the window wears: the list opens on that
/// side's theme, ticks it and highlights it, and the window previews it. A
/// typed query is cleared, so the list shows that theme instead of keeping
/// a match from the other side, which Enter would then save for this one.
/// The preview's baseline, which Esc restores, stays the theme from before
/// the picker. The side the picker already saves to, and a fixed theme,
/// which has no sides, change nothing. Returns whether anything changed.
pub(super) fn switch_theme_picker_slot(
    state: &mut crate::window_state::ThemePickerState,
    cfg: &Config,
    active: &'static str,
    slot: kettle_render::ThemeSlot,
    tr: &kettle_i18n::Translator,
) -> bool {
    let theme = match slot {
        kettle_render::ThemeSlot::Dark => &cfg.dark_theme,
        kettle_render::ThemeSlot::Light => &cfg.light_theme,
    };
    if active == "theme" || theme_picker_save_key(Some(state), active) == theme_slot_key(slot) {
        return false;
    }
    state.slot = Some(slot);
    state.opened_on = theme.clone();
    state
        .query
        .replace_all("", crate::modal_input::MAX_MODAL_INPUT_BYTES);
    state.selected = theme_picker_matches(&state.query, &state.opened_on, tr)
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case(theme.trim()))
        .unwrap_or(0);
    true
}

/// The theme picker's key hint, for the painted header and its projection
/// alike: Enter keeps the pick, or only saves it for the side the window is
/// not on (see [`theme_picker_enter_hint`]). With `tab`, a picker with sides
/// also says which side Tab switches to.
pub(super) fn theme_picker_hints(
    save_key: &str,
    active: &str,
    tab: bool,
) -> Vec<(&'static str, kettle_i18n::Text)> {
    use kettle_i18n::Text;
    let mut hints = vec![
        ("Enter", theme_picker_enter_hint(save_key, active)),
        ("↑↓", Text::PickerHintSelect),
    ];
    if tab && let ThemePickerTab::Side(slot) = theme_picker_tab(save_key, false) {
        hints.push((
            "Tab:",
            match slot {
                kettle_render::ThemeSlot::Light => Text::PickerHintTabLightTheme,
                kettle_render::ThemeSlot::Dark => Text::PickerHintTabDarkTheme,
            },
        ));
    }
    hints.push(("Esc", Text::PickerHintRestore));
    hints
}

/// The theme picker header's hint in `field`, measured as the painter
/// measures it in cells `cw` wide: the whole hint, or without its Tab part
/// when only that fits whole beside the query. A header that has room for
/// neither keeps the whole one, which the painter shortens to its complete
/// last action or leaves out.
pub(super) fn theme_picker_header_hint(
    field: &kettle_render::TextFieldOverlay,
    cw: f32,
    tr: &kettle_i18n::Translator,
    save_key: &str,
    active: &str,
) -> String {
    let full = picker_hint(tr, &theme_picker_hints(save_key, active, true));
    let fits = |hint: &String| {
        let mut probe = field.clone();
        probe.hint = hint.clone();
        // Whole: the painter's fallback for a picker header keeps a
        // shortened hint, which does not count as this one fitting.
        kettle_render::text_field_labels(&probe, cw).1 == format!("{hint}  ")
    };
    if fits(&full) {
        return full;
    }
    let short = picker_hint(tr, &theme_picker_hints(save_key, active, false));
    if fits(&short) { short } else { full }
}

/// What Tab does in the theme picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThemePickerTab {
    /// Edit the other side's theme, as a click on it on the status line does.
    Side(kettle_render::ThemeSlot),
    /// Move the selection, down or up, as in the other pickers.
    Row { forward: bool },
}

/// What Tab (or Shift+Tab, with `shift`) does in a theme picker that saves
/// to `save_key`. A picker with sides switches to the other one, so both
/// keys toggle between the two; a fixed theme has no sides, and Tab moves
/// down a row and Shift+Tab up one. The arrows move rows either way.
pub(super) fn theme_picker_tab(save_key: &str, shift: bool) -> ThemePickerTab {
    use kettle_render::ThemeSlot;
    match save_key {
        "dark-theme" => ThemePickerTab::Side(ThemeSlot::Light),
        "light-theme" => ThemePickerTab::Side(ThemeSlot::Dark),
        _ => ThemePickerTab::Row { forward: !shift },
    }
}

/// What Enter does in the theme picker's key hint: keep the pick, or only
/// save it for the side the window is not on.
pub(super) fn theme_picker_enter_hint(save_key: &str, active: &str) -> kettle_i18n::Text {
    use kettle_i18n::Text;
    match save_key {
        _ if save_key == active => Text::PickerHintKeep,
        "light-theme" => Text::PickerHintSaveForLight,
        "dark-theme" => Text::PickerHintSaveForDark,
        _ => Text::PickerHintKeep,
    }
}

/// Escape from a picker must restore the new side after the system or the
/// schedule switches sides (see `App::wear_followed_theme`).
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
    imported: &[String],
    tr: &kettle_i18n::Translator,
) -> PickerList {
    let rows = ssh_rows(query, hosts, imported);
    let selected = ssh_clamp_selection(&rows, selected);
    let row = |label: String, hint: String, enabled: bool| ContextMenuRow {
        toggle: None,
        swatches: None,
        matched_chars: None,
        label,
        separator: false,
        enabled,
        hint,
    };
    let rows = rows
        .iter()
        .map(|entry| match *entry {
            SshRow::Configured(index) => row(hosts[index].0.clone(), String::new(), true),
            SshRow::Imported(index) => row(
                imported[index].clone(),
                tr.picker_ssh_config_source(kettle_config::ssh_config::SSH_CONFIG_DISPLAY_PATH),
                true,
            ),
            SshRow::ConnectTyped => row(
                tr.picker_ssh_connect_typed(query.trim()),
                String::new(),
                true,
            ),
            SshRow::Placeholder if query.trim().is_empty() => row(
                tr.text(kettle_i18n::Text::PickerNoConfiguredHosts)
                    .to_string(),
                tr.text(kettle_i18n::Text::PickerTypedTarget).to_string(),
                false,
            ),
            SshRow::Placeholder => row(
                tr.text(kettle_i18n::Text::PickerSshInvalidTarget)
                    .to_string(),
                String::new(),
                false,
            ),
            SshRow::SaveTyped => row(tr.picker_ssh_save_typed(query.trim()), String::new(), true),
            SshRow::Remove(index) => {
                row(tr.picker_ssh_remove(&hosts[index].0), String::new(), true)
            }
        })
        .collect();
    PickerList {
        status: None,
        rows,
        selected,
        footer: None,
    }
}

/// The open SSH launcher's rows. The overlay painter, pointer hit-testing and
/// accessibility all call this one builder.
pub(super) fn ssh_picker_list_for(
    ws: &WindowState,
    cfg: &Config,
    tr: &kettle_i18n::Translator,
) -> Option<PickerList> {
    let (query, selected) = ws.ssh_input.as_ref()?;
    Some(ssh_picker_list(
        query,
        *selected,
        &cfg.ssh_hosts,
        &ws.ssh_imported,
        tr,
    ))
}

/// The SSH launcher's hint lane: the status line after a save or removal,
/// else the key hint. Shared by the overlay and the text-field projection so
/// the painted and announced text cannot drift.
pub(super) fn ssh_picker_hint(status: Option<&str>, tr: &kettle_i18n::Translator) -> String {
    use kettle_i18n::Text as T;
    if let Some(status) = status {
        return status.to_string();
    }
    picker_hint(
        tr,
        &[
            ("Enter", T::PickerHintConnect),
            ("Tab", T::PickerHintComplete),
            ("↑↓", T::PickerHintSelect),
            ("Esc", T::PickerHintCancel),
        ],
    )
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
    /// The theme picker's footer under the rows: whether the sample card
    /// fits, how many status lines do, and its height.
    pub(super) footer: Option<(bool, usize, f32)>,
}

/// Rows the theme picker paints whole, at most. It has the room: nothing
/// else is open, and a longer list shows more of the themes' looks at once.
const THEME_PICKER_MAX_VISIBLE_ROWS: usize = 14;
/// Rows the theme picker keeps however short the window: its footer gives
/// way first.
const THEME_PICKER_MIN_VISIBLE_ROWS: usize = 4;

/// How many whole rows a list of `total` rows `rows_h` high paints, in the
/// content area the painter, hit tests and accessibility share.
fn painted_rows(total: usize, rows_h: f32, row_h: f32) -> usize {
    let (_, content_h) = kettle_render::menu::picker_scroll_content(rows_h);
    ((content_h / row_h + 0.001).floor() as usize).min(total)
}

/// Physical menu pixels: fixed padding follows the menu painter, while cell
/// metrics and top chrome already include the window's scale.
pub(super) fn picker_card_geometry(
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
    row_count: usize,
) -> Option<PickerGeometry> {
    picker_card_geometry_with(surface, cell, top_chrome, row_count, 0.0, None)
}

/// [`picker_card_geometry`] with a pinned status band of `status_height`
/// between the query header and the rows.
fn picker_card_geometry_with_status(
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
    row_count: usize,
    status_height: f32,
) -> Option<PickerGeometry> {
    picker_card_geometry_with(surface, cell, top_chrome, row_count, status_height, None)
}

/// [`picker_card_geometry`] for the theme picker: up to 14 rows, then its
/// footer in the room left. The footer gives way so the list keeps at least
/// 4 rows (or the whole list): the sample card goes first, then one status
/// line, then the other. The header is where every picker has it.
/// `fits` says whether the card and the status lines fit a given width.
/// A pinned status band of `status_height` sits between header and rows, as
/// in [`picker_card_geometry_with_status`].
pub(super) fn theme_picker_card_geometry(
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
    row_count: usize,
    status_height: f32,
    fits: impl Fn(f32) -> (bool, bool),
) -> Option<PickerGeometry> {
    picker_card_geometry_with(
        surface,
        cell,
        top_chrome,
        row_count,
        status_height,
        Some(&fits),
    )
}

fn picker_card_geometry_with(
    surface: (f32, f32),
    cell: (f32, f32),
    top_chrome: f32,
    row_count: usize,
    status_height: f32,
    theme_footer: Option<&dyn Fn(f32) -> (bool, bool)>,
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
    let row_h = kettle_render::menu::row_height(ch);
    let spacing = remaining - header_h - status_height - row_h;
    if spacing < 0.0 {
        return None;
    }
    let bottom = 16.0_f32.min(spacing * 0.5);
    let top = 24.0_f32.max(remaining * 0.08).min(spacing - bottom);
    let y = top_chrome + top;
    let available = sh - bottom - y - header_h - status_height;
    let header = (x, y, width, header_h);
    let rows_y = y + header_h + status_height;
    let Some(fits) = theme_footer else {
        let rows_h = picker_rows_height(row_count, PICKER_MAX_VISIBLE_ROWS, available, row_h);
        return Some(PickerGeometry {
            header,
            rows: (x, rows_y, width, rows_h),
            footer: None,
        });
    };
    let cap = THEME_PICKER_MAX_VISIBLE_ROWS;
    let (card_fits, status_fits) = fits(width);
    let footer = [(true, 2), (false, 2), (false, 1)]
        .into_iter()
        .filter(|&(sample, _)| status_fits && (card_fits || !sample))
        .find_map(|(sample, lines)| {
            let footer_h = kettle_render::theme_footer_height(ch, sample, lines);
            let rows_h = picker_rows_height(row_count, cap, available - footer_h, row_h);
            let want = row_count.min(THEME_PICKER_MIN_VISIBLE_ROWS);
            (rows_h + footer_h <= available + 0.01
                && painted_rows(row_count, rows_h, row_h) >= want)
                .then_some(((sample, lines, footer_h), rows_h))
        });
    let (footer, rows_h) = match footer {
        Some((footer, rows_h)) => (Some(footer), rows_h),
        None => (None, picker_rows_height(row_count, cap, available, row_h)),
    };
    Some(PickerGeometry {
        header,
        rows: (x, rows_y, width, rows_h),
        footer,
    })
}

/// The theme picker's footer for `state`, before it is fitted: how the theme
/// is chosen, the theme of each side and the one `theme_key` saves to, the
/// contrast setting, and the highlighted theme as the sample.
pub(super) fn theme_picker_footer(
    state: &crate::window_state::ThemePickerState,
    cfg: &Config,
    theme_key: &str,
    tr: &kettle_i18n::Translator,
) -> kettle_render::ThemePickerFooter {
    use kettle_render::{ThemeFollow, ThemeSlot};
    // A schedule decides between the two sides, whatever the mode says.
    let follow = if cfg.theme_schedule.is_some() {
        ThemeFollow::Schedule
    } else {
        match cfg.theme_mode {
            kettle_config::ThemeMode::Auto => ThemeFollow::System,
            kettle_config::ThemeMode::Light => ThemeFollow::Light,
            kettle_config::ThemeMode::Dark => ThemeFollow::Dark,
            kettle_config::ThemeMode::Explicit => ThemeFollow::Fixed,
        }
    };
    let editing = match theme_key {
        "light-theme" => Some(ThemeSlot::Light),
        "dark-theme" => Some(ThemeSlot::Dark),
        _ => None,
    };
    // The window already wears the highlighted theme while it previews, so
    // the open picker does not parse a theme on every frame.
    let sample = match theme_picker_selection(state, tr) {
        Some(name) if name != cfg.theme_name => cfg.theme_by_name(name),
        _ => cfg.theme.clone(),
    };
    let mut footer = kettle_render::ThemePickerFooter {
        follow,
        editing,
        dark_theme: cfg.dark_theme.clone(),
        light_theme: cfg.light_theme.clone(),
        fixed_theme: state.opened_on.clone(),
        minimum_contrast: cfg.minimum_contrast,
        sample,
        show_sample: true,
        status_lines: 2,
        height: 0.0,
        min_columns: (0, 0),
        cell: (0.0, 0.0),
    };
    footer.min_columns = kettle_render::theme_footer_min_columns(&footer, tr);
    footer
}

/// The theme picker's rows with its footer.
pub(super) fn theme_picker_list_with_footer(
    state: &crate::window_state::ThemePickerState,
    cfg: &Config,
    theme_key: &str,
    tr: &kettle_i18n::Translator,
) -> PickerList {
    let mut list = theme_picker_list(&state.query, state.selected, &state.opened_on, tr);
    list.footer = Some(theme_picker_footer(state, cfg, theme_key, tr));
    list
}

/// The results rectangle shares its x and width with the query header.
pub(super) fn picker_context_menu(
    mut list: PickerList,
    surface: (f32, f32),
    cell: (f32, f32),
    text_cell: (f32, f32),
    top_chrome: f32,
) -> Option<ContextMenu> {
    let base = picker_card_geometry(surface, cell, top_chrome, list.rows.len())?;
    // Status is supplementary to the query and selected action. Try complete
    // guidance, then the explanation, then a truthful compact message; never
    // hide the whole picker just because a restored window cannot fit a band.
    let status = list.status.as_ref().and_then(|status| {
        [
            format!("{}\n{}", status.message, status.guidance),
            status.message.to_string(),
            status.compact.to_string(),
        ]
        .into_iter()
        .map(|text| kettle_render::MenuStatus::new(&text, base.header.2, text_cell))
        .find(|status| {
            picker_card_geometry_with_status(
                surface,
                cell,
                top_chrome,
                list.rows.len(),
                status.height(),
            )
            .is_some()
        })
    });
    let status_height = status
        .as_ref()
        .map_or(0.0, kettle_render::MenuStatus::height);
    let geometry = if let Some(footer) = &list.footer {
        theme_picker_card_geometry(
            surface,
            cell,
            top_chrome,
            list.rows.len(),
            status_height,
            |width| footer.fits(width, cell.0),
        )?
    } else {
        picker_card_geometry_with_status(surface, cell, top_chrome, list.rows.len(), status_height)?
    };
    let footer = list.footer.take().and_then(|mut footer| {
        let (sample, lines, height) = geometry.footer?;
        footer.show_sample = sample;
        footer.status_lines = lines;
        footer.height = height;
        footer.cell = cell;
        Some(footer)
    });
    let (x, y, width, height) = geometry.rows;
    let row_height = kettle_render::menu::row_height(cell.1);
    let max_columns = kettle_render::menu::panel_columns(width, cell.0);
    // The strip is one column: when any row is too narrow for it, no row keeps it.
    if list
        .rows
        .iter()
        .any(|row| row.swatches.is_some() && kettle_render::menu_row_chars(row) > max_columns)
    {
        for row in &mut list.rows {
            row.swatches = None;
            // The padding only lined the strips up; without them it would
            // cost the name columns.
            let hint = row.hint.trim_start().to_string();
            row.hint = hint;
        }
    }
    for row in &mut list.rows {
        fit_context_menu_row(row, max_columns);
    }
    let (_, content_h) = kettle_render::menu::picker_scroll_content(height);
    let scroll_offset = picker_scroll_offset(list.rows.len(), list.selected, content_h, row_height);
    Some(ContextMenu {
        anchor: (x, y),
        rows: list.rows,
        highlight: list.selected,
        scroll_offset,
        panel_w_clamped: width,
        panel_h_clamped: height,
        a11y_label: None,
        status,
        picker_header_height: geometry.header.3,
        picker_footer: footer,
    })
}

/// The open picker's rows as a menu panel: the first of `pickers` that is
/// open, in key-routing order.
pub(super) fn picker_overlay_context_menu(
    pickers: [Option<PickerList>; 4],
    surface: (f32, f32),
    cell: (f32, f32),
    text_cell: (f32, f32),
    top_chrome: f32,
) -> Option<ContextMenu> {
    pickers
        .into_iter()
        .flatten()
        .next()
        .and_then(|list| picker_context_menu(list, surface, cell, text_cell, top_chrome))
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
    theme_key: &str,
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
            ws.palette_profiles.as_deref(),
            ws.palette_bound_only,
            cfg.broadcast_default,
        ))
    } else if let Some(state) = &ws.theme_picker_input {
        Some(theme_picker_list_with_footer(state, cfg, theme_key, tr))
    } else if let Some((query, selected)) = &ws.layout_picker_input {
        Some(layout_picker_list(
            query,
            *selected,
            &ws.layout_picker_entries,
            tr,
        ))
    } else {
        ssh_picker_list_for(ws, cfg, tr)
    }
}

fn picker_result_row(menu: &ContextMenu, cell_h: f32, cursor: (f64, f64)) -> Option<usize> {
    let (px, py) = (cursor.0 as f32, cursor.1 as f32);
    let (x, y) = menu.anchor;
    let row_h = kettle_render::menu::row_height(cell_h);
    let (inset, content_h) = kettle_render::menu::picker_scroll_content(menu.panel_h_clamped);
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

/// What a press on the theme picker's footer lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FooterPress {
    Contrast,
    /// One side on the status line: the picker edits that side's theme.
    Slot(kettle_render::ThemeSlot),
    Footer,
}

/// Where a press at `cursor` lands in `menu`'s footer, measured with the
/// painter's own layout; `None` outside it.
pub(super) fn theme_footer_press(
    menu: &ContextMenu,
    tr: &kettle_i18n::Translator,
    cell: (f32, f32),
    cursor: winit::dpi::PhysicalPosition<f64>,
) -> Option<FooterPress> {
    let layout = menu.picker_footer_layout(tr, cell)?;
    let (x, y) = (cursor.x as f32, cursor.y as f32);
    if layout
        .contrast_button
        .is_some_and(|button| rect_contains(button, x, y))
    {
        Some(FooterPress::Contrast)
    } else if let Some(&(_, slot)) = layout
        .slot_chips
        .iter()
        .find(|(chip, _)| rect_contains(*chip, x, y))
    {
        Some(FooterPress::Slot(slot))
    } else {
        rect_contains(layout.rect, x, y).then_some(FooterPress::Footer)
    }
}

pub(super) fn apply_picker_pointer(
    ws: &mut WindowState,
    cfg: &Config,
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
    let row_h = kettle_render::menu::row_height(cell_h);
    let (_, content_h) = kettle_render::menu::picker_scroll_content(h);
    let selected = if matches!(event, PickerPointer::Press)
        && let Some(row) = picker_result_row(menu, cell_h, (ws.cursor.x, ws.cursor.y))
    {
        row
    } else {
        // Pickers have no arrow strips at their cut edges; only the wheel
        // scrolls them.
        let PickerPointer::Wheel(delta) = event else {
            return false;
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
        // the newly exposed row and keeps keyboard/preview routing in sync. A
        // disabled row (a placeholder) is never selected: the nearest enabled
        // row above it in view is. Nor is the SSH launcher's Save or Remove
        // row, which takes a deliberate Down or a click on it.
        let wheel_rows = match &ws.ssh_input {
            Some((query, _))
                if ws.palette_input.is_none()
                    && ws.theme_picker_input.is_none()
                    && ws.layout_picker_input.is_none() =>
            {
                ssh_wheel_limit(&ssh_rows(query, &cfg.ssh_hosts, &ws.ssh_imported))
            }
            _ => None,
        }
        .unwrap_or(usize::MAX);
        if offset == menu.scroll_offset {
            menu.highlight
        } else {
            let exposed = offset + visible - 1;
            (offset..=exposed)
                .rev()
                .find(|&index| {
                    index < wheel_rows
                        && menu
                            .rows
                            .get(index)
                            .is_some_and(|row| row.enabled && !row.separator)
                })
                .unwrap_or(menu.highlight)
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
        let Some(list) = open_picker_list(
            ws,
            &self.cfg,
            self.theme_picker_key_for(ws),
            &self.ui_text,
            self.update_notice().as_ref(),
        ) else {
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
        let Some(menu) = picker_context_menu(
            list,
            surface,
            cell,
            (
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            ),
            self.picker_top_chrome(ws),
        ) else {
            return false;
        };
        // The theme picker's footer takes its own presses: its contrast
        // button steps the setting, and the rest of it is part of the card.
        if matches!(event, PickerPointer::Press)
            && let Some(press) = theme_footer_press(&menu, &self.ui_text, cell, ws.cursor)
        {
            match press {
                FooterPress::Contrast => self.step_picker_contrast(ws),
                FooterPress::Slot(slot) => self.switch_theme_slot(ws, slot),
                FooterPress::Footer => {}
            }
            return true;
        }
        let activate = matches!(event, PickerPointer::Press)
            && picker_result_row(&menu, cell.1, (ws.cursor.x, ws.cursor.y)).is_some();
        let handled = apply_picker_pointer(ws, &self.cfg, &menu, cell.1, event);
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

/// What Enter in the Save row's name editor does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SshSavePlan {
    /// `ssh-host` cannot hold this name; the editor stays open.
    Invalid,
    /// Write the host, expecting the file to hold `expect` for the name.
    Write {
        name: String,
        expect: kettle_config::ssh_config::HostExpect,
    },
    /// The name is saved with another target: ask before replacing it.
    Replace { name: String, old_target: String },
}

/// The Save of `target` under the name typed as `input`, judged against the
/// configured `hosts`. Names compare ignoring ASCII case, as the launcher's
/// exact row does.
pub(super) fn ssh_save_plan(hosts: &[(String, String)], input: &str, target: &str) -> SshSavePlan {
    use kettle_config::ssh_config::{HostExpect, is_valid_ssh_host_name, is_valid_ssh_target};
    let name = input.trim();
    if !is_valid_ssh_host_name(name) || !is_valid_ssh_target(target) {
        return SshSavePlan::Invalid;
    }
    let name = name.to_string();
    match hosts
        .iter()
        .find(|(saved, _)| saved.eq_ignore_ascii_case(&name))
    {
        None => SshSavePlan::Write {
            name,
            expect: HostExpect::Absent,
        },
        Some((_, old)) if old == target => SshSavePlan::Write {
            name,
            expect: HostExpect::Target(old.clone()),
        },
        Some((_, old)) => SshSavePlan::Replace {
            name,
            old_target: old.clone(),
        },
    }
}

/// The question before the configured host `name` is removed. Cancel is the
/// default and Remove is destructive. The body names the OpenSSH file by the
/// platform's display path, the one the imported rows show.
pub(super) fn ssh_remove_dialog(
    tr: &kettle_i18n::Translator,
    name: String,
    target: String,
) -> ConfirmDialogState {
    ConfirmDialogState {
        heading: tr.confirm_title_remove_ssh_host(&name),
        body: tr.confirm_body_remove_ssh_host(
            &name,
            &target,
            kettle_config::ssh_config::SSH_CONFIG_DISPLAY_PATH,
        ),
        buttons: vec![
            ConfirmButton::Cancel,
            ConfirmButton::Confirm {
                label: tr.text(kettle_i18n::Text::ConfirmButtonRemove).to_string(),
                destructive: true,
            },
        ],
        focus_idx: 0,
        on_confirm: ConfirmAction::RemoveSshHost { name, target },
        scroll: 0,
        armed_press: None,
        space_pressed: None,
    }
}

/// The question before a Save gives the existing host `name` a new target.
/// Cancel is the default; Replace is not destructive, as nothing is lost
/// that saving the old target again would not restore.
pub(super) fn ssh_replace_dialog(
    tr: &kettle_i18n::Translator,
    name: String,
    old_target: String,
    new_target: String,
) -> ConfirmDialogState {
    ConfirmDialogState {
        heading: tr.confirm_title_replace_ssh_host(&name),
        body: tr.confirm_body_replace_ssh_host(&name, &old_target, &new_target),
        buttons: vec![
            ConfirmButton::Cancel,
            ConfirmButton::Confirm {
                label: tr.text(kettle_i18n::Text::ConfirmButtonReplace).to_string(),
                destructive: false,
            },
        ],
        focus_idx: 0,
        on_confirm: ConfirmAction::ReplaceSshHost {
            name,
            old_target,
            new_target,
        },
        scroll: 0,
        armed_press: None,
        space_pressed: None,
    }
}

/// `hosts` after `upsert_ssh_host` wrote `name` and `target`: the first host
/// with the name (ignoring ASCII case) takes them and later ones go, as in
/// the file; a new name goes last.
pub(super) fn upsert_ssh_host_in(hosts: &mut Vec<(String, String)>, name: &str, target: &str) {
    let mut found = false;
    hosts.retain_mut(|(saved, to)| {
        if !saved.eq_ignore_ascii_case(name) {
            return true;
        }
        if found {
            return false;
        }
        found = true;
        *saved = name.to_string();
        *to = target.to_string();
        true
    });
    if !found {
        hosts.push((name.to_string(), target.to_string()));
    }
}

/// `hosts` after `remove_ssh_host` dropped `name` (ignoring ASCII case) with
/// exactly `target`.
pub(super) fn remove_ssh_host_in(hosts: &mut Vec<(String, String)>, name: &str, target: &str) {
    hosts.retain(|(saved, to)| !(saved.eq_ignore_ascii_case(name) && to == target));
}

/// Save `name` with `target` to the config at `path`, then to `hosts`, so the
/// launcher shows it without waiting for the file watcher. A failure is
/// logged by its kind only: never the name, the target or the path.
pub(super) fn save_ssh_host_to(
    path: Option<&std::path::Path>,
    hosts: &mut Vec<(String, String)>,
    name: &str,
    target: &str,
    expect: &kettle_config::ssh_config::HostExpect,
) -> Result<(), std::io::ErrorKind> {
    let written = path.ok_or(std::io::ErrorKind::NotFound).and_then(|path| {
        kettle_config::ssh_config::upsert_ssh_host(path, name, target, expect)
            .map_err(|error| error.kind())
    });
    match written {
        Ok(_) => {
            upsert_ssh_host_in(hosts, name, target);
            Ok(())
        }
        Err(kind) => {
            log::warn!("ssh host not saved: {kind:?}");
            Err(kind)
        }
    }
}

/// Remove `name` with `target` from the config at `path`, then from `hosts`.
/// Logged as [`save_ssh_host_to`] is.
pub(super) fn remove_ssh_host_from(
    path: Option<&std::path::Path>,
    hosts: &mut Vec<(String, String)>,
    name: &str,
    target: &str,
) -> Result<(), std::io::ErrorKind> {
    let written = path.ok_or(std::io::ErrorKind::NotFound).and_then(|path| {
        kettle_config::ssh_config::remove_ssh_host(path, name, target).map_err(|error| error.kind())
    });
    match written {
        Ok(_) => {
            remove_ssh_host_in(hosts, name, target);
            Ok(())
        }
        Err(kind) => {
            log::warn!("ssh host not removed: {kind:?}");
            Err(kind)
        }
    }
}

/// Put an open launcher's selection back on row 0 when `hosts` is not the
/// list it was showing. Called for every window after a reload and after a
/// Save or Remove in any window, since `cfg` is shared: rows shift, and an
/// index that pointed at a host could otherwise land on the Save or Remove
/// row that ends a shorter list. Row 0 is never an action row.
pub(super) fn sync_ssh_hosts(ws: &mut WindowState, hosts: &[(String, String)]) {
    if ws.ssh_hosts_shown == hosts {
        return;
    }
    ws.ssh_hosts_shown = hosts.to_vec();
    if let Some((_, selected)) = ws.ssh_input.as_mut() {
        *selected = 0;
        ws.accessibility_pending = true;
    }
}

/// Open the launcher again after a save, a removal or a failed write, with
/// `query` typed and `status` in its hint lane. `select` names the configured
/// host to select by identity, not by rank; otherwise row 0 is selected.
pub(super) fn reopen_ssh_launcher(
    ws: &mut WindowState,
    cfg: &Config,
    ssh_config: impl FnOnce() -> Option<std::path::PathBuf>,
    query: &str,
    select: Option<(&str, &str)>,
    status: String,
) {
    open_ssh_launcher(ws, cfg, ssh_config);
    let Some((editor, selected)) = ws.ssh_input.as_mut() else {
        return;
    };
    let _ = editor.replace_all(query, crate::modal_input::MAX_MODAL_INPUT_BYTES);
    if let Some((name, target)) = select {
        let rows = ssh_rows(editor, &cfg.ssh_hosts, &ws.ssh_imported);
        let row = rows.iter().position(|row| {
            matches!(row, SshRow::Configured(index)
                if cfg.ssh_hosts.get(*index).is_some_and(|(n, t)| n == name && t == target))
        });
        *selected = row.unwrap_or(0);
    }
    ws.ssh_status = Some(crate::window_state::SshStatus {
        text: status,
        query: editor.text().to_string(),
    });
    ws.accessibility_pending = true;
}

/// What the App still has to do after a Save or Remove step changed the
/// window: everything that needs the event loop or other windows.
#[derive(Debug)]
pub(super) enum SshHostEffect {
    /// The config changed: reset other windows' launcher selections.
    Changed,
    /// Ask before replacing the saved host.
    Ask(Box<ConfirmDialogState>),
    /// The write was refused or failed; notify.
    NotSaved,
    NotRemoved,
    /// Nothing more.
    None,
}

/// Enter in the Save row's name editor, with the editor already taken out of
/// `ws`. An unusable name, or a write that fails, puts the editor back so the
/// typed name is kept. A name saved with another target asks first. A save
/// reopens the launcher on the saved host.
pub(super) fn submit_ssh_host_name(
    ws: &mut WindowState,
    cfg: &mut Config,
    path: Option<&std::path::Path>,
    ssh_config: impl FnOnce() -> Option<std::path::PathBuf>,
    state: TitleEditState,
    tr: &kettle_i18n::Translator,
) -> SshHostEffect {
    let TitleEditTarget::SshHost(target) = &state.target else {
        return SshHostEffect::None;
    };
    let target = target.clone();
    match ssh_save_plan(&cfg.ssh_hosts, state.input.text(), &target) {
        SshSavePlan::Invalid => {
            ws.editing_title = Some(state);
            SshHostEffect::NotSaved
        }
        SshSavePlan::Replace { name, old_target } => {
            SshHostEffect::Ask(Box::new(ssh_replace_dialog(tr, name, old_target, target)))
        }
        SshSavePlan::Write { name, expect } => {
            if save_ssh_host_to(path, &mut cfg.ssh_hosts, &name, &target, &expect).is_err() {
                ws.editing_title = Some(state);
                return SshHostEffect::NotSaved;
            }
            let status = tr.ssh_status_saved(&name);
            reopen_ssh_launcher(ws, cfg, ssh_config, &name, Some((&name, &target)), status);
            SshHostEffect::Changed
        }
    }
}

/// A confirmed Replace or Remove. A Replace writes only if the file still
/// gives the name the target the question showed. Success reopens the
/// launcher as a Save or a removal does; a failure reopens it on what was
/// typed (the new target, or the name) with a "not saved" or "not removed"
/// status, so the failure shows even with notifications off.
pub(super) fn confirm_ssh_host_change(
    ws: &mut WindowState,
    cfg: &mut Config,
    path: Option<&std::path::Path>,
    ssh_config: impl FnOnce() -> Option<std::path::PathBuf>,
    action: ConfirmAction,
    tr: &kettle_i18n::Translator,
) -> SshHostEffect {
    use kettle_config::ssh_config::HostExpect;
    match action {
        ConfirmAction::ReplaceSshHost {
            name,
            old_target,
            new_target,
        } => {
            let expect = HostExpect::Target(old_target);
            if save_ssh_host_to(path, &mut cfg.ssh_hosts, &name, &new_target, &expect).is_err() {
                let status = tr.ssh_status_not_saved(&name);
                reopen_ssh_launcher(ws, cfg, ssh_config, &new_target, None, status);
                return SshHostEffect::NotSaved;
            }
            let status = tr.ssh_status_saved(&name);
            reopen_ssh_launcher(
                ws,
                cfg,
                ssh_config,
                &name,
                Some((&name, &new_target)),
                status,
            );
            SshHostEffect::Changed
        }
        ConfirmAction::RemoveSshHost { name, target } => {
            if remove_ssh_host_from(path, &mut cfg.ssh_hosts, &name, &target).is_err() {
                let status = tr.ssh_status_not_removed(&name);
                reopen_ssh_launcher(ws, cfg, ssh_config, &name, None, status);
                return SshHostEffect::NotRemoved;
            }
            let status = tr.ssh_status_removed(&name);
            reopen_ssh_launcher(ws, cfg, ssh_config, "", None, status);
            SshHostEffect::Changed
        }
        _ => SshHostEffect::None,
    }
}

impl App {
    /// The SSH launcher's Save row: close it and ask for a name.
    pub(super) fn begin_ssh_host_save(&mut self, ws: &mut WindowState, target: &str) {
        self.close_all_modals(ws);
        ws.editing_title = begin_ssh_host_name_edit(target);
        ws.pending_resize = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// The SSH launcher's Remove row: close it and ask, Cancel first.
    pub(super) fn confirm_ssh_host_removal(
        &mut self,
        ws: &mut WindowState,
        name: String,
        target: String,
    ) {
        self.close_all_modals(ws);
        self.install_confirm_dialog(ws, ssh_remove_dialog(&self.ui_text, name, target));
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// The config file Save and Remove write: the active one, which is the
    /// profile file under `--profile`.
    fn ssh_host_config_path(&self) -> Option<std::path::PathBuf> {
        self.config_path
            .clone()
            .or_else(kettle_config::Config::default_path)
    }

    /// Enter in the Save row's name editor.
    pub(super) fn apply_ssh_host_name(&mut self, ws: &mut WindowState, state: TitleEditState) {
        let path = self.ssh_host_config_path();
        self.close_all_modals(ws);
        let tr = self.ui_text;
        let effect = submit_ssh_host_name(
            ws,
            &mut self.cfg,
            path.as_deref(),
            kettle_config::ssh_config::user_ssh_config_path,
            state,
            &tr,
        );
        self.finish_ssh_host_effect(ws, effect);
    }

    /// A confirmed Replace or Remove question.
    pub(super) fn run_ssh_host_confirm(&mut self, ws: &mut WindowState, action: ConfirmAction) {
        let path = self.ssh_host_config_path();
        self.close_all_modals(ws);
        let tr = self.ui_text;
        let effect = confirm_ssh_host_change(
            ws,
            &mut self.cfg,
            path.as_deref(),
            kettle_config::ssh_config::user_ssh_config_path,
            action,
            &tr,
        );
        self.finish_ssh_host_effect(ws, effect);
    }

    fn finish_ssh_host_effect(&mut self, ws: &mut WindowState, effect: SshHostEffect) {
        use kettle_i18n::Text;
        match effect {
            SshHostEffect::Changed => {
                // `cfg` is shared: every other open launcher's rows moved.
                for sibling in self.windows.values_mut() {
                    sync_ssh_hosts(sibling, &self.cfg.ssh_hosts);
                }
            }
            SshHostEffect::Ask(dialog) => self.install_confirm_dialog(ws, *dialog),
            SshHostEffect::NotSaved => fire_notify(
                self.ui_text.text(Text::NotifySshHostNotSaved),
                self.ui_text.text(Text::NotifySshHostNotSavedBody),
            ),
            SshHostEffect::NotRemoved => fire_notify(
                self.ui_text.text(Text::NotifySshHostNotRemoved),
                self.ui_text.text(Text::NotifySshHostNotRemovedBody),
            ),
            SshHostEffect::None => {}
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }
}
