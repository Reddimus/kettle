//! Command-palette model: the list of user-facing actions and a fuzzy
//! ranking over their labels. Pure and UI-agnostic — the app drives the
//! overlay; this just answers "given this query, which commands, in what
//! order?". Reuses [`crate::fuzzy`].

use kettle_i18n::{Language, Text, Translator};

use crate::fuzzy;
use crate::keybinds::Action;

/// The palette command registry: a catalogue label plus the [`Action`] it
/// dispatches. Ordered roughly by how often it is reached for; this order
/// is also the tie-break and the empty-query order.
pub fn commands() -> Vec<(Text, Action)> {
    use Action::*;
    use Text as T;
    vec![
        (T::PaletteNewTab, NewTab),
        (T::PaletteCloseTab, CloseTab),
        (T::PaletteUndoCloseTab, UndoCloseTab),
        (T::PaletteDuplicateTab, DuplicateTab),
        (T::PaletteDuplicatePane, DuplicatePane),
        (T::PaletteNextTab, NextTab),
        (T::PalettePrevTab, PrevTab),
        (T::PaletteMoveTabLeft, MoveTabLeft),
        (T::PaletteMoveTabRight, MoveTabRight),
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
        (T::PaletteSendNewline, SendNewline),
        (T::PaletteOpenLayoutPicker, OpenLayoutPicker),
        (T::PaletteOpenThemePicker, OpenThemePicker),
        (T::PaletteOpenMediaShelf, OpenMediaShelf),
        (T::PalettePreviewLink, PreviewLink),
        (T::PalettePreviewClipboardPath, PreviewClipboardPath),
        (T::PalettePreviewNext, PreviewNext),
        (T::PalettePreviewPrevious, PreviewPrevious),
        (T::PaletteClosePreview, ClosePreview),
        (T::PalettePreviewSource, PreviewSource),
        (T::PalettePreviewCanvas, PreviewCanvas),
        (T::PalettePreviewCopy, PreviewCopy),
        (T::PalettePreviewReload, PreviewReload),
        (T::PalettePreviewZoomIn, PreviewZoomIn),
        (T::PalettePreviewZoomOut, PreviewZoomOut),
        (T::PalettePreviewFit, PreviewFit),
        (T::PaletteFocusPreview, FocusPreview),
        (T::PaletteRenderClipboardAsDiagram, RenderClipboardAsDiagram),
        (T::PaletteRenderSelectionAsDiagram, RenderSelectionAsDiagram),
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
        (T::PaletteToggleFullscreen, ToggleFullscreen),
        (T::PaletteToggleBroadcastAll, ToggleBroadcastAll),
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
    ]
}

/// Indices into `cmds` that match `query` against their labels in `tr`'s
/// language, best first. Outside English, the English label matches too, so a
/// command name from the docs still finds its command. Ties (and an empty
/// query) preserve registry order, so the palette is stable as you type.
pub fn rank(query: &str, cmds: &[(Text, Action)], tr: &Translator) -> Vec<usize> {
    let english = Translator::new(Language::En);
    let alias = tr.language() != Language::En;
    let mut scored: Vec<(usize, i32)> = cmds
        .iter()
        .enumerate()
        .filter_map(|(i, (label, _))| {
            let shown = fuzzy::score(query, tr.text(*label));
            let english = alias
                .then(|| fuzzy::score(query, english.text(*label)))
                .flatten();
            shown.max(english).map(|s| (i, s))
        })
        .collect();
    // Sort by score desc, then original index asc (stable tie-break).
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let excluded: &[Action] = &[
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
            // tied to inline overlays (the title-edit ones open their own
            // input prompts), or send raw text to the focused PTY (insert-
            // number) which doesn't fit the palette's "do a thing" model.
            // The palette doesn't need a row for each; they remain
            // reachable via keybinds.
            EditWindowTitle,
            EditTabTitle,
            EditPaneTitle,
            InsertPaneNumber,
            InsertPanePadded,
            InsertPaneName,
            OpenCwdInFileManager,
            // Parametric, and the payload IS the command — there is nothing to
            // enumerate and nothing a fixed row could send. Reachable by
            // keybind, which is the point of it.
            SendText(String::new()),
            // The update-banner actions only do anything
            // while the update banner is on screen (a no-op + debug log
            // otherwise), so they'd be dead rows in the palette most of the
            // time. They stay bindable for keyboard access to the banner.
            OpenUpdate,
            DismissUpdate,
        ];
        // Enumerate every Action variant explicitly via this exhaustive
        // list; if a future variant is added the match below fails to
        // compile, forcing whoever added it to also categorize it.
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
            SendNewline,
            OpenLayoutPicker,
            OpenThemePicker,
            OpenMediaShelf,
            PreviewLink,
            PreviewClipboardPath,
            PreviewNext,
            PreviewPrevious,
            ClosePreview,
            PreviewSource,
            PreviewCanvas,
            PreviewCopy,
            PreviewReload,
            PreviewZoomIn,
            PreviewZoomOut,
            PreviewFit,
            FocusPreview,
            RenderClipboardAsDiagram,
            RenderSelectionAsDiagram,
            EditConfig,
            SetScrollbarAlways,
            SetScrollbarAuto,
            SetScrollbarNever,
            SetAskBeforeClosingAlways,
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
            ToggleBroadcastOff,
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
        // Compile-time exhaustiveness check: if a new Action variant is
        // added, this match must be updated, which forces the developer
        // to add a row to `every_action` above as well.
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
                | SendNewline
                | OpenLayoutPicker
                | OpenThemePicker
                | OpenMediaShelf
                | PreviewLink
                | PreviewClipboardPath
                | PreviewNext
                | PreviewPrevious
                | ClosePreview
                | PreviewSource
                | PreviewCanvas
                | PreviewCopy
                | PreviewReload
                | PreviewZoomIn
                | PreviewZoomOut
                | PreviewFit
                | FocusPreview
                | RenderClipboardAsDiagram
                | RenderSelectionAsDiagram
                | EditConfig
                | SetScrollbarAlways
                | SetScrollbarAuto
                | SetScrollbarNever
                | SetAskBeforeClosingAlways
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
                | ToggleBroadcastAll
                | ToggleBroadcastGroup
                | ToggleBroadcastWindow
                | ToggleBroadcastOff
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

    /// Every entry has its own label in both languages, so no two commands
    /// read the same.
    #[test]
    fn labels_are_distinct_in_each_language() {
        let cmds = commands();
        for tr in [EN, ES] {
            let mut seen = std::collections::BTreeSet::new();
            for (label, action) in &cmds {
                assert!(
                    seen.insert(tr.text(*label)),
                    "{action:?}: {}",
                    tr.text(*label)
                );
            }
        }
    }
}
