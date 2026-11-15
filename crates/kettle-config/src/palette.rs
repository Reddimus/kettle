//! Command-palette model: the list of user-facing actions and a fuzzy
//! ranking over their labels. Pure and UI-agnostic — the app drives the
//! overlay; this just answers "given this query, which commands, in what
//! order?". Reuses [`crate::fuzzy`].

use kettle_i18n::{Language, Text, Translator};

use crate::BroadcastDefault;
use crate::fuzzy;
use crate::keybinds::Action;

/// A heading of the empty palette. Groups come in the order people reach for
/// them, tabs first, and a typed query ignores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PaletteGroup {
    Tabs,
    Splits,
    Panes,
    Windows,
    SearchAndSelection,
    Scrolling,
    TextSize,
    Appearance,
    Terminal,
    BroadcastInput,
    Previews,
    /// The SSH launcher, saved layouts and profiles: other ways to start.
    LayoutsProfilesSsh,
    Settings,
    Help,
}

impl PaletteGroup {
    /// The heading's text.
    pub fn text(self) -> Text {
        match self {
            Self::Tabs => Text::PaletteGroupTabs,
            Self::Splits => Text::PaletteGroupSplits,
            Self::Panes => Text::PaletteGroupPanes,
            Self::Windows => Text::PaletteGroupWindows,
            Self::SearchAndSelection => Text::PaletteGroupSearchAndSelection,
            Self::Scrolling => Text::PaletteGroupScrolling,
            Self::TextSize => Text::PaletteGroupTextSize,
            Self::Appearance => Text::PaletteGroupAppearance,
            Self::Terminal => Text::PaletteGroupTerminal,
            Self::BroadcastInput => Text::PaletteGroupBroadcastInput,
            Self::Previews => Text::PaletteGroupPreviews,
            Self::LayoutsProfilesSsh => Text::PaletteGroupLayoutsProfilesSsh,
            Self::Settings => Text::PaletteGroupSettings,
            Self::Help => Text::PaletteGroupHelp,
        }
    }
}

/// The palette command registry: each command's group, its catalogue label
/// and the [`Action`] it dispatches. Groups are contiguous and in
/// [`PaletteGroup`] order, and inside each the commands people reach for most
/// come first, bound split directions before unbound ones. This order is the
/// empty palette's and the final tie-break of a query's.
pub fn grouped_commands() -> Vec<(PaletteGroup, Text, Action)> {
    use Action::*;
    use PaletteGroup as G;
    use Text as T;
    let mut commands = vec![
        (G::Tabs, T::PaletteNewTab, NewTab),
        (G::Tabs, T::PaletteCloseTab, CloseTab),
        (G::Tabs, T::PaletteNextTab, NextTab),
        (G::Tabs, T::PalettePrevTab, PrevTab),
        (G::Tabs, T::PaletteUndoCloseTab, UndoCloseTab),
        (G::Tabs, T::PaletteDuplicateTab, DuplicateTab),
        (G::Tabs, T::PaletteRenameTab, EditTabTitle),
        (G::Tabs, T::PaletteMoveTabLeft, MoveTabLeft),
        (G::Tabs, T::PaletteMoveTabRight, MoveTabRight),
        (G::Splits, T::PaletteSplitRight, SplitRight),
        (G::Splits, T::PaletteSplitDown, SplitDown),
        (G::Splits, T::PaletteSplitAuto, SplitAuto),
        (G::Splits, T::PaletteSplitLeft, SplitLeft),
        (G::Splits, T::PaletteSplitUp, SplitUp),
        (G::Panes, T::PaletteDuplicatePane, DuplicatePane),
        (G::Panes, T::PaletteClosePane, ClosePane),
        (G::Panes, T::PaletteEqualizeSplits, EqualizeSplits),
        (G::Panes, T::PaletteToggleZoom, ToggleZoom),
        (G::Panes, T::PaletteScaledZoom, ScaledZoom),
        (G::Panes, T::PaletteFocusNext, FocusNext),
        (G::Panes, T::PaletteFocusPrev, FocusPrev),
        (G::Panes, T::PaletteMovePaneLeft, MovePaneLeft),
        (G::Panes, T::PaletteMovePaneRight, MovePaneRight),
        (G::Panes, T::PaletteMovePaneUp, MovePaneUp),
        (G::Panes, T::PaletteMovePaneDown, MovePaneDown),
        (G::Panes, T::PaletteRotateCw, RotateCw),
        (G::Panes, T::PaletteRotateCcw, RotateCcw),
        (G::Panes, T::PaletteTogglePaneReadOnly, TogglePaneReadOnly),
        (G::Panes, T::PaletteSendNewline, SendNewline),
        (G::Windows, T::PaletteNewWindow, NewWindow),
        (G::Windows, T::PaletteCloseWindow, CloseWindow),
        (G::Windows, T::PaletteMoveTabToNewWindow, MoveTabToNewWindow),
        (G::Windows, T::PaletteMoveTabToWindow, MoveTabToWindow),
        (G::Windows, T::PaletteToggleFullscreen, ToggleFullscreen),
        (G::Windows, T::QuickTerminalToggle, ToggleQuickTerminal),
        (
            G::Windows,
            T::PaletteToggleWindowVisibility,
            ToggleWindowVisibility,
        ),
        (G::SearchAndSelection, T::PaletteStartSearch, StartSearch),
        (G::SearchAndSelection, T::PaletteHintMode, HintMode),
        (G::SearchAndSelection, T::PaletteCopy, Copy),
        (G::SearchAndSelection, T::PalettePaste, Paste),
        (G::SearchAndSelection, T::PalettePastePrimary, PastePrimary),
        (G::SearchAndSelection, T::PaletteSelectAll, SelectAll),
        (G::SearchAndSelection, T::PaletteSelectToTop, SelectToTop),
        (
            G::SearchAndSelection,
            T::PaletteSelectToBottom,
            SelectToBottom,
        ),
        (
            G::SearchAndSelection,
            T::PaletteSelectCommandOutput,
            SelectCommandOutput,
        ),
        (G::SearchAndSelection, T::PaletteToggleViMode, ToggleViMode),
        (G::Scrolling, T::PaletteScrollPageUp, ScrollPageUp),
        (G::Scrolling, T::PaletteScrollPageDown, ScrollPageDown),
        (G::Scrolling, T::PaletteScrollPageUpHalf, ScrollPageUpHalf),
        (
            G::Scrolling,
            T::PaletteScrollPageDownHalf,
            ScrollPageDownHalf,
        ),
        (G::Scrolling, T::PaletteScrollLineUp, ScrollLineUp),
        (G::Scrolling, T::PaletteScrollLineDown, ScrollLineDown),
        (G::Scrolling, T::PaletteScrollToTop, ScrollToTop),
        (G::Scrolling, T::PaletteScrollToBottom, ScrollToBottom),
        (G::Scrolling, T::PaletteJumpPrevPrompt, JumpPrevPrompt),
        (G::Scrolling, T::PaletteJumpNextPrompt, JumpNextPrompt),
        (G::TextSize, T::PaletteIncreaseFontSize, IncreaseFontSize),
        (G::TextSize, T::PaletteDecreaseFontSize, DecreaseFontSize),
        (G::TextSize, T::PaletteResetFontSize, ResetFontSize),
        (G::TextSize, T::PaletteZoomInAll, ZoomInAll),
        (G::TextSize, T::PaletteZoomOutAll, ZoomOutAll),
        (G::TextSize, T::PaletteZoomNormalAll, ZoomNormalAll),
        (G::Appearance, T::PaletteOpenThemePicker, OpenThemePicker),
        (G::Appearance, T::PaletteNextTheme, NextTheme),
        (G::Appearance, T::PalettePrevTheme, PrevTheme),
        (G::Appearance, T::PaletteToggleLightDark, ToggleLightDark),
        (G::Appearance, T::PaletteToggleScrollbar, ToggleScrollbar),
        (G::Terminal, T::PaletteClearHistory, ClearHistory),
        (G::Terminal, T::PaletteReset, Reset),
        (G::Terminal, T::PaletteResetAndClear, ResetAndClear),
        (G::Terminal, T::PaletteToggleSessionLog, ToggleSessionLog),
        (G::Terminal, T::PaletteTakeScreenshot, TakeScreenshot),
        (
            G::BroadcastInput,
            T::PaletteToggleBroadcastAll,
            ToggleBroadcastAll,
        ),
        (
            G::BroadcastInput,
            T::PaletteToggleBroadcastAll,
            ToggleBroadcastPanesInTab,
        ),
        (
            G::BroadcastInput,
            T::PaletteToggleBroadcastGroup,
            ToggleBroadcastGroup,
        ),
        (
            G::BroadcastInput,
            T::PaletteToggleBroadcastWindow,
            ToggleBroadcastWindow,
        ),
        (
            G::BroadcastInput,
            T::PaletteToggleBroadcastOff,
            ToggleBroadcastOff,
        ),
        (G::BroadcastInput, T::PaletteCreateGroup, CreateGroup),
        (G::BroadcastInput, T::PaletteEditPaneGroup, EditPaneGroup),
        (G::BroadcastInput, T::PaletteGroupTab, GroupTab),
        (G::BroadcastInput, T::PaletteGroupWindow, GroupWindow),
        (G::BroadcastInput, T::PaletteUngroupTab, UngroupTab),
        (G::BroadcastInput, T::PaletteUngroupWindow, UngroupWindow),
        (G::BroadcastInput, T::PaletteGroupAll, GroupAll),
        (G::BroadcastInput, T::PaletteUngroupAll, UngroupAll),
        (G::BroadcastInput, T::PaletteToggleGroupAll, ToggleGroupAll),
        (G::BroadcastInput, T::PaletteToggleGroupTab, ToggleGroupTab),
        (
            G::BroadcastInput,
            T::PaletteToggleGroupWindow,
            ToggleGroupWindow,
        ),
        (G::Previews, T::PaletteOpenMediaShelf, OpenMediaShelf),
        (G::Previews, T::PalettePreviewLink, PreviewLink),
        (
            G::Previews,
            T::PalettePreviewClipboardPath,
            PreviewClipboardPath,
        ),
        (G::Previews, T::PalettePreviewNext, PreviewNext),
        (G::Previews, T::PalettePreviewPrevious, PreviewPrevious),
        (
            G::Previews,
            T::PalettePreviewNextDiagram,
            PreviewNextDiagram,
        ),
        (
            G::Previews,
            T::PalettePreviewPreviousDiagram,
            PreviewPreviousDiagram,
        ),
        (G::Previews, T::PaletteClosePreview, ClosePreview),
        (G::Previews, T::PalettePreviewSource, PreviewSource),
        (G::Previews, T::PalettePreviewCanvas, PreviewCanvas),
        (G::Previews, T::PalettePreviewCopy, PreviewCopy),
        (G::Previews, T::PalettePreviewReload, PreviewReload),
        (G::Previews, T::PalettePreviewPlay, PreviewPlay),
        (G::Previews, T::PaletteVideoPlay, VideoPlay),
        (G::Previews, T::PaletteVideoMute, VideoMute),
        (G::Previews, T::PalettePreviewZoomIn, PreviewZoomIn),
        (G::Previews, T::PalettePreviewZoomOut, PreviewZoomOut),
        (G::Previews, T::PalettePreviewFit, PreviewFit),
        (G::Previews, T::PaletteFocusPreview, FocusPreview),
        (
            G::Previews,
            T::PaletteRenderClipboardAsDiagram,
            RenderClipboardAsDiagram,
        ),
        (
            G::Previews,
            T::PaletteRenderSelectionAsDiagram,
            RenderSelectionAsDiagram,
        ),
        (G::LayoutsProfilesSsh, T::PaletteOpenSsh, OpenSsh),
        (G::LayoutsProfilesSsh, T::MenuOpenLayout, OpenLayoutPicker),
        (G::LayoutsProfilesSsh, T::MenuSaveLayout, SaveLayout),
        (G::LayoutsProfilesSsh, T::PaletteNextProfile, NextProfile),
        (G::LayoutsProfilesSsh, T::PalettePrevProfile, PrevProfile),
        (G::Settings, T::PaletteOpenSettings, OpenSettings),
        (G::Settings, T::PaletteEditConfig, EditConfig),
        (G::Settings, T::PaletteReloadConfig, ReloadConfig),
        (
            G::Settings,
            T::PaletteSetScrollbarAlways,
            SetScrollbarAlways,
        ),
        (G::Settings, T::PaletteSetScrollbarAuto, SetScrollbarAuto),
        (G::Settings, T::PaletteSetScrollbarNever, SetScrollbarNever),
        (
            G::Settings,
            T::PaletteSetAskBeforeClosingAlways,
            SetAskBeforeClosingAlways,
        ),
        (
            G::Settings,
            T::PaletteSetAskBeforeClosingBusyOrMultiple,
            SetAskBeforeClosingBusyOrMultiple,
        ),
        (
            G::Settings,
            T::PaletteSetAskBeforeClosingMultiple,
            SetAskBeforeClosingMultiple,
        ),
        (
            G::Settings,
            T::PaletteSetAskBeforeClosingNever,
            SetAskBeforeClosingNever,
        ),
        (G::Settings, T::PaletteToggleCursorBlink, ToggleCursorBlink),
        (
            G::Settings,
            T::PaletteToggleCopyOnSelect,
            ToggleCopyOnSelect,
        ),
        (G::Settings, T::PaletteSetBellOff, SetBellOff),
        (G::Settings, T::PaletteSetBellVisual, SetBellVisual),
        (G::Settings, T::PaletteSetBellAttention, SetBellAttention),
        (G::Settings, T::PaletteSetBellBoth, SetBellBoth),
        (G::Settings, T::PaletteToggleMouseHide, ToggleMouseHide),
        (G::Help, T::PaletteShowKeybinds, ShowKeybinds),
        (G::Help, T::PaletteShowHelp, ShowHelp),
        (G::Help, T::PaletteCopyAgentSetup, CopyAgentSetup),
        (G::Help, T::PaletteAbout, About),
    ];
    // Only X11 and Wayland have a primary selection. Elsewhere the action
    // falls back to an ordinary paste, which the palette already offers.
    if !cfg!(target_os = "linux") {
        commands.retain(|(_, _, action)| *action != PastePrimary);
    }
    commands
}

/// [`grouped_commands`] without the groups.
pub fn commands() -> Vec<(Text, Action)> {
    grouped_commands()
        .into_iter()
        .map(|(_, label, action)| (label, action))
        .collect()
}

/// The palette's commands under `broadcast-default = default`. The tab and
/// window scopes are each listed once. `ToggleBroadcastAll`, the chord's
/// action, stands for the scope it turns on, with that scope's label, so the
/// chord's hint sits on the row it belongs to; the scope's own toggle is then
/// left out. Under `off` the chord can only stop broadcasting, which "Stop
/// broadcasting input" already offers, so it is left out instead.
pub fn grouped_commands_for(default: BroadcastDefault) -> Vec<(PaletteGroup, Text, Action)> {
    let (chord_label, replaced) = match default {
        BroadcastDefault::Group => (
            Some(Text::PaletteToggleBroadcastAll),
            Action::ToggleBroadcastPanesInTab,
        ),
        BroadcastDefault::All => (
            Some(Text::PaletteToggleBroadcastWindow),
            Action::ToggleBroadcastWindow,
        ),
        BroadcastDefault::Off => (None, Action::ToggleBroadcastAll),
    };
    grouped_commands()
        .into_iter()
        .filter(|(_, _, action)| *action != replaced)
        .filter_map(|(group, label, action)| match action {
            Action::ToggleBroadcastAll => chord_label.map(|label| (group, label, action)),
            _ => Some((group, label, action)),
        })
        .collect()
}

/// [`grouped_commands_for`] without the groups.
pub fn commands_for(default: BroadcastDefault) -> Vec<(Text, Action)> {
    grouped_commands_for(default)
        .into_iter()
        .map(|(_, label, action)| (label, action))
        .collect()
}

/// Indices into `cmds` that match `query` against their labels in `tr`'s
/// language, best first. Outside English, the English label matches too, so a
/// command name from the docs still finds its command. Ties (and an empty
/// query) preserve registry order, so the palette is stable as you type.
/// Every row counts as having no shortcut; the palette itself ranks
/// [`PaletteEntry`] rows that say which do.
pub fn rank(query: &str, cmds: &[(Text, Action)], tr: &Translator) -> Vec<usize> {
    let entries: Vec<PaletteEntry> = cmds
        .iter()
        .map(|(label, action)| PaletteEntry::new(PaletteLabel::Text(*label), action.clone()))
        .collect();
    rank_entries(query, &entries, tr)
}

/// The secondary text the palette paints after a command's label: only what
/// the label cannot say, such as which way a split's divider runs or that a
/// command needs shell integration. Most commands have none, and a profile or
/// update row never has one.
pub fn description(action: &Action) -> Option<Text> {
    use Action::*;
    use Text as T;
    Some(match action {
        SplitRight => T::PaletteDescSplitRight,
        SplitDown => T::PaletteDescSplitDown,
        SplitAuto => T::PaletteDescSplitAuto,
        NewTab => T::PaletteDescNewTab,
        UndoCloseTab => T::PaletteDescUndoCloseTab,
        DuplicateTab => T::PaletteDescDuplicateTab,
        DuplicatePane => T::PaletteDescDuplicatePane,
        EqualizeSplits => T::PaletteDescEqualizeSplits,
        SelectCommandOutput => T::PaletteDescNeedsShellIntegration,
        // The prompt rows have their own message: their labels already name
        // the shell, and in Spanish they are long.
        JumpPrevPrompt | JumpNextPrompt => T::PaletteDescPromptNeedsShellIntegration,
        ShowHelp => T::PaletteDescShowHelp,
        _ => return None,
    })
}

/// A palette row's label: catalogue text, or text built at run time, such
/// as one naming a release, with its English form so a query in English
/// still finds it in another language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteLabel {
    Text(Text),
    Dynamic { shown: String, english: String },
}

impl PaletteLabel {
    /// The label in `tr`'s language.
    pub fn shown<'a>(&'a self, tr: &Translator) -> &'a str {
        match self {
            Self::Text(text) => tr.text(*text),
            Self::Dynamic { shown, .. } => shown,
        }
    }

    fn english<'a>(&'a self, english: &Translator) -> &'a str {
        match self {
            Self::Text(text) => english.text(*text),
            Self::Dynamic { english, .. } => english,
        }
    }
}

/// One row the palette ranks: its label, the action it runs, and whether
/// that action has a shortcut to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    pub label: PaletteLabel,
    pub action: Action,
    /// Between two rows the query matches equally well, the one with a
    /// shortcut ranks first, so `split` lists Split right and Split down,
    /// which have one, above Split left and Split up, which do not.
    pub bound: bool,
    /// The heading the row sits under in the empty palette; `None` lists it
    /// under no heading.
    pub group: Option<PaletteGroup>,
}

impl PaletteEntry {
    /// A row with no shortcut.
    pub fn new(label: PaletteLabel, action: Action) -> Self {
        Self {
            label,
            action,
            bound: false,
            group: None,
        }
    }
}

/// One row [`rank_matches`] keeps: its index and the positions of the
/// matched chars in its displayed label, from the alignment that ranked it.
/// `None` when only the English alias of a translated label matched, or only
/// the description did, so a highlight never lands on chars of the wrong
/// text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedEntry {
    pub index: usize,
    pub matched_chars: Option<Vec<usize>>,
}

/// [`rank`] over rows that may carry run-time labels.
pub fn rank_entries(query: &str, entries: &[PaletteEntry], tr: &Translator) -> Vec<usize> {
    rank_matches(query, entries, tr)
        .into_iter()
        .map(|entry| entry.index)
        .collect()
}

/// A kept row: its index, its (bonus, score) and the positions it marks.
type Scored = (usize, (i32, i32), Option<Vec<usize>>);

/// Label rows below which a query also searches descriptions.
const DESCRIPTION_PASS_BELOW: usize = 3;
/// Shortest query, in chars, that searches descriptions.
const DESCRIPTION_PASS_MIN_CHARS: usize = 4;

/// [`rank_entries`] with each kept row's matched chars.
///
/// A row matches strongly when each scattered piece of the query starts a
/// word, or a query of four or more chars runs contiguously anywhere (see
/// [`fuzzy::align_strong`]). While any row
/// matches strongly, rows the query only lies scattered through are left out,
/// so `font` lists the font-size commands and not `Focus next pane`; with no
/// strong match every subsequence match is listed.
///
/// Rows are ordered by how well the query lies in the label, then rows with a
/// shortcut first, then shorter labels, then registry order. An empty query
/// lists every row in registry order.
///
/// A query of four or more chars that matches fewer than three labels
/// strongly also finds rows whose [`description`] it matches strongly,
/// appended in registry order without highlights. Such a row counts as a
/// strong match, so labels the query only lies scattered through drop out:
/// `divider` lists Split right and Split down, and `split`, which six labels
/// match, gains nothing.
pub fn rank_matches(query: &str, entries: &[PaletteEntry], tr: &Translator) -> Vec<RankedEntry> {
    if query.is_empty() {
        return (0..entries.len())
            .map(|index| RankedEntry {
                index,
                matched_chars: Some(Vec::new()),
            })
            .collect();
    }
    let english = Translator::new(Language::En);
    let alias = tr.language() != Language::En;
    // The best (bonus, score) over the shown label and its English alias,
    // with the shown label's positions.
    let best = |shown: Option<fuzzy::Alignment>, alias: Option<fuzzy::Alignment>| {
        let key = |a: &fuzzy::Alignment| (a.bonus, a.score);
        let key = shown.as_ref().map(key).max(alias.as_ref().map(key))?;
        Some((key, shown.map(|a| a.positions)))
    };
    let matches: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let shown = entry.label.shown(tr);
            let alias = alias.then(|| entry.label.english(&english));
            let any = best(
                fuzzy::align(query, shown),
                alias.and_then(|alias| fuzzy::align(query, alias)),
            )?;
            let strong = best(
                fuzzy::align_strong(query, shown),
                alias.and_then(|alias| fuzzy::align_strong(query, alias)),
            );
            Some((index, strong, any))
        })
        .collect();
    let strong_labels = matches
        .iter()
        .filter(|(_, strong, _)| strong.is_some())
        .count();
    // Rows found only through their description, when the labels leave few:
    // each must match strongly, so it counts as a strong match below.
    let described: Vec<usize> = if strong_labels < DESCRIPTION_PASS_BELOW
        && query.chars().count() >= DESCRIPTION_PASS_MIN_CHARS
    {
        entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                !matches
                    .iter()
                    .any(|(matched, strong, _)| matched == index && strong.is_some())
                    && description(&entry.action).is_some_and(|text| {
                        fuzzy::align_strong(query, tr.text(text)).is_some()
                            || (alias && fuzzy::align_strong(query, english.text(text)).is_some())
                    })
            })
            .map(|(index, _)| index)
            .collect()
    } else {
        Vec::new()
    };
    let strong_only = strong_labels > 0 || !described.is_empty();
    let mut scored: Vec<Scored> = matches
        .into_iter()
        .filter_map(|(index, strong, any)| {
            let (key, positions) = if strong_only { strong? } else { any };
            Some((index, key, positions))
        })
        .collect();
    scored.sort_by(|a, b| {
        let ((bonus_a, score_a), (bonus_b, score_b)) = (a.1, b.1);
        bonus_b
            .cmp(&bonus_a)
            .then(entries[b.0].bound.cmp(&entries[a.0].bound))
            .then(score_b.cmp(&score_a))
            .then(a.0.cmp(&b.0))
    });
    scored
        .into_iter()
        .map(|(index, _, matched_chars)| RankedEntry {
            index,
            matched_chars,
        })
        .chain(described.into_iter().map(|index| RankedEntry {
            index,
            matched_chars: None,
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_positions_follow_fuzzy_unicode_and_spanish_labels() {
        for (query, label, expected) in [
            // The `t` that starts `tab`, not the one ending `Split`.
            ("spt", "Split tab", Some(vec![0, 1, 6])),
            ("AÑO", "Mi año", Some(vec![3, 4, 5])),
            ("ct", "Cerrar pestaña", Some(vec![0, 10])),
            ("İ", "İstanbul", Some(vec![0])),
            ("界a", "世界a", Some(vec![1, 2])),
            ("", "Cambiar tema", Some(vec![])),
            ("xyz", "Cambiar tema", None),
        ] {
            assert_eq!(fuzzy::matched_char_positions(query, label), expected);
            assert_eq!(
                fuzzy::matched_char_positions(query, label).is_some(),
                fuzzy::score(query, label).is_some()
            );
        }
    }

    /// Select Command Output is a palette command under every broadcast
    /// default, labelled in both languages, and a query for its words finds
    /// it first, in English from a Spanish palette as well.
    #[test]
    fn select_command_output_is_a_palette_command() {
        for default in [
            BroadcastDefault::Group,
            BroadcastDefault::All,
            BroadcastDefault::Off,
        ] {
            assert!(commands_for(default).contains(&(
                Text::PaletteSelectCommandOutput,
                Action::SelectCommandOutput
            )));
        }
        let en = Translator::new(Language::En);
        let es = Translator::new(Language::Es);
        assert_eq!(
            en.text(Text::PaletteSelectCommandOutput),
            "Select output of last command"
        );
        assert_eq!(
            es.text(Text::PaletteSelectCommandOutput),
            "Seleccionar la salida del último comando"
        );
        let cmds = commands();
        for (query, tr) in [
            ("select output", &en),
            ("salida del último", &es),
            ("select output", &es),
        ] {
            let first = rank(query, &cmds, tr)[0];
            assert_eq!(cmds[first].1, Action::SelectCommandOutput, "{query:?}");
        }
    }

    #[test]
    fn palette_includes_every_user_facing_action() {
        // Drift guard: a new Action variant must not reach only the keymap
        // and `--list-actions` while the palette (Ctrl+Shift+K) misses it.
        // `action_names_round_trip_through_from_name` (keybinds.rs) guards
        // the same kind of drift between `from_name` and `action_names`.
        // Every action *intended* for the palette must have at least one
        // entry. Variants excluded on purpose (geometric / parametric /
        // palette-itself) are listed below, so a future exclusion is a
        // conscious choice, not an oversight.
        use Action::*;
        let cmds = commands();
        // Linear lookup over `cmds` (small list; readability wins). Action
        // doesn't derive Hash, so HashSet isn't an option without an
        // upstream change to keybinds.rs that this test shouldn't drag in.
        let listed: Vec<&Action> = cmds.iter().map(|(_, a)| a).collect();
        // Variants we *deliberately* skip in the palette: geometric
        // motions (focus/resize directions — keyboard-only), parametric
        // (GotoTab(N) — can't enumerate), the palette itself (`Ctrl+
        // Shift+K` → CommandPalette would loop weirdly).
        let mut excluded: Vec<Action> = vec![
            FocusUp,
            FocusDown,
            FocusLeft,
            FocusRight,
            ResizeUp,
            ResizeDown,
            ResizeLeft,
            ResizeRight,
            GotoTab(0),     // placeholder for the parametric family
            NewTabShell(0), // dropdown-parity: parametric, like GotoTab
            CommandPalette,
            // OpenContextMenu is the right-click handler
            // itself — surfacing it inside the palette would be a
            // weird self-reference (palette → context menu → palette
            // entry). Triggered by the mouse only, not user-typed.
            OpenContextMenu,
            // Terminator-parity actions excluded from the palette
            // because they're either parametric (no enumeration target),
            // tied to inline overlays (the window and pane title editors
            // open their own input prompts; the tab one is listed anyway,
            // as "Rename tab…", so a rename needs no right-click), or send raw
            // text to the focused PTY (insert-number) which doesn't fit the
            // palette's "do a thing" model. They remain reachable via
            // keybinds.
            EditWindowTitle,
            EditPaneTitle,
            InsertPaneNumber,
            InsertPanePadded,
            InsertPaneName,
            OpenCwdInFileManager,
            // Runtime profile discovery supplies these in kettle-ui.
            LaunchProfile(String::new()),
            OpenProfilesDirectory,
            // Parametric, and the payload IS the command — there is nothing to
            // enumerate and nothing a fixed row could send. Reachable by
            // keybind, which is the point of it.
            SendText(String::new()),
            // The update notice's two actions name a release, so they are
            // not in this static list: the app adds them, labelled with the
            // tag, only while a notice is active (`palette_entries`).
            OpenUpdate,
            DismissUpdate,
        ];
        // Outside Linux there is no primary selection; `commands()` drops it.
        if !cfg!(target_os = "linux") {
            excluded.push(PastePrimary);
        }
        // Keep this registry-coverage list in sync with the exhaustive match below.
        let every_action: Vec<Action> = vec![
            Copy,
            Paste,
            SelectAll,
            SelectToTop,
            SelectToBottom,
            NewTab,
            CloseTab,
            NextTab,
            PrevTab,
            MoveTabLeft,
            MoveTabRight,
            SplitRight,
            SplitDown,
            SplitAuto,
            SplitLeft,
            SplitUp,
            ClosePane,
            CloseWindow,
            NewWindow,
            FocusNext,
            FocusPrev,
            FocusUp,
            FocusDown,
            FocusLeft,
            FocusRight,
            ResizeUp,
            ResizeDown,
            ResizeLeft,
            ResizeRight,
            EqualizeSplits,
            ToggleZoom,
            ScaledZoom,
            ShowHelp,
            ShowKeybinds,
            SendNewline,
            OpenLayoutPicker,
            SaveLayout,
            LaunchProfile(String::new()),
            OpenProfilesDirectory,
            OpenThemePicker,
            OpenMediaShelf,
            PreviewLink,
            PreviewClipboardPath,
            PreviewNext,
            PreviewPrevious,
            PreviewNextDiagram,
            PreviewPreviousDiagram,
            ClosePreview,
            PreviewSource,
            PreviewCanvas,
            PreviewCopy,
            PreviewReload,
            PreviewPlay,
            VideoPlay,
            VideoMute,
            PreviewZoomIn,
            PreviewZoomOut,
            PreviewFit,
            FocusPreview,
            RenderClipboardAsDiagram,
            RenderSelectionAsDiagram,
            CopyAgentSetup,
            EditConfig,
            SetScrollbarAlways,
            SetScrollbarAuto,
            SetScrollbarNever,
            SetAskBeforeClosingAlways,
            SetAskBeforeClosingBusyOrMultiple,
            SetAskBeforeClosingMultiple,
            SetAskBeforeClosingNever,
            ToggleCursorBlink,
            ToggleCopyOnSelect,
            SetBellOff,
            SetBellVisual,
            SetBellAttention,
            SetBellBoth,
            ToggleMouseHide,
            IncreaseFontSize,
            DecreaseFontSize,
            ResetFontSize,
            StartSearch,
            ToggleBroadcastAll,
            ToggleBroadcastGroup,
            ToggleBroadcastWindow,
            ToggleBroadcastPanesInTab,
            ToggleBroadcastOff,
            ToggleQuickTerminal,
            ToggleFullscreen,
            Reset,
            ClearHistory,
            ScrollPageUp,
            ScrollPageDown,
            ScrollLineUp,
            ScrollLineDown,
            ScrollToTop,
            ScrollToBottom,
            JumpPrevPrompt,
            JumpNextPrompt,
            SelectCommandOutput,
            ToggleViMode,
            OpenSsh,
            ReloadConfig,
            CommandPalette,
            HintMode,
            NextTheme,
            PrevTheme,
            ToggleLightDark,
            ToggleSessionLog,
            TogglePaneReadOnly,
            TakeScreenshot,
            CreateGroup,
            GroupTab,
            GroupWindow,
            UngroupTab,
            UngroupWindow,
            GroupAll,
            UngroupAll,
            ToggleGroupAll,
            ToggleGroupTab,
            ToggleGroupWindow,
            OpenContextMenu,
            UndoCloseTab,
            DuplicateTab,
            DuplicatePane,
            // Terminator-parity actions.
            RotateCw,
            RotateCcw,
            ToggleScrollbar,
            EditWindowTitle,
            EditTabTitle,
            EditPaneTitle,
            InsertPaneNumber,
            InsertPanePadded,
            InsertPaneName,
            OpenCwdInFileManager,
            NextProfile,
            PrevProfile,
            ZoomInAll,
            ZoomOutAll,
            ZoomNormalAll,
            ResetAndClear,
            ScrollPageUpHalf,
            ScrollPageDownHalf,
            PastePrimary,
            ToggleWindowVisibility,
            MoveTabToWindow,
            MoveTabToNewWindow,
            EditPaneGroup,
            OpenSettings,
            OpenUpdate,
            DismissUpdate,
            GotoTab(0),
            // Dropdown parity: About is palette-listed; NewTabShell is
            // parametric like GotoTab (the dropdown + Ctrl+Shift+N reach it).
            About,
            NewTabShell(0),
            // Same sentinel spelling the `excluded` row uses: that check is
            // value equality, so the two have to agree exactly (as GotoTab(0)
            // and NewTabShell(0) already do).
            SendText(String::new()),
            MovePaneLeft,
            MovePaneRight,
            MovePaneUp,
            MovePaneDown,
        ];
        // New Action variants require a match arm; also add them to every_action.
        for a in &every_action {
            match a {
                SelectAll
                | SelectToTop
                | SelectToBottom
                | Copy
                | Paste
                | NewTab
                | CloseTab
                | NextTab
                | PrevTab
                | MoveTabLeft
                | MoveTabRight
                | SplitRight
                | SplitDown
                | SplitAuto
                | SplitLeft
                | SplitUp
                | ClosePane
                | CloseWindow
                | NewWindow
                | FocusNext
                | FocusPrev
                | FocusUp
                | FocusDown
                | FocusLeft
                | FocusRight
                | ResizeUp
                | ResizeDown
                | ResizeLeft
                | ResizeRight
                | EqualizeSplits
                | ToggleZoom
                | ScaledZoom
                | ShowHelp
                | ShowKeybinds
                | SendNewline
                | SaveLayout
                | LaunchProfile(_)
                | OpenProfilesDirectory
                | OpenLayoutPicker
                | OpenThemePicker
                | OpenMediaShelf
                | PreviewLink
                | PreviewClipboardPath
                | PreviewNext
                | PreviewPrevious
                | PreviewNextDiagram
                | PreviewPreviousDiagram
                | ClosePreview
                | PreviewSource
                | PreviewCanvas
                | PreviewCopy
                | PreviewReload
                | PreviewPlay
                | VideoPlay
                | VideoMute
                | PreviewZoomIn
                | PreviewZoomOut
                | PreviewFit
                | FocusPreview
                | RenderClipboardAsDiagram
                | RenderSelectionAsDiagram
                | CopyAgentSetup
                | EditConfig
                | SetScrollbarAlways
                | SetScrollbarAuto
                | SetScrollbarNever
                | SetAskBeforeClosingAlways
                | SetAskBeforeClosingBusyOrMultiple
                | SetAskBeforeClosingMultiple
                | SetAskBeforeClosingNever
                | ToggleCursorBlink
                | ToggleCopyOnSelect
                | SetBellOff
                | SetBellVisual
                | SetBellAttention
                | SetBellBoth
                | ToggleMouseHide
                | IncreaseFontSize
                | DecreaseFontSize
                | ResetFontSize
                | StartSearch
                | ToggleSearchRegex
                | ToggleBroadcastAll
                | ToggleBroadcastGroup
                | ToggleBroadcastWindow
                | ToggleBroadcastPanesInTab
                | ToggleBroadcastOff
                | ToggleQuickTerminal
                | ToggleFullscreen
                | Reset
                | ClearHistory
                | ScrollPageUp
                | ScrollPageDown
                | ScrollLineUp
                | ScrollLineDown
                | ScrollToTop
                | ScrollToBottom
                | JumpPrevPrompt
                | JumpNextPrompt
                | SelectCommandOutput
                | OpenSsh
                | ReloadConfig
                | CommandPalette
                | HintMode
                | NextTheme
                | PrevTheme
                | ToggleLightDark
                | ToggleSessionLog
                | TakeScreenshot
                | CreateGroup
                | GroupTab
                | GroupWindow
                | GroupAll
                | UngroupAll
                | ToggleGroupAll
                | ToggleGroupTab
                | ToggleGroupWindow
                | UngroupTab
                | UngroupWindow
                | OpenContextMenu
                | UndoCloseTab
                | DuplicateTab
                | DuplicatePane
                | ToggleViMode
                | RotateCw
                | RotateCcw
                | ToggleScrollbar
                | EditWindowTitle
                | EditTabTitle
                | EditPaneTitle
                | InsertPaneNumber
                | InsertPanePadded
                | InsertPaneName
                | OpenCwdInFileManager
                | NextProfile
                | PrevProfile
                | ZoomInAll
                | ZoomOutAll
                | ZoomNormalAll
                | ResetAndClear
                | ScrollPageUpHalf
                | ScrollPageDownHalf
                | PastePrimary
                | ToggleWindowVisibility
                | MoveTabToWindow
                | MoveTabToNewWindow
                | EditPaneGroup
                | OpenSettings
                | OpenUpdate
                | DismissUpdate
                | TogglePaneReadOnly
                | MovePaneLeft
                | MovePaneRight
                | MovePaneUp
                | MovePaneDown
                | GotoTab(_)
                | NewTabShell(_)
                | SendText(_)
                | About => {}
            }
        }
        let missing: Vec<&Action> = every_action
            .iter()
            .filter(|a| !excluded.contains(a) && !listed.contains(a))
            .collect();
        assert!(
            missing.is_empty(),
            "palette missing actions: {missing:?} — either add them to \
             commands() or add to the `excluded` list above with a \
             one-line rationale"
        );
    }

    const EN: Translator = Translator::new(Language::En);
    const ES: Translator = Translator::new(Language::Es);

    /// Each broadcast scope keeps a palette row under every
    /// `broadcast-default`, and the chord's action is the row for the scope it
    /// turns on, labelled with that scope (`labels_are_distinct_in_each_language`
    /// checks the labels in English and Spanish). Under `group`
    /// that is the tab, under `all` the window, and under `off`, where the
    /// chord only stops broadcasting, it has no row.
    #[test]
    fn the_chord_row_follows_broadcast_default() {
        use Action::*;
        for (default, chord_label) in [
            (
                BroadcastDefault::Group,
                Some(Text::PaletteToggleBroadcastAll),
            ),
            (
                BroadcastDefault::All,
                Some(Text::PaletteToggleBroadcastWindow),
            ),
            (BroadcastDefault::Off, None),
        ] {
            let cmds = commands_for(default);
            assert_eq!(
                cmds.iter()
                    .find(|(_, action)| *action == ToggleBroadcastAll)
                    .map(|(label, _)| *label),
                chord_label,
                "{default:?}"
            );
            for scope in [
                Text::PaletteToggleBroadcastAll,
                Text::PaletteToggleBroadcastWindow,
                Text::PaletteToggleBroadcastGroup,
                Text::PaletteToggleBroadcastOff,
            ] {
                assert!(
                    cmds.iter().any(|(label, _)| *label == scope),
                    "{default:?} lost {scope:?}"
                );
            }
            let tab = cmds
                .iter()
                .find(|(label, _)| *label == Text::PaletteToggleBroadcastAll)
                .map(|(_, action)| action.clone());
            assert_eq!(
                tab,
                Some(if default == BroadcastDefault::Group {
                    ToggleBroadcastAll
                } else {
                    ToggleBroadcastPanesInTab
                }),
                "{default:?}"
            );
        }
    }

    #[test]
    fn primary_selection_paste_is_offered_only_where_it_exists() {
        let offered = commands()
            .iter()
            .any(|(_, action)| *action == Action::PastePrimary);
        assert_eq!(offered, cfg!(target_os = "linux"));
    }

    #[test]
    fn empty_query_returns_all_in_registry_order() {
        let cmds = commands();
        for tr in [EN, ES] {
            let r = rank("", &cmds, &tr);
            assert_eq!(r.len(), cmds.len());
            assert!(r.iter().enumerate().all(|(i, &idx)| i == idx), "stable");
        }
    }

    #[test]
    fn query_filters_and_ranks() {
        let cmds = commands();
        let r = rank("split", &cmds, &EN);
        assert!(!r.is_empty());
        // Every result actually contains the subsequence.
        assert!(
            r.iter()
                .all(|&i| fuzzy::score("split", EN.text(cmds[i].0)).is_some())
        );
        // The top hit for "split" is a split command.
        assert!(EN.text(cmds[r[0]].0).to_lowercase().contains("split"));
        // Non-matching query → empty.
        assert!(rank("zzzqqq", &cmds, &EN).is_empty());
    }

    #[test]
    fn abbreviation_finds_the_expected_action() {
        let cmds = commands();
        // "nt" → "New tab" should rank first (word-initials).
        let r = rank("nt", &cmds, &EN);
        assert_eq!(cmds[r[0]].1, Action::NewTab, "nt → New tab");
        // "fullscreen" resolves to the toggle.
        let r2 = rank("fullscreen", &cmds, &EN);
        assert_eq!(cmds[r2[0]].1, Action::ToggleFullscreen);
    }

    /// A Spanish palette ranks its Spanish labels, and an English name from
    /// the docs still finds its command.
    #[test]
    fn spanish_labels_rank_and_english_names_still_match() {
        let cmds = commands();
        let r = rank("nueva pestaña", &cmds, &ES);
        assert_eq!(cmds[r[0]].1, Action::NewTab);
        let r = rank("pantalla completa", &cmds, &ES);
        assert_eq!(cmds[r[0]].1, Action::ToggleFullscreen);
        let r = rank("fullscreen", &cmds, &ES);
        assert_eq!(cmds[r[0]].1, Action::ToggleFullscreen);
        // English does not match Spanish words.
        assert!(rank("pantalla completa", &cmds, &EN).is_empty());
    }

    /// Every entry the palette shows has its own label in both languages,
    /// under every `broadcast-default`, so no two commands read the same.
    /// (`commands()` alone lists both tab-scope toggles under one label;
    /// `commands_for` shows one.)
    #[test]
    fn labels_are_distinct_in_each_language() {
        for default in [
            BroadcastDefault::Group,
            BroadcastDefault::All,
            BroadcastDefault::Off,
        ] {
            let cmds = commands_for(default);
            for tr in [EN, ES] {
                let mut seen = std::collections::BTreeSet::new();
                for (label, action) in &cmds {
                    assert!(
                        seen.insert(tr.text(*label)),
                        "{default:?} {action:?}: {}",
                        tr.text(*label)
                    );
                }
            }
        }
    }

    fn top(query: &str, tr: &Translator) -> Vec<Action> {
        let cmds = commands();
        rank(query, &cmds, tr)
            .into_iter()
            .map(|i| cmds[i].1.clone())
            .collect()
    }

    /// Whether `query` lies in `label` with each char at a word start (after
    /// a separator or at a camelCase hump) or right after the previous one,
    /// by exhaustive search.
    fn word_aligned(query: &str, label: &str) -> bool {
        fn search(
            q: &[char],
            c: &[char],
            lower: &[char],
            from: usize,
            prev: Option<usize>,
        ) -> bool {
            let Some((&first, rest)) = q.split_first() else {
                return true;
            };
            (from..c.len()).any(|ci| {
                let start = ci == 0
                    || matches!(c[ci - 1], '-' | '_' | '.' | ' ' | '/' | ':')
                    || (c[ci - 1].is_lowercase() && c[ci].is_uppercase());
                lower[ci] == first
                    && (start || prev == Some(ci.wrapping_sub(1)))
                    && search(rest, c, lower, ci + 1, Some(ci))
            })
        }
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let q: Vec<char> = query.chars().map(fold).collect();
        let c: Vec<char> = label.chars().collect();
        let lower: Vec<char> = c.iter().copied().map(fold).collect();
        search(&q, &c, &lower, 0, None)
    }

    /// Weak matches drop out while a strong one exists: `font` runs a font
    /// command, not `Focus next pane`, which it lies scattered through. With
    /// no strong match, every match stays.
    #[test]
    fn weak_matches_drop_out_when_a_strong_one_exists() {
        let font = top("font", &EN);
        assert!(
            matches!(font[0], Action::ResetFontSize | Action::IncreaseFontSize),
            "{font:?}"
        );
        assert!(!font.contains(&Action::FocusNext), "{font:?}");
        let split = top("split", &EN);
        for action in [
            Action::SplitRight,
            Action::SplitDown,
            Action::SplitAuto,
            Action::SplitLeft,
            Action::SplitUp,
            Action::EqualizeSplits,
        ] {
            assert!(split.contains(&action), "{action:?} in {split:?}");
        }
        assert!(!split.contains(&Action::ToggleBroadcastAll), "{split:?}");
        assert!(
            !split.contains(&Action::SetAskBeforeClosingBusyOrMultiple),
            "{split:?}"
        );
        let tab = top("tab", &EN);
        assert!(tab.contains(&Action::NextTab) && tab.contains(&Action::DuplicateTab));
        // Whitespace typed in the query does not make the match weak.
        assert_eq!(top("sp ri", &EN)[0], Action::SplitRight);
        // A run of four or more chars is strong even mid-word, so "screen"
        // keeps "Toggle fullscreen"; a shorter one is not, so "scr" drops it
        // while "Take screenshot" starts a word with it.
        let screen = top("screen", &EN);
        assert!(screen.contains(&Action::TakeScreenshot), "{screen:?}");
        assert!(screen.contains(&Action::ToggleFullscreen), "{screen:?}");
        let scr = top("scr", &EN);
        assert!(scr.contains(&Action::TakeScreenshot), "{scr:?}");
        assert!(!scr.contains(&Action::ToggleFullscreen), "{scr:?}");
        // Up to three chars, the rows kept are exactly those whose label has
        // an alignment with every char at a word start or after the previous
        // one, found here by exhaustive search.
        for query in ["e", "t", "ne"] {
            let mut kept: Vec<_> = top(query, &EN);
            let mut words: Vec<_> = commands()
                .into_iter()
                .filter(|(label, _)| word_aligned(query, EN.text(*label)))
                .map(|(_, action)| action)
                .collect();
            assert!(!words.is_empty(), "{query:?}");
            let key = |action: &Action| format!("{action:?}");
            kept.sort_by_key(key);
            words.sort_by_key(key);
            assert_eq!(kept, words, "{query:?}");
        }
        // A single letter keeps only rows with a word starting with it.
        let e = top("e", &EN);
        let cmds = commands();
        for (label, action) in &cmds {
            let words_start_e = EN
                .text(*label)
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word.starts_with(['e', 'E']));
            assert_eq!(e.contains(action), words_start_e, "{action:?} for \"e\"");
        }
        // A single letter still lists every row with a word starting with it.
        let t = top("t", &EN);
        for (label, action) in &cmds {
            let words_start_t = EN
                .text(*label)
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word.starts_with(['t', 'T']));
            if words_start_t {
                assert!(t.contains(action), "{action:?} for \"t\"");
            }
        }
        // With no strong match anywhere, every subsequence match stays listed.
        let query = "fcnp";
        assert!(
            cmds.iter()
                .all(|(label, _)| fuzzy::align_strong(query, EN.text(*label)).is_none())
        );
        let weak = cmds
            .iter()
            .filter(|(label, _)| fuzzy::score(query, EN.text(*label)).is_some())
            .count();
        assert!(weak > 0);
        assert_eq!(rank(query, &cmds, &EN).len(), weak);
    }

    /// Highlights come from the alignment that ranked the row: `theme` marks
    /// the word in `Next theme`, and a Spanish row found only through its
    /// English alias marks nothing rather than chars of its own label.
    #[test]
    fn highlights_follow_the_ranking_alignment() {
        let cmds = commands();
        let entries: Vec<_> = cmds
            .iter()
            .map(|(label, action)| PaletteEntry::new(PaletteLabel::Text(*label), action.clone()))
            .collect();
        let ranked = rank_matches("theme", &entries, &EN);
        let next = ranked
            .iter()
            .find(|entry| cmds[entry.index].1 == Action::NextTheme)
            .expect("Next theme");
        assert_eq!(next.matched_chars, Some((5..10).collect()));
        let ranked = rank_matches("fullscreen", &entries, &ES);
        assert_eq!(cmds[ranked[0].index].1, Action::ToggleFullscreen);
        assert_eq!(ranked[0].matched_chars, None);
        // A Spanish query on a Spanish label marks that label.
        let ranked = rank_matches("pantalla", &entries, &ES);
        assert_eq!(cmds[ranked[0].index].1, Action::ToggleFullscreen);
        let label: Vec<char> = ES.text(cmds[ranked[0].index].0).chars().collect();
        let marked: String = ranked[0]
            .matched_chars
            .as_ref()
            .expect("shown label matched")
            .iter()
            .map(|&i| label[i])
            .collect();
        assert_eq!(marked.to_lowercase(), "pantalla");
    }

    /// `rank` is `rank_entries` over catalogue labels: same order for every
    /// query, in both languages.
    #[test]
    fn rank_entries_matches_rank_for_static_labels() {
        let cmds = commands();
        let entries: Vec<_> = cmds
            .iter()
            .map(|(label, action)| PaletteEntry::new(PaletteLabel::Text(*label), action.clone()))
            .collect();
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            for query in ["", "split", "tab", "pane", "zzz"] {
                assert_eq!(rank(query, &cmds, &tr), rank_entries(query, &entries, &tr));
            }
        }
    }

    /// The registry's rows with `bound` taken from the default keymap, as the
    /// palette builds them.
    fn default_entries() -> (Vec<(Text, Action)>, Vec<PaletteEntry>) {
        let bindings = crate::keybinds::defaults();
        let cmds = commands();
        let entries = cmds
            .iter()
            .map(|(label, action)| PaletteEntry {
                bound: crate::keybinds::ui_hint_label(&bindings, action).is_some(),
                ..PaletteEntry::new(PaletteLabel::Text(*label), action.clone())
            })
            .collect();
        (cmds, entries)
    }

    /// `split` lists the split directions that have a shortcut (right, down,
    /// automatically) before the two that have none (left, up), in English
    /// and in Spanish, however long each label is: Split automatically is
    /// the longest label in both languages and must not fall under an
    /// unbound row. Equalize splits, a weaker match, comes last, and no row
    /// is found through its description.
    #[test]
    fn split_ranks_bound_directions_first() {
        use Action::*;
        let (cmds, entries) = default_entries();
        let order = |query: &str, tr: &Translator| -> Vec<Action> {
            rank_matches(query, &entries, tr)
                .into_iter()
                .map(|entry| cmds[entry.index].1.clone())
                .collect()
        };
        assert_eq!(
            order("split", &EN),
            [
                SplitRight,
                SplitDown,
                SplitAuto,
                SplitUp,
                SplitLeft,
                EqualizeSplits
            ]
        );
        assert_eq!(
            order("dividir", &ES),
            [SplitRight, SplitDown, SplitAuto, SplitLeft, SplitUp]
        );
        // The English name still finds them from a Spanish palette.
        assert_eq!(order("split", &ES)[..3], [SplitRight, SplitDown, SplitAuto]);
    }

    /// Descriptions are searched only after labels, and only by a query of
    /// four or more chars that leaves fewer than three label rows: `divider`
    /// finds Split right and Split down through theirs, in either language,
    /// with nothing highlighted; `split`, which six labels match, and the
    /// three-char `div` find nothing that way.
    #[test]
    fn descriptions_match_only_after_labels() {
        use Action::*;
        let (cmds, entries) = default_entries();
        let found = |query: &str, tr: &Translator| -> Vec<(Action, bool)> {
            rank_matches(query, &entries, tr)
                .into_iter()
                .map(|entry| (cmds[entry.index].1.clone(), entry.matched_chars.is_some()))
                .collect()
        };
        assert_eq!(
            found("divider", &EN),
            [(SplitRight, false), (SplitDown, false)]
        );
        assert_eq!(
            found("divisor", &ES),
            [(SplitRight, false), (SplitDown, false)]
        );
        // The English word from a Spanish palette: Split down through its
        // English description, Split right through its own label, which
        // "divider" spells out ("Divi…der…").
        assert_eq!(
            found("divider", &ES),
            [(SplitRight, true), (SplitDown, false)]
        );
        let shell = found("shell integration", &EN);
        for action in [SelectCommandOutput, JumpPrevPrompt, JumpNextPrompt] {
            assert!(
                shell.contains(&(action.clone(), false)),
                "{action:?} in {shell:?}"
            );
        }
        let split = found("split", &EN);
        assert!(
            split.iter().all(|(_, highlighted)| *highlighted),
            "{split:?}"
        );
        assert!(!split.iter().any(|(action, _)| *action == DuplicatePane));
        // Three chars are too few: whatever `div` finds, its label matched.
        let div = found("div", &EN);
        assert!(div.iter().all(|(_, highlighted)| *highlighted), "{div:?}");
        assert!(
            !div.iter().any(|(action, _)| *action == SplitRight),
            "{div:?}"
        );
        // Every described command's description is its own text in both
        // languages, and no description repeats its label.
        for tr in [EN, ES] {
            for (label, action) in &cmds {
                if let Some(text) = description(action) {
                    let (label, text) = (tr.text(*label), tr.text(text));
                    assert!(!text.is_empty(), "{action:?}");
                    assert!(
                        !label.to_lowercase().contains(&text.to_lowercase()),
                        "{action:?}: {text:?} repeats {label:?}"
                    );
                }
            }
        }
    }

    /// The registry is grouped: walking it never returns to a group it left,
    /// groups come in [`PaletteGroup`] order, every group has commands, and
    /// the first screen's commands lead their groups: the four tab commands
    /// with chords, then the bound split directions.
    #[test]
    fn groups_are_contiguous_in_order_and_lead_with_the_common_commands() {
        use Action::*;
        let grouped = grouped_commands();
        let mut seen: Vec<PaletteGroup> = Vec::new();
        for (group, _, action) in &grouped {
            if seen.last() != Some(group) {
                assert!(!seen.contains(group), "{group:?} again at {action:?}");
                assert!(
                    seen.last().is_none_or(|last| last < group),
                    "{group:?} out of order"
                );
                seen.push(*group);
            }
        }
        assert_eq!(seen.len(), 14, "{seen:?}");
        let first = |group: PaletteGroup, n: usize| -> Vec<Action> {
            grouped
                .iter()
                .filter(|(g, _, _)| *g == group)
                .take(n)
                .map(|(_, _, action)| action.clone())
                .collect()
        };
        assert_eq!(
            first(PaletteGroup::Tabs, 4),
            [NewTab, CloseTab, NextTab, PrevTab]
        );
        assert_eq!(
            first(PaletteGroup::Splits, 5),
            [SplitRight, SplitDown, SplitAuto, SplitLeft, SplitUp]
        );
        assert_eq!(seen[..2], [PaletteGroup::Tabs, PaletteGroup::Splits]);
        // Every group has its own heading text in both languages.
        for tr in [EN, ES] {
            let mut names = std::collections::BTreeSet::new();
            for group in &seen {
                assert!(names.insert(tr.text(group.text())), "{group:?}");
            }
        }
    }
}
