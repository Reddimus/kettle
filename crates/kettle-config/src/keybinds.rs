//! Keybinding model: actions, triggers, Ghostty `keybind = trigger=action`
//! parsing, and the Terminator-compatible default set.

use std::collections::HashMap;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Mods: u8 {
        const SHIFT = 1;
        const CTRL  = 2;
        const ALT   = 4;
        const SUPER = 8;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Tab,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Trigger {
    pub mods: Mods,
    pub key: Key,
}

impl Trigger {
    pub fn new(mods: Mods, key: Key) -> Self {
        Self { mods, key }
    }

    /// Human-readable label, e.g. `Ctrl+Shift+E`, `Alt+Left`, `F5`.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.mods.contains(Mods::CTRL) {
            parts.push("Ctrl".into());
        }
        if self.mods.contains(Mods::ALT) {
            parts.push("Alt".into());
        }
        if self.mods.contains(Mods::SHIFT) {
            parts.push("Shift".into());
        }
        if self.mods.contains(Mods::SUPER) {
            parts.push("Super".into());
        }
        parts.push(match self.key {
            Key::Char(c) => c.to_ascii_uppercase().to_string(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::PageUp => "PageUp".into(),
            Key::PageDown => "PageDown".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::Enter => "Enter".into(),
            Key::Tab => "Tab".into(),
            Key::F(n) => format!("F{n}"),
        });
        parts.join("+")
    }
}

/// User-facing label for one `Action`. Most variants use Rust's `Debug`
/// derive (`Copy`, `NewTab`, `SplitRight`, …) — short enough and already
/// matches the binding-table heading style. The exception is parametric
/// variants like `GotoTab(0)` whose Debug form leaks the 0-based internal
/// index; we render the 1-based human form so `kettle --list-keybinds`
/// reads "Goto tab 1" instead of "GotoTab(0)".
pub fn action_label(a: &Action) -> String {
    match a {
        Action::GotoTab(i) => format!("Goto tab {}", i + 1),
        other => format!("{other:?}"),
    }
}

/// Human-readable lines for the default keymap, sorted by trigger label —
/// powers `kettle --list-keybinds` (no config) so the binding set is
/// discoverable without reading the source.
pub fn describe_defaults() -> Vec<String> {
    describe(&defaults())
}

/// Human-readable lines for an arbitrary binding map, sorted by trigger
/// label. Used by `kettle --list-keybinds` together with `--config FILE`
/// so the user can introspect their *effective* keymap (defaults +
/// user overrides + unbinds applied) without restarting kettle and
/// inspecting it by hand.
pub fn describe(bindings: &Bindings) -> Vec<String> {
    let mut lines: Vec<(String, String)> = bindings
        .iter()
        .map(|(t, a)| (t.label(), action_label(a)))
        .collect();
    lines.sort();
    lines
        .into_iter()
        .map(|(t, a)| format!("{t:<16}  {a}"))
        .collect()
}

/// Every action kettle can bind. Names match Ghostty actions where they exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
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
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    JumpPrevPrompt,
    JumpNextPrompt,
    OpenSsh,
    ReloadConfig,
    CommandPalette,
    HintMode,
    NextTheme,
    PrevTheme,
    GotoTab(u8),
}

impl Action {
    pub(crate) fn from_name(s: &str) -> Option<Action> {
        use Action::*;
        Some(match s {
            "copy_to_clipboard" | "copy" => Copy,
            "paste_from_clipboard" | "paste" => Paste,
            "new_tab" => NewTab,
            "close_tab" => CloseTab,
            "next_tab" => NextTab,
            "previous_tab" | "prev_tab" => PrevTab,
            "move_tab_left" => MoveTabLeft,
            "move_tab_right" => MoveTabRight,
            // Terminator semantics: split_horiz = horizontal divider
            // (panes top/bottom) = our SplitDown; split_vert = vertical
            // divider (panes left/right) = our SplitRight.
            "new_split:right" | "split_right" | "split_vert" => SplitRight,
            "new_split:down" | "split_down" | "split_horiz" => SplitDown,
            "split_auto" => SplitAuto,
            "close_surface" | "close_pane" | "close_term" => ClosePane,
            "close_window" => CloseWindow,
            "new_window" => NewWindow,
            "focus_next" | "go_next" => FocusNext,
            "focus_prev" | "go_prev" => FocusPrev,
            "goto_split:up" | "go_up" => FocusUp,
            "goto_split:down" | "go_down" => FocusDown,
            "goto_split:left" | "go_left" => FocusLeft,
            "goto_split:right" | "go_right" => FocusRight,
            "resize_up" => ResizeUp,
            "resize_down" => ResizeDown,
            "resize_left" => ResizeLeft,
            "resize_right" => ResizeRight,
            "toggle_split_zoom" | "toggle_zoom" => ToggleZoom,
            "increase_font_size" | "zoom_in" => IncreaseFontSize,
            "decrease_font_size" | "zoom_out" => DecreaseFontSize,
            "reset_font_size" | "zoom_normal" => ResetFontSize,
            "start_search" | "search" => StartSearch,
            "broadcast_all" | "group_all" => ToggleBroadcastAll,
            "broadcast_off" | "ungroup_all" => ToggleBroadcastOff,
            "toggle_fullscreen" | "full_screen" => ToggleFullscreen,
            "reset" => Reset,
            "scroll_page_up" => ScrollPageUp,
            "scroll_page_down" => ScrollPageDown,
            "scroll_to_top" => ScrollToTop,
            "scroll_to_bottom" => ScrollToBottom,
            "jump_to_prompt_prev" | "prev_prompt" => JumpPrevPrompt,
            "jump_to_prompt_next" | "next_prompt" => JumpNextPrompt,
            "new_ssh" | "ssh" => OpenSsh,
            "command_palette" | "palette" => CommandPalette,
            "hint_mode" | "hints" | "quick_select" => HintMode,
            "next_theme" => NextTheme,
            "prev_theme" | "previous_theme" => PrevTheme,
            "reload_config" => ReloadConfig,
            // `goto_tab:N` where N is 1-based (Terminator / kitty syntax —
            // "Alt+1 = first tab" is the user mental model). Internally we
            // store the zero-based index so the handler can clamp against
            // `tabs.len()` without an off-by-one dance.
            other => {
                if let Some(rest) = other.strip_prefix("goto_tab:")
                    && let Ok(n) = rest.parse::<u8>()
                    && n >= 1
                {
                    return Some(GotoTab(n - 1));
                }
                return None;
            }
        })
    }
}

fn parse_key(s: &str) -> Option<Key> {
    let l = s.to_ascii_lowercase();
    Some(match l.as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "page_up" | "pageup" | "prior" => Key::PageUp,
        "page_down" | "pagedown" | "next" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "plus" => Key::Char('+'),
        "minus" => Key::Char('-'),
        "equal" => Key::Char('='),
        _ => {
            if let Some(n) = l.strip_prefix('f')
                && let Ok(num) = n.parse::<u8>()
            {
                return Some(Key::F(num));
            }
            let mut ch = l.chars();
            let c = ch.next()?;
            if ch.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    })
}

/// Parse a Ghostty trigger such as `ctrl+shift+o`.
pub fn parse_trigger(s: &str) -> Option<Trigger> {
    let mut mods = Mods::empty();
    let mut key = None;
    for part in s.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "shift" => mods |= Mods::SHIFT,
            "ctrl" | "control" => mods |= Mods::CTRL,
            "alt" | "opt" | "option" => mods |= Mods::ALT,
            "super" | "cmd" | "command" => mods |= Mods::SUPER,
            other => key = parse_key(other),
        }
    }
    Some(Trigger::new(mods, key?))
}

pub type Bindings = HashMap<Trigger, Action>;

/// Terminator-compatible defaults (see plan / terminatorlib/config.py).
pub fn defaults() -> Bindings {
    use Action::*;
    use Key::*;
    let c = Mods::CTRL;
    let cs = Mods::CTRL | Mods::SHIFT;
    let a = Mods::ALT;
    let su = Mods::SUPER;
    let sus = Mods::SUPER | Mods::SHIFT;
    let mut m = Bindings::new();
    let mut bind = |mods: Mods, k: Key, act: Action| {
        m.insert(Trigger::new(mods, k), act);
    };
    // Terminator parity: Ctrl+Shift+O = split horizontally (top/bottom),
    // Ctrl+Shift+E = split vertically (left/right).
    bind(cs, Char('o'), SplitDown);
    bind(cs, Char('e'), SplitRight);
    bind(cs, Char('a'), SplitAuto);
    bind(cs, Char('t'), NewTab);
    bind(cs, Char('w'), ClosePane);
    bind(cs, Char('q'), CloseWindow);
    bind(cs, Char('i'), NewWindow);
    bind(cs, Char('n'), FocusNext);
    bind(cs, Char('p'), FocusPrev);
    bind(a, Up, FocusUp);
    bind(a, Down, FocusDown);
    bind(a, Left, FocusLeft);
    bind(a, Right, FocusRight);
    bind(cs, Up, ResizeUp);
    bind(cs, Down, ResizeDown);
    bind(cs, Left, ResizeLeft);
    bind(cs, Right, ResizeRight);
    // Terminator-style Shift+Arrow split resize.
    let sh = Mods::SHIFT;
    bind(sh, Up, ResizeUp);
    bind(sh, Down, ResizeDown);
    bind(sh, Left, ResizeLeft);
    bind(sh, Right, ResizeRight);
    bind(c, PageDown, NextTab);
    bind(c, PageUp, PrevTab);
    bind(cs, PageDown, MoveTabRight);
    bind(cs, PageUp, MoveTabLeft);
    bind(cs, Char('c'), Copy);
    bind(cs, Char('v'), Paste);
    bind(cs, Char('f'), StartSearch);
    // Font-size zoom: every variant of "Ctrl + the plus/minus area" maps
    // to the same action. The `+` glyph on a US layout *is* `Shift+=` —
    // winit reports the chord as `mods = Ctrl+Shift, key = '+'`, which
    // wouldn't match a bare `Ctrl+Plus` binding. Cover all four sensible
    // combinations so muscle memory works regardless of layout / whether
    // the user thinks of it as `Ctrl++`, `Ctrl+=`, or `Ctrl+Shift+=`.
    bind(c, Char('+'), IncreaseFontSize);
    bind(c, Char('='), IncreaseFontSize);
    bind(cs, Char('+'), IncreaseFontSize);
    bind(cs, Char('='), IncreaseFontSize);
    bind(c, Char('-'), DecreaseFontSize);
    // `Ctrl+_` (== `Ctrl+Shift+-` on US) — same logic as Ctrl+Plus above.
    bind(cs, Char('-'), DecreaseFontSize);
    bind(cs, Char('_'), DecreaseFontSize);
    bind(c, Char('0'), ResetFontSize);
    bind(cs, Char('x'), ToggleZoom);
    bind(cs, Char('r'), Reset);
    bind(su, Char('g'), ToggleBroadcastAll);
    bind(sus, Char('g'), ToggleBroadcastOff);
    bind(Mods::empty(), F(11), ToggleFullscreen);
    bind(cs, Char('m'), ReloadConfig);
    bind(c, Up, JumpPrevPrompt);
    bind(c, Down, JumpNextPrompt);
    bind(cs, Char('s'), OpenSsh);
    bind(cs, Char('k'), CommandPalette);
    bind(cs, Char('h'), HintMode);
    bind(Mods::SHIFT, PageUp, ScrollPageUp);
    bind(Mods::SHIFT, PageDown, ScrollPageDown);
    bind(Mods::SHIFT, Home, ScrollToTop);
    bind(Mods::SHIFT, End, ScrollToBottom);
    // Alt+1..9 jumps to tab 1..9 (kitty / Terminator / Ghostty parity).
    // No-op when the requested tab doesn't exist — the app-side handler
    // already clamps against `tabs.len()`.
    for n in 1u8..=9 {
        bind(a, Char((b'0' + n) as char), GotoTab(n - 1));
    }
    m
}

/// Apply a `keybind = trigger=action` line on top of an existing map.
///
/// The action text `unbind` (also `none`, or empty after the `=`) **removes**
/// the trigger from the map rather than inserting — that's the only way for
/// a user to remove a default like `Ctrl+Shift+C` they want their shell or
/// another tool to receive instead. Matches Ghostty's `unbind` and WezTerm's
/// `DisableDefaultAssignment` / Alacritty's empty-action behavior.
pub fn apply_keybind(map: &mut Bindings, value: &str) {
    if value.is_empty() {
        return;
    }
    let Some((trig, act)) = value.split_once('=') else {
        return;
    };
    let Some(t) = parse_trigger(trig) else {
        return;
    };
    let act_trim = act.trim();
    if is_unbind_token(act_trim) {
        map.remove(&t);
        return;
    }
    if let Some(a) = Action::from_name(act_trim) {
        map.insert(t, a);
    }
}

/// Recognize the unbind sentinels: empty, `unbind`, `none`, `null`, `false`.
/// Pure so `detect_malformed_values` and `apply_keybind` agree on what's
/// valid action text (a sentinel here means "remove", not "malformed").
pub(crate) fn is_unbind_token(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "" | "unbind" | "none" | "null" | "false"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_label_formats() {
        let t = Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('e'));
        assert_eq!(t.label(), "Ctrl+Shift+E");
        assert_eq!(Trigger::new(Mods::ALT, Key::Left).label(), "Alt+Left");
        assert_eq!(Trigger::new(Mods::empty(), Key::F(5)).label(), "F5");
    }

    #[test]
    fn font_size_binds_cover_us_layout_shift_variants() {
        // On a US keyboard the "Ctrl+Plus" chord is actually
        // `Ctrl+Shift+=` (Shift held because `+` lives on `=`). winit
        // reports it as `mods = Ctrl+Shift, key = '+'` — without a
        // Ctrl+Shift+Plus binding the chord did nothing. Same family
        // for Ctrl+Shift+= and Ctrl+Shift+- (== Ctrl+_).
        let d = defaults();
        let c = Mods::CTRL;
        let cs = Mods::CTRL | Mods::SHIFT;
        for (mods, k, expected) in [
            (c, '+', Action::IncreaseFontSize),
            (c, '=', Action::IncreaseFontSize),
            (cs, '+', Action::IncreaseFontSize),
            (cs, '=', Action::IncreaseFontSize),
            (c, '-', Action::DecreaseFontSize),
            (cs, '-', Action::DecreaseFontSize),
            (cs, '_', Action::DecreaseFontSize),
        ] {
            let t = Trigger::new(mods, Key::Char(k));
            assert_eq!(
                d.get(&t),
                Some(&expected),
                "{t:?} should map to {expected:?}"
            );
        }
    }

    #[test]
    fn describe_reflects_user_overrides_and_unbinds() {
        // Cycle-103 contract: `--list-keybinds --config FILE` should
        // show the *effective* keymap, not the built-in defaults. The
        // pure `describe(&Bindings)` is what powers it; here we drive
        // it directly with a hand-built map to confirm: overrides
        // appear with the override action, unbound triggers don't
        // appear at all, and the output is sorted.
        let mut m = defaults();
        // Override a default to a different action.
        apply_keybind(&mut m, "ctrl+shift+t=close_tab");
        // Unbind another default.
        apply_keybind(&mut m, "ctrl+shift+c=unbind");

        let lines = describe(&m);
        // Override surfaces with the new action label.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Ctrl+Shift+T") && l.contains("CloseTab")),
            "override should appear with the new action: {lines:?}"
        );
        // No line should still show Ctrl+Shift+T → NewTab.
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("Ctrl+Shift+T") && l.contains("NewTab")),
            "stale default should not coexist with override: {lines:?}"
        );
        // Unbound trigger is gone entirely (no Ctrl+Shift+C line).
        assert!(
            !lines.iter().any(|l| l.starts_with("Ctrl+Shift+C")),
            "unbound trigger should not appear: {lines:?}"
        );
        // Output remains sorted by trigger label.
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted, "describe must return sorted lines");
        // And `describe_defaults` is still just `describe(&defaults())`.
        assert_eq!(describe_defaults(), describe(&defaults()));
    }

    #[test]
    fn alt_digit_keys_go_to_tab() {
        let d = defaults();
        // Alt+1 → GotoTab(0), …, Alt+9 → GotoTab(8). Every modern terminal
        // binds these (kitty / Terminator / iTerm2 / Ghostty). User mental
        // model is 1-based ("Alt+1 = first tab"); internally we store the
        // zero-based index the handler indexes into `tabs`.
        for n in 1u8..=9 {
            let t = Trigger::new(Mods::ALT, Key::Char((b'0' + n) as char));
            match d.get(&t) {
                Some(Action::GotoTab(i)) => assert_eq!(*i, n - 1, "Alt+{n} → tab {}", n - 1),
                other => panic!("Alt+{n} not bound to GotoTab: {other:?}"),
            }
        }
        // Alt+0 is *not* bound (browsers use Cmd+9 for "last tab" but kitty
        // / Terminator don't bind 0, so neither do we — kept free for the
        // user to bind manually if they want "last tab" semantics).
        let t0 = Trigger::new(Mods::ALT, Key::Char('0'));
        assert!(!d.contains_key(&t0), "Alt+0 should be free");
    }

    #[test]
    fn apply_keybind_unbind_removes_default() {
        // Default map ships with Ctrl+Shift+C → Copy. A user whose shell
        // wants Ctrl+Shift+C for itself (e.g. some readline kits) had no
        // way to remove it — `apply_keybind` only ever *inserted*. Now
        // `keybind = ctrl+shift+c = unbind` removes it; aliases are
        // `none` / `null` / `false` / empty (all map to "no action").
        let mut m = defaults();
        let trig = Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('c'));
        assert_eq!(m.get(&trig), Some(&Action::Copy), "ships bound by default");

        apply_keybind(&mut m, "ctrl+shift+c=unbind");
        assert!(!m.contains_key(&trig), "unbind removes the entry");

        // Re-bind to confirm map state is still healthy after a removal.
        apply_keybind(&mut m, "ctrl+shift+c=copy");
        assert_eq!(m.get(&trig), Some(&Action::Copy));

        // Every documented alias.
        for tok in ["unbind", "none", "null", "false", ""] {
            let mut mm = defaults();
            apply_keybind(&mut mm, &format!("ctrl+shift+c={tok}"));
            assert!(!mm.contains_key(&trig), "alias {tok:?} should also unbind");
        }

        // Unbind on an *unbound* trigger is a no-op (not an error).
        let mut mm = defaults();
        let unused = Trigger::new(Mods::CTRL | Mods::ALT, Key::Char('§'));
        let before = mm.len();
        apply_keybind(&mut mm, "ctrl+alt+§=unbind");
        assert_eq!(mm.len(), before, "unbinding a free trigger is a no-op");
        let _ = unused;
    }

    #[test]
    fn is_unbind_token_recognizes_aliases() {
        for tok in ["unbind", "Unbind", "UNBIND", "none", "null", "false", ""] {
            assert!(is_unbind_token(tok), "{tok:?} should be unbind");
        }
        // Anything else is a real action name (or a typo `from_name`
        // will reject — not unbind).
        for tok in ["copy", "no_action", "disabled", "off", "x"] {
            assert!(!is_unbind_token(tok), "{tok:?} should NOT be unbind");
        }
    }

    #[test]
    fn from_name_parses_goto_tab_one_based() {
        // `goto_tab:1` is the first tab — 1-based to match the keybind
        // intuition. Internally → GotoTab(0).
        assert!(matches!(
            Action::from_name("goto_tab:1"),
            Some(Action::GotoTab(0))
        ));
        assert!(matches!(
            Action::from_name("goto_tab:9"),
            Some(Action::GotoTab(8))
        ));
        // Zero is rejected (user mental model is 1-based; refuse the
        // ambiguity rather than silently mapping it to GotoTab(0)).
        assert!(Action::from_name("goto_tab:0").is_none());
        // Garbage values → None so unknown-key reporting still kicks in.
        assert!(Action::from_name("goto_tab:abc").is_none());
        assert!(Action::from_name("goto_tab:").is_none());
    }

    #[test]
    fn action_label_renders_goto_tab_with_one_based_index() {
        // `--list-keybinds` reads these labels; Debug-derived
        // `GotoTab(0)` leaks the 0-based internal index — a user looking
        // at the listing would think Alt+1 → tab 0. Use the 1-based
        // human form. Other variants use the Debug derive verbatim so
        // the existing labels (Copy, NewTab, SplitRight, …) don't
        // change.
        assert_eq!(action_label(&Action::GotoTab(0)), "Goto tab 1");
        assert_eq!(action_label(&Action::GotoTab(8)), "Goto tab 9");
        assert_eq!(action_label(&Action::Copy), "Copy");
        assert_eq!(action_label(&Action::SplitRight), "SplitRight");
    }

    #[test]
    fn describe_defaults_is_sorted_and_complete() {
        let lines = describe_defaults();
        assert_eq!(lines.len(), defaults().len(), "one line per binding");
        // Sorted by the (label, action) key.
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted);
        // A known Terminator binding is present and labelled.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Ctrl+Shift+E") && l.contains("SplitRight")),
            "expected the Ctrl+Shift+E split binding, got {lines:?}"
        );
    }
}
