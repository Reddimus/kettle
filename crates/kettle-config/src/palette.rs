//! Command-palette model: the list of user-facing actions and a fuzzy
//! ranking over their labels. Pure and UI-agnostic — the app drives the
//! overlay; this just answers "given this query, which commands, in what
//! order?". Reuses [`crate::fuzzy`].

use kettle_i18n::{Language, Text, Translator};

use crate::BroadcastDefault;
use crate::fuzzy;
use crate::keybinds::Action;

/// The palette command registry: a catalogue label plus the [`Action`] it
/// dispatches. Ordered roughly by how often it is reached for; this order
/// is also the tie-break and the empty-query order.
pub fn commands() -> Vec<(Text, Action)> {
    use Action::*;
    use Text as T;
    let mut commands = vec![
        (T::PaletteNewTab, NewTab),
        (T::PaletteCloseTab, CloseTab),
        (T::PaletteUndoCloseTab, UndoCloseTab),
        (T::PaletteDuplicateTab, DuplicateTab),
        (T::PaletteDuplicatePane, DuplicatePane),
        (T::PaletteNextTab, NextTab),
        (T::PalettePrevTab, PrevTab),
        (T::PaletteMoveTabLeft, MoveTabLeft),
        (T::PaletteMoveTabRight, MoveTabRight),
        (T::PaletteRenameTab, EditTabTitle),
        (T::PaletteSplitRight, SplitRight),
        (T::PaletteSplitDown, SplitDown),
        (T::PaletteSplitAuto, SplitAuto),
        (T::PaletteSplitLeft, SplitLeft),
        (T::PaletteSplitUp, SplitUp),
        (T::PaletteClosePane, ClosePane),
        (T::PaletteEqualizeSplits, EqualizeSplits),
        (T::PaletteToggleZoom, ToggleZoom),
        (T::PaletteScaledZoom, ScaledZoom),
        (T::PaletteShowHelp, ShowHelp),
        (T::PaletteShowKeybinds, ShowKeybinds),
        (T::PaletteSendNewline, SendNewline),
        (T::MenuOpenLayout, OpenLayoutPicker),
        (T::MenuSaveLayout, SaveLayout),
        (T::PaletteOpenThemePicker, OpenThemePicker),
        (T::PaletteOpenMediaShelf, OpenMediaShelf),
        (T::PalettePreviewLink, PreviewLink),
        (T::PalettePreviewClipboardPath, PreviewClipboardPath),
        (T::PalettePreviewNext, PreviewNext),
        (T::PalettePreviewPrevious, PreviewPrevious),
        (T::PalettePreviewNextDiagram, PreviewNextDiagram),
        (T::PalettePreviewPreviousDiagram, PreviewPreviousDiagram),
        (T::PaletteClosePreview, ClosePreview),
        (T::PalettePreviewSource, PreviewSource),
        (T::PalettePreviewCanvas, PreviewCanvas),
        (T::PalettePreviewCopy, PreviewCopy),
        (T::PalettePreviewReload, PreviewReload),
        (T::PalettePreviewPlay, PreviewPlay),
        (T::PaletteVideoPlay, VideoPlay),
        (T::PaletteVideoMute, VideoMute),
        (T::PalettePreviewZoomIn, PreviewZoomIn),
        (T::PalettePreviewZoomOut, PreviewZoomOut),
        (T::PalettePreviewFit, PreviewFit),
        (T::PaletteFocusPreview, FocusPreview),
        (T::PaletteRenderClipboardAsDiagram, RenderClipboardAsDiagram),
        (T::PaletteRenderSelectionAsDiagram, RenderSelectionAsDiagram),
        (T::PaletteCopyAgentSetup, CopyAgentSetup),
        (T::PaletteOpenSettings, OpenSettings),
        (T::PaletteAbout, About),
        (T::PaletteEditConfig, EditConfig),
        (T::PaletteSetScrollbarAlways, SetScrollbarAlways),
        (T::PaletteSetScrollbarAuto, SetScrollbarAuto),
        (T::PaletteSetScrollbarNever, SetScrollbarNever),
        (
            T::PaletteSetAskBeforeClosingAlways,
            SetAskBeforeClosingAlways,
        ),
        (
            T::PaletteSetAskBeforeClosingBusyOrMultiple,
            SetAskBeforeClosingBusyOrMultiple,
        ),
        (
            T::PaletteSetAskBeforeClosingMultiple,
            SetAskBeforeClosingMultiple,
        ),
        (T::PaletteSetAskBeforeClosingNever, SetAskBeforeClosingNever),
        (T::PaletteToggleCursorBlink, ToggleCursorBlink),
        (T::PaletteToggleCopyOnSelect, ToggleCopyOnSelect),
        (T::PaletteSetBellOff, SetBellOff),
        (T::PaletteSetBellVisual, SetBellVisual),
        (T::PaletteSetBellAttention, SetBellAttention),
        (T::PaletteSetBellBoth, SetBellBoth),
        (T::PaletteToggleMouseHide, ToggleMouseHide),
        (T::PaletteFocusNext, FocusNext),
        (T::PaletteFocusPrev, FocusPrev),
        (T::PaletteNewWindow, NewWindow),
        (T::PaletteCloseWindow, CloseWindow),
        (T::PaletteStartSearch, StartSearch),
        (T::PaletteHintMode, HintMode),
        (T::PaletteOpenSsh, OpenSsh),
        (T::PaletteCopy, Copy),
        (T::PalettePaste, Paste),
        (T::PaletteSelectAll, SelectAll),
        (T::PaletteSelectToTop, SelectToTop),
        (T::PaletteSelectToBottom, SelectToBottom),
        (T::PaletteIncreaseFontSize, IncreaseFontSize),
        (T::PaletteDecreaseFontSize, DecreaseFontSize),
        (T::PaletteResetFontSize, ResetFontSize),
        (T::QuickTerminalToggle, ToggleQuickTerminal),
        (T::PaletteToggleFullscreen, ToggleFullscreen),
        (T::PaletteToggleBroadcastAll, ToggleBroadcastAll),
        // The same tab scope, whatever `broadcast-default` says;
        // `commands_for` lists one of the two.
        (T::PaletteToggleBroadcastAll, ToggleBroadcastPanesInTab),
        (T::PaletteToggleBroadcastGroup, ToggleBroadcastGroup),
        (T::PaletteToggleBroadcastWindow, ToggleBroadcastWindow),
        (T::PaletteToggleBroadcastOff, ToggleBroadcastOff),
        (T::PaletteScrollLineUp, ScrollLineUp),
        (T::PaletteScrollLineDown, ScrollLineDown),
        (T::PaletteScrollPageUp, ScrollPageUp),
        (T::PaletteScrollPageDown, ScrollPageDown),
        (T::PaletteScrollToTop, ScrollToTop),
        (T::PaletteScrollToBottom, ScrollToBottom),
        (T::PaletteJumpPrevPrompt, JumpPrevPrompt),
        (T::PaletteJumpNextPrompt, JumpNextPrompt),
        (T::PaletteToggleViMode, ToggleViMode),
        // Terminator-parity entries.
        (T::PaletteRotateCw, RotateCw),
        (T::PaletteRotateCcw, RotateCcw),
        (T::PaletteMovePaneLeft, MovePaneLeft),
        (T::PaletteMovePaneRight, MovePaneRight),
        (T::PaletteMovePaneUp, MovePaneUp),
        (T::PaletteMovePaneDown, MovePaneDown),
        (T::PaletteToggleScrollbar, ToggleScrollbar),
        (T::PaletteNextProfile, NextProfile),
        (T::PalettePrevProfile, PrevProfile),
        (T::PaletteZoomInAll, ZoomInAll),
        (T::PaletteZoomOutAll, ZoomOutAll),
        (T::PaletteZoomNormalAll, ZoomNormalAll),
        (T::PaletteResetAndClear, ResetAndClear),
        (T::PaletteScrollPageUpHalf, ScrollPageUpHalf),
        (T::PaletteScrollPageDownHalf, ScrollPageDownHalf),
        (T::PalettePastePrimary, PastePrimary),
        (T::PaletteToggleWindowVisibility, ToggleWindowVisibility),
        (T::PaletteMoveTabToWindow, MoveTabToWindow),
        (T::PaletteMoveTabToNewWindow, MoveTabToNewWindow),
        (T::PaletteEditPaneGroup, EditPaneGroup),
        (T::PaletteNextTheme, NextTheme),
        (T::PalettePrevTheme, PrevTheme),
        (T::PaletteToggleLightDark, ToggleLightDark),
        (T::PaletteToggleSessionLog, ToggleSessionLog),
        // Terminator parity ("Read only"): drop user input to the
        // focused pane while letting its output keep flowing.
        (T::PaletteTogglePaneReadOnly, TogglePaneReadOnly),
        (T::PaletteTakeScreenshot, TakeScreenshot),
        (T::PaletteCreateGroup, CreateGroup),
        (T::PaletteGroupTab, GroupTab),
        (T::PaletteGroupWindow, GroupWindow),
        (T::PaletteUngroupTab, UngroupTab),
        (T::PaletteUngroupWindow, UngroupWindow),
        (T::PaletteGroupAll, GroupAll),
        (T::PaletteUngroupAll, UngroupAll),
        (T::PaletteToggleGroupAll, ToggleGroupAll),
        (T::PaletteToggleGroupTab, ToggleGroupTab),
        (T::PaletteToggleGroupWindow, ToggleGroupWindow),
        (T::PaletteReset, Reset),
        (T::PaletteClearHistory, ClearHistory),
        (T::PaletteReloadConfig, ReloadConfig),
    ];
    // Only X11 and Wayland have a primary selection. Elsewhere the action
    // falls back to an ordinary paste, which the palette already offers.
    if !cfg!(target_os = "linux") {
        commands.retain(|(_, action)| *action != PastePrimary);
    }
    commands
}

/// The palette's commands under `broadcast-default = default`. The tab and
/// window scopes are each listed once. `ToggleBroadcastAll`, the chord's
/// action, stands for the scope it turns on, with that scope's label, so the
/// chord's hint sits on the row it belongs to; the scope's own toggle is then
/// left out. Under `off` the chord can only stop broadcasting, which "Stop
/// broadcasting input" already offers, so it is left out instead.
pub fn commands_for(default: BroadcastDefault) -> Vec<(Text, Action)> {
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
    commands()
        .into_iter()
        .filter(|(_, action)| *action != replaced)
        .filter_map(|(label, action)| match action {
            Action::ToggleBroadcastAll => chord_label.map(|label| (label, action)),
            _ => Some((label, action)),
        })
        .collect()
}

/// Indices into `cmds` that match `query` against their labels in `tr`'s
/// language, best first. Outside English, the English label matches too, so a
/// command name from the docs still finds its command. Ties (and an empty
/// query) preserve registry order, so the palette is stable as you type.
pub fn rank(query: &str, cmds: &[(Text, Action)], tr: &Translator) -> Vec<usize> {
    let entries: Vec<(PaletteLabel, Action)> = cmds
        .iter()
        .map(|(label, action)| (PaletteLabel::Text(*label), action.clone()))
        .collect();
    rank_entries(query, &entries, tr)
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

/// One row [`rank_matches`] keeps: its index and the positions of the
/// matched chars in its displayed label, from the alignment that ranked it.
/// `None` when only the English alias of a translated label matched, so a
/// highlight never lands on chars of the wrong label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedEntry {
    pub index: usize,
    pub matched_chars: Option<Vec<usize>>,
}

/// [`rank`] over rows that may carry run-time labels.
pub fn rank_entries(
    query: &str,
    entries: &[(PaletteLabel, Action)],
    tr: &Translator,
) -> Vec<usize> {
    rank_matches(query, entries, tr)
        .into_iter()
        .map(|entry| entry.index)
        .collect()
}

/// [`rank_entries`] with each kept row's matched chars.
///
/// A row matches strongly when each scattered piece of the query starts a
/// word, or a query of four or more chars runs contiguously anywhere (see
/// [`fuzzy::align_strong`]). While any row
/// matches strongly, rows the query only lies scattered through are left out,
/// so `font` lists the font-size commands and not `Focus next pane`; with no
/// strong match every subsequence match is listed.
pub fn rank_matches(
    query: &str,
    entries: &[(PaletteLabel, Action)],
    tr: &Translator,
) -> Vec<RankedEntry> {
    let english = Translator::new(Language::En);
    let alias = tr.language() != Language::En;
    // The best score over the shown label and its English alias, with the
    // shown label's positions.
    let best = |shown: Option<fuzzy::Alignment>, alias: Option<fuzzy::Alignment>| {
        let score = shown
            .as_ref()
            .map(|a| a.score)
            .max(alias.as_ref().map(|a| a.score))?;
        Some((score, shown.map(|a| a.positions)))
    };
    let matches: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, (label, _))| {
            let shown = label.shown(tr);
            let alias = alias.then(|| label.english(&english));
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
    let strong_only = matches.iter().any(|(_, strong, _)| strong.is_some());
    let mut scored: Vec<(usize, i32, Option<Vec<usize>>)> = matches
        .into_iter()
        .filter_map(|(index, strong, any)| {
            let (score, positions) = if strong_only { strong? } else { any };
            Some((index, score, positions))
        })
        .collect();
    // Sort by score desc, then original index asc (stable tie-break).
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored
        .into_iter()
        .map(|(index, _, matched_chars)| RankedEntry {
            index,
            matched_chars,
        })
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
            .map(|(label, action)| (PaletteLabel::Text(*label), action.clone()))
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
            .map(|(label, action)| (PaletteLabel::Text(*label), action.clone()))
            .collect();
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            for query in ["", "split", "tab", "pane", "zzz"] {
                assert_eq!(rank(query, &cmds, &tr), rank_entries(query, &entries, &tr));
            }
        }
    }
}
