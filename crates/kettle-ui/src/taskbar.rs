//! Cycle 745: OS taskbar progress indicator driven by OSC 9;4
//! (`kettle_core::Progress`, surfaced by `Terminal::progress`). PowerShell 7's
//! `Write-Progress` (with `$PSStyle.Progress.UseOSCIndicator = $true`) and
//! `winget` emit the sequence; this mirrors Windows Terminal by reflecting the
//! FOCUSED pane's progress onto the window's taskbar button.
//!
//! Cross-platform: the Windows path talks to `ITaskbarList3`; every other
//! platform gets a no-op (a macOS dock badge could follow via objc2).

use kettle_core::Progress;
use winit::window::Window;

/// Reflects the focused pane's OSC 9;4 progress onto the OS taskbar button.
/// Remembers the last-applied state so the per-frame poll skips redundant
/// COM calls.
pub struct Taskbar {
    inner: imp::Inner,
    last: Option<Progress>,
}

impl Taskbar {
    pub fn new() -> Self {
        Self {
            inner: imp::Inner::new(),
            last: None,
        }
    }

    /// Apply `progress` — the focused pane's latest OSC 9;4 state, or `None`
    /// when nothing is focused/reporting — to `window`'s taskbar button.
    /// Cheap no-op when unchanged since the previous call.
    pub fn apply(&mut self, window: &Window, progress: Option<Progress>) {
        if progress == self.last {
            return;
        }
        log::debug!("taskbar: focused-pane OSC 9;4 progress -> {progress:?}");
        self.last = progress;
        self.inner.set(window, progress);
    }

    /// Clear any outstanding OS attention request (the taskbar-button flash)
    /// on `window`. Cycle 869: winit's `request_user_attention(None)` does not
    /// reliably stop the Windows 11 taskbar flash once it's started, so on
    /// Windows this issues `FlashWindowEx(FLASHW_STOP)` directly. No-op on
    /// other platforms (the winit clear handles those). Best-effort.
    pub fn clear_attention(&self, window: &Window) {
        self.inner.clear_attention(window);
    }
}

impl Default for Taskbar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
mod imp {
    use super::{Progress, Window};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED,
        TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{FLASHW_STOP, FLASHWINFO, FlashWindowEx};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub struct Inner {
        // Lazily created on first use: COM must be initialized first, which
        // winit does via `OleInitialize` on the event-loop (STA) thread.
        list: Option<ITaskbarList3>,
        tried: bool,
    }

    impl Inner {
        pub fn new() -> Self {
            Self {
                list: None,
                tried: false,
            }
        }

        fn list(&mut self) -> Option<&ITaskbarList3> {
            if !self.tried {
                self.tried = true;
                // SAFETY: CoCreateInstance on the STA the winit loop set up.
                // On failure (COM not init / no shell) we keep `None` and
                // simply skip taskbar updates — never panics.
                let created: Option<ITaskbarList3> =
                    unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok() };
                if let Some(l) = &created {
                    // SAFETY: HrInit must be called once before use.
                    unsafe {
                        let _ = l.HrInit();
                    }
                }
                self.list = created;
            }
            self.list.as_ref()
        }

        pub fn set(&mut self, window: &Window, progress: Option<Progress>) {
            let Some(hwnd) = hwnd_of(window) else {
                log::debug!("taskbar: no Win32 hwnd yet; skipping");
                return;
            };
            let Some(list) = self.list() else {
                log::debug!("taskbar: ITaskbarList3 unavailable; skipping");
                return;
            };
            // SAFETY: `hwnd` is this process's live top-level window; the COM
            // calls only reference it. A taskbar hint is best-effort — any
            // error is logged and swallowed, never propagated, so it can't
            // take down the terminal.
            let r: windows::core::Result<()> = unsafe {
                match progress {
                    None | Some(Progress::Clear) => list.SetProgressState(hwnd, TBPF_NOPROGRESS),
                    Some(Progress::Indeterminate) => {
                        list.SetProgressState(hwnd, TBPF_INDETERMINATE)
                    }
                    Some(Progress::Normal(p)) => list
                        .SetProgressState(hwnd, TBPF_NORMAL)
                        .and_then(|()| list.SetProgressValue(hwnd, p as u64, 100)),
                    Some(Progress::Error(p)) => list
                        .SetProgressState(hwnd, TBPF_ERROR)
                        .and_then(|()| list.SetProgressValue(hwnd, p as u64, 100)),
                    Some(Progress::Warning(p)) => list
                        .SetProgressState(hwnd, TBPF_PAUSED)
                        .and_then(|()| list.SetProgressValue(hwnd, p as u64, 100)),
                }
            };
            match r {
                Ok(()) => log::debug!("taskbar: applied {progress:?} via ITaskbarList3"),
                Err(e) => log::debug!("taskbar: ITaskbarList3 call failed: {e}"),
            }
        }

        pub fn clear_attention(&self, window: &Window) {
            let Some(hwnd) = hwnd_of(window) else {
                return;
            };
            // SAFETY: `hwnd` is this process's live top-level window;
            // FlashWindowEx with FLASHW_STOP only stops the taskbar-button
            // flash. Best-effort — any failure is harmless and ignored.
            unsafe {
                let _ = FlashWindowEx(&stop_flash(hwnd));
            }
        }
    }

    /// Build a `FLASHWINFO` that STOPS any taskbar-button flash for `hwnd`.
    /// `cbSize` MUST equal `size_of::<FLASHWINFO>()` or `FlashWindowEx`
    /// silently no-ops — exactly what the unit test below guards against.
    fn stop_flash(hwnd: HWND) -> FLASHWINFO {
        FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_STOP,
            uCount: 0,
            dwTimeout: 0,
        }
    }

    fn hwnd_of(window: &Window) -> Option<HWND> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{FLASHW_STOP, FLASHWINFO, HWND, stop_flash};

        #[test]
        fn stop_flash_has_correct_size_and_stop_flag() {
            let info = stop_flash(HWND(std::ptr::null_mut()));
            assert_eq!(
                info.cbSize as usize,
                std::mem::size_of::<FLASHWINFO>(),
                "FLASHWINFO.cbSize must equal the struct size or FlashWindowEx \
                 silently no-ops"
            );
            assert_eq!(info.dwFlags, FLASHW_STOP, "the clear must use FLASHW_STOP");
            assert_eq!(info.uCount, 0);
            assert_eq!(info.dwTimeout, 0);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Progress, Window};

    pub struct Inner;

    impl Inner {
        pub fn new() -> Self {
            Inner
        }

        /// No taskbar-progress API is wired up off Windows yet (a macOS dock
        /// badge could be added via objc2 in a later cycle).
        pub fn set(&mut self, _window: &Window, _progress: Option<Progress>) {}

        /// No taskbar attention API off Windows; the winit
        /// `request_user_attention(None)` path handles those platforms.
        pub fn clear_attention(&self, _window: &Window) {}
    }
}
