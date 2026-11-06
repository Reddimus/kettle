//! CLIPBOARD text through the bridge's own `wl_data_device`.
//!
//! A compositor tells a client about the selection through one data device per
//! seat; Mutter, for one, sends `selection` only to the first device a client
//! created. The bridge already owns that device for drag and drop, so the
//! clipboard reads and writes through it instead of a second device that would
//! never hear of a selection.
//!
//! The bridge records the current offer, its best text type and the latest
//! input serial while it dispatches on the UI thread. Reads run on the
//! clipboard worker: they send `receive` for the recorded offer and read the
//! pipe within the caller's size cap and deadline. Wayland proxies are safe to
//! use from that thread. A selection Kettle itself owns is answered from
//! memory, because its `send` event is dispatched on the UI thread, which is
//! waiting for this read.
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Read, Result};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
};
use wayland_client::{Connection, QueueHandle};

/// Text types, most preferred first.
pub(super) const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];
/// Sources kept for answering `send`; older ones are cancelled by the
/// compositor once a newer selection replaces them.
const MAX_SOURCES: usize = 4;

/// The preferred text type among `current` and `offered`.
pub(super) fn better_text_mime(
    current: Option<&'static str>,
    offered: &str,
) -> Option<&'static str> {
    let rank = |mime: &str| TEXT_MIMES.iter().position(|known| *known == mime);
    let Some(new) = rank(offered) else {
        return current;
    };
    match current.and_then(rank) {
        Some(old) if old <= new => current,
        _ => Some(TEXT_MIMES[new]),
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
    offer: Option<(wl_data_offer::WlDataOffer, &'static str)>,
    sources: BTreeMap<u64, (wl_data_source::WlDataSource, Arc<str>)>,
    owned: Option<u64>,
    next: u64,
}

pub(crate) struct Selection(Mutex<Inner>);

/// The process's one selection state, shared by the bridge and the worker.
pub(crate) fn shared() -> &'static Selection {
    static SELECTION: OnceLock<Selection> = OnceLock::new();
    SELECTION.get_or_init(|| Selection(Mutex::new(Inner::default())))
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

    /// The compositor's new selection offer (or none), replacing the last one.
    pub(super) fn offer(&self, offer: Option<(wl_data_offer::WlDataOffer, &'static str)>) {
        if let Some((old, _)) = std::mem::replace(&mut self.lock().offer, offer) {
            old.destroy();
        }
    }

    pub(super) fn text_for(&self, id: u64) -> Option<Arc<str>> {
        self.lock().sources.get(&id).map(|(_, text)| text.clone())
    }

    pub(super) fn cancelled(&self, id: u64) {
        let mut inner = self.lock();
        if inner.owned == Some(id) {
            inner.owned = None;
        }
        if let Some((source, _)) = inner.sources.remove(&id) {
            source.destroy();
        }
    }

    pub(super) fn close(&self) {
        let mut inner = self.lock();
        if let Some((offer, _)) = inner.offer.take() {
            offer.destroy();
        }
        for (_, (source, _)) in std::mem::take(&mut inner.sources) {
            source.destroy();
        }
        inner.owned = None;
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
        let (offer, mime, connection) = {
            let inner = self.lock();
            if let Some(text) = inner
                .owned
                .and_then(|id| inner.sources.get(&id))
                .map(|(_, text)| text.clone())
            {
                return if text.len() > max {
                    Err(Error::new(
                        ErrorKind::FileTooLarge,
                        "selection exceeds the cap",
                    ))
                } else {
                    Ok(text.to_string())
                };
            }
            let connection = inner
                .connection
                .clone()
                .ok_or_else(|| Error::new(ErrorKind::NotConnected, "no Wayland bridge"))?;
            let (offer, mime) = inner
                .offer
                .clone()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "the selection is empty"))?;
            (offer, mime, connection)
        };
        let (mut reader, writer) = UnixStream::pair()?;
        offer.receive(mime.to_string(), writer.as_fd());
        connection
            .flush()
            .map_err(|error| Error::other(format!("flush: {error}")))?;
        drop(writer);
        read_bounded(&mut reader, max, deadline)
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
            return Err(Error::new(
                ErrorKind::NotConnected,
                "no seat input yet to authorize a selection",
            ));
        };
        let id = inner.next;
        inner.next += 1;
        let source = manager.create_data_source(&queue, ClipSource(id));
        for mime in TEXT_MIMES {
            source.offer(mime.to_string());
        }
        device.set_selection(Some(&source), serial);
        inner.sources.insert(id, (source, Arc::from(text)));
        inner.owned = Some(id);
        while inner.sources.len() > MAX_SOURCES {
            if let Some((_, (old, _))) = inner.sources.pop_first() {
                old.destroy();
            }
        }
        let connection = inner.connection.clone();
        drop(inner);
        connection
            .ok_or_else(|| Error::new(ErrorKind::NotConnected, "no Wayland bridge"))?
            .flush()
            .map_err(|error| Error::other(format!("flush: {error}")))
    }
}

/// Read to end of file within `max` bytes and `deadline`.
pub(super) fn read_bounded(
    reader: &mut UnixStream,
    max: usize,
    deadline: Instant,
) -> Result<String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::new(ErrorKind::TimedOut, "selection read timed out"));
        }
        reader.set_read_timeout(Some(left))?;
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.len() > max {
                    return Err(Error::new(
                        ErrorKind::FileTooLarge,
                        "selection exceeds the cap",
                    ));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Error::new(ErrorKind::TimedOut, "selection read timed out"));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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

    #[test]
    fn text_types_rank_utf8_first_and_ignore_others() {
        assert_eq!(better_text_mime(None, "image/png"), None);
        assert_eq!(better_text_mime(None, "STRING"), Some("STRING"));
        assert_eq!(
            better_text_mime(Some("STRING"), "text/plain"),
            Some("text/plain")
        );
        assert_eq!(
            better_text_mime(Some("text/plain"), "text/plain;charset=utf-8"),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(
            better_text_mime(Some("text/plain;charset=utf-8"), "UTF8_STRING"),
            Some("text/plain;charset=utf-8")
        );
    }

    #[test]
    fn bounded_reads_stop_at_the_cap_the_deadline_and_eof() {
        use std::io::Write;
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all("héllo".as_bytes()).unwrap();
        drop(writer);
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        assert_eq!(read_bounded(&mut reader, 6, deadline).unwrap(), "héllo");

        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"0123456789").unwrap();
        drop(writer);
        let error = read_bounded(&mut reader, 4, deadline).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::FileTooLarge);

        let (mut reader, _writer) = UnixStream::pair().unwrap();
        let soon = Instant::now() + std::time::Duration::from_millis(50);
        assert_eq!(
            read_bounded(&mut reader, 4, soon).unwrap_err().kind(),
            ErrorKind::TimedOut
        );

        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(&[b'a', 0xff, b'b']).unwrap();
        drop(writer);
        assert_eq!(
            read_bounded(&mut reader, 8, deadline).unwrap(),
            "a\u{fffd}b"
        );
    }

    #[test]
    fn an_empty_selection_is_not_found_and_store_needs_input() {
        let selection = Selection(Mutex::new(Inner {
            connection: None,
            ..Inner::default()
        }));
        let deadline = Instant::now() + std::time::Duration::from_millis(50);
        assert_eq!(
            selection.load(8, deadline).unwrap_err().kind(),
            ErrorKind::NotConnected
        );
        assert_eq!(
            selection.store("x".into()).unwrap_err().kind(),
            ErrorKind::NotConnected
        );
        assert!(!selection.available());
    }
}
