//! The portable menu-bar model and its macOS installation.
//!
//! Kettle key equivalents are display hints. The root NSMenu declines them,
//! leaving the original event to winit, including modal handling, keybind
//! resolution, program-ownership yielding and press/release tracking. Mouse
//! selections send actions through the event-loop proxy, and so do the
//! native window items AppKit would otherwise perform past winit (full
//! screen, Minimize and Zoom), whether chosen or typed.

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
    /// AppKit's own full-screen toggle, sent to the key window. Having it in
    /// the View menu keeps AppKit from adding a second Enter Full Screen.
    ToggleFullScreen,
    Minimize,
    Zoom,
    BringAllToFront,
}

/// A window change Kettle makes through winit for a native Window menu item.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    Minimize,
    Zoom,
}

/// Where Kettle sends an item's selection, through the event-loop proxy.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    Action(Action),
    Window(WindowCommand),
}

#[cfg(any(target_os = "macos", test))]
impl NativeAction {
    /// The AppKit action the item carries. Keeping the system's selector on
    /// the full-screen item is what stops AppKit adding a second one.
    pub(crate) fn selector(self) -> &'static str {
        match self {
            Self::About => "orderFrontStandardAboutPanel:",
            Self::Hide => "hide:",
            Self::HideOthers => "hideOtherApplications:",
            Self::ShowAll => "unhideAllApplications:",
            Self::Quit => "terminate:",
            Self::ToggleFullScreen => "toggleFullScreen:",
            Self::Minimize => "performMiniaturize:",
            Self::Zoom => "performZoom:",
            Self::BringAllToFront => "arrangeInFront:",
        }
    }

    /// Kettle answers these selectors itself. AppKit's own versions skip
    /// winit: `toggleFullScreen:` does nothing on a borderless window, which
    /// winit's `set_fullscreen` first gives a title bar, and
    /// `performMiniaturize:` and `performZoom:` press title-bar buttons a
    /// borderless window lacks. The others go to AppKit unchanged.
    pub(crate) fn route(self) -> Option<Route> {
        match self {
            Self::ToggleFullScreen => Some(Route::Action(Action::ToggleFullscreen)),
            Self::Minimize => Some(Route::Window(WindowCommand::Minimize)),
            Self::Zoom => Some(Route::Window(WindowCommand::Zoom)),
            Self::About
            | Self::Hide
            | Self::HideOthers
            | Self::ShowAll
            | Self::Quit
            | Self::BringAllToFront => None,
        }
    }

    /// The window items need a focused window; Minimize and Zoom are off in
    /// full screen, as in the system's own Window menu.
    fn enabled(self, context: Context) -> bool {
        match self {
            Self::ToggleFullScreen => context.window,
            Self::Minimize | Self::Zoom => context.window && !context.fullscreen,
            Self::About
            | Self::Hide
            | Self::HideOthers
            | Self::ShowAll
            | Self::Quit
            | Self::BringAllToFront => true,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Kettle(Action),
    Native(NativeAction),
}

#[cfg(any(target_os = "macos", test))]
impl Command {
    /// The item's AppKit action selector.
    pub(crate) fn selector(&self) -> &'static str {
        match self {
            Self::Kettle(_) => "kettleMenuAction:",
            Self::Native(action) => action.selector(),
        }
    }

    /// Where a Kettle target sends the selection; `None` leaves it to AppKit.
    pub(crate) fn route(&self) -> Option<Route> {
        match self {
            Self::Kettle(action) => Some(Route::Action(action.clone())),
            Self::Native(action) => action.route(),
        }
    }
}

/// The items AppKit may perform from the keyboard: the native ones, in menu
/// order. Kettle's own equivalents stay off this list so that the original
/// key event reaches winit.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn key_equivalent_rows(model: &[Menu]) -> Vec<&Row> {
    fn collect<'a>(rows: &'a [Row], out: &mut Vec<&'a Row>) {
        for row in rows {
            match row {
                Row::Item {
                    command: Command::Native(_),
                    ..
                } => out.push(row),
                Row::Submenu { rows, .. } => collect(rows, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for menu in model {
        collect(&menu.rows, &mut out);
    }
    out
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Row {
    Item {
        title: String,
        command: Command,
        shortcut: Option<Trigger>,
        shortcut_label: Option<String>,
        enabled: bool,
        /// Shown with a checkmark (`NSControlStateValueOn`).
        checked: bool,
    },
    Submenu {
        title: &'static str,
        rows: Vec<Row>,
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
    /// AppKit's Help menu, which gains the system's menu search field.
    help: bool,
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
    /// The `detachable-tabs` config key; off, a tab cannot leave its window.
    pub detachable_tabs: bool,
    /// The focused window is full screen.
    pub fullscreen: bool,
    /// What the window broadcasts to, for the Broadcast Input checkmark.
    pub broadcast: crate::app::BroadcastKind,
    /// The focused pane has a group, so Panes in This Group can turn on.
    pub pane_group: bool,
    /// The `broadcast-default` config key: which row the chord's action runs.
    pub broadcast_default: kettle_config::BroadcastDefault,
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
            // The moves wrap at the ends of the bar (`Mux::nudge_active_tab`),
            // so with two tabs every position moves and the active index is
            // not needed here.
            Action::NextTab | Action::PrevTab | Action::MoveTabLeft | Action::MoveTabRight => {
                self.tabs > 1
            }
            // A lone tab moved out would leave the same window behind.
            Action::MoveTabToNewWindow => self.detachable_tabs && self.tabs > 1,
            Action::ToggleZoom => self.zoomed || self.panes > 1,
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                self.renderer
            }
            Action::SelectAll
            | Action::ClearHistory
            | Action::ClosePane
            | Action::SplitRight
            | Action::SplitDown
            | Action::SplitLeft
            | Action::SplitUp
            | Action::StartSearch
            | Action::HintMode => self.pane,
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
pub(crate) fn menu_model(
    tr: &Translator,
    bindings: &Bindings,
    context: Context,
    inventory: &ProfileInventory,
) -> Vec<Menu> {
    menu_model_with_profiles(tr, bindings, context, inventory.profiles())
}

/// One worker, one pending invalidation and one latest result. Enumeration
/// never holds the result lock; dispatch only reads the event-loop-owned cache.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct ProfileInventory {
    cached: Vec<String>,
    requests: Option<std::sync::mpsc::SyncSender<()>>,
    latest: std::sync::Arc<std::sync::Mutex<Option<Vec<String>>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(target_os = "macos", test))]
impl ProfileInventory {
    #[cfg(target_os = "macos")]
    pub(crate) fn new(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        Self::with_enumerator(kettle_config::Config::list_profiles, wake)
    }

    fn with_enumerator(
        enumerate: impl Fn() -> Vec<String> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let (requests, rx) = std::sync::mpsc::sync_channel(1);
        let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
        let result = latest.clone();
        let worker = std::thread::Builder::new()
            .name("kettle-profiles".into())
            .spawn(move || {
                loop {
                    let profiles = enumerate();
                    let notify = result
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace(profiles)
                        .is_none();
                    if notify {
                        wake();
                    }
                    // Poll on the worker as well: profile creation/removal need not
                    // touch the active config file watched by live reload.
                    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            cached: Vec::new(),
            requests: Some(requests),
            latest,
            worker: Some(worker),
        })
    }

    pub(crate) fn refresh(&self) {
        if let Some(requests) = &self.requests {
            let _ = requests.try_send(());
        }
    }

    pub(crate) fn complete(&mut self) {
        if let Some(profiles) = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.cached = profiles;
        }
    }

    fn profiles(&self) -> &[String] {
        &self.cached
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for ProfileInventory {
    fn drop(&mut self) {
        self.requests.take();
        // A stalled filesystem must not stall event-loop shutdown either.
        self.worker.take();
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn menu_model_with_profiles(
    tr: &Translator,
    bindings: &Bindings,
    context: Context,
    profiles: &[String],
) -> Vec<Menu> {
    use Row::{Separator as Sep, Services};
    let command_bindings: Bindings = bindings
        .iter()
        .filter(|(trigger, _)| trigger.mods.contains(Mods::SUPER))
        .map(|(trigger, action)| (*trigger, action.clone()))
        .collect();
    // `available` narrows the action's own enabled state; `checked` marks it.
    let toggle_item = |text, action: Action, available: bool, checked: bool| {
        let shortcut = command_shortcut(&command_bindings, &action).or_else(|| {
            matches!(action, Action::HintMode)
                .then(|| command_shortcut(bindings, &action))
                .flatten()
        });
        Row::Item {
            title: tr.text(text).to_string(),
            enabled: available && context.enables(&action),
            command: Command::Kettle(action),
            shortcut: shortcut.as_ref().map(|(trigger, _)| *trigger),
            shortcut_label: shortcut.map(|(_, label)| label),
            checked,
        }
    };
    let item = |text, action: Action| toggle_item(text, action, true, false);
    let native = |text, action: NativeAction, chord: Option<Trigger>| {
        let shortcut = chord.filter(|trigger| !bindings.contains_key(trigger));
        Row::Item {
            title: tr.text(text).to_string(),
            command: Command::Native(action),
            shortcut,
            shortcut_label: shortcut.map(|trigger| trigger.label()),
            enabled: action.enabled(context),
            checked: false,
        }
    };
    let chord = |key, mods| Some(Trigger::new(mods | Mods::SUPER, Key::Char(key)));
    let menu = |text, rows, windows| Menu {
        title: tr.text(text),
        rows,
        windows,
        help: false,
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
                Row::Submenu {
                    title: tr.text(T::MenuNewWindowProfile),
                    rows: crate::app::profile_launch_entries(profiles, tr)
                        .into_iter()
                        .map(|(label, action)| Row::Item {
                            title: label.shown(tr).to_string(),
                            enabled: context.enables(&action),
                            command: Command::Kettle(action),
                            shortcut: None,
                            shortcut_label: None,
                            checked: false,
                        })
                        .collect(),
                },
                item(T::MenuNewTab, Action::NewTab),
                item(T::MenuSaveLayout, Action::SaveLayout),
                item(T::MenuOpenLayout, Action::OpenLayoutPicker),
                item(T::MacosMenuSshLauncher, Action::OpenSsh),
                Row::Submenu {
                    title: tr.text(T::MenuBroadcastInput),
                    rows: crate::app::broadcast_rows(
                        context.broadcast_default,
                        context.broadcast,
                        context.pane_group,
                    )
                    .into_iter()
                    .map(|row| toggle_item(row.kind.text(), row.action, row.enabled, row.checked))
                    .collect(),
                },
                Sep,
                item(T::MenuSplitRight, Action::SplitRight),
                item(T::MenuSplitLeft, Action::SplitLeft),
                item(T::MenuSplitDown, Action::SplitDown),
                item(T::MenuSplitUp, Action::SplitUp),
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
                item(T::MacosMenuQuickSelect, Action::HintMode),
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
                item(T::MacosMenuQuickTerminal, Action::ToggleQuickTerminal),
                Sep,
                // AppKit's own selector, so the system adds no second item,
                // answered by Kettle through winit (`NativeAction::route`).
                // The title follows the focused window; F11 and the green
                // button toggle the same native full screen.
                native(
                    if context.fullscreen {
                        T::MacosMenuExitFullscreen
                    } else {
                        T::MacosMenuFullscreen
                    },
                    NativeAction::ToggleFullScreen,
                    chord('f', Mods::CTRL),
                ),
            ],
            false,
        ),
        menu(
            T::MacosMenuWindow,
            vec![
                native(
                    T::MacosMenuMinimize,
                    NativeAction::Minimize,
                    chord('m', Mods::empty()),
                ),
                native(T::MacosMenuZoomWindow, NativeAction::Zoom, None),
                Sep,
                item(T::MacosMenuNextTab, Action::NextTab),
                item(T::MacosMenuPreviousTab, Action::PrevTab),
                Sep,
                item(T::MacosMenuRenameTab, Action::EditTabTitle),
                item(T::MacosMenuMoveTabLeft, Action::MoveTabLeft),
                item(T::MacosMenuMoveTabRight, Action::MoveTabRight),
                item(T::MacosMenuMoveTabToNewWindow, Action::MoveTabToNewWindow),
                Sep,
                native(
                    T::MacosMenuBringAllToFront,
                    NativeAction::BringAllToFront,
                    None,
                ),
            ],
            true,
        ),
        Menu {
            help: true,
            ..menu(
                T::MacosMenuHelp,
                vec![
                    item(T::MacosMenuKettleHelp, Action::ShowHelp),
                    item(T::MacosMenuKeyboardShortcuts, Action::ShowKeybinds),
                ],
                false,
            )
        },
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
    inventory: &ProfileInventory,
) {
    let tr = Translator::new(cfg.language.resolve(kettle_i18n::os_locale));
    imp::sync(proxy, menu_model(&tr, &cfg.keybinds, context, inventory));
}

/// Make a Window menu item's change through winit, which works on a borderless
/// window too. A full-screen window is left alone, as AppKit leaves it.
#[cfg(target_os = "macos")]
pub(crate) fn perform(command: WindowCommand, window: &winit::window::Window) {
    if window.fullscreen().is_some() {
        return;
    }
    match command {
        WindowCommand::Minimize => window.set_minimized(true),
        WindowCommand::Zoom => window.set_maximized(!window.is_maximized()),
    }
}

#[cfg(test)]
#[path = "macos_menu/tests.rs"]
mod tests;

#[cfg(test)]
fn kettle_actions(rows: &[Row]) -> Vec<&Action> {
    let mut actions = Vec::new();
    for row in rows {
        match row {
            Row::Item {
                command: Command::Kettle(action),
                ..
            } => actions.push(action),
            Row::Submenu { rows, .. } => actions.extend(kettle_actions(rows)),
            _ => {}
        }
    }
    actions
}
