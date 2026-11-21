use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use worker::{PrimaryProbe, TextProvider, TextWorker};

const ALL: PasteWant = PasteWant {
    files: true,
    images: true,
};

#[test]
fn backend_choice_uses_the_actual_display() {
    use winit::raw_window_handle::*;
    assert_eq!(
        text_backend(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            std::ptr::NonNull::dangling()
        ))),
        TextBackend::Native
    );
    assert_eq!(
        text_backend(RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0))),
        TextBackend::Arboard
    );
    assert_eq!(
        text_backend(RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0))),
        TextBackend::Arboard
    );
    assert_eq!(
        text_backend(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        TextBackend::Arboard
    );
    assert_eq!(
        text_backend(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
        TextBackend::Arboard
    );
}

#[derive(Default)]
struct Provider {
    selections: [String; 2],
    thread: Option<std::thread::ThreadId>,
    /// What a format-at-a-time clipboard holds besides text.
    files: Vec<PathBuf>,
    pixels: Option<Vec<u8>>,
    text_error: Option<fn() -> arboard::Error>,
    files_error: Option<fn() -> arboard::Error>,
    image_error: Option<fn() -> arboard::Error>,
    /// The formats a paste asked for, in order.
    asked: Vec<&'static str>,
}
impl TextProvider for Provider {
    fn load(&mut self, primary: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
        assert_ne!(self.thread, Some(std::thread::current().id()));
        Ok(self.selections[usize::from(primary)].clone())
    }
    fn store(&mut self, primary: bool, text: String, _: Instant) -> Result<(), arboard::Error> {
        assert_ne!(self.thread, Some(std::thread::current().id()));
        self.selections[usize::from(primary)] = text;
        Ok(())
    }
}

fn clipboard() -> Clipboard {
    let thread = Some(std::thread::current().id());
    Clipboard {
        arboard: None,
        worker: Some(
            TextWorker::start(move || {
                Ok(Provider {
                    thread,
                    ..Default::default()
                })
            })
            .unwrap(),
        ),
    }
}

#[test]
fn native_text_without_arboard_keeps_channels_order_caps_and_media_fallback() {
    let mut clipboard = clipboard();
    for (primary, text) in [(false, "clipboard"), (true, "primary"), (false, "next")] {
        clipboard.set_selection(primary, text.into()).unwrap();
        assert_eq!(
            clipboard
                .get_selection_bounded(primary, text.len(), Instant::now() + worker::READ_TIMEOUT)
                .unwrap(),
            text
        );
        assert!(matches!(
            clipboard.get_selection_bounded(
                primary,
                text.len() - 1,
                Instant::now() + worker::READ_TIMEOUT
            ),
            Err(arboard::Error::LimitExceeded)
        ));
    }
    assert_eq!(clipboard.get_text().unwrap(), "next");
    // A provider of text alone gives a paste its text and names no files.
    assert!(matches!(
        clipboard.paste_content(ALL, 100),
        PasteContent::Text(Ok(text)) if text == "next"
    ));
    assert!(matches!(
        clipboard.file_list(),
        Err(arboard::Error::ContentNotAvailable)
    ));
    assert!(matches!(
        clipboard.get_selection_bounded(false, 100, Instant::now()),
        Err(arboard::Error::Timeout)
    ));
}

#[test]
fn native_read_deadline_does_not_wait_for_a_stalled_provider_or_mix_replies() {
    let (entered_tx, entered) = crossbeam_channel::bounded(1);
    let (release, blocked) = crossbeam_channel::bounded(1);
    struct Stalled {
        entered: crossbeam_channel::Sender<()>,
        release: crossbeam_channel::Receiver<()>,
        first: bool,
    }
    impl TextProvider for Stalled {
        fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
            if self.first {
                self.first = false;
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok("stale".into())
            } else {
                Ok("fresh".into())
            }
        }
        fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
            Ok(())
        }
    }
    let worker = TextWorker::start(move || {
        Ok(Stalled {
            entered: entered_tx,
            release: blocked,
            first: true,
        })
    })
    .unwrap();
    let reader = worker.clone();
    let started = Instant::now();
    let join = std::thread::spawn(move || {
        reader.load(false, 100, Instant::now() + Duration::from_millis(100))
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(join.join().unwrap(), Err(arboard::Error::Timeout)));
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    assert_eq!(
        worker
            .load(true, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "fresh"
    );
}

#[test]
fn native_initialization_failure_is_reported_for_arboard_fallback() {
    assert!(matches!(
        TextWorker::start::<Provider>(|| Err(arboard::Error::ClipboardNotSupported)),
        Err(arboard::Error::ClipboardNotSupported)
    ));
}

#[test]
fn wayland_text_uses_the_bridge_device_without_another_dependency() {
    // A second data device would never hear of the selection on Mutter; the
    // clipboard goes through the bridge's device instead.
    let manifest = include_str!("../../Cargo.toml");
    assert!(!manifest.contains("smithay-clipboard"));
    let module = include_str!("../clipboard.rs");
    assert!(module.contains("#[cfg(target_os = \"linux\")]\nmod wayland;"));
    let adapter = include_str!("wayland.rs");
    assert!(
        adapter.contains(".load(max, deadline)") && adapter.contains("crate::wayland::selection()")
    );
}

#[test]
fn native_worker_refuses_a_full_queue_without_spawning_more_readers() {
    let (entered_tx, entered) = crossbeam_channel::bounded(1);
    let (release, blocked) = crossbeam_channel::bounded(1);
    struct Held {
        entered: crossbeam_channel::Sender<()>,
        release: crossbeam_channel::Receiver<()>,
    }
    impl TextProvider for Held {
        fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok("first".into())
        }
        fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
            Ok(())
        }
    }
    let worker = TextWorker::start(move || {
        Ok(Held {
            entered: entered_tx,
            release: blocked,
        })
    })
    .unwrap();
    let first = worker.clone();
    let first =
        std::thread::spawn(move || first.load(false, 10, Instant::now() + worker::READ_TIMEOUT));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        worker.load(true, 10, Instant::now() + Duration::from_millis(20)),
        Err(arboard::Error::Timeout)
    ));
    let started = Instant::now();
    assert!(matches!(
        worker.load(false, 10, Instant::now() + worker::READ_TIMEOUT),
        Err(arboard::Error::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap(), "first");
}

struct ClipboardOnly(Provider);

impl TextProvider for ClipboardOnly {
    fn primary_probe(&self) -> PrimaryProbe {
        Arc::new(|| false)
    }

    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        if primary {
            return Err(arboard::Error::ContentNotAvailable);
        }
        TextProvider::load(&mut self.0, primary, max, deadline)
    }

    fn store(
        &mut self,
        primary: bool,
        text: String,
        deadline: Instant,
    ) -> Result<(), arboard::Error> {
        if primary {
            return Err(arboard::Error::ClipboardNotSupported);
        }
        TextProvider::store(&mut self.0, primary, text, deadline)
    }
}

impl SelectionFallback for Provider {
    fn store(&mut self, primary: bool, text: String) -> Result<(), arboard::Error> {
        self.selections[usize::from(primary)] = text;
        Ok(())
    }

    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        self.asked.push("text");
        if let Some(error) = self.text_error {
            return Err(error());
        }
        let text = &self.selections[usize::from(primary)];
        if Instant::now() >= deadline {
            Err(arboard::Error::Timeout)
        } else if text.len() > max {
            Err(arboard::Error::LimitExceeded)
        } else {
            Ok(text.clone())
        }
    }

    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        self.asked.push("files");
        if let Some(error) = self.files_error {
            return Err(error());
        }
        Ok(self.files.clone())
    }

    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        self.asked.push("image");
        if let Some(error) = self.image_error {
            return Err(error());
        }
        let bytes = self
            .pixels
            .clone()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        Ok(arboard::ImageData {
            width: bytes.len() / 4,
            height: 1,
            bytes: bytes.into(),
        })
    }
}

fn clipboard_only_with_fallback() -> Clipboard<Provider> {
    Clipboard {
        arboard: Some(Provider {
            selections: ["fallback clipboard".into(), "external primary".into()],
            ..Default::default()
        }),
        worker: Some(
            TextWorker::start(|| {
                Ok(ClipboardOnly(Provider {
                    selections: ["native clipboard".into(), String::new()],
                    ..Default::default()
                }))
            })
            .unwrap(),
        ),
    }
}

#[test]
fn unsupported_primary_reads_use_fallback_and_keep_native_clipboard() {
    let mut clipboard = clipboard_only_with_fallback();
    assert_eq!(
        clipboard
            .get_selection_bounded(true, 100, Instant::now() + worker::READ_TIMEOUT)
            .ok()
            .as_deref(),
        Some("external primary"),
        "PRIMARY read must reach the fallback"
    );
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "native clipboard"
    );
    assert!(matches!(
        clipboard.get_selection_bounded(true, 3, Instant::now() + worker::READ_TIMEOUT),
        Err(arboard::Error::LimitExceeded)
    ));
    assert!(matches!(
        clipboard.get_selection_bounded(true, 100, Instant::now()),
        Err(arboard::Error::Timeout)
    ));
    clipboard.arboard = None;
    assert!(matches!(
        clipboard.get_selection_bounded(true, 100, Instant::now() + worker::READ_TIMEOUT),
        Err(arboard::Error::ContentNotAvailable)
    ));
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "native clipboard",
        "missing PRIMARY must not alias CLIPBOARD"
    );
}

#[test]
fn unsupported_primary_writes_use_fallback_and_keep_native_clipboard() {
    let mut clipboard = clipboard_only_with_fallback();
    assert!(
        clipboard
            .set_selection(true, "selected text".into())
            .is_ok(),
        "PRIMARY write must reach the fallback"
    );
    clipboard.set_text("copied text".into()).unwrap();
    assert_eq!(
        clipboard
            .get_selection_bounded(true, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "selected text"
    );
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "copied text"
    );
    // Observe each backend independently so routing both channels to the
    // fallback cannot pass by reading its own misplaced writes.
    let native = clipboard.worker.take();
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "fallback clipboard"
    );
    assert_eq!(
        clipboard
            .get_selection_bounded(true, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "selected text"
    );
    clipboard.set_selection(true, "x11 primary".into()).unwrap();
    clipboard.set_text("x11 clipboard".into()).unwrap();
    assert_eq!(
        clipboard
            .get_selection_bounded(true, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "x11 primary"
    );
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "x11 clipboard"
    );
    clipboard.worker = native;
    clipboard.arboard = None;
    assert!(matches!(
        clipboard.set_selection(true, "unavailable".into()),
        Err(arboard::Error::ContentNotAvailable)
    ));
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "copied text",
        "failed PRIMARY writes must not overwrite native CLIPBOARD"
    );
}

fn error_name(error: &arboard::Error) -> &'static str {
    match error {
        arboard::Error::ContentNotAvailable => "ContentNotAvailable",
        arboard::Error::Timeout => "Timeout",
        arboard::Error::LimitExceeded => "LimitExceeded",
        _ => "another error",
    }
}

/// A paste's outcome, for comparing.
fn outcome(content: PasteContent) -> String {
    match content {
        PasteContent::Files(paths) => format!("files {paths:?}"),
        PasteContent::Image(PasteImage::Rgba(image)) => format!("pixels {:?}", image.bytes),
        PasteContent::Image(PasteImage::Png(png)) => format!("png {png:?}"),
        PasteContent::Text(Ok(text)) => format!("text {text:?}"),
        PasteContent::Text(Err(error)) => format!("no text: {}", error_name(&error)),
        PasteContent::FilesOff => "files off".into(),
        PasteContent::Refused(refusal) => format!("refused {refusal:?}"),
    }
}

/// A clipboard asked one format at a time, as arboard is on X11, macOS and
/// Windows, holding `text`, `pixels` and `files`.
fn holding(text: &str, pixels: bool, files: &[&str]) -> Clipboard<Provider> {
    Clipboard {
        arboard: Some(Provider {
            selections: [text.into(), String::new()],
            pixels: pixels.then(|| vec![1, 2, 3, 255]),
            files: files.iter().map(PathBuf::from).collect(),
            ..Default::default()
        }),
        worker: None,
    }
}

fn asked(clipboard: &Clipboard<Provider>) -> &[&'static str] {
    &clipboard.arboard.as_ref().unwrap().asked
}

#[test]
fn text_wins_over_an_image_on_a_format_at_a_time_clipboard() {
    // Spreadsheet cells: the text and a picture of it.
    let mut both = holding("a\tb", true, &[]);
    assert_eq!(outcome(both.paste_content(ALL, 100)), "text \"a\\tb\"");
    assert_eq!(asked(&both), ["files", "text"], "the image is not read");

    // A screenshot: pixels with no text behind them.
    let mut image = holding("", true, &[]);
    assert_eq!(
        outcome(image.paste_content(ALL, 100)),
        "pixels [1, 2, 3, 255]"
    );
    assert_eq!(asked(&image), ["files", "text", "image"]);

    // `paste-images = off` leaves an image-only clipboard with nothing.
    let mut image = holding("", true, &[]);
    let no_images = PasteWant {
        files: true,
        images: false,
    };
    assert_eq!(outcome(image.paste_content(no_images, 100)), "text \"\"");
    assert_eq!(asked(&image), ["files", "text"]);

    // A copied file still comes first, whatever else is offered.
    let mut file = holding("/tmp/a", true, &["/tmp/a"]);
    assert_eq!(outcome(file.paste_content(ALL, 100)), "files [\"/tmp/a\"]");
    assert_eq!(asked(&file), ["files"]);

    // Text that is too large or timed out is still the text: the paste says
    // so instead of pasting a picture in its place.
    let mut large = holding("0123456789", true, &[]);
    assert_eq!(
        outcome(large.paste_content(ALL, 4)),
        "no text: LimitExceeded"
    );
    assert_eq!(asked(&large), ["files", "text"]);
    let mut slow = holding("", true, &[]);
    slow.arboard.as_mut().unwrap().text_error = Some(|| arboard::Error::Timeout);
    assert_eq!(outcome(slow.paste_content(ALL, 100)), "no text: Timeout");
    assert_eq!(asked(&slow), ["files", "text"]);
    // A clipboard with no text format at all goes on to its image.
    let mut none = holding("", true, &[]);
    none.arboard.as_mut().unwrap().text_error = Some(|| arboard::Error::ContentNotAvailable);
    assert_eq!(
        outcome(none.paste_content(ALL, 100)),
        "pixels [1, 2, 3, 255]"
    );
    let mut empty = holding("", false, &[]);
    assert_eq!(outcome(empty.paste_content(ALL, 100)), "text \"\"");
}

#[test]
fn paste_files_off_pastes_neither_copied_files_nor_their_text_or_image() {
    let off = PasteWant {
        files: false,
        images: true,
    };
    // A file manager's copy: the list, the paths as text, sometimes a picture.
    let mut copied = holding("/home/u/a b.png", true, &["/home/u/a b.png"]);
    assert_eq!(outcome(copied.paste_content(off, 100)), "files off");
    assert_eq!(asked(&copied), ["files"], "neither text nor image is read");
    // Without a file list the policy changes nothing.
    let mut text = holding("/home/u/typed", false, &[]);
    assert_eq!(
        outcome(text.paste_content(off, 100)),
        "text \"/home/u/typed\""
    );
    let mut image = holding("", true, &[]);
    assert_eq!(
        outcome(image.paste_content(off, 100)),
        "pixels [1, 2, 3, 255]"
    );
}

#[test]
fn an_image_that_is_there_and_cannot_be_read_is_said_not_passed_over() {
    let unreadable = |error: fn() -> arboard::Error, want: PasteWant| {
        let mut clipboard = holding("", false, &[]);
        clipboard.arboard.as_mut().unwrap().image_error = Some(error);
        let content = outcome(clipboard.paste_content(want, 100));
        (content, asked(&clipboard).to_vec())
    };
    let read = ["files", "text", "image"];
    // X11: a PNG over the 64 MiB read cap or the decode limits, and one
    // that does not decode. macOS and Windows: a bitmap that does not decode.
    assert_eq!(
        unreadable(|| arboard::Error::LimitExceeded, ALL),
        ("refused Image".into(), read.to_vec())
    );
    assert_eq!(
        unreadable(|| arboard::Error::ConversionFailure, ALL),
        ("refused Image".into(), read.to_vec())
    );
    // An owner that offers an image and never sends it.
    assert_eq!(
        unreadable(|| arboard::Error::Timeout, ALL),
        ("no text: Timeout".into(), read.to_vec())
    );
    // No image on the clipboard is no news: nothing is pasted, quietly.
    assert_eq!(
        unreadable(|| arboard::Error::ContentNotAvailable, ALL).0,
        "text \"\""
    );
    // Neither is an owner that answers the request with another type.
    assert_eq!(
        unreadable(
            || arboard::Error::Unknown {
                description: "incorrect type received from clipboard".into()
            },
            ALL
        )
        .0,
        "text \"\""
    );
    // With `paste-images = off` the image is not asked for, so nothing is said.
    let no_images = PasteWant {
        files: true,
        images: false,
    };
    assert_eq!(
        unreadable(|| arboard::Error::LimitExceeded, no_images),
        ("text \"\"".into(), vec!["files", "text"])
    );
    // Text still wins without the image being asked for at all.
    let mut both = holding("the text", false, &[]);
    both.arboard.as_mut().unwrap().image_error = Some(|| arboard::Error::LimitExceeded);
    assert_eq!(outcome(both.paste_content(ALL, 100)), "text \"the text\"");
    assert_eq!(asked(&both), ["files", "text"]);

    // A file-list read that fails does not show a list exists (xclip answers
    // every X11 request with its text), so the text is still pasted.
    for error in [
        (|| arboard::Error::Timeout) as fn() -> arboard::Error,
        || arboard::Error::LimitExceeded,
        || arboard::Error::ContentNotAvailable,
    ] {
        let mut text = holding("a megabyte of text", false, &[]);
        text.arboard.as_mut().unwrap().files_error = Some(error);
        assert_eq!(
            outcome(text.paste_content(ALL, 100)),
            "text \"a megabyte of text\""
        );
    }
}

/// A native provider that negotiates: it holds files or a PNG.
struct Negotiating {
    paste: fn(PasteWant) -> PasteContent,
    files: Option<Vec<PathBuf>>,
}

impl TextProvider for Negotiating {
    fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
        Err(arboard::Error::ContentNotAvailable)
    }
    fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
        Ok(())
    }
    fn paste(&mut self, want: PasteWant, _: usize, _: Instant) -> PasteContent {
        (self.paste)(want)
    }
    fn files(&mut self, _: Instant) -> Option<Vec<PathBuf>> {
        self.files.clone()
    }
}

#[test]
fn native_files_and_images_are_pasted_without_arboard_and_arboard_is_not_asked() {
    let native = |paste: fn(PasteWant) -> PasteContent, files: Option<Vec<PathBuf>>| {
        Some(TextWorker::start(move || Ok(Negotiating { paste, files })).unwrap())
    };
    // No arboard at all: a Wayland session without XWayland.
    let mut clipboard = Clipboard::<Provider> {
        arboard: None,
        worker: native(
            |want| {
                assert_eq!(want, ALL, "the policy reaches the provider");
                PasteContent::Image(PasteImage::Png(vec![0x89, b'P', b'N', b'G']))
            },
            Some(vec![PathBuf::from("/tmp/a b")]),
        ),
    };
    assert_eq!(
        outcome(clipboard.paste_content(ALL, 100)),
        "png [137, 80, 78, 71]"
    );
    assert_eq!(clipboard.file_list().unwrap(), [PathBuf::from("/tmp/a b")]);

    // With arboard present the bridge is still the authority: a native
    // refusal is the answer, and arboard is not asked for a second opinion.
    let mut clipboard = Clipboard {
        arboard: Some(Provider {
            selections: ["x11 text".into(), String::new()],
            files: vec![PathBuf::from("/x11/file")],
            pixels: Some(vec![9, 9, 9, 9]),
            ..Default::default()
        }),
        worker: native(|_| PasteContent::Refused(PasteRefusal::Files), None),
    };
    assert_eq!(outcome(clipboard.paste_content(ALL, 100)), "refused Files");
    assert!(matches!(
        clipboard.file_list(),
        Err(arboard::Error::ContentNotAvailable)
    ));
    assert!(asked(&clipboard).is_empty());
}

#[test]
fn a_stalled_native_paste_returns_at_its_deadline() {
    let (entered_tx, entered) = crossbeam_channel::bounded(1);
    let (release, blocked) = crossbeam_channel::bounded::<()>(1);
    struct Stalled {
        entered: crossbeam_channel::Sender<()>,
        release: crossbeam_channel::Receiver<()>,
    }
    impl TextProvider for Stalled {
        fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
            Ok("fresh".into())
        }
        fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
            Ok(())
        }
        fn paste(&mut self, _: PasteWant, _: usize, _: Instant) -> PasteContent {
            self.entered.send(()).unwrap();
            let _ = self.release.recv_timeout(Duration::from_secs(5));
            PasteContent::Files(vec![PathBuf::from("/stale")])
        }
    }
    let worker = TextWorker::start(move || {
        Ok(Stalled {
            entered: entered_tx,
            release: blocked,
        })
    })
    .unwrap();
    let started = Instant::now();
    let content = worker.paste(ALL, 100, Instant::now() + Duration::from_millis(100));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(outcome(content), "no text: Timeout");
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(1));
    // The late files answer no later request.
    release.send(()).unwrap();
    assert_eq!(
        worker
            .load(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "fresh"
    );
}

/// A native provider whose PRIMARY support can change while it runs, as the
/// bridge's does when the compositor announces the protocol or goes away.
struct Announced(Provider, Arc<AtomicBool>);

impl TextProvider for Announced {
    fn primary_probe(&self) -> PrimaryProbe {
        let supported = self.1.clone();
        Arc::new(move || supported.load(Ordering::Acquire))
    }
    fn load(&mut self, primary: bool, max: usize, at: Instant) -> Result<String, arboard::Error> {
        TextProvider::load(&mut self.0, primary, max, at)
    }
    fn store(&mut self, primary: bool, text: String, at: Instant) -> Result<(), arboard::Error> {
        TextProvider::store(&mut self.0, primary, text, at)
    }
}

#[test]
fn primary_is_native_while_the_protocol_is_there_and_falls_back_while_it_is_not() {
    let supported = Arc::new(AtomicBool::new(false));
    let announced = supported.clone();
    let mut clipboard = Clipboard {
        arboard: Some(Provider {
            selections: [String::new(), "x11 primary".into()],
            ..Default::default()
        }),
        worker: Some(
            TextWorker::start(move || Ok(Announced(Provider::default(), announced))).unwrap(),
        ),
    };
    let primary = |clipboard: &mut Clipboard<Provider>| {
        clipboard.get_selection_bounded(true, 100, Instant::now() + worker::READ_TIMEOUT)
    };
    // The worker starts before the compositor's globals are known.
    assert_eq!(primary(&mut clipboard).unwrap(), "x11 primary");

    // The primary-selection global arrives: selecting text writes PRIMARY
    // natively and a middle click reads it back, without XWayland.
    supported.store(true, Ordering::Release);
    clipboard
        .set_selection(true, "selected on Wayland".into())
        .unwrap();
    assert_eq!(primary(&mut clipboard).unwrap(), "selected on Wayland");
    assert_eq!(
        clipboard.arboard.as_ref().unwrap().selections[1],
        "x11 primary",
        "the fallback is not written while PRIMARY is native"
    );
    let arboard = clipboard.arboard.take();
    assert_eq!(
        primary(&mut clipboard).unwrap(),
        "selected on Wayland",
        "native PRIMARY needs no arboard"
    );
    clipboard.arboard = arboard;

    // The bridge closes: PRIMARY is the fallback's again, in both directions.
    supported.store(false, Ordering::Release);
    assert_eq!(primary(&mut clipboard).unwrap(), "x11 primary");
    clipboard
        .set_selection(true, "selected on X11".into())
        .unwrap();
    assert_eq!(
        clipboard.arboard.as_ref().unwrap().selections[1],
        "selected on X11"
    );
    // CLIPBOARD never moved off the native provider.
    clipboard.set_text("copied".into()).unwrap();
    assert_eq!(
        clipboard
            .get_selection_bounded(false, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "copied"
    );
    assert_eq!(clipboard.arboard.as_ref().unwrap().selections[0], "");
}

/// A format-at-a-time clipboard whose owner leaves the first request for one
/// format unanswered until it is released, as a Windows program that promised
/// its text and renders it late does. A late text is rendered onto the
/// clipboard when it comes, so a store served while the owner stalled would
/// be overwritten by the text from before it.
struct LateOwner {
    text: Arc<std::sync::Mutex<String>>,
    stalls: Option<&'static str>,
    entered: crossbeam_channel::Sender<std::thread::ThreadId>,
    release: crossbeam_channel::Receiver<()>,
    /// The formats asked for and the stores made, in order.
    asked: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

impl LateOwner {
    /// Whether the owner stalled on this request.
    fn asked(&mut self, format: &'static str) -> bool {
        self.asked.lock().unwrap().push(format);
        if self.stalls == Some(format) {
            self.stalls = None;
            self.entered.send(std::thread::current().id()).unwrap();
            let _ = self.release.recv_timeout(Duration::from_secs(5));
            return true;
        }
        false
    }
}

impl SelectionFallback for LateOwner {
    fn store(&mut self, _: bool, text: String) -> Result<(), arboard::Error> {
        self.asked.lock().unwrap().push("store");
        *self.text.lock().unwrap() = text;
        Ok(())
    }

    fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
        let promised = self.text.lock().unwrap().clone();
        if self.asked("text") {
            *self.text.lock().unwrap() = promised.clone();
        }
        Ok(promised)
    }

    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        self.asked("files");
        Ok(Vec::new())
    }

    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        self.asked("image");
        Err(arboard::Error::ContentNotAvailable)
    }
}

/// A serialized clipboard holding `text`, whose owner is late with `stalls`.
struct Late {
    clipboard: Clipboard<LateOwner>,
    /// The thread the late request was made on.
    entered: crossbeam_channel::Receiver<std::thread::ThreadId>,
    release: crossbeam_channel::Sender<()>,
    asked: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

fn late_owner(text: &str, stalls: &'static str) -> Late {
    let (entered_tx, entered) = crossbeam_channel::bounded(1);
    let (release, blocked) = crossbeam_channel::bounded(1);
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let text = Arc::new(std::sync::Mutex::new(text.to_owned()));
    let log = asked.clone();
    let clipboard = Clipboard::serialized(move || {
        Ok(LateOwner {
            text,
            stalls: Some(stalls),
            entered: entered_tx,
            release: blocked,
            asked: log,
        })
    })
    .unwrap();
    Late {
        clipboard,
        entered,
        release,
        asked,
    }
}

/// Wait until a request stands in the worker's queue, behind the one the
/// owner has not answered.
fn wait_until_queued(worker: &TextWorker) {
    let give_up = Instant::now() + Duration::from_secs(5);
    while worker.queued() == 0 {
        assert!(Instant::now() < give_up, "no request was queued");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until the worker has taken every queued request.
fn wait_until_taken(worker: &TextWorker) {
    let give_up = Instant::now() + Duration::from_secs(5);
    while worker.queued() != 0 {
        assert!(Instant::now() < give_up, "the worker took no request");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A short read is retried when it expired before the worker took it: a
/// loaded runner can start the worker late, and an expired request is then
/// rightly never put to the owner. Gives the thread the owner was asked on.
/// A clipboard with no worker is asked on the caller's thread, and its read
/// is never retried.
fn read_until_the_owner_is_asked<T>(
    queue: Option<&TextWorker>,
    within: Duration,
    mut read: impl FnMut(Instant) -> Result<String, arboard::Error>,
    asked: impl Fn() -> Option<T>,
) -> T {
    for _ in 0..5 {
        let started = Instant::now();
        let result = read(started + within);
        let waited = started.elapsed();
        assert!(
            matches!(result, Err(arboard::Error::Timeout)),
            "a read its owner has not answered returned {result:?} after {waited:?}"
        );
        assert!(
            waited < within + Duration::from_secs(1),
            "the read held its caller for {waited:?}"
        );
        if let Some(queue) = queue {
            wait_until_taken(queue);
        }
        if let Some(asked) = asked() {
            return asked;
        }
    }
    panic!("the owner was never asked");
}

#[test]
fn a_serialized_clipboard_stops_waiting_for_a_late_owner_and_keeps_requests_in_order() {
    let Late {
        mut clipboard,
        entered,
        release,
        asked,
    } = late_owner("late", "text");

    // An OSC 52 load: the caller has its answer at the deadline, while the
    // owner is still being waited for on another thread.
    let queue = clipboard.worker.clone();
    let asked_on = read_until_the_owner_is_asked(
        queue.as_ref(),
        Duration::from_millis(100),
        |deadline| clipboard.get_selection_bounded(false, 100, deadline),
        || entered.recv_timeout(Duration::from_secs(1)).ok(),
    );
    assert_ne!(asked_on, std::thread::current().id());

    // The OSC 52 store that follows waits its turn behind that load: it is
    // in the queue while the owner still stalls. The load's late text
    // answers nothing, and does not replace the store.
    let writer = std::thread::spawn(move || {
        let stored = clipboard.set_selection(false, "next".into());
        (clipboard, stored)
    });
    wait_until_queued(queue.as_ref().unwrap());
    assert_eq!(
        *asked.lock().unwrap(),
        ["text"],
        "the store reached the owner while its load stalled"
    );
    release.send(()).unwrap();
    let (mut clipboard, stored) = writer.join().unwrap();
    stored.unwrap();
    assert_eq!(clipboard.get_text().unwrap(), "next");
    assert_eq!(
        *asked.lock().unwrap(),
        ["text", "store", "text"],
        "the store is served after the stalled load, once"
    );
}

#[test]
fn a_serialized_clipboard_pastes_off_the_callers_thread_and_within_a_pastes_wait() {
    let Late {
        mut clipboard,
        entered,
        release,
        asked,
    } = late_owner("the text", "files");
    // The file list is asked for first, and the owner is slow to answer: the
    // paste has no text by the time a paste may wait.
    let started = Instant::now();
    let pasted = outcome(clipboard.paste_content(ALL, 100));
    let waited = started.elapsed();
    assert_eq!(pasted, "no text: Timeout", "after {waited:?}");
    assert!(
        waited < READ_WITHIN + Duration::from_secs(1),
        "the paste held its caller for {waited:?}"
    );
    assert_ne!(
        entered.recv_timeout(Duration::from_secs(5)).unwrap(),
        std::thread::current().id()
    );
    // The file list arrives after the paste's deadline. The paste's text is
    // then not asked for at all: nobody waits for it, and asking would keep
    // the thread from the next request for as long again.
    release.send(()).unwrap();
    assert_eq!(clipboard.get_text().unwrap(), "the text");
    assert_eq!(
        *asked.lock().unwrap(),
        ["files", "text"],
        "only the read after the paste asks for the text"
    );
}

#[test]
fn a_paste_whose_text_came_late_does_not_ask_for_the_image() {
    // No text: a paste in time would go on to the image.
    let Late {
        mut clipboard,
        entered,
        release,
        asked,
    } = late_owner("", "text");
    assert_eq!(
        outcome(clipboard.paste_content(ALL, 100)),
        "no text: Timeout"
    );
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    assert_eq!(clipboard.get_text().unwrap(), "");
    assert_eq!(
        *asked.lock().unwrap(),
        ["files", "text", "text"],
        "a paste past its deadline asks the owner for nothing more"
    );
}

/// A format-at-a-time clipboard that records the bounds its text is asked
/// under.
#[derive(Default)]
struct Bounded {
    asked: Vec<(usize, Duration)>,
}

impl SelectionFallback for Bounded {
    fn store(&mut self, _: bool, _: String) -> Result<(), arboard::Error> {
        Ok(())
    }

    fn load(&mut self, _: bool, max: usize, deadline: Instant) -> Result<String, arboard::Error> {
        self.asked
            .push((max, deadline.saturating_duration_since(Instant::now())));
        Ok("field text".into())
    }

    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        Ok(Vec::new())
    }

    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        Err(arboard::Error::ContentNotAvailable)
    }
}

#[test]
fn text_for_a_field_is_read_under_a_pastes_size_and_time_bounds() {
    let mut clipboard = Clipboard {
        arboard: Some(Bounded::default()),
        worker: None,
    };
    assert_eq!(clipboard.get_text().unwrap(), "field text");
    let asked = clipboard.arboard.unwrap().asked;
    assert_eq!(asked.len(), 1, "the text is asked for once");
    let (max, within) = asked[0];
    assert_eq!(max, 4 << 20);
    assert!(
        within <= READ_WITHIN && within > READ_WITHIN - Duration::from_secs(1),
        "the text may take {within:?}"
    );
}

/// A Windows clipboard owner that promises text and renders it late. The
/// test below is the only one that touches the desktop's clipboard, and it
/// runs only where that is expected: on a CI runner, or when asked for by
/// name.
#[cfg(windows)]
mod late_windows_owner {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::Mutex;
    use std::time::Duration;

    use crossbeam_channel::{Receiver, Sender, bounded};

    type Handle = *mut c_void;
    type WndProc = unsafe extern "system" fn(Handle, u32, usize, isize) -> isize;

    #[repr(C)]
    struct WndClassW {
        style: u32,
        wnd_proc: Option<WndProc>,
        class_extra: i32,
        window_extra: i32,
        instance: Handle,
        icon: Handle,
        cursor: Handle,
        background: Handle,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[repr(C)]
    struct Msg {
        hwnd: Handle,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        point: [i32; 2],
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(class: *const WndClassW) -> u16;
        fn CreateWindowExW(
            ex_style: u32,
            class_name: *const u16,
            window_name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Handle,
            menu: Handle,
            instance: Handle,
            param: *mut c_void,
        ) -> Handle;
        fn DefWindowProcW(hwnd: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
        fn GetMessageW(message: *mut Msg, hwnd: Handle, first: u32, last: u32) -> i32;
        fn DispatchMessageW(message: *const Msg) -> isize;
        fn PostMessageW(hwnd: Handle, message: u32, wparam: usize, lparam: isize) -> i32;
        fn DestroyWindow(hwnd: Handle) -> i32;
        fn PostQuitMessage(code: i32);
        fn OpenClipboard(owner: Handle) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, data: Handle) -> Handle;
        fn CloseClipboard() -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> Handle;
        fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
        fn GlobalLock(memory: Handle) -> *mut c_void;
        fn GlobalUnlock(memory: Handle) -> i32;
    }

    const WM_DESTROY: u32 = 0x0002;
    const WM_CLOSE: u32 = 0x0010;
    const WM_RENDERFORMAT: u32 = 0x0305;
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    /// The parent of a message-only window: never shown, and still sent
    /// `WM_RENDERFORMAT`.
    const HWND_MESSAGE: isize = -3;

    /// The longest the owner leaves a reader waiting. It is finite so that a
    /// reader with no deadline of its own fails the test and does not hang it.
    pub(super) const STALLS_FOR: Duration = Duration::from_secs(5);
    /// What the owner renders once it does answer.
    pub(super) const LATE_TEXT: &str = "late";

    /// Tells the test the owner was asked, and holds the answer back.
    type Stall = (Sender<()>, Receiver<()>);
    static STALL: Mutex<Option<Stall>> = Mutex::new(None);

    pub(super) fn allowed() -> bool {
        let is = |name: &str, value: &str| {
            std::env::var_os(name).is_some_and(|found| found.as_os_str() == value)
        };
        is("GITHUB_ACTIONS", "true") || is("KETTLE_WINDOWS_CLIPBOARD_TEST", "1")
    }

    unsafe extern "system" fn window_proc(
        hwnd: Handle,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        match message {
            WM_RENDERFORMAT => {
                // A reader is inside `GetClipboardData` until this returns.
                let stall = STALL.lock().ok().and_then(|stall| stall.clone());
                if let Some((entered, release)) = stall {
                    let _ = entered.try_send(());
                    let _ = release.recv_timeout(STALLS_FOR);
                }
                if wparam == CF_UNICODETEXT as usize {
                    let text: Vec<u16> = LATE_TEXT.encode_utf16().chain([0]).collect();
                    // SAFETY: the allocation holds `text` and its terminator.
                    // The reader has the clipboard open, as rendering needs,
                    // and the clipboard owns the memory once it is set.
                    unsafe {
                        let memory = GlobalAlloc(GMEM_MOVEABLE, text.len() * 2);
                        let units = GlobalLock(memory).cast::<u16>();
                        if !units.is_null() {
                            std::ptr::copy_nonoverlapping(text.as_ptr(), units, text.len());
                            GlobalUnlock(memory);
                            SetClipboardData(CF_UNICODETEXT, memory);
                        }
                    }
                }
                0
            }
            WM_CLOSE => {
                // SAFETY: `hwnd` is this thread's window.
                unsafe { DestroyWindow(hwnd) };
                0
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: the arguments are the ones this procedure was given.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    pub(super) struct Owner {
        hwnd: isize,
        thread: Option<std::thread::JoinHandle<()>>,
        /// Signalled when a reader has asked for the promised text.
        pub(super) asked: Receiver<()>,
        /// Lets the owner render its text.
        pub(super) release: Sender<()>,
    }

    impl Owner {
        /// Take the clipboard with text promised and not rendered.
        pub(super) fn start() -> Self {
            let (entered, asked) = bounded(1);
            let (release, held) = bounded(1);
            *STALL.lock().unwrap() = Some((entered, held));
            let (ready, started) = bounded(1);
            let thread = std::thread::spawn(move || {
                let class: Vec<u16> = "KettleTestLateClipboardOwner\0".encode_utf16().collect();
                // SAFETY: the class and its name outlive the window, which is
                // created, used and destroyed on this thread.
                let hwnd = unsafe {
                    let instance = GetModuleHandleW(null());
                    let registered = RegisterClassW(&WndClassW {
                        style: 0,
                        wnd_proc: Some(window_proc),
                        class_extra: 0,
                        window_extra: 0,
                        instance,
                        icon: null_mut(),
                        cursor: null_mut(),
                        background: null_mut(),
                        menu_name: null(),
                        class_name: class.as_ptr(),
                    });
                    if registered == 0 {
                        null_mut()
                    } else {
                        CreateWindowExW(
                            0,
                            class.as_ptr(),
                            class.as_ptr(),
                            0,
                            0,
                            0,
                            0,
                            0,
                            HWND_MESSAGE as Handle,
                            null_mut(),
                            instance,
                            null_mut(),
                        )
                    }
                };
                // SAFETY: plain clipboard calls from the window's own thread. A
                // null handle promises the format: the owner is asked for the
                // data, with `WM_RENDERFORMAT`, when a reader wants it.
                let promised = !hwnd.is_null()
                    && unsafe {
                        let mut opened = false;
                        for _ in 0..100 {
                            if OpenClipboard(hwnd) != 0 {
                                opened = true;
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        let emptied = opened && EmptyClipboard() != 0;
                        if emptied {
                            SetClipboardData(CF_UNICODETEXT, null_mut());
                        }
                        (opened && CloseClipboard() != 0) && emptied
                    };
                ready.send(promised.then_some(hwnd as isize)).unwrap();
                if !promised {
                    if !hwnd.is_null() {
                        // SAFETY: the window was created on this thread, and
                        // no `Owner` will be made to close it.
                        unsafe { DestroyWindow(hwnd) };
                    }
                    return;
                }
                // SAFETY: the message loop of the thread that owns the window.
                unsafe {
                    let mut message = std::mem::zeroed::<Msg>();
                    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                        DispatchMessageW(&message);
                    }
                }
            });
            let hwnd = started
                .recv_timeout(Duration::from_secs(10))
                .expect("the owner thread starts")
                .expect("the owner takes the clipboard");
            Self {
                hwnd,
                thread: Some(thread),
                asked,
                release,
            }
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            if let Ok(mut stall) = STALL.lock() {
                *stall = None;
            }
            // SAFETY: posting to a window handle; a stale handle is refused.
            unsafe { PostMessageW(self.hwnd as Handle, WM_CLOSE, 0, 0) };
            if let Some(thread) = self.thread.take()
                && !std::thread::panicking()
            {
                thread.join().unwrap();
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn a_stalled_windows_clipboard_owner_cannot_hold_the_ui_or_reorder_osc_52() {
    use late_windows_owner::{LATE_TEXT, Owner, STALLS_FOR};
    if !late_windows_owner::allowed() {
        eprintln!("SKIP: replaces the desktop clipboard; set KETTLE_WINDOWS_CLIPBOARD_TEST=1");
        return;
    }
    let owner = Owner::start();
    let mut clipboard = Clipboard::new().expect("the Windows clipboard");

    // An OSC 52 load of text the owner promised and has not rendered. The
    // owner stalls far longer than the read may take.
    let within = Duration::from_millis(300);
    assert!(STALLS_FOR > within * 10);
    let queue = clipboard.worker.clone().unwrap();
    read_until_the_owner_is_asked(
        Some(&queue),
        within,
        |deadline| clipboard.get_selection_bounded(false, 100, deadline),
        || owner.asked.recv_timeout(Duration::from_secs(1)).ok(),
    );

    // The OSC 52 store that follows is made while the owner still stalls: it
    // is in the queue before the owner is let go. It is served after the
    // stalled load, whose late text answers no request and does not replace
    // what was stored.
    let writer = std::thread::spawn(move || {
        let stored = clipboard.set_selection(false, "next".into());
        (clipboard, stored)
    });
    wait_until_queued(&queue);
    owner.release.send(()).unwrap();
    let (mut clipboard, stored) = writer.join().unwrap();
    stored.expect("the store behind the stalled load");
    let text = clipboard
        .get_selection_bounded(false, 100, Instant::now() + READ_WITHIN)
        .expect("the load after the store");
    assert_ne!(text, LATE_TEXT);
    assert_eq!(text, "next");
}

/// A second process that owns a private, uniquely named pasteboard and
/// promises its text lazily, as an `NSPasteboardItemDataProvider`. AppKit
/// delivers a provider's requests on its main run loop, so the owner is a
/// JavaScript for Automation script, whose `run` is on the main thread, and
/// not a thread of the test process. The system pasteboard is not touched.
#[cfg(target_os = "macos")]
mod late_macos_owner {
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// The longest the owner leaves a reader waiting. It is finite so that a
    /// reader with no deadline of its own fails the test and does not hang it.
    pub(super) const STALLS_FOR: Duration = Duration::from_secs(5);
    /// What the owner provides once it does answer.
    pub(super) const LATE_TEXT: &str = "late";

    /// Arguments: the pasteboard's name, a private directory for the
    /// handshake files, and how long the owner may stall, in seconds.
    const SCRIPT: &str = r#"
ObjC.import('AppKit');
ObjC.import('Foundation');
function run(argv) {
    const [name, dir, stallSeconds] = argv;
    const files = $.NSFileManager.defaultManager;
    const mark = (file) => files.createFileAtPathContentsAttributes(dir + '/' + file, $(), $());
    ObjC.registerSubclass({
        name: 'KettleTestLatePasteboardProvider',
        protocols: ['NSPasteboardItemDataProvider'],
        methods: {
            'pasteboard:item:provideDataForType:': {
                types: ['void', ['id', 'id', 'id']],
                implementation: function (board, item, type) {
                    // A reader is inside `stringForType:` until this returns.
                    mark('asked');
                    const until = $.NSDate.dateWithTimeIntervalSinceNow(Number(stallSeconds));
                    while (!files.fileExistsAtPath(dir + '/release') && until.timeIntervalSinceNow > 0)
                        $.NSThread.sleepForTimeInterval(0.01);
                    item.setStringForType('late', type);
                },
            },
        },
    });
    const board = $.NSPasteboard.pasteboardWithName(name);
    const item = $.NSPasteboardItem.alloc.init;
    item.setDataProviderForTypes($.KettleTestLatePasteboardProvider.alloc.init,
        $(['public.utf8-plain-text']));
    board.clearContents;
    if (!board.writeObjects($([item]))) return 'the private pasteboard refused the item';
    mark('ready');
    const end = $.NSDate.dateWithTimeIntervalSinceNow(120);
    while (!files.fileExistsAtPath(dir + '/quit') && end.timeIntervalSinceNow > 0)
        $.NSRunLoop.mainRunLoop.runUntilDate($.NSDate.dateWithTimeIntervalSinceNow(0.02));
    board.releaseGlobally;
    return '';
}
"#;

    pub(super) struct Owner {
        child: Child,
        dir: tempfile::TempDir,
        pub(super) name: String,
    }

    fn wait_for(path: &Path, within: Duration) -> bool {
        let give_up = Instant::now() + within;
        while !path.exists() {
            if Instant::now() >= give_up {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    impl Owner {
        /// Take a new private pasteboard with text promised and not provided.
        pub(super) fn start() -> Self {
            let dir = tempfile::tempdir().expect("a private directory");
            let script = dir.path().join("owner.js");
            std::fs::write(&script, SCRIPT).expect("the owner script");
            let name = format!(
                "com.kettle.test.late-owner.{}.{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let child = Command::new("/usr/bin/osascript")
                .args(["-l", "JavaScript"])
                .arg(&script)
                .arg(&name)
                .arg(dir.path())
                .arg(STALLS_FOR.as_secs().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .expect("osascript starts");
            let owner = Self { child, dir, name };
            assert!(
                wait_for(&owner.dir.path().join("ready"), Duration::from_secs(20)),
                "the owner did not take its private pasteboard"
            );
            owner
        }

        /// Whether a reader has asked for the promised text.
        pub(super) fn asked(&self, within: Duration) -> bool {
            wait_for(&self.dir.path().join("asked"), within)
        }

        /// Lets the owner provide its text.
        pub(super) fn release(&self) {
            std::fs::write(self.dir.path().join("release"), b"").unwrap();
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = std::fs::write(self.dir.path().join("release"), b"");
            let _ = std::fs::write(self.dir.path().join("quit"), b"");
            let give_up = Instant::now() + STALLS_FOR + Duration::from_secs(5);
            while Instant::now() < give_up {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // The owner is this test's own child.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_late_macos_pasteboard_owner_cannot_hold_the_ui_or_reorder_osc_52() {
    use late_macos_owner::{LATE_TEXT, Owner, STALLS_FOR};
    let owner = Owner::start();
    let name = owner.name.clone();
    let mut clipboard =
        Clipboard::open(move || Ok(arboard::Clipboard::with_macos_pasteboard_name(&name)))
            .expect("the private pasteboard");

    // An OSC 52 load of text the owner promised and has not provided. The
    // owner stalls far longer than the read may take.
    let within = Duration::from_millis(300);
    assert!(STALLS_FOR > within * 10);
    let queue = clipboard.worker.clone();
    read_until_the_owner_is_asked(
        queue.as_ref(),
        within,
        |deadline| clipboard.get_selection_bounded(false, 100, deadline),
        || owner.asked(Duration::from_secs(1)).then_some(()),
    );

    // The OSC 52 store that follows is made while the owner still stalls: it
    // is in the queue before the owner is let go. It is served after the
    // stalled load, whose late text answers no request and does not replace
    // what was stored.
    let writer = std::thread::spawn(move || {
        let stored = clipboard.set_selection(false, "next".into());
        (clipboard, stored)
    });
    wait_until_queued(queue.as_ref().unwrap());
    owner.release();
    let (mut clipboard, stored) = writer.join().unwrap();
    stored.expect("the store behind the stalled load");
    let text = clipboard
        .get_selection_bounded(false, 100, Instant::now() + READ_WITHIN)
        .expect("the load after the store");
    assert_ne!(text, LATE_TEXT);
    assert_eq!(text, "next");
}
