//! Platform text selection and the unchanged arboard image/file path.
use std::time::Instant;

#[cfg(any(target_os = "linux", test))]
use winit::raw_window_handle::RawDisplayHandle;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(any(target_os = "linux", test))]
mod worker;

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextBackend {
    Native,
    Arboard,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn text_backend(display: RawDisplayHandle) -> TextBackend {
    match display {
        RawDisplayHandle::Wayland(_) => TextBackend::Native,
        _ => TextBackend::Arboard,
    }
}

pub(crate) struct Clipboard {
    arboard: Option<arboard::Clipboard>,
    #[cfg(any(target_os = "linux", test))]
    wayland: Option<worker::TextWorker>,
}

impl Clipboard {
    pub(crate) fn new() -> Option<Self> {
        match arboard::Clipboard::new() {
            Ok(arboard) => Some(Self {
                arboard: Some(arboard),
                #[cfg(any(target_os = "linux", test))]
                wayland: None,
            }),
            Err(error) => {
                log::warn!("arboard clipboard unavailable: {error}");
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_wayland(clipboard: &mut Option<Self>, window: &winit::window::Window) {
        use winit::raw_window_handle::HasDisplayHandle;
        let Ok(handle) = window.display_handle() else {
            return;
        };
        if text_backend(handle.as_raw()) != TextBackend::Native {
            return;
        }
        if let Some(worker) = wayland::clipboard() {
            let clipboard = clipboard.get_or_insert_with(|| Self {
                arboard: None,
                wayland: None,
            });
            clipboard.wayland = Some(worker);
        }
    }

    pub(crate) fn set_text(&mut self, text: String) -> Result<(), arboard::Error> {
        self.set_selection(false, text)
    }

    pub(crate) fn set_selection(
        &mut self,
        primary: bool,
        text: String,
    ) -> Result<(), arboard::Error> {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland {
            return wayland.store(primary, text);
        }
        let clipboard = self
            .arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            return clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text);
        }
        let _ = primary;
        clipboard.set_text(text)
    }

    pub(crate) fn get_text(&mut self) -> Result<String, arboard::Error> {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland {
            return wayland.load(
                false,
                (4 << 20) + 1,
                Instant::now() + std::time::Duration::from_secs(2),
            );
        }
        self.arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?
            .get_text()
    }

    pub(crate) fn get_selection_bounded(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland {
            return wayland.load(primary, max, deadline);
        }
        let clipboard = self
            .arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            return clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text_bounded(max, deadline);
        }
        let _ = primary;
        clipboard.get_text_bounded(max, deadline)
    }

    pub(crate) fn get_image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        let Some(clipboard) = self.arboard.as_mut() else {
            // Every paste asks for an image first; startup already warned once
            // that arboard is missing, so this stays out of the default log.
            log::debug!("clipboard image paste unavailable: no arboard clipboard");
            return Err(arboard::Error::ContentNotAvailable);
        };
        clipboard.get_image()
    }

    pub(crate) fn file_list(&mut self) -> Result<Vec<std::path::PathBuf>, arboard::Error> {
        self.arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?
            .get()
            .file_list()
    }
}

/// Store text through the Wayland bridge when it is connected; `None` leaves
/// the caller to use arboard.
#[cfg(target_os = "linux")]
pub(crate) fn store_wayland_text(text: &str) -> Option<Result<(), arboard::Error>> {
    let selection = crate::wayland::selection();
    selection.available().then(|| {
        selection
            .store(text.to_owned())
            .map_err(|_| arboard::Error::ContentNotAvailable)
    })
}

#[cfg(test)]
mod tests;
