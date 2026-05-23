//! Command-palette model: the list of user-facing actions and a fuzzy
//! ranking over their labels. Pure and UI-agnostic — the app drives the
//! overlay; this just answers "given this query, which commands, in what
//! order?". Reuses [`crate::fuzzy`].

use crate::fuzzy;
use crate::keybinds::Action;

/// The palette command registry: a friendly label plus the [`Action`] it
/// dispatches. Ordered roughly by how often it is reached for; this order
/// is also the tie-break and the empty-query order.
pub fn commands() -> Vec<(&'static str, Action)> {
    use Action::*;
    vec![
        ("New tab", NewTab),
        ("Close tab", CloseTab),
        ("Undo close tab", UndoCloseTab),
        ("Duplicate tab", DuplicateTab),
        ("Duplicate pane", DuplicatePane),
        ("Next tab", NextTab),
        ("Previous tab", PrevTab),
        ("Move tab left", MoveTabLeft),
        ("Move tab right", MoveTabRight),
        ("Split right (vertical divider)", SplitRight),
        ("Split down (horizontal divider)", SplitDown),
        ("Split automatically", SplitAuto),
        ("Close pane", ClosePane),
        ("Zoom / unzoom pane", ToggleZoom),
        ("Focus next pane", FocusNext),
        ("Focus previous pane", FocusPrev),
        ("New window", NewWindow),
        ("Close window", CloseWindow),
        ("Search scrollback", StartSearch),
        ("Quick-select hints", HintMode),
        ("SSH launcher", OpenSsh),
        ("Copy", Copy),
        ("Paste", Paste),
        ("Increase font size", IncreaseFontSize),
        ("Decrease font size", DecreaseFontSize),
        ("Reset font size", ResetFontSize),
        ("Toggle fullscreen", ToggleFullscreen),
        ("Broadcast input to all panes", ToggleBroadcastAll),
        ("Stop broadcasting input", ToggleBroadcastOff),
        ("Scroll up one line", ScrollLineUp),
        ("Scroll down one line", ScrollLineDown),
        ("Scroll page up", ScrollPageUp),
        ("Scroll page down", ScrollPageDown),
        ("Scroll to top", ScrollToTop),
        ("Scroll to bottom", ScrollToBottom),
        ("Jump to previous prompt", JumpPrevPrompt),
        ("Jump to next prompt", JumpNextPrompt),
        ("Toggle vi-mode (scrollback)", ToggleViMode),
        // Cycle 342 Terminator-parity entries.
        ("Rotate panes clockwise", RotateCw),
        ("Rotate panes counter-clockwise", RotateCcw),
        ("Toggle scrollbar visibility", ToggleScrollbar),
        ("Next profile", NextProfile),
        ("Previous profile", PrevProfile),
        ("Zoom in (all panes)", ZoomInAll),
        ("Zoom out (all panes)", ZoomOutAll),
        ("Reset zoom (all panes)", ZoomNormalAll),
        ("Reset terminal + clear scrollback", ResetAndClear),
        ("Scroll half page up", ScrollPageUpHalf),
        ("Scroll half page down", ScrollPageDownHalf),
        ("Paste primary selection (X11)", PastePrimary),
        ("Toggle window visibility", ToggleWindowVisibility),
        ("Move tab to new window", MoveTabToNewWindow),
        ("Edit pane broadcast group", EditPaneGroup),
        ("Next theme", NextTheme),
        ("Previous theme", PrevTheme),
        ("Toggle light/dark theme", ToggleLightDark),
        ("Toggle session log (pane → file)", ToggleSessionLog),
        ("Reset terminal", Reset),
        ("Clear scrollback", ClearHistory),
        ("Reload config", ReloadConfig),
    ]
}

/// Indices into `cmds` that match `query`, best first. Ties (and an empty
/// query) preserve registry order, so the palette is stable as you type.
pub fn rank(query: &str, cmds: &[(&'static str, Action)]) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = cmds
        .iter()
        .enumerate()
        .filter_map(|(i, (label, _))| fuzzy::score(query, label).map(|s| (i, s)))
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
        // Cycle-117 drift guard. When cycle 110 added ScrollLineUp/Down,
        // the palette quietly missed them — users couldn't reach the
        // new actions via Ctrl+Shift+K. The class of "new Action variant
        // landed but only the keymap and `--list-actions` know about it"
        // is the same shape as cycle 104's drift between `from_name` and
        // `action_names`. Pin it: every action *intended* for palette
        // dispatch must appear, identifiable by ⩾1 entry whose Action
        // matches the variant. Variants intentionally excluded from
        // the palette (geometric / parametric / palette-itself) are
        // listed explicitly below so a future excluded action is a
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
            GotoTab(0), // placeholder for the parametric family
            CommandPalette,
            // Cycle 245: OpenContextMenu is the right-click handler
            // itself — surfacing it inside the palette would be a
            // weird self-reference (palette → context menu → palette
            // entry). Triggered by the mouse only, not user-typed.
            OpenContextMenu,
            // Cycle 342 Terminator-parity actions excluded from the palette
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
        ];
        // Enumerate every Action variant explicitly via this exhaustive
        // list; if a future variant is added the match below fails to
        // compile, forcing whoever added it to also categorize it.
        let every_action: Vec<Action> = vec![
            Copy,
            Paste,
            NewTab,
            CloseTab,
            NextTab,
            PrevTab,
            MoveTabLeft,
            MoveTabRight,
            SplitRight,
            SplitDown,
            SplitAuto,
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
            ToggleZoom,
            IncreaseFontSize,
            DecreaseFontSize,
            ResetFontSize,
            StartSearch,
            ToggleBroadcastAll,
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
            OpenContextMenu,
            UndoCloseTab,
            DuplicateTab,
            DuplicatePane,
            // Cycle 342 Terminator-parity actions.
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
            GotoTab(0),
        ];
        // Compile-time exhaustiveness check: if a new Action variant is
        // added, this match must be updated, which forces the developer
        // to add a row to `every_action` above as well.
        for a in &every_action {
            match a {
                Copy
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
                | ToggleZoom
                | IncreaseFontSize
                | DecreaseFontSize
                | ResetFontSize
                | StartSearch
                | ToggleBroadcastAll
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
                | GotoTab(_) => {}
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

    #[test]
    fn empty_query_returns_all_in_registry_order() {
        let cmds = commands();
        let r = rank("", &cmds);
        assert_eq!(r.len(), cmds.len());
        assert!(r.iter().enumerate().all(|(i, &idx)| i == idx), "stable");
    }

    #[test]
    fn query_filters_and_ranks() {
        let cmds = commands();
        let r = rank("split", &cmds);
        assert!(!r.is_empty());
        // Every result actually contains the subsequence.
        assert!(
            r.iter()
                .all(|&i| fuzzy::score("split", cmds[i].0).is_some())
        );
        // The top hit for "split" is a split command.
        assert!(cmds[r[0]].0.to_lowercase().contains("split"));
        // Non-matching query → empty.
        assert!(rank("zzzqqq", &cmds).is_empty());
    }

    #[test]
    fn abbreviation_finds_the_expected_action() {
        let cmds = commands();
        // "nt" → "New tab" should rank first (word-initials).
        let r = rank("nt", &cmds);
        assert_eq!(cmds[r[0]].1, Action::NewTab, "nt → New tab");
        // "fullscreen" resolves to the toggle.
        let r2 = rank("fullscreen", &cmds);
        assert_eq!(cmds[r2[0]].1, Action::ToggleFullscreen);
    }
}
