//! The portable menu-bar model and its macOS installation.
//!
//! Kettle key equivalents are display hints. The root NSMenu declines them,
//! leaving the original event to winit, including modal handling, keybind
//! resolution, program-ownership yielding and press/release tracking. Only
//! mouse selections send actions through the event-loop proxy.

#[cfg(any(target_os = "macos", test))]
use kettle_config::keybinds::{Action, Bindings, Key, Mods, Trigger, hint_label};
#[cfg(any(target_os = "macos", test))]
use kettle_i18n::{Text as T, Translator};

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeAction {
    About,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Kettle(Action),
    Native(NativeAction),
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Row {
    Item {
        title: &'static str,
        command: Command,
        shortcut: Option<Trigger>,
        shortcut_label: Option<String>,
        enabled: bool,
    },
    Services(&'static str),
    Separator,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Menu {
    title: &'static str,
    rows: Vec<Row>,
    windows: bool,
}

/// Facts from the focused window; no AppKit, window or terminal handles.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Context {
    pub window: bool,
    pub pane: bool,
    pub tabs: usize,
    pub panes: usize,
    pub zoomed: bool,
    pub clipboard: bool,
    pub selection: bool,
    pub smart_copy: bool,
    pub writable: bool,
    pub renderer: bool,
}

#[cfg(any(target_os = "macos", test))]
impl Context {
    pub(crate) fn enables(self, action: &Action) -> bool {
        if !self.window {
            return false;
        }
        match action {
            Action::Copy => self.pane && self.clipboard && (!self.smart_copy || self.selection),
            Action::Paste => self.pane && self.clipboard && self.writable,
            Action::NextTab | Action::PrevTab => self.tabs > 1,
            Action::ToggleZoom => self.zoomed || self.panes > 1,
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                self.renderer
            }
            Action::SelectAll
            | Action::ClearHistory
            | Action::ClosePane
            | Action::SplitRight
            | Action::SplitDown
            | Action::StartSearch => self.pane,
            _ => true,
        }
    }
}

/// Use the same ranking and formatting as every other shortcut hint, but
/// consider only Command chords. A shorter Ctrl chord must not hide a Cmd one.
#[cfg(any(target_os = "macos", test))]
fn command_shortcut(bindings: &Bindings, action: &Action) -> Option<(Trigger, String)> {
    let label = hint_label(bindings, action)?;
    let trigger = bindings.keys().find(|trigger| trigger.label() == label)?;
    Some((*trigger, label))
}

/// Pure, platform-independent menu construction. Native app commands retain
/// their standard equivalents unless the live keymap owns that chord.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn menu_model(tr: &Translator, bindings: &Bindings, context: Context) -> Vec<Menu> {
    use Row::{Separator as Sep, Services};
    let command_bindings: Bindings = bindings
        .iter()
        .filter(|(trigger, _)| trigger.mods.contains(Mods::SUPER))
        .map(|(trigger, action)| (*trigger, action.clone()))
        .collect();
    let item = |text, action: Action| {
        let shortcut = command_shortcut(&command_bindings, &action);
        Row::Item {
            title: tr.text(text),
            enabled: context.enables(&action),
            command: Command::Kettle(action),
            shortcut: shortcut.as_ref().map(|(trigger, _)| *trigger),
            shortcut_label: shortcut.map(|(_, label)| label),
        }
    };
    let native = |text, action, chord: Option<Trigger>| {
        let shortcut = chord.filter(|trigger| !bindings.contains_key(trigger));
        Row::Item {
            title: tr.text(text),
            command: Command::Native(action),
            shortcut,
            shortcut_label: shortcut.map(|trigger| trigger.label()),
            enabled: true,
        }
    };
    let chord = |key, mods| Some(Trigger::new(mods | Mods::SUPER, Key::Char(key)));
    let menu = |text, rows, windows| Menu {
        title: tr.text(text),
        rows,
        windows,
    };
    vec![
        menu(
            T::MacosMenuApp,
            vec![
                native(T::MacosMenuAbout, NativeAction::About, None),
                Sep,
                item(T::MenuSettings, Action::OpenSettings),
                Sep,
                Services(tr.text(T::MacosMenuServices)),
                Sep,
                native(
                    T::MacosMenuHide,
                    NativeAction::Hide,
                    chord('h', Mods::empty()),
                ),
                native(
                    T::MacosMenuHideOthers,
                    NativeAction::HideOthers,
                    chord('h', Mods::ALT),
                ),
                native(T::MacosMenuShowAll, NativeAction::ShowAll, None),
                Sep,
                native(
                    T::MacosMenuQuit,
                    NativeAction::Quit,
                    chord('q', Mods::empty()),
                ),
            ],
            false,
        ),
        menu(
            T::MacosMenuShell,
            vec![
                item(T::MenuNewWindow, Action::NewWindow),
                item(T::MenuNewTab, Action::NewTab),
                Sep,
                item(T::MenuSplitRight, Action::SplitRight),
                item(T::MenuSplitDown, Action::SplitDown),
                Sep,
                item(T::MenuClosePane, Action::ClosePane),
                item(T::MenuCloseTab, Action::CloseTab),
                item(T::MenuCloseWindow, Action::CloseWindow),
            ],
            false,
        ),
        menu(
            T::MacosMenuEdit,
            vec![
                item(T::MenuCopy, Action::Copy),
                item(T::MenuPaste, Action::Paste),
                item(T::MacosMenuSelectAll, Action::SelectAll),
                Sep,
                item(T::MacosMenuFind, Action::StartSearch),
                Sep,
                item(T::MacosMenuClear, Action::ClearHistory),
            ],
            false,
        ),
        menu(
            T::MacosMenuView,
            vec![
                item(T::MacosMenuPalette, Action::CommandPalette),
                item(T::MacosMenuTheme, Action::OpenThemePicker),
                Sep,
                item(T::MacosMenuFontIncrease, Action::IncreaseFontSize),
                item(T::MacosMenuFontDecrease, Action::DecreaseFontSize),
                item(T::MacosMenuFontReset, Action::ResetFontSize),
                Sep,
                item(T::MacosMenuZoomPane, Action::ToggleZoom),
                item(T::MacosMenuFullscreen, Action::ToggleFullscreen),
            ],
            false,
        ),
        // No Minimize, Zoom Window or Bring All to Front Action exists.
        menu(
            T::MacosMenuWindow,
            vec![
                item(T::MacosMenuNextTab, Action::NextTab),
                item(T::MacosMenuPreviousTab, Action::PrevTab),
            ],
            true,
        ),
    ]
}

#[cfg(target_os = "macos")]
#[path = "macos_menu/native.rs"]
mod imp;

/// Reconcile the model; installation is confined to the AppKit main thread.
#[cfg(target_os = "macos")]
pub(crate) fn sync(
    proxy: &winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
    cfg: &kettle_config::Config,
    context: Context,
) {
    let tr = Translator::new(cfg.language.resolve(kettle_i18n::os_locale));
    imp::sync(proxy, menu_model(&tr, &cfg.keybinds, context));
}

#[cfg(test)]
#[path = "macos_menu/tests.rs"]
mod tests;
