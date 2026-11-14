//! CLIPBOARD through the bridge's own `wl_data_device`.
//!
//! A compositor tells a client about the selection through one data device per
//! seat; Mutter, for one, sends `selection` only to the first device a client
//! created. The bridge already owns that device for drag and drop, so the
//! clipboard reads and writes through it instead of a second device that would
//! never hear of a selection.
//!
//! The bridge records the current offer, the types it lists and the latest
//! input serial while it dispatches on the UI thread. Reads run on
//! the clipboard worker: they send `receive` for the recorded offer and read
//! the pipe within the caller's size cap and deadline. Wayland proxies are
//! safe to use from that thread. A selection Kettle itself owns is answered
//! from memory, because its `send` event is dispatched on the UI thread, which
//! is waiting for this read. What a paste takes from an offer is decided in
//! [`crate::wayland::offer`].
use std::io::{Error, ErrorKind, Result};
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
};
use wayland_client::{Connection, QueueHandle};

use super::super::offer::{self, Channel, Kinds, Offered, Owned, TEXT_MIMES, Transfer};
use crate::clipboard::{PasteContent, PasteWant};

impl Offered for wl_data_offer::WlDataOffer {
    fn receive(&self, mime: &'static str, fd: BorrowedFd<'_>) {
        wl_data_offer::WlDataOffer::receive(self, mime.to_string(), fd);
    }

    fn discard(self) {
        self.destroy();
    }
}

impl Owned for wl_data_source::WlDataSource {
    fn discard(self) {
        self.destroy();
    }
}

/// User data of a clipboard source, distinct from drag sources.
pub(super) struct ClipSource(pub(super) u64);

#[derive(Default)]
struct Inner {
    connection: Option<Connection>,
    queue: Option<QueueHandle<super::State>>,
    manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    device: Option<wl_data_device::WlDataDevice>,
    serial: Option<u32>,
    clipboard: Channel<wl_data_offer::WlDataOffer, wl_data_source::WlDataSource>,
    next: u64,
}

pub(crate) struct Selection(Mutex<Inner>);

/// The process's one selection state, shared by the bridge and the worker.
pub(crate) fn shared() -> &'static Selection {
    static SELECTION: OnceLock<Selection> = OnceLock::new();
    SELECTION.get_or_init(|| Selection(Mutex::new(Inner::default())))
}

fn no_bridge() -> Error {
    Error::new(ErrorKind::NotConnected, "no Wayland bridge")
}

fn flush(connection: &Connection) -> Result<()> {
    connection
        .flush()
        .map_err(|error| Error::other(format!("flush: {error}")))
}

impl Selection {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn attach(&self, connection: Connection, queue: QueueHandle<super::State>) {
        let mut inner = self.lock();
        inner.connection = Some(connection);
        inner.queue = Some(queue);
    }

    pub(super) fn set_manager(&self, manager: wl_data_device_manager::WlDataDeviceManager) {
        self.lock().manager = Some(manager);
    }

    /// Input on a seat: its device and serial authorize the next selection
    /// this client sets.
    pub(super) fn input(&self, device: Option<&wl_data_device::WlDataDevice>, serial: u32) {
        let mut inner = self.lock();
        if let Some(device) = device {
            inner.device = Some(device.clone());
        }
        inner.serial = Some(serial);
    }

    /// The compositor's new CLIPBOARD offer (or none), replacing the last one.
    pub(super) fn offer(&self, offer: Option<(wl_data_offer::WlDataOffer, Kinds)>) {
        self.lock().clipboard.set_offer(offer);
    }

    pub(super) fn text_for(&self, id: u64) -> Option<Arc<str>> {
        self.lock().clipboard.text_for(id)
    }

    pub(super) fn cancelled(&self, id: u64) {
        self.lock().clipboard.cancelled(id);
    }

    pub(super) fn close(&self) {
        let mut inner = self.lock();
        inner.clipboard.close();
        inner.device = None;
        inner.manager = None;
    }

    /// Whether the bridge is connected and has a data device manager.
    pub(crate) fn available(&self) -> bool {
        let inner = self.lock();
        inner.connection.is_some() && inner.manager.is_some()
    }

    /// Read the CLIPBOARD text: at most `max` bytes (one more is an error) by
    /// `deadline`. Invalid UTF-8 is replaced rather than refused.
    pub(crate) fn load(&self, max: usize, deadline: Instant) -> Result<String> {
        let (held, connection) = {
            let inner = self.lock();
            (inner.clipboard.held(), inner.connection.clone())
        };
        transfer(connection, deadline, |transfer| {
            offer::load_text(held, max, transfer)
        })
    }

    /// What a paste takes from CLIPBOARD under `want`.
    pub(crate) fn paste(
        &self,
        want: PasteWant,
        text_max: usize,
        deadline: Instant,
    ) -> PasteContent {
        let (held, connection) = {
            let inner = self.lock();
            (inner.clipboard.held(), inner.connection.clone())
        };
        transfer(connection, deadline, |transfer| {
            offer::paste(held, want, text_max, transfer)
        })
    }

    /// The local files CLIPBOARD names.
    pub(crate) fn files(&self, deadline: Instant) -> Option<Vec<PathBuf>> {
        let (held, connection) = {
            let inner = self.lock();
            (inner.clipboard.held(), inner.connection.clone())
        };
        transfer(connection, deadline, |transfer| {
            offer::local_files(held, transfer)
        })
    }

    /// Own the CLIPBOARD selection with `text`, authorized by the latest input.
    pub(crate) fn store(&self, text: String) -> Result<()> {
        let mut inner = self.lock();
        let (Some(manager), Some(queue), Some(device), Some(serial)) = (
            inner.manager.clone(),
            inner.queue.clone(),
            inner.device.clone(),
            inner.serial,
        ) else {
            return Err(no_input());
        };
        let id = inner.next;
        inner.next += 1;
        let source = manager.create_data_source(&queue, ClipSource(id));
        for mime in TEXT_MIMES {
            source.offer(mime.to_string());
        }
        device.set_selection(Some(&source), serial);
        inner.clipboard.own(id, source, Arc::from(text));
        let connection = inner.connection.clone();
        drop(inner);
        flush(&connection.ok_or_else(no_bridge)?)
    }
}

fn no_input() -> Error {
    Error::new(
        ErrorKind::NotConnected,
        "no seat input yet to authorize a selection",
    )
}

/// Run one read against the compositor. A selection this client owns needs
/// no connection, so a missing bridge fails only a read that would use it.
fn transfer<T>(
    connection: Option<Connection>,
    deadline: Instant,
    run: impl FnOnce(&Transfer<'_>) -> T,
) -> T {
    let send = || flush(connection.as_ref().ok_or_else(no_bridge)?);
    run(&Transfer {
        flush: &send,
        deadline,
    })
}

/// Answer a `send` request: write `text` to the requester's descriptor on a
/// short-lived thread, giving up after a few seconds if it stops reading.
pub(super) fn send(text: Arc<str>, fd: std::os::fd::OwnedFd) {
    let spawned = std::thread::Builder::new()
        .name("kettle-wayland-send".into())
        .spawn(move || {
            use std::io::Write;
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            let mut file = std::fs::File::from(fd);
            // SAFETY: fcntl on a descriptor this thread owns; only the status
            // flags change.
            unsafe {
                use std::os::fd::AsRawFd;
                let raw = file.as_raw_fd();
                let flags = libc::fcntl(raw, libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
            let mut rest = text.as_bytes();
            while !rest.is_empty() && Instant::now() < deadline {
                match file.write(rest) {
                    Ok(0) => break,
                    Ok(n) => rest = &rest[n..],
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
    if spawned.is_err() {
        log::warn!("clipboard: could not start a writer for a selection request");
    }
}

impl wayland_client::Dispatch<wl_data_source::WlDataSource, ClipSource> for super::State {
    fn event(
        _: &mut Self,
        _: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        id: &ClipSource,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                if TEXT_MIMES.contains(&mime_type.as_str())
                    && let Some(text) = shared().text_for(id.0)
                {
                    send(text, fd);
                }
            }
            wl_data_source::Event::Cancelled => shared().cancelled(id.0),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::offer::Held;

    #[test]
    fn an_empty_selection_is_not_found_and_store_needs_input() {
        let selection = Selection(Mutex::new(Inner::default()));
        let deadline = Instant::now() + std::time::Duration::from_millis(50);
        assert_eq!(
            selection.load(8, deadline).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            selection.store("x".into()).unwrap_err().kind(),
            ErrorKind::NotConnected
        );
        assert!(!selection.available());
    }

    #[test]
    fn a_bridge_with_no_selection_gives_a_paste_nothing() {
        let selection = Selection(Mutex::new(Inner::default()));
        let deadline = Instant::now() + std::time::Duration::from_millis(50);
        assert!(matches!(selection.lock().clipboard.held(), Held::Empty));
        assert!(matches!(
            selection.paste(
                PasteWant {
                    files: true,
                    images: true
                },
                8,
                deadline
            ),
            PasteContent::Text(Err(arboard::Error::ContentNotAvailable))
        ));
        assert_eq!(selection.files(deadline), None);
    }
}
