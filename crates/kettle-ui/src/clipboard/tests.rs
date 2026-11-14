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
        wayland: Some(
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
        Ok(self.files.clone())
    }

    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        self.asked.push("image");
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
        wayland: Some(
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
    let native = clipboard.wayland.take();
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
    clipboard.wayland = native;
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
        wayland: None,
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
        wayland: native(
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
        wayland: native(|_| PasteContent::Refused(PasteRefusal::Files), None),
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
        wayland: Some(
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
