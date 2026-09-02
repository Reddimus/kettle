//! macOS Dock context menu: the rows kettle contributes when you right-click
//! its Dock icon.
//!
//! macOS supplies Options / Show All Windows / Hide / Quit on its own. Every
//! row above those comes from `applicationDockMenu:`, an optional
//! `NSApplicationDelegate` method — there is no `NSApplication.dockMenu`
//! property and no Info.plist key that works without a compiled nib. The
//! open-window title list is separately gated on `NSApplication.windowsMenu`
//! being non-nil; it is *not* free for apps with ordinary `NSWindow`s.
//!
//! winit owns the application delegate and implements neither, so without this
//! module kettle's Dock menu is the bare system set.
//!
//! Cross-platform on purpose, with the `cfg` inside `imp`: a `cfg` at the call
//! site in `App::run_with` would leave text the source census still sees on
//! every target, the same trap documented on `pty_key_modifiers`.

use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

/// What a kettle-supplied Dock row asks the app to do.
///
/// Each maps onto an `Action` that already exists, so the Dock menu introduces
/// no new user-facing action and nothing has to be categorized in the palette.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockCommand {
    NewWindow,
    NewTab,
}

#[cfg(any(target_os = "macos", test))]
impl DockCommand {
    pub fn action(self) -> kettle_config::Action {
        match self {
            Self::NewWindow => kettle_config::Action::NewWindow,
            Self::NewTab => kettle_config::Action::NewTab,
        }
    }
}

/// The rows kettle contributes, top to bottom, above the system section.
/// Matches Ghostty's Dock menu, which is the closest peer.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn dock_menu_model() -> [(&'static str, DockCommand); 2] {
    [
        ("New Window", DockCommand::NewWindow),
        ("New Tab", DockCommand::NewTab),
    ]
}

/// Wire the Dock menu and the open-window list onto the running application.
///
/// Call once, after the event loop is built (winit registers and installs its
/// delegate inside `EventLoop::build`) and before `run_app`. A no-op off macOS.
pub(crate) fn install(proxy: EventLoopProxy<UserEvent>) {
    imp::install(proxy);
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, NSObjectProtocol, Sel};
    use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSString, ns_string};
    use winit::event_loop::EventLoopProxy;

    use super::{DockCommand, dock_menu_model};
    use crate::app::UserEvent;

    /// Runtime subclass of winit's `WinitApplicationDelegate`, carrying the one
    /// method winit does not implement. A subclass rather than a replacement
    /// delegate: winit's private `ApplicationDelegate::get` panics on any other
    /// delegate object, and it is reached from the swizzled `sendEvent:` and
    /// both run-loop observers, so a proxy delegate dies within milliseconds.
    /// `isKindOfClass:` accepts a subclass, so this stays invisible to winit.
    const DELEGATE_SUBCLASS: &str = "KettleApplicationDelegate";

    struct DockState {
        /// The Dock re-asks for the menu on every right-click, sometimes more
        /// than once per show, and the returned pointer is borrowed — so the
        /// menu is built once and owned here for the life of the process.
        menu: Retained<NSMenu>,
        /// An `NSMenuItem`'s `target` is an unretained reference. Without an
        /// owner here the rows would message a freed object.
        _target: Retained<DockTarget>,
        proxy: EventLoopProxy<UserEvent>,
    }

    // `Retained` of a `MainThreadOnly` class is `!Send`, so this cannot be a
    // `OnceLock`. Every access happens on the AppKit main thread.
    thread_local! {
        static STATE: RefCell<Option<DockState>> = const { RefCell::new(None) };
    }

    fn dispatch(command: DockCommand) {
        // The Dock does not activate the app when a row is chosen: without
        // this the new window opens behind whatever was frontmost. Activation
        // is asynchronous, so nothing here may assert on `isActive`.
        if let Some(mtm) = MainThreadMarker::new() {
            #[allow(deprecated)]
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        }
        STATE.with(|state| {
            if let Some(state) = state.borrow().as_ref() {
                // A send after loop shutdown is a legitimate no-op.
                let _ = state.proxy.send_event(UserEvent::DockCommand(command));
            }
        });
    }

    declare_class!(
        /// Target for the Dock rows. AppKit invokes these on the main thread
        /// but outside winit's dispatch, with no `&mut App` and no
        /// `ActiveEventLoop`, so each one only pokes the event-loop proxy —
        /// the same shape as `AccessibilityActions`.
        struct DockTarget;

        unsafe impl ClassType for DockTarget {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "KettleDockTarget";
        }

        impl DeclaredClass for DockTarget {
            type Ivars = ();
        }

        unsafe impl NSObjectProtocol for DockTarget {}

        unsafe impl DockTarget {
            #[method(kettleDockNewWindow:)]
            fn new_window(&self, _sender: Option<&AnyObject>) {
                dispatch(DockCommand::NewWindow);
            }

            #[method(kettleDockNewTab:)]
            fn new_tab(&self, _sender: Option<&AnyObject>) {
                dispatch(DockCommand::NewTab);
            }
        }
    );

    impl DockTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = mtm.alloc().set_ivars(());
            unsafe { msg_send_id![super(this), init] }
        }
    }

    fn selector_for(command: DockCommand) -> Sel {
        match command {
            DockCommand::NewWindow => sel!(kettleDockNewWindow:),
            DockCommand::NewTab => sel!(kettleDockNewTab:),
        }
    }

    /// `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`.
    ///
    /// The receiver is a raw pointer, not `&AnyObject`: a reference makes the
    /// function pointer higher-ranked over its lifetime, and objc2 implements
    /// `MethodImplementation` only for each concrete lifetime. Returns `id`
    /// rather than `*mut NSMenu`; both encode as `@`.
    extern "C" fn dock_menu(
        _this: *const AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) -> *mut AnyObject {
        STATE.with(|state| match state.borrow().as_ref() {
            Some(state) => Retained::as_ptr(&state.menu) as *mut AnyObject,
            None => std::ptr::null_mut(),
        })
    }

    fn build_menu(mtm: MainThreadMarker, target: &DockTarget) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);
        let target_obj: *const AnyObject = (target as *const DockTarget).cast();
        for (title, command) in dock_menu_model() {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc(),
                    &NSString::from_str(title),
                    Some(selector_for(command)),
                    // No key equivalent: a Dock row must never claim a chord
                    // that would otherwise reach the PTY.
                    ns_string!(""),
                )
            };
            // SAFETY: `target_obj` points at the `DockTarget` the caller keeps
            // alive in `DockState` for the life of the process.
            unsafe { item.setTarget(Some(&*target_obj)) };
            menu.addItem(&item);
        }
        menu
    }

    pub(super) fn install(proxy: EventLoopProxy<UserEvent>) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("dock menu: install called off the main thread");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = (unsafe { app.delegate() }) else {
            log::warn!("dock menu: no application delegate to extend");
            return;
        };
        // SAFETY: every Objective-C object is an `AnyObject`; this only
        // re-borrows the delegate winit already owns.
        let delegate_obj: &AnyObject = unsafe { &*Retained::as_ptr(&delegate).cast::<AnyObject>() };
        let delegate_class = delegate_obj.class();

        // Drift guard against a winit upgrade: if winit ever supplies its own
        // dock menu, defer to it rather than defining the selector twice.
        if delegate_class
            .instance_method(sel!(applicationDockMenu:))
            .is_some()
        {
            log::info!(
                "dock menu: {} already implements applicationDockMenu:; leaving it alone",
                delegate_class.name()
            );
            return;
        }

        let target = DockTarget::new(mtm);
        let menu = build_menu(mtm, &target);
        STATE.with(|state| {
            *state.borrow_mut() = Some(DockState {
                menu,
                _target: target,
                proxy,
            })
        });

        let subclass = match AnyClass::get(DELEGATE_SUBCLASS) {
            // Objective-C class names are process-global. A class this
            // function did not create could carry a different superclass,
            // instance size or method table, and `set_class` would then be
            // undefined behavior rather than a no-op. `install` runs exactly
            // once per process, so reaching this arm means something else
            // claimed the name: leave the delegate alone.
            Some(_) => {
                log::warn!(
                    "dock menu: a class named {DELEGATE_SUBCLASS} is already \
                     registered and was not created here; not swizzling"
                );
                return;
            }
            None => {
                let Some(mut builder) = ClassBuilder::new(DELEGATE_SUBCLASS, delegate_class) else {
                    log::warn!("dock menu: could not allocate {DELEGATE_SUBCLASS}");
                    return;
                };
                // SAFETY: the signature matches `applicationDockMenu:` — one
                // `id` argument, an `id` return — and the implementation only
                // hands back a menu the process owns. The cast is required:
                // a bare function *item* does not satisfy
                // `MethodImplementation`, only a function *pointer* does.
                unsafe {
                    builder.add_method(
                        sel!(applicationDockMenu:),
                        dock_menu
                            as extern "C" fn(
                                *const AnyObject,
                                Sel,
                                *mut AnyObject,
                            ) -> *mut AnyObject,
                    );
                }
                builder.register()
            }
        };

        // SAFETY: `object_setClass`'s three requirements hold — the new class
        // is a direct subclass of the delegate's own class, it declares no
        // ivars so the instance size is unchanged, and it overrides nothing.
        let previous = unsafe { AnyObject::set_class(delegate_obj, subclass) };
        debug_assert_eq!(previous.name(), delegate_class.name());

        // The open-window title list. Gated purely on this property being
        // non-nil; AppKit populates and maintains the menu itself, and it is
        // never installed in the menu bar, so no chord is claimed and the menu
        // bar is unchanged. winit's later `setMainMenu:` does not clear it.
        let windows_menu = NSMenu::new(mtm);
        unsafe { app.setWindowsMenu(Some(&windows_menu)) };

        log::debug!("dock menu: installed on {}", delegate_obj.class().name());
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use winit::event_loop::EventLoopProxy;

    use crate::app::UserEvent;

    /// No Dock outside macOS. Windows taskbar jump lists and the freedesktop
    /// `Actions=` desktop-entry equivalent are separate surfaces.
    pub(super) fn install(_proxy: EventLoopProxy<UserEvent>) {}
}

#[cfg(test)]
mod tests {
    use super::{DockCommand, dock_menu_model};

    #[test]
    fn dock_menu_offers_new_window_then_new_tab() {
        let model = dock_menu_model();
        assert_eq!(
            model.iter().map(|(title, _)| *title).collect::<Vec<&str>>(),
            vec!["New Window", "New Tab"],
            "the Dock rows and their order are user-visible"
        );
        assert_eq!(
            model.map(|(_, command)| command),
            [DockCommand::NewWindow, DockCommand::NewTab]
        );
    }

    #[test]
    fn dock_titles_are_plain_ascii_without_ellipsis() {
        for (title, _) in dock_menu_model() {
            assert!(
                title.is_ascii(),
                "{title:?} must stay ASCII for the AX-driven smoke to match it"
            );
            assert!(
                !title.ends_with('…') && !title.ends_with("..."),
                "{title:?} acts immediately, so it must not promise a dialog"
            );
        }
    }

    /// The Dock reuses actions kettle already has. If this ever needs a new
    /// `Action` variant, `palette_includes_every_user_facing_action` has to be
    /// satisfied too — that is the signal to stop and categorize it.
    #[test]
    fn dock_commands_map_onto_existing_actions() {
        assert_eq!(
            DockCommand::NewWindow.action(),
            kettle_config::Action::NewWindow
        );
        assert_eq!(DockCommand::NewTab.action(), kettle_config::Action::NewTab);
    }

    /// Source census: the call site must stay free of `#[cfg]`, with the
    /// platform split inside `imp`. A `cfg` in `run_with` would make that
    /// function read differently on each target — the trap already documented
    /// on `pty_key_modifiers`.
    #[test]
    fn install_is_called_unconditionally_from_run_with() {
        let source = kettle_test_support::production_source(include_str!("app.rs"));
        let needle = "crate::macos_dock::install(proxy.clone());";
        assert_eq!(
            source.matches(needle).count(),
            1,
            "run_with must install the Dock menu exactly once"
        );
        let at = source.find(needle).expect("install call site");
        let preceding = &source[at.saturating_sub(240)..at];
        assert!(
            !preceding.contains("#[cfg("),
            "the Dock install call site must not be platform-gated;              keep the cfg inside macos_dock::imp"
        );
    }

    /// The other half of the same invariant, on this file: both `imp` modules
    /// must exist, so a non-macOS build gets a real no-op rather than a
    /// missing symbol.
    #[test]
    fn dock_module_keeps_both_platform_arms() {
        let source = kettle_test_support::production_source(include_str!("macos_dock.rs"));
        assert!(
            source.contains("#[cfg(target_os = \"macos\")]\nmod imp {"),
            "the macOS implementation must stay behind a target_os gate"
        );
        assert!(
            source.contains("#[cfg(not(target_os = \"macos\"))]\nmod imp {"),
            "every other target needs a real no-op imp, not a missing module"
        );
    }

    /// Drift guard for the runtime subclass. kettle grafts
    /// `applicationDockMenu:` onto winit's own `WinitApplicationDelegate`,
    /// which is a private type whose class name and method set are not part of
    /// winit's public API. A version bump has to be reviewed against
    /// `macos_dock::imp::install` — in particular the already-implements check,
    /// which defers to winit if it ever grows its own dock menu.
    ///
    /// The manifest requirement is a caret, so it alone would let an ordinary
    /// `cargo update` move to another 0.30.x without review. The lockfile is
    /// what actually gets compiled, so that is what this pins.
    #[test]
    fn dock_menu_pins_the_winit_version_it_subclasses() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("winit = { version = \"0.30.13\""),
            "the Dock menu subclasses winit's private application delegate; \
             re-verify macos_dock::imp::install before changing this pin"
        );
        let lock = include_str!("../../../Cargo.lock");
        let resolved = lock
            .split("name = \"winit\"\nversion = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("Cargo.lock resolves winit");
        assert_eq!(
            resolved, "0.30.13",
            "winit resolved to {resolved}, but the Dock menu subclasses its \
             private WinitApplicationDelegate — re-verify \
             macos_dock::imp::install against the new version before bumping"
        );
    }

    /// The AppKit features the module messages must be declared here rather
    /// than borrowed from winit's copy through Cargo feature unification.
    #[test]
    fn appkit_menu_features_are_declared_not_inherited() {
        let manifest = include_str!("../Cargo.toml");
        let app_kit = manifest
            .split("objc2-app-kit = {")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .expect("kettle-ui declares objc2-app-kit with a feature list");
        for feature in ["NSApplication", "NSMenu", "NSMenuItem"] {
            assert!(
                app_kit.contains(&format!("\"{feature}\"")),
                "objc2-app-kit must enable {feature} for the Dock menu"
            );
        }
    }
}
