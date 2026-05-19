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

/// Human-readable lines for the default keymap, sorted by trigger label —
/// powers `kettle --list-keybinds` so the binding set is discoverable
/// without reading the source.
pub fn describe_defaults() -> Vec<String> {
    let mut lines: Vec<(String, String)> = defaults()
        .iter()
        .map(|(t, a)| (t.label(), format!("{a:?}")))
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
    fn from_name(s: &str) -> Option<Action> {
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
            _ => return None,
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
    bind(c, Char('+'), IncreaseFontSize);
    bind(c, Char('='), IncreaseFontSize);
    bind(c, Char('-'), DecreaseFontSize);
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
    m
}

/// Apply a `keybind = trigger=action` line on top of an existing map.
pub fn apply_keybind(map: &mut Bindings, value: &str) {
    if value.is_empty() {
        return;
    }
    let Some((trig, act)) = value.split_once('=') else {
        return;
    };
    if let (Some(t), Some(a)) = (parse_trigger(trig), Action::from_name(act.trim())) {
        map.insert(t, a);
    }
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
