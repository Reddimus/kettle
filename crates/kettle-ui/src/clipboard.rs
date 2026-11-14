//! Platform selections, and what a paste takes from the clipboard.
//!
//! One order holds on every platform: a file list, then text, then an image.
//! Text wins over an image because a source that offers both, such as
//! spreadsheet cells, means the text. On native Wayland the bridge negotiates
//! from the offer's type list (`crate::wayland`); everywhere else arboard is
//! asked in that order.
use std::path::PathBuf;
use std::time::{Duration, Instant};

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

/// What a paste may take besides text: the `paste-files` and `paste-images`
/// policies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PasteWant {
    pub(crate) files: bool,
    pub(crate) images: bool,
}

/// What the clipboard gives a paste.
pub(crate) enum PasteContent {
    Files(Vec<PathBuf>),
    Image(PasteImage),
    /// The text, or why there is none. `ContentNotAvailable` is an empty
    /// clipboard; a timeout or an oversize text is said to the user.
    Text(Result<String, arboard::Error>),
    /// The clipboard holds local files and `paste-files` is off: nothing is
    /// pasted, not from the list and not from the plain-text copy of the
    /// paths that file managers add beside it.
    FilesOff,
    /// Nothing is pasted and the user is told why.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    Refused(PasteRefusal),
}

pub(crate) enum PasteImage {
    /// Pixels arboard decoded.
    Rgba(arboard::ImageData<'static>),
    /// Encoded bytes from another program, untrusted and still to be decoded.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    Png(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteRefusal {
    /// A malformed or oversize file list.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    Files,
    /// An image that could not be read.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    Image,
}

/// The longest a clipboard read may take.
const READ_WITHIN: Duration = Duration::from_secs(2);

pub(crate) struct Clipboard<F = arboard::Clipboard> {
    arboard: Option<F>,
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

    pub(crate) fn get_text(&mut self) -> Result<String, arboard::Error> {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland {
            return wayland.load(false, (4 << 20) + 1, Instant::now() + READ_WITHIN);
        }
        self.arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?
            .get_text()
    }
}

/// The platform selection boundary, injectable without opening an OS clipboard.
pub(crate) trait SelectionFallback {
    fn store(&mut self, primary: bool, text: String) -> Result<(), arboard::Error>;
    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error>;
    /// The CLIPBOARD file list; empty when it holds none.
    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error>;
    /// The CLIPBOARD bitmap, decoded.
    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error>;
}

impl SelectionFallback for arboard::Clipboard {
    fn store(&mut self, primary: bool, text: String) -> Result<(), arboard::Error> {
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            return self.set().clipboard(LinuxClipboardKind::Primary).text(text);
        }
        let _ = primary;
        self.set_text(text)
    }

    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            return self
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text_bounded(max, deadline);
        }
        let _ = primary;
        self.get_text_bounded(max, deadline)
    }

    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        self.get().file_list()
    }

    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        self.get_image()
    }
}

/// What a paste takes from a clipboard that is asked one format at a time.
fn fallback_paste(
    clipboard: &mut impl SelectionFallback,
    want: PasteWant,
    text_max: usize,
) -> PasteContent {
    // The list is read with `paste-files` off as well: copied files must
    // then paste nothing, rather than the text copy of their paths.
    if let Ok(paths) = clipboard.files()
        && !paths.is_empty()
    {
        return if want.files {
            PasteContent::Files(paths)
        } else {
            PasteContent::FilesOff
        };
    }
    let text = clipboard.load(false, text_max, Instant::now() + READ_WITHIN);
    match &text {
        Ok(text) if text.is_empty() => {}
        // A text that timed out or is too large is still the text.
        Ok(_) | Err(arboard::Error::Timeout | arboard::Error::LimitExceeded) => {
            return PasteContent::Text(text);
        }
        Err(_) => {}
    }
    if want.images
        && let Ok(image) = clipboard.image()
    {
        return PasteContent::Image(PasteImage::Rgba(image));
    }
    PasteContent::Text(text)
}

// PRIMARY is native where the compositor has the primary-selection protocol.
// Where it does not, that channel stays on arboard (XWayland); native
// CLIPBOARD errors are never treated as a reason to fall back.
impl<F: SelectionFallback> Clipboard<F> {
    /// What a paste takes from CLIPBOARD under `want`, with text capped at
    /// `text_max` bytes. On Wayland the bridge is the authority and arboard is
    /// not asked as well: an X11-owned selection reaches a focused Wayland
    /// client as an ordinary offer, and a second probe would only double the
    /// worst-case wait.
    pub(crate) fn paste_content(&mut self, want: PasteWant, text_max: usize) -> PasteContent {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland {
            return wayland.paste(want, text_max, Instant::now() + READ_WITHIN);
        }
        match self.arboard.as_mut() {
            Some(clipboard) => fallback_paste(clipboard, want, text_max),
            None => PasteContent::Text(Err(arboard::Error::ContentNotAvailable)),
        }
    }

    /// The local files CLIPBOARD names, for a preview of what a paste brings.
    pub(crate) fn file_list(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland {
            return wayland.files(Instant::now() + READ_WITHIN);
        }
        self.arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?
            .files()
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
        if let Some(wayland) = &self.wayland
            && (!primary || wayland.supports_primary())
        {
            return wayland.store(primary, text);
        }
        let clipboard = self
            .arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        clipboard.store(primary, text)
    }

    pub(crate) fn get_selection_bounded(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        #[cfg(any(target_os = "linux", test))]
        if let Some(wayland) = &self.wayland
            && (!primary || wayland.supports_primary())
        {
            return wayland.load(primary, max, deadline);
        }
        let clipboard = self
            .arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        clipboard.load(primary, max, deadline)
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
