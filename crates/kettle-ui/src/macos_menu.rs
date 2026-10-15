//! macOS menu bar: Kettle's application menu in the UI's language.
//!
//! winit builds an English application menu as the app finishes launching
//! (About, Services, Hide, Hide Others, Show All, Quit), before the first
//! `resumed`, so that an app can replace it. This replaces it with the same
//! rows, the same standard actions and the same key equivalents, so no chord
//! that reaches the PTY changes, titled in the language Kettle runs in, and
//! naming the product the same whether Kettle runs from its bundle or from
//! the bare executable. Kettle's own commands live in its palette and
//! context menus, not the menu bar.
//!
//! Cross-platform on purpose, with the `cfg` inside `imp`, as in
//! `macos_dock`.

/// The standard action a row of the application menu sends.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppAction {
    /// `orderFrontStandardAboutPanel:`
    About,
    /// `hide:`
    Hide,
    /// `hideOtherApplications:`
    HideOthers,
    /// `unhideAllApplications:`
    ShowAll,
    /// `terminate:`
    Quit,
}

/// One row of the application menu, top to bottom.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMenuRow {
    /// A row that sends `action`, with `key` as its Command key equivalent
    /// (none when empty), Option joining Command when `option`.
    Item {
        title: &'static str,
        action: AppAction,
        key: &'static str,
        option: bool,
    },
    /// The Services submenu, which macOS fills.
    Services(&'static str),
    Separator,
}

/// The application menu, in the UI's language: winit's rows, actions and
/// key equivalents.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn app_menu_model(tr: &kettle_i18n::Translator) -> [AppMenuRow; 8] {
    use AppMenuRow::{Item, Separator, Services};
    use kettle_i18n::Text as T;
    let item = |title, action, key, option| Item {
        title: tr.text(title),
        action,
        key,
        option,
    };
    [
        item(T::MacosMenuAbout, AppAction::About, "", false),
        Separator,
        Services(tr.text(T::MacosMenuServices)),
        item(T::MacosMenuHide, AppAction::Hide, "h", false),
        item(T::MacosMenuHideOthers, AppAction::HideOthers, "h", true),
        item(T::MacosMenuShowAll, AppAction::ShowAll, "", false),
        Separator,
        item(T::MacosMenuQuit, AppAction::Quit, "q", false),
    ]
}

/// Replace winit's application menu with Kettle's, once, from the first
/// `resumed`. A no-op off macOS.
pub(crate) fn install(tr: &kettle_i18n::Translator) {
    imp::install(tr);
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::Cell;

    use objc2::runtime::Sel;
    use objc2::sel;
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSString};

    use super::{AppAction, AppMenuRow, app_menu_model};

    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }

    fn selector_for(action: AppAction) -> Sel {
        match action {
            AppAction::About => sel!(orderFrontStandardAboutPanel:),
            AppAction::Hide => sel!(hide:),
            AppAction::HideOthers => sel!(hideOtherApplications:),
            AppAction::ShowAll => sel!(unhideAllApplications:),
            AppAction::Quit => sel!(terminate:),
        }
    }

    pub(super) fn install(tr: &kettle_i18n::Translator) {
        if INSTALLED.with(|installed| installed.replace(true)) {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("menu bar: install called off the main thread");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let menubar = NSMenu::new(mtm);
        let app_item = NSMenuItem::new(mtm);
        menubar.addItem(&app_item);
        let app_menu = NSMenu::new(mtm);
        let mut services = None;
        for row in app_menu_model(tr) {
            match row {
                AppMenuRow::Separator => app_menu.addItem(&NSMenuItem::separatorItem(mtm)),
                AppMenuRow::Services(title) => {
                    // SAFETY: a plain title, no action and no key
                    // equivalent; macOS fills the submenu it is given.
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            mtm.alloc(),
                            &NSString::from_str(title),
                            None,
                            &NSString::from_str(""),
                        )
                    };
                    let submenu = NSMenu::new(mtm);
                    item.setSubmenu(Some(&submenu));
                    app_menu.addItem(&item);
                    services = Some(submenu);
                }
                AppMenuRow::Item {
                    title,
                    action,
                    key,
                    option,
                } => {
                    // SAFETY: each selector is a standard NSApplication
                    // action the responder chain answers, as winit's own
                    // menu sends.
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            mtm.alloc(),
                            &NSString::from_str(title),
                            Some(selector_for(action)),
                            &NSString::from_str(key),
                        )
                    };
                    if option {
                        item.setKeyEquivalentModifierMask(
                            NSEventModifierFlags::NSEventModifierFlagOption
                                | NSEventModifierFlags::NSEventModifierFlagCommand,
                        );
                    }
                    app_menu.addItem(&item);
                }
            }
        }
        app_item.setSubmenu(Some(&app_menu));
        if let Some(services) = &services {
            // SAFETY: the services menu is a live NSMenu this process owns.
            unsafe { app.setServicesMenu(Some(services)) };
        }
        app.setMainMenu(Some(&menubar));
        log::debug!("menu bar: installed in the UI's language");
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// No menu bar outside macOS: Kettle's menus there are its own.
    pub(super) fn install(_tr: &kettle_i18n::Translator) {}
}

#[cfg(test)]
mod tests {
    use super::{AppAction, AppMenuRow, app_menu_model};
    use kettle_i18n::{Language, Translator};

    const EN: Translator = Translator::new(Language::En);
    const ES: Translator = Translator::new(Language::Es);

    /// The application menu keeps winit's rows, actions and key
    /// equivalents, so no chord changes, and names them in the UI's
    /// language, the product named the same however Kettle was launched.
    #[test]
    fn the_app_menu_keeps_winit_s_rows_in_the_ui_s_language() {
        let shape = |tr: &Translator| {
            app_menu_model(tr)
                .map(|row| match row {
                    AppMenuRow::Item {
                        action,
                        key,
                        option,
                        ..
                    } => Some((action, key, option)),
                    AppMenuRow::Services(_) | AppMenuRow::Separator => None,
                })
                .to_vec()
        };
        let expected = vec![
            Some((AppAction::About, "", false)),
            None,
            None,
            Some((AppAction::Hide, "h", false)),
            Some((AppAction::HideOthers, "h", true)),
            Some((AppAction::ShowAll, "", false)),
            None,
            Some((AppAction::Quit, "q", false)),
        ];
        assert_eq!(shape(&EN), expected);
        assert_eq!(shape(&ES), expected);
        let titles = |tr: &Translator| {
            app_menu_model(tr)
                .iter()
                .filter_map(|row| match row {
                    AppMenuRow::Item { title, .. } | AppMenuRow::Services(title) => Some(*title),
                    AppMenuRow::Separator => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            titles(&EN),
            [
                "About Kettle",
                "Services",
                "Hide Kettle",
                "Hide Others",
                "Show All",
                "Quit Kettle"
            ]
        );
        let spanish = titles(&ES);
        assert_eq!(spanish.len(), 6);
        for (english, spanish) in titles(&EN).iter().zip(&spanish) {
            assert_ne!(english, spanish, "untranslated");
        }
        assert!(
            spanish
                .iter()
                .filter(|title| title.contains("Kettle"))
                .count()
                == 3
        );
    }

    /// The first `resumed`, after winit's menu is up, installs Kettle's,
    /// unconditionally, the platform split inside `imp`.
    #[test]
    fn resumed_installs_the_app_menu() {
        let source = kettle_test_support::production_source(&crate::app::app_source());
        let needle = "crate::macos_menu::install(&self.ui_text);";
        assert_eq!(source.matches(needle).count(), 1);
        let resumed = source
            .split("fn resumed(&mut self, event_loop: &ActiveEventLoop) {")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .expect("resumed");
        assert!(resumed.contains(needle));
        let at = source.find(needle).expect("install call site");
        assert!(!source[at.saturating_sub(240)..at].contains("#[cfg("));
    }
}
