//! AppKit objects stay on the main thread, outside App's mutable dispatch.
use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem,
};
use objc2_foundation::{MainThreadMarker, NSString};
use winit::event_loop::EventLoopProxy;

use super::{Key, Menu, Mods, Route, Row, Trigger};
use crate::app::UserEvent;

#[path = "equivalents.rs"]
mod equivalents;
use equivalents::new_menu;

struct State {
    model: Vec<Menu>,
}

struct Invocation {
    route: Route,
    proxy: EventLoopProxy<UserEvent>,
}

impl Invocation {
    fn send(&self) {
        let event = match &self.route {
            Route::Action(action) => UserEvent::MenuCommand(action.clone()),
            Route::Window(command) => UserEvent::MenuWindow(*command),
        };
        let _ = self.proxy.send_event(event);
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

declare_class!(
    struct MenuTarget;
    // SAFETY: declare_class provides NSObject layout; AppKit uses this only on the main thread.
    unsafe impl ClassType for MenuTarget {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "KettleMenuTarget";
    }
    impl DeclaredClass for MenuTarget { type Ivars = Invocation; }
    // SAFETY: NSObject's protocol implementation is inherited unchanged.
    unsafe impl NSObjectProtocol for MenuTarget {}
    // SAFETY: every selector has AppKit's void(id) action signature. An item
    // sends only its own action to its target, so each method sends that
    // item's route; the native selectors are the ones `NativeAction::route`
    // claims, and AppKit keeps recognising them on the item.
    unsafe impl MenuTarget {
        #[method(kettleMenuAction:)]
        fn activate(&self, _sender: &NSMenuItem) {
            self.ivars().send();
        }

        #[method(toggleFullScreen:)]
        fn toggle_full_screen(&self, _sender: &NSMenuItem) {
            self.ivars().send();
        }

        #[method(performMiniaturize:)]
        fn perform_miniaturize(&self, _sender: &NSMenuItem) {
            self.ivars().send();
        }

        #[method(performZoom:)]
        fn perform_zoom(&self, _sender: &NSMenuItem) {
            self.ivars().send();
        }
    }
);

fn equivalent(trigger: Trigger) -> (String, NSEventModifierFlags) {
    use NSEventModifierFlags as F;
    let mut flags = F::empty();
    for (mods, flag) in [
        (Mods::SUPER, F::NSEventModifierFlagCommand),
        (Mods::CTRL, F::NSEventModifierFlagControl),
        (Mods::ALT, F::NSEventModifierFlagOption),
        (Mods::SHIFT, F::NSEventModifierFlagShift),
    ] {
        if trigger.mods.contains(mods) {
            flags |= flag;
        }
    }
    let key = match trigger.key {
        Key::Char(c) => c.to_string(),
        Key::Up => "\u{f700}".into(),
        Key::Down => "\u{f701}".into(),
        Key::Left => "\u{f702}".into(),
        Key::Right => "\u{f703}".into(),
        Key::F(n) => char::from_u32(0xf703 + u32::from(n))
            .unwrap_or_default()
            .to_string(),
        Key::Home => "\u{f729}".into(),
        Key::End => "\u{f72b}".into(),
        Key::PageUp => "\u{f72c}".into(),
        Key::PageDown => "\u{f72d}".into(),
        Key::Enter => "\r".into(),
        Key::Tab => "\t".into(),
        Key::Escape => "\u{1b}".into(),
        Key::Backspace => "\u{8}".into(),
        Key::Delete => "\u{f728}".into(),
    };
    (key, flags)
}

/// One menu item for `row`, which must be `Row::Item`. A routed item owns its
/// target, so a rebuilt menu cannot retarget a selection in an old one that
/// AppKit is still tracking.
fn make_item(
    mtm: MainThreadMarker,
    proxy: &EventLoopProxy<UserEvent>,
    row: &Row,
) -> Option<Retained<NSMenuItem>> {
    let Row::Item {
        title,
        command,
        shortcut,
        enabled,
        checked,
        ..
    } = row
    else {
        return None;
    };
    let (key, flags) = shortcut
        .map(equivalent)
        .unwrap_or_else(|| (String::new(), NSEventModifierFlags::empty()));
    // SAFETY: the selector is a standard AppKit action or one MenuTarget
    // implements, and the item gets that target below.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            Some(Sel::register(command.selector())),
            &NSString::from_str(&key),
        )
    };
    item.setKeyEquivalentModifierMask(flags);
    // SAFETY: enabled and checked come from the focused window's model.
    unsafe {
        item.setEnabled(*enabled);
        item.setState(if *checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }
    if let Some(route) = command.route() {
        let target = mtm.alloc::<MenuTarget>().set_ivars(Invocation {
            route,
            proxy: proxy.clone(),
        });
        // SAFETY: newly allocated NSObject subclass with initialized ivars.
        let target: Retained<MenuTarget> = unsafe { msg_send_id![super(target), init] };
        let target_obj: &AnyObject = &target;
        // SAFETY: representedObject retains the target for this item's lifetime.
        unsafe {
            item.setRepresentedObject(Some(target_obj));
            item.setTarget(Some(target_obj));
        }
    }
    Some(item)
}

pub(super) fn sync(proxy: &EventLoopProxy<UserEvent>, model: Vec<Menu>) {
    if STATE.with(|state| state.borrow().as_ref().is_some_and(|s| s.model == model)) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // The menu AppKit performs key equivalents from: native items only, each
    // built as its visible twin, Kettle target and enabled state included.
    let native = NSMenu::new(mtm);
    // SAFETY: a newly created main-thread menu with explicit enabled state.
    unsafe { native.setAutoenablesItems(false) };
    for row in super::key_equivalent_rows(&model) {
        if let Some(item) = make_item(mtm, proxy, row) {
            native.addItem(&item);
        }
    }
    let bar = new_menu(mtm, &native);
    for menu in &model {
        let submenu = new_menu(mtm, &native);
        // SAFETY: this is our live main-thread menu, validated by the portable model.
        unsafe {
            submenu.setTitle(&NSString::from_str(menu.title));
            submenu.setAutoenablesItems(false);
        }
        append_rows(mtm, &submenu, &native, &app, proxy, &menu.rows);
        let item = NSMenuItem::new(mtm);
        // SAFETY: this is a newly allocated main-thread menu item.
        unsafe { item.setTitle(&NSString::from_str(menu.title)) };
        item.setSubmenu(Some(&submenu));
        bar.addItem(&item);
        if menu.windows {
            // SAFETY: AppKit retains this menu and fills it with open windows.
            unsafe { app.setWindowsMenu(Some(&submenu)) };
        }
        if menu.help {
            // SAFETY: AppKit retains this menu and adds its menu search field.
            unsafe { app.setHelpMenu(Some(&submenu)) };
        }
    }
    app.setMainMenu(Some(&bar));
    STATE.with(|state| *state.borrow_mut() = Some(State { model }));
}

fn append_rows(
    mtm: MainThreadMarker,
    submenu: &NSMenu,
    native: &Retained<NSMenu>,
    app: &NSApplication,
    proxy: &EventLoopProxy<UserEvent>,
    rows: &[Row],
) {
    for row in rows {
        match row {
            Row::Submenu { title, rows } => {
                let child = new_menu(mtm, native);
                // SAFETY: newly created main-thread menu with explicit enabled state.
                unsafe {
                    child.setAutoenablesItems(false);
                }
                append_rows(mtm, &child, native, app, proxy, rows);
                let item = NSMenuItem::new(mtm);
                // SAFETY: newly allocated main-thread menu item.
                unsafe {
                    item.setTitle(&NSString::from_str(title));
                }
                item.setSubmenu(Some(&child));
                submenu.addItem(&item);
            }
            Row::Info(title) => {
                let item = NSMenuItem::new(mtm);
                // SAFETY: a non-action status on a newly allocated main-thread item.
                unsafe {
                    item.setTitle(&NSString::from_str(title));
                    item.setEnabled(false);
                }
                submenu.addItem(&item);
            }
            Row::Separator => submenu.addItem(&NSMenuItem::separatorItem(mtm)),
            Row::Services(title) => {
                let item = NSMenuItem::new(mtm);
                // SAFETY: this is a newly allocated main-thread menu item.
                unsafe { item.setTitle(&NSString::from_str(title)) };
                let services = NSMenu::new(mtm);
                item.setSubmenu(Some(&services));
                submenu.addItem(&item);
                // SAFETY: AppKit retains the live services menu.
                unsafe { app.setServicesMenu(Some(&services)) };
            }
            Row::Item { .. } => {
                if let Some(item) = make_item(mtm, proxy, row) {
                    submenu.addItem(&item);
                }
            }
        }
    }
}
