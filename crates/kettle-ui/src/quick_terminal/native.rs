//! Native quick-window hints and focus return. All AppKit calls run on the UI thread.
use super::Rect;
use winit::window::Window;

#[derive(Default)]
pub(crate) struct PreviousApp {
    #[cfg(target_os = "macos")]
    app: Option<objc2::rc::Retained<objc2_app_kit::NSRunningApplication>>,
    #[cfg(all(unix, not(target_os = "macos")))]
    window: Option<u32>,
    #[cfg(windows)]
    window: Option<windows::Win32::Foundation::HWND>,
}

impl PreviousApp {
    pub fn capture() -> Self {
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::NSWorkspace;
            // SAFETY: called on the event-loop main thread; AppKit retains the result.
            let app = unsafe { NSWorkspace::sharedWorkspace().frontmostApplication() };
            Self {
                app: app.filter(|app| {
                    // SAFETY: the retained NSRunningApplication has a valid pid accessor.
                    (unsafe { app.processIdentifier() }) != std::process::id() as i32
                }),
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self {
                window: x11_active(),
            }
        }
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
            // SAFETY: querying the foreground HWND has no preconditions.
            let window = unsafe { GetForegroundWindow() };
            Self {
                window: (!window.is_invalid()).then_some(window),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self::default()
        }
    }
    pub fn available(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.app.is_some()
        }
        #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
        {
            self.window.is_some()
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }
    pub fn restore(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(app) = self.app.take() {
            use objc2_app_kit::NSApplicationActivationOptions;
            // SAFETY: retained app object, main thread, valid activation options.
            unsafe {
                app.activateWithOptions(NSApplicationActivationOptions::empty());
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(window) = self.window.take() {
            x11_activate(window);
        }
        #[cfg(windows)]
        if let Some(window) = self.window.take() {
            // SAFETY: the OS validates the saved foreground handle if its app has exited.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(window);
            }
        }
    }
}

pub(crate) fn active_screen() -> Option<Rect> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSScreen;
        let mtm = objc2_foundation::MainThreadMarker::new()?;
        let screen = NSScreen::mainScreen(mtm)?;
        let frame = screen.frame();
        let primary = NSScreen::screens(mtm).first()?.frame();
        let scale = screen.backingScaleFactor();
        // AppKit uses a bottom-left origin in points; winit uses top-left physical pixels.
        Some(Rect {
            x: (frame.origin.x * scale) as i32,
            y: ((primary.size.height - frame.origin.y - frame.size.height) * scale) as i32,
            width: (frame.size.width * scale) as u32,
            height: (frame.size.height * scale) as u32,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub(crate) fn configure(window: &Window) {
    window.set_decorations(false);
    window.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSView, NSWindowCollectionBehavior as Behavior};
        use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        // SAFETY: winit owns a live NSView, borrowed only on the main thread.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        if let Some(window) = view.window() {
            // SAFETY: the live NSWindow is accessed on the UI thread with valid behavior bits.
            unsafe {
                window.setExcludedFromWindowsMenu(true);
                window.setCollectionBehavior(
                    Behavior::CanJoinAllSpaces
                        | Behavior::FullScreenAuxiliary
                        | Behavior::IgnoresCycle,
                );
            }
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{
                GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_APPWINDOW,
                WS_EX_TOOLWINDOW,
            },
        };
        use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            let hwnd = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
            // SAFETY: winit owns this live HWND on its UI thread. Tool windows are excluded from Alt-Tab as well as the taskbar.
            unsafe {
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    (style & !(WS_EX_APPWINDOW.0 as isize)) | WS_EX_TOOLWINDOW.0 as isize,
                );
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    x11_configure(window);
}

#[cfg(all(unix, not(target_os = "macos")))]
fn x11_active() -> Option<u32> {
    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{AtomEnum, ConnectionExt as _},
    };
    let (conn, screen) = x11rb::connect(None).ok()?;
    let atom = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .ok()?
        .reply()
        .ok()?
        .atom;
    conn.get_property(
        false,
        conn.setup().roots[screen].root,
        atom,
        AtomEnum::WINDOW,
        0,
        1,
    )
    .ok()?
    .reply()
    .ok()?
    .value32()?
    .next()
    .filter(|id| *id != 0)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn x11_activate(window: u32) {
    use x11rb::{connection::Connection as _, protocol::xproto::*};
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return;
    };
    let Ok(cookie) = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW") else {
        return;
    };
    let Ok(atom) = cookie.reply() else { return };
    let event = ClientMessageEvent::new(32, window, atom.atom, [2, x11rb::CURRENT_TIME, 0, 0, 0]);
    let _ = conn.send_event(
        false,
        conn.setup().roots[screen].root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    );
    let _ = conn.flush();
}

#[cfg(all(unix, not(target_os = "macos")))]
fn x11_configure(window: &Window) {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use x11rb::{connection::Connection as _, protocol::xproto::*, wrapper::ConnectionExt as _};
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Xlib(handle) = handle.as_raw() else {
        return;
    };
    let Ok((conn, _)) = x11rb::connect(None) else {
        return;
    };
    let atom = |name: &[u8]| {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
    };
    let Some(state) = atom(b"_NET_WM_STATE") else {
        return;
    };
    let atoms: Option<Vec<_>> = [
        b"_NET_WM_STATE_ABOVE".as_slice(),
        b"_NET_WM_STATE_SKIP_TASKBAR",
        b"_NET_WM_STATE_SKIP_PAGER",
    ]
    .into_iter()
    .map(atom)
    .collect();
    if let Some(atoms) = atoms {
        let _ = conn.change_property32(
            PropMode::APPEND,
            handle.window as u32,
            state,
            AtomEnum::ATOM,
            &atoms,
        );
        let _ = conn.flush();
    }
}

/// AppKit menus and app-owned panels can take window focus without switching apps.
pub(crate) fn other_app_active() -> bool {
    #[cfg(target_os = "macos")]
    {
        PreviousApp::capture().available()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}
