// AppKit requires the process main thread, so this test has no libtest harness.
#[cfg(target_os = "macos")]
#[path = "../src/macos_menu/equivalents.rs"]
mod equivalents;

#[cfg(target_os = "macos")]
mod appkit {
    use std::cell::Cell;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol};
    use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
    use objc2_app_kit::{
        NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem,
    };
    use objc2_foundation::{MainThreadMarker, NSPoint, NSString};

    declare_class!(
        struct Target;
        // SAFETY: NSObject layout and main-thread-only access come from declare_class.
        unsafe impl ClassType for Target {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "KettleMenuEquivalentTestTarget";
        }
        impl DeclaredClass for Target { type Ivars = Cell<usize>; }
        // SAFETY: NSObject's protocol implementation is inherited unchanged.
        unsafe impl NSObjectProtocol for Target {}
        // SAFETY: the action has AppKit's void(id) signature.
        unsafe impl Target {
            #[method(invoke:)]
            fn invoke(&self, _sender: &NSMenuItem) {
                self.ivars().set(self.ivars().get() + 1);
            }
        }
    );

    fn item(mtm: MainThreadMarker, key: &str, target: &Target) -> Retained<NSMenuItem> {
        // SAFETY: the selector is implemented above, and target outlives the menu items.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(key),
                Some(sel!(invoke:)),
                &NSString::from_str(key),
            )
        };
        item.setKeyEquivalentModifierMask(NSEventModifierFlags::NSEventModifierFlagCommand);
        // SAFETY: the target is retained until after all event delivery.
        unsafe { item.setTarget(Some(target)) };
        item
    }

    fn key_event(key: &str) -> Retained<NSEvent> {
        // SAFETY: construct a synthetic key-down with valid strings and no window/context.
        unsafe {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown,
                NSPoint::new(0.0, 0.0),
                NSEventModifierFlags::NSEventModifierFlagCommand,
                0.0,
                0,
                None,
                &NSString::from_str(key),
                &NSString::from_str(key),
                false,
                0,
            )
            .expect("key event")
        }
    }

    pub(super) fn services_equivalents() {
        let mtm = MainThreadMarker::new().expect("AppKit test must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        let target = mtm.alloc::<Target>().set_ivars(Cell::new(0));
        // SAFETY: initialize a newly allocated NSObject subclass.
        let target: Retained<Target> = unsafe { msg_send_id![super(target), init] };
        let native = NSMenu::new(mtm);
        native.addItem(&item(mtm, "h", &target));
        let bar = super::equivalents::new_menu(mtm, &native);
        let submenu = super::equivalents::new_menu(mtm, &native);
        let kettle_item = item(mtm, "t", &target);
        submenu.addItem(&kettle_item);
        let parent = NSMenuItem::new(mtm);
        parent.setSubmenu(Some(&submenu));
        bar.addItem(&parent);

        let services = NSMenu::new(mtm);
        let nested = NSMenu::new(mtm);
        let action = item(mtm, "u", &target);
        nested.addItem(&action);
        let category = NSMenuItem::new(mtm);
        category.setSubmenu(Some(&nested));
        services.addItem(&category);
        let services_item = NSMenuItem::new(mtm);
        services_item.setSubmenu(Some(&services));
        submenu.addItem(&services_item);
        // SAFETY: register live menus, as production sync does. No system Services are modified.
        unsafe {
            app.setServicesMenu(Some(&services));
            native.setAutoenablesItems(false);
            services.setAutoenablesItems(false);
            nested.setAutoenablesItems(false);
            submenu.setAutoenablesItems(false);
        }
        app.setMainMenu(Some(&bar));

        // SAFETY: all menus and events are live and this is the AppKit main thread.
        unsafe {
            assert!(services.performKeyEquivalent(&key_event("u")));
            assert_eq!(target.ivars().get(), 1);
            target.ivars().set(0);
            assert!(bar.performKeyEquivalent(&key_event("h")));
            assert_eq!(target.ivars().get(), 1);
            assert!(
                bar.performKeyEquivalent(&key_event("u")),
                "registered Services equivalents must remain reachable"
            );
            assert_eq!(target.ivars().get(), 2);
            assert!(submenu.performKeyEquivalent(&key_event("u")));
            assert_eq!(target.ivars().get(), 3);
            assert!(!bar.performKeyEquivalent(&key_event("t")));
            assert!(!submenu.performKeyEquivalent(&key_event("t")));
            kettle_item.setEnabled(false);
            assert!(!bar.performKeyEquivalent(&key_event("t")));
            assert_eq!(target.ivars().get(), 3);
            action.setEnabled(false);
            let disabled_service_result = services.performKeyEquivalent(&key_event("u"));
            assert_eq!(target.ivars().get(), 3);
            assert_eq!(
                bar.performKeyEquivalent(&key_event("u")),
                disabled_service_result
            );
            assert_eq!(target.ivars().get(), 3);
            app.setServicesMenu(None);
        }
        app.setMainMenu(None);
    }
}

fn main() {
    const TEST: &str = "macos_menu_native_services_equivalents";
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        println!("{TEST}: test");
        return;
    }
    if args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .any(|filter| !TEST.contains(filter))
    {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        appkit::services_equivalents();
        println!("test {TEST} ... ok");
    }
    #[cfg(not(target_os = "macos"))]
    println!("test {TEST} ... skipped (requires macOS AppKit)");
}
