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
        ("Next tab", NextTab),
        ("Previous tab", PrevTab),
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
        ("SSH launcher", OpenSsh),
        ("Copy", Copy),
        ("Paste", Paste),
        ("Increase font size", IncreaseFontSize),
        ("Decrease font size", DecreaseFontSize),
        ("Reset font size", ResetFontSize),
        ("Toggle fullscreen", ToggleFullscreen),
        ("Broadcast input to all panes", ToggleBroadcastAll),
        ("Stop broadcasting input", ToggleBroadcastOff),
        ("Scroll to top", ScrollToTop),
        ("Scroll to bottom", ScrollToBottom),
        ("Jump to previous prompt", JumpPrevPrompt),
        ("Jump to next prompt", JumpNextPrompt),
        ("Next theme", NextTheme),
        ("Previous theme", PrevTheme),
        ("Reset terminal", Reset),
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
