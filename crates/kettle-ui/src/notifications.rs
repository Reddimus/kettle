//! Bounded desktop-notification dispatch.
//!
//! Platform notification services are external IPC. They may block, fail, or
//! panic, so no winit event-loop path calls them directly.

#[cfg(target_os = "linux")]
mod linux;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 64;

/// Failed tracked notifications waiting for the UI, ids only.
const FAILURE_CAPACITY: usize = 64;

#[derive(Debug)]
struct DesktopNotification {
    /// The UI's id for a notification it wants to hear about if delivery
    /// fails, so it can show the text in the window instead.
    id: Option<u64>,
    title: String,
    body: String,
}

#[derive(Debug)]
enum Message {
    Show(DesktopNotification),
    Flush(SyncSender<()>),
}

struct Dispatcher {
    sender: SyncSender<Message>,
    // Retain the handle for the lifetime of the process. Normal GUI shutdown
    // performs a bounded flush; a stuck platform service must never turn that
    // flush into an unbounded join.
    _worker: std::thread::JoinHandle<()>,
}

static DISPATCHER: OnceLock<Option<Dispatcher>> = OnceLock::new();
static QUEUE_FULL: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);

/// Ids of tracked notifications the platform service refused, failed on or
/// panicked in, from the worker to the UI. Bounded and text-free: the UI
/// keeps the text itself while delivery is in flight.
struct Failures {
    sender: SyncSender<u64>,
    receiver: Mutex<Receiver<u64>>,
    wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Failures {
    fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        Self {
            sender,
            receiver: Mutex::new(receiver),
            wake: OnceLock::new(),
        }
    }

    /// A full channel drops the id: the UI's pending entry then expires.
    fn report(&self, id: u64) {
        if self.sender.try_send(id).is_ok()
            && let Some(wake) = self.wake.get()
        {
            wake();
        }
    }

    fn take(&self) -> Vec<u64> {
        self.receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_iter()
            .take(FAILURE_CAPACITY)
            .collect()
    }
}

fn failures() -> &'static Failures {
    static FAILURES: OnceLock<Failures> = OnceLock::new();
    FAILURES.get_or_init(|| Failures::new(FAILURE_CAPACITY))
}

/// Call `wake` (once registered, for the life of the process) whenever a
/// tracked notification fails, so the UI drains
/// [`take_failed_desktop_notifications`] without polling.
pub fn on_desktop_notification_failure(wake: impl Fn() + Send + Sync + 'static) {
    let _ = failures().wake.set(Box::new(wake));
}

/// The ids of tracked notifications whose delivery failed since the last
/// call, oldest first.
pub fn take_failed_desktop_notifications() -> Vec<u64> {
    failures().take()
}

/// Kettle's bundle identifier, as `packaging/macos/Info.plist` declares it.
#[cfg(any(target_os = "macos", test))]
const KETTLE_BUNDLE_ID: &str = "org.kettle.terminal";

/// The application macOS delivers Kettle's notifications as: the running
/// bundle's own identifier, or Kettle's when it runs outside a bundle, as a
/// build from source does.
#[cfg(any(target_os = "macos", test))]
fn notification_application(main_bundle: Option<String>) -> String {
    main_bundle
        .filter(|identifier| !identifier.is_empty())
        .unwrap_or_else(|| KETTLE_BUNDLE_ID.to_owned())
}

/// Name, once and before the first notification, the application macOS
/// delivers them as. Left to itself, the notification library asks
/// AppleScript for the application called `use_default`, and macOS answers
/// by asking the user, in a "Choose Application" window, where that is.
/// Naming one, even one macOS cannot find, settles it for the process.
#[cfg(target_os = "macos")]
fn claim_application() {
    static CLAIMED: std::sync::Once = std::sync::Once::new();
    CLAIMED.call_once(|| {
        let main_bundle = objc2_core_foundation_03::CFBundle::main_bundle()
            .and_then(|bundle| bundle.identifier())
            .map(|identifier| identifier.to_string());
        let application = notification_application(main_bundle);
        if let Err(error) = notify_rust::set_application(&application) {
            log::debug!("kettle.notify: notifications are not delivered as {application}: {error}");
        }
    });
}

/// Whether the platform accepted the notification.
#[cfg(not(target_os = "linux"))]
fn show(notification: DesktopNotification) -> bool {
    #[cfg(target_os = "macos")]
    claim_application();
    let mut native = notify_rust::Notification::new();
    native.summary(&notification.title);
    if !notification.body.is_empty() {
        native.body(&notification.body);
    }
    native.appname("kettle");
    match native.show() {
        Ok(_) => true,
        Err(error) => {
            log::warn!("kettle.notify: notification send failed: {error}");
            false
        }
    }
}

/// `body` as text inside freedesktop notification body markup: `&`, `<`
/// and `>` become entities, so a program's text never becomes a tag.
#[cfg(any(target_os = "linux", test))]
fn escape_body_markup(body: &str) -> std::borrow::Cow<'_, str> {
    if !body.contains(['&', '<', '>']) {
        return std::borrow::Cow::Borrowed(body);
    }
    let mut out = String::with_capacity(body.len() + 16);
    for ch in body.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ch => out.push(ch),
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(any(target_os = "linux", test))]
fn discard_closed_sender<C>(sender: &mut Option<C>, is_closed: impl FnOnce(&C) -> bool) {
    if sender.as_ref().is_some_and(is_closed) {
        *sender = None;
    }
}

fn spawn_dispatcher(
    capacity: usize,
    mut backend: impl FnMut(DesktopNotification) -> bool + Send + 'static,
    failed: impl Fn(u64) + Send + 'static,
) -> std::io::Result<Dispatcher> {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let worker = std::thread::Builder::new()
        .name("kettle-notifications".to_string())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Show(notification) => {
                        let id = notification.id;
                        // OS notification backends are outside Kettle's trust
                        // boundary. One panic must not disconnect the
                        // process-wide dispatcher.
                        let delivered =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                backend(notification)
                            }))
                            .unwrap_or_else(|_| {
                                log::warn!("kettle.notify: notification backend panicked");
                                false
                            });
                        if !delivered && let Some(id) = id {
                            failed(id);
                        }
                    }
                    Message::Flush(acknowledge) => {
                        let _ = acknowledge.send(());
                    }
                }
            }
        })?;
    Ok(Dispatcher {
        sender,
        _worker: worker,
    })
}

fn dispatcher() -> Option<&'static Dispatcher> {
    DISPATCHER
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let backend = {
                let mut backend = linux::Backend::default();
                move |notification| backend.show(notification)
            };
            #[cfg(not(target_os = "linux"))]
            let backend = show;
            match spawn_dispatcher(QUEUE_CAPACITY, backend, |id| failures().report(id)) {
                Ok(dispatcher) => Some(dispatcher),
                Err(error) => {
                    log::warn!("kettle.notify: notification worker could not start: {error}");
                    None
                }
            }
        })
        .as_ref()
}

fn try_queue(
    sender: &SyncSender<Message>,
    id: Option<u64>,
    title: &str,
    body: &str,
) -> Result<(), TrySendError<Message>> {
    sender.try_send(Message::Show(DesktopNotification {
        id,
        title: title.to_string(),
        body: body.to_string(),
    }))
}

/// Queue a desktop notification without waiting on the platform service.
///
/// The single bounded worker preserves order. If the platform service stalls,
/// later notifications are dropped instead of freezing rendering, input, or
/// control replies.
pub fn queue_desktop_notification(title: &str, body: &str) {
    admit(None, title, body);
}

/// [`queue_desktop_notification`] for a notification the UI tracks by `id`.
/// `false` when it was not admitted (no worker, a full queue or a stopped
/// worker): the UI knows at once it will not be shown. A later failure in
/// the platform service is reported through
/// [`take_failed_desktop_notifications`].
pub fn queue_tracked_desktop_notification(id: u64, title: &str, body: &str) -> bool {
    admit(Some(id), title, body)
}

fn admit(id: Option<u64>, title: &str, body: &str) -> bool {
    let Some(dispatcher) = dispatcher() else {
        return false;
    };
    match try_queue(&dispatcher.sender, id, title, body) {
        Ok(()) => {
            QUEUE_FULL.store(false, Ordering::Release);
            true
        }
        Err(TrySendError::Full(_)) => {
            if !QUEUE_FULL.swap(true, Ordering::AcqRel) {
                log::warn!("kettle.notify: notification queue is full; dropping notifications");
            }
            false
        }
        Err(TrySendError::Disconnected(_)) => {
            if !DISCONNECTED.swap(true, Ordering::AcqRel) {
                log::warn!("kettle.notify: notification worker stopped; dropping notifications");
            }
            false
        }
    }
}

fn flush_sender(sender: &SyncSender<Message>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let (acknowledge, acknowledged) = mpsc::sync_channel(0);
    let mut message = Message::Flush(acknowledge);
    loop {
        match sender.try_send(message) {
            Ok(()) => break,
            Err(TrySendError::Full(returned)) => {
                message = returned;
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return false;
                };
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return false;
    };
    acknowledged.recv_timeout(remaining).is_ok()
}

/// Give already-admitted notifications a short chance to leave during normal
/// GUI shutdown. This is deliberately bounded: a hung platform service must
/// not make Kettle impossible to close.
pub fn flush_desktop_notifications(timeout: Duration) {
    let Some(dispatcher) = DISPATCHER.get().and_then(Option::as_ref) else {
        return;
    };
    if !flush_sender(&dispatcher.sender, timeout) {
        log::debug!("kettle.notify: notification drain did not finish before exit");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Dispatcher, Failures, KETTLE_BUNDLE_ID, Message, flush_sender, notification_application,
        spawn_dispatcher, try_queue,
    };
    use std::sync::mpsc;
    use std::time::Duration;

    /// Markup in a program's body becomes text: tags, links and image
    /// references are escaped, entities too, and plain text is untouched.
    #[test]
    fn body_markup_is_escaped_into_text() {
        use super::escape_body_markup;
        assert_eq!(
            escape_body_markup("<b>x</b> <a href=\"u\">l</a> <img src=\"/f\"/> &amp;"),
            "&lt;b&gt;x&lt;/b&gt; &lt;a href=\"u\"&gt;l&lt;/a&gt; &lt;img src=\"/f\"/&gt; &amp;amp;"
        );
        assert!(matches!(
            escape_body_markup("Build finished"),
            std::borrow::Cow::Borrowed("Build finished")
        ));
    }

    #[test]
    fn recoverable_call_errors_keep_the_notification_sender() {
        let original = std::sync::Arc::new(());
        let mut sender = Some(original.clone());
        for _ in ["timeout", "method error", "invalid reply"] {
            super::discard_closed_sender(&mut sender, |_| false);
            assert!(
                sender
                    .as_ref()
                    .is_some_and(|sender| std::sync::Arc::ptr_eq(sender, &original)),
                "recoverable call error disconnected the original sender"
            );
        }
        super::discard_closed_sender(&mut sender, |_| true);
        assert!(sender.is_none(), "closed transport must be replaced");
        assert_eq!(std::sync::Arc::strong_count(&original), 1);
    }

    /// Notifications go out as the running bundle when there is one, else
    /// as Kettle, whose identifier is the one its bundle declares.
    #[test]
    fn notifications_go_out_as_the_running_bundle_or_kettle() {
        assert_eq!(
            notification_application(Some("org.example.app".into())),
            "org.example.app"
        );
        for none in [None, Some(String::new())] {
            assert_eq!(notification_application(none), KETTLE_BUNDLE_ID);
        }
        let plist = include_str!("../../../packaging/macos/Info.plist");
        let declared = plist
            .split("<key>CFBundleIdentifier</key>")
            .nth(1)
            .and_then(|rest| rest.split("<string>").nth(1))
            .and_then(|rest| rest.split("</string>").next());
        assert_eq!(declared, Some(KETTLE_BUNDLE_ID));
    }

    /// The application is named before the first notification is sent, on
    /// macOS, so the library never asks AppleScript for `use_default`.
    #[test]
    fn the_application_is_named_before_the_first_notification() {
        let src = include_str!("notifications.rs");
        let show = src
            .split("fn show(notification: DesktopNotification) -> bool {")
            .nth(1)
            .expect("show");
        let claim = show.find("claim_application();").expect("claimed");
        let sent = show.find("native.show()").expect("sent");
        assert!(claim < sent);
    }

    #[test]
    fn admission_is_bounded_and_nonblocking() {
        let (sender, receiver) = mpsc::sync_channel(1);
        try_queue(&sender, None, "first", "body").unwrap();
        let full = try_queue(&sender, None, "second", "later")
            .expect_err("a full notification queue must reject immediately");
        let mpsc::TrySendError::Full(Message::Show(rejected)) = full else {
            panic!("a live full queue returned the wrong admission result")
        };
        assert_eq!(rejected.title, "second");
        assert_eq!(rejected.body, "later");
        let Message::Show(admitted) = receiver.recv().unwrap() else {
            panic!("the admitted message changed kind")
        };
        assert_eq!(admitted.title, "first");
        assert_eq!(admitted.body, "body");

        drop(receiver);
        assert!(matches!(
            try_queue(&sender, None, "third", "gone"),
            Err(mpsc::TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn flush_waits_for_preceding_messages_without_joining_the_worker() {
        let (sender, receiver) = mpsc::sync_channel(1);
        try_queue(&sender, None, "first", "body").unwrap();
        let worker = std::thread::spawn(move || {
            let Message::Show(notification) = receiver.recv().unwrap() else {
                panic!("notification must precede the flush")
            };
            assert_eq!(notification.title, "first");
            let Message::Flush(acknowledge) = receiver.recv().unwrap() else {
                panic!("flush message missing")
            };
            acknowledge.send(()).unwrap();
        });
        assert!(flush_sender(&sender, Duration::from_secs(1)));
        drop(sender);
        worker.join().unwrap();
    }

    #[test]
    fn a_full_stalled_queue_keeps_flush_bounded() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        try_queue(&sender, None, "first", "body").unwrap();
        let started = std::time::Instant::now();
        assert!(!flush_sender(&sender, Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn a_stalled_backend_cannot_block_admission_or_bounded_shutdown() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(0);
        let (release_sender, release_receiver) = mpsc::sync_channel(0);
        let dispatcher = spawn_dispatcher(
            1,
            move |notification| {
                if notification.title == "block" {
                    entered_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                }
                true
            },
            |_| {},
        )
        .unwrap();

        try_queue(&dispatcher.sender, None, "block", "backend").unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("the injected backend never started");

        let admission_started = std::time::Instant::now();
        try_queue(&dispatcher.sender, None, "queued", "later").unwrap();
        assert!(matches!(
            try_queue(&dispatcher.sender, None, "dropped", "full"),
            Err(mpsc::TrySendError::Full(_))
        ));
        assert!(
            admission_started.elapsed() < Duration::from_millis(250),
            "a blocked backend reached the caller instead of only its worker"
        );

        let shutdown_started = std::time::Instant::now();
        assert!(!flush_sender(&dispatcher.sender, Duration::from_millis(20)));
        assert!(
            shutdown_started.elapsed() < Duration::from_millis(250),
            "bounded shutdown waited for a stalled notification backend"
        );

        release_sender.send(()).unwrap();
        assert!(flush_sender(&dispatcher.sender, Duration::from_secs(1)));
        let Dispatcher {
            sender,
            _worker: worker,
        } = dispatcher;
        drop(sender);
        worker.join().unwrap();
    }

    /// A tracked notification the backend refuses, fails on or panics in
    /// comes back to the UI by id, in order and without its text; one it
    /// delivers, and an untracked one, do not. The channel back is bounded.
    #[test]
    fn failed_tracked_notifications_come_back_by_id_only() {
        let (failed_sender, failed) = mpsc::channel();
        let dispatcher = spawn_dispatcher(
            8,
            |notification| match notification.title.as_str() {
                "refused" => false,
                "panics" => panic!("backend"),
                _ => true,
            },
            move |id| failed_sender.send(id).unwrap(),
        )
        .unwrap();
        for (id, title) in [
            (Some(1), "shown"),
            (Some(2), "refused"),
            (None, "refused"),
            (Some(3), "panics"),
            (Some(4), "shown"),
        ] {
            try_queue(&dispatcher.sender, id, title, "secret body").unwrap();
        }
        assert!(flush_sender(&dispatcher.sender, Duration::from_secs(2)));
        assert_eq!(failed.try_iter().collect::<Vec<_>>(), vec![2, 3]);

        let failures = Failures::new(2);
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = woken.clone();
        let _ = failures.wake.set(Box::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        for id in 10..20 {
            failures.report(id);
        }
        assert_eq!(failures.take(), vec![10, 11], "bounded, oldest kept");
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert!(failures.take().is_empty());
    }
}
