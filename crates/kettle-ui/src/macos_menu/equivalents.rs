//! Native-only key-equivalent routing, shared by the bar and its submenus.
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability};
use objc2_app_kit::{NSApplication, NSEvent, NSMenu};
use objc2_foundation::MainThreadMarker;

declare_class!(
    pub(super) struct MenuBar;
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
            // Native application commands and registered Services can consume
            // equivalents. Query the live Services menu so AppKit can update it.
            // Every Kettle equivalent returns NO here and follows the original
            // NSApplication -> NSWindow -> winit KeyboardInput path, even when
            // its menu item is disabled. The original event retains keyboard
            // layout, physical code, repeat and modifiers, as well as key-up.
            // SAFETY: all menus and the original event are live; self is main-thread-only.
            Bool::new(unsafe {
                self.ivars().performKeyEquivalent(event)
                    || NSApplication::sharedApplication(MainThreadMarker::from(self))
                        .servicesMenu()
                        .is_some_and(|services| services.performKeyEquivalent(event))
            })
        }
    }
);

pub(super) fn new_menu(mtm: MainThreadMarker, native: &Retained<NSMenu>) -> Retained<MenuBar> {
    let menu = mtm.alloc::<MenuBar>().set_ivars(native.clone());
    // SAFETY: this is a newly allocated NSMenu subclass with initialized ivars.
    unsafe { msg_send_id![super(menu), init] }
}
