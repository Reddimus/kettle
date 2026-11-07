use super::*;
use std::time::{Duration, Instant};
use worker::{TextProvider, TextWorker};

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
    assert!(matches!(
        clipboard.get_selection_bounded(false, 100, Instant::now()),
        Err(arboard::Error::Timeout)
    ));
    assert!(matches!(
        clipboard.get_image(),
        Err(arboard::Error::ContentNotAvailable)
    ));
    assert!(matches!(
        clipboard.file_list(),
        Err(arboard::Error::ContentNotAvailable)
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
    fn supports_primary(&self) -> bool {
        false
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
        let text = &self.selections[usize::from(primary)];
        if Instant::now() >= deadline {
            Err(arboard::Error::Timeout)
        } else if text.len() > max {
            Err(arboard::Error::LimitExceeded)
        } else {
            Ok(text.clone())
        }
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
