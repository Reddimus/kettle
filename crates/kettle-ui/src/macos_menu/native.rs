//! AppKit objects stay on the main thread, outside App's mutable dispatch.
use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};
use winit::event_loop::EventLoopProxy;

use super::{Command, Key, Menu, Mods, NativeAction, Row, Trigger};
use crate::app::UserEvent;

struct State {
    model: Vec<Menu>,
    // NSMenuItem.target is unretained.
    target: Retained<MenuTarget>,
    proxy: EventLoopProxy<UserEvent>,
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
    impl DeclaredClass for MenuTarget { type Ivars = (); }
    // SAFETY: NSObject's protocol implementation is inherited unchanged.
    unsafe impl NSObjectProtocol for MenuTarget {}
    // SAFETY: the selector has AppKit's void(id) action signature.
    unsafe impl MenuTarget {
        #[method(kettleMenuAction:)]
        fn activate(&self, sender: &NSMenuItem) {
            STATE.with(|state| {
                let state = state.borrow();
                let Some(state) = state.as_ref() else { return };
                let command = state.model.iter().flat_map(|menu| &menu.rows)
                    .filter_map(|row| match row {
                        Row::Item { command: Command::Kettle(action), .. } => Some(action),
                        _ => None,
                    }).nth({
                        // SAFETY: sender is the live menu item AppKit passed to this action.
                        unsafe { sender.tag() as usize }
                    });
                if let Some(action) = command {
                    let _ = state.proxy.send_event(UserEvent::MenuCommand(action.clone()));
                }
            });
        }
    }
);

declare_class!(
    struct MenuBar;
    // SAFETY: declare_class provides NSMenu layout and main-thread-only AppKit access.
    unsafe impl ClassType for MenuBar {
        type Super = NSMenu;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "KettleMenuBar";
    }
    impl DeclaredClass for MenuBar {
        // An isolated menu containing only native application commands. Calling
        // super would also perform Kettle equivalents before winit can yield.
        type Ivars = Retained<NSMenu>;
    }
    // SAFETY: NSObject's protocol implementation is inherited unchanged.
    unsafe impl NSObjectProtocol for MenuBar {}
    // SAFETY: this override has NSMenu's BOOL(NSEvent*) signature.
    unsafe impl MenuBar {
        #[method(performKeyEquivalent:)]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            // Only the native About/Hide/Quit menu can consume an equivalent.
            // Every Kettle equivalent returns NO here and follows the original
            // NSApplication -> NSWindow -> winit KeyboardInput path, even when
            // its menu item is disabled. The original event retains keyboard
            // layout, physical code, repeat and modifiers, as well as key-up.
            // SAFETY: the ivar is a live NSMenu and event is AppKit's original event.
            Bool::new(unsafe { self.ivars().performKeyEquivalent(event) })
        }
    }
);

fn selector_for(action: NativeAction) -> Sel {
    match action {
        NativeAction::About => sel!(orderFrontStandardAboutPanel:),
        NativeAction::Hide => sel!(hide:),
        NativeAction::HideOthers => sel!(hideOtherApplications:),
        NativeAction::ShowAll => sel!(unhideAllApplications:),
        NativeAction::Quit => sel!(terminate:),
    }
}

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
        Key::Backspace => "\u{8}".into(),
        Key::Delete => "\u{f728}".into(),
    };
    (key, flags)
}

fn make_item(
    mtm: MainThreadMarker,
    title: &str,
    selector: Sel,
    shortcut: Option<Trigger>,
) -> Retained<NSMenuItem> {
    let (key, flags) = shortcut
        .map(equivalent)
        .unwrap_or_else(|| (String::new(), NSEventModifierFlags::empty()));
    // SAFETY: selector is either our void(id) target or a standard AppKit action.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            Some(selector),
            &NSString::from_str(&key),
        )
    };
    item.setKeyEquivalentModifierMask(flags);
    item
}

fn new_menu(mtm: MainThreadMarker, native: &Retained<NSMenu>) -> Retained<MenuBar> {
    let menu = mtm.alloc::<MenuBar>().set_ivars(native.clone());
    // SAFETY: this is a newly allocated NSMenu subclass with initialized ivars.
    unsafe { msg_send_id![super(menu), init] }
}

pub(super) fn sync(proxy: &EventLoopProxy<UserEvent>, model: Vec<Menu>) {
    if STATE.with(|state| state.borrow().as_ref().is_some_and(|s| s.model == model)) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let native = NSMenu::new(mtm);
    let bar = new_menu(mtm, &native);
    // AppKit can retain a replaced menu while tracking it. Reuse the target
    // for the process lifetime so those unretained target pointers stay valid.
    let target = STATE
        .with(|state| state.borrow().as_ref().map(|s| s.target.clone()))
        .unwrap_or_else(|| {
            let target = mtm.alloc::<MenuTarget>().set_ivars(());
            // SAFETY: this is a newly allocated NSObject subclass with initialized ivars.
            unsafe { msg_send_id![super(target), init] }
        });
    let mut tag = 0;
    for menu in &model {
        let submenu = new_menu(mtm, &native);
        // SAFETY: this is our live main-thread menu, validated by the portable model.
        unsafe {
            submenu.setTitle(&NSString::from_str(menu.title));
            submenu.setAutoenablesItems(false);
        }
        for row in &menu.rows {
            match row {
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
                Row::Item {
                    title,
                    command,
                    shortcut,
                    enabled,
                    ..
                } => {
                    let selector = match command {
                        Command::Kettle(_) => sel!(kettleMenuAction:),
                        Command::Native(action) => selector_for(*action),
                    };
                    let item = make_item(mtm, title, selector, *shortcut);
                    // SAFETY: enabled comes from the focused window's model.
                    unsafe { item.setEnabled(*enabled) };
                    if matches!(command, Command::Kettle(_)) {
                        // SAFETY: our target interprets these sequential nonnegative tags.
                        unsafe { item.setTag(tag) };
                        tag += 1;
                        let target_obj: &AnyObject = &target;
                        // SAFETY: State retains target for every menu item's lifetime.
                        unsafe { item.setTarget(Some(target_obj)) };
                    } else {
                        native.addItem(&make_item(mtm, title, selector, *shortcut));
                    }
                    submenu.addItem(&item);
                }
            }
        }
        let item = NSMenuItem::new(mtm);
        // SAFETY: this is a newly allocated main-thread menu item.
        unsafe { item.setTitle(&NSString::from_str(menu.title)) };
        item.setSubmenu(Some(&submenu));
        bar.addItem(&item);
        if menu.windows {
            // SAFETY: AppKit retains this menu and fills it with open windows.
            unsafe { app.setWindowsMenu(Some(&submenu)) };
        }
    }
    app.setMainMenu(Some(&bar));
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            model,
            target,
            proxy: proxy.clone(),
        })
    });
}
