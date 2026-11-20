//! Session-bus connections that size no buffer from an oversized message.
//!
//! zbus reads the 16-byte primary header of a message and then resizes its
//! receive buffer to the declared length, up to the 128 MiB the specification
//! allows, before the rest has arrived. Any peer on the bus can address a
//! message to a connection's unique name, so every connection Kettle opens
//! reads through [`Bounded`], which looks at that header first and reads a
//! message longer than [`MAX_MESSAGE_BYTES`] into a small scratch buffer and
//! drops it. zbus never sees the message, and the connection stays up: closing
//! it would let the same peer reset Kettle's view of the desktop settings. A
//! length past the specification's limit still ends the connection unread, as
//! it does in zbus.
use std::os::fd::OwnedFd;
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_io::Async;
use zbus::address::{Address, Transport, transport::UnixSocket};
use zbus::connection::socket::{ReadHalf, Socket as _, Split, WriteHalf};
use zbus::connection::{Builder, Connection};

/// The longest message, header included, that a Kettle bus connection takes.
///
/// The replies and signals Kettle reads are a few hundred bytes, and those it
/// checks after they arrive are refused above 16 KiB at most, so this leaves
/// room for them and their headers. With zbus's 64-message queue a stream
/// holds at most 4 MiB.
pub(crate) const MAX_MESSAGE_BYTES: u64 = 64 * 1024;
/// The longest message the D-Bus specification allows.
const SPECIFICATION_MAX: u64 = 128 * 1024 * 1024;
const PRIMARY_HEADER: usize = 16;
/// The most connection attempts that may wait in `connect(2)` at once. Callers
/// give up on an attempt after a timeout and try again, and a bus that accepts
/// nothing would otherwise keep one more thread for each.
const MAX_DIALS: usize = 64;
static DIALS: AtomicUsize = AtomicUsize::new(0);

/// Connect to the bus at `address`, or to the session bus, through [`Bounded`].
///
/// Only unix socket addresses are supported, which is what a Linux session bus
/// listens on; any other transport is refused rather than read unbounded. The
/// connection passes no file descriptors.
pub(crate) async fn connect(
    address: Option<&str>,
    configure: impl FnOnce(Builder<'static>) -> Builder<'static>,
) -> zbus::Result<Connection> {
    let address = match address {
        Some(address) => Address::from_str(address)?,
        None => Address::session()?,
    };
    let target = match address.transport() {
        Transport::Unix(unix) => match unix.path() {
            UnixSocket::File(path) => SocketAddr::from_pathname(path)?,
            UnixSocket::Abstract(name) => SocketAddr::from_abstract_name(name.as_encoded_bytes())?,
            _ => return Err(zbus::Error::Unsupported),
        },
        _ => return Err(zbus::Error::Unsupported),
    };
    let stream = dial(&DIALS, move || UnixStream::connect_addr(&target))?
        .recv()
        .await
        .map_err(|_| zbus::Error::Failure("session bus connection failed".into()))??;
    let split = Async::new(stream)?.split();
    let (read, write) = split.take();
    let read: Box<dyn ReadHalf> = Box::new(Bounded::new(read));
    let write: Box<dyn WriteHalf> = Box::new(write);
    let connection = configure(Builder::socket(Split::new(read, write)))
        .build()
        .await?;
    // `Builder::address` checks this during the handshake; a caller-supplied
    // socket has no address to check against.
    if let Some(expected) = address.guid()
        && expected.as_str() != connection.server_guid().as_str()
    {
        return Err(zbus::Error::Handshake(
            "session bus is not the one its address names".into(),
        ));
    }
    Ok(connection)
}

/// A place among the attempts that `dials` counts, given back when dropped.
struct Dial(&'static AtomicUsize);

impl Dial {
    fn begin(dials: &'static AtomicUsize) -> Option<Self> {
        dials
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |waiting| {
                (waiting < MAX_DIALS).then_some(waiting + 1)
            })
            .ok()
            .map(|_| Self(dials))
    }
}

impl Drop for Dial {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Run `connect` on a thread of its own and hand back where its result
/// arrives. connect(2) on a unix socket can block while the listener's
/// backlog is full, and callers bound the wait with a timer; an attempt past
/// [`MAX_DIALS`] still waiting is refused instead of started.
fn dial(
    dials: &'static AtomicUsize,
    connect: impl FnOnce() -> std::io::Result<UnixStream> + Send + 'static,
) -> zbus::Result<async_channel::Receiver<std::io::Result<UnixStream>>> {
    let Some(place) = Dial::begin(dials) else {
        return Err(zbus::Error::Failure(
            "too many session bus connection attempts are waiting".into(),
        ));
    };
    let (connected, stream) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kettle-dbus-connect".into())
        .spawn(move || {
            // Dropped last, once the result is on its way.
            let _place = place;
            let _ = connected.send_blocking(connect());
        })?;
    Ok(stream)
}

/// A read half that drops each message longer than [`MAX_MESSAGE_BYTES`].
#[derive(Debug)]
pub(crate) struct Bounded<R> {
    inner: R,
    /// Bytes of a dropped message still to read. Kept here so that a dropped
    /// `receive_message` future resumes on a message boundary.
    skip: u64,
}

impl<R> Bounded<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner, skip: 0 }
    }
}

/// The length of the message a primary header opens, or `None` when its
/// endianness flag is not one D-Bus defines.
fn declared_len(header: &[u8; PRIMARY_HEADER]) -> Option<u64> {
    let read: fn([u8; 4]) -> u32 = match header[0] {
        b'l' => u32::from_le_bytes,
        b'B' => u32::from_be_bytes,
        _ => return None,
    };
    let field = |at: usize| {
        u64::from(read([
            header[at],
            header[at + 1],
            header[at + 2],
            header[at + 3],
        ]))
    };
    let fields = PRIMARY_HEADER as u64 + field(12);
    Some(fields.next_multiple_of(8) + field(4))
}

fn closed() -> zbus::Error {
    std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "failed to receive message",
    )
    .into()
}

#[async_trait::async_trait]
impl<R: ReadHalf> ReadHalf for Bounded<R> {
    async fn receive_message(
        &mut self,
        seq: u64,
        already_received_bytes: &mut Vec<u8>,
        already_received_fds: &mut Vec<OwnedFd>,
    ) -> zbus::Result<zbus::Message> {
        let mut scratch = [0; 4096];
        loop {
            while self.skip > 0 {
                if !already_received_bytes.is_empty() {
                    let buffered = already_received_bytes.len() as u64;
                    let taken = self.skip.min(buffered);
                    already_received_bytes.drain(..taken as usize);
                    self.skip -= taken;
                    continue;
                }
                let room = self.skip.min(scratch.len() as u64) as usize;
                // Descriptors that came with a dropped message close here.
                let (read, _) = self.inner.recvmsg(&mut scratch[..room]).await?;
                if read == 0 {
                    return Err(closed());
                }
                self.skip -= read as u64;
            }
            // The handshake can leave part of a header, or several messages,
            // in `already_received_bytes`; the header is completed there so
            // that zbus's own reader takes it from where it expects it.
            while already_received_bytes.len() < PRIMARY_HEADER {
                let room = PRIMARY_HEADER - already_received_bytes.len();
                let (read, _) = self.inner.recvmsg(&mut scratch[..room]).await?;
                if read == 0 {
                    return Err(closed());
                }
                already_received_bytes.extend_from_slice(&scratch[..read]);
            }
            let header: [u8; PRIMARY_HEADER] = already_received_bytes[..PRIMARY_HEADER]
                .try_into()
                .expect("a primary header is 16 bytes");
            match declared_len(&header) {
                Some(len) if len > SPECIFICATION_MAX => return Err(zbus::Error::ExcessData),
                Some(len) if len > MAX_MESSAGE_BYTES => {
                    self.skip = len;
                    log::debug!("dropped a {len}-byte session bus message unread");
                }
                // In bounds, or malformed and zbus's to refuse.
                _ => {
                    return self
                        .inner
                        .receive_message(seq, already_received_bytes, already_received_fds)
                        .await;
                }
            }
        }
    }

    async fn recvmsg(&mut self, buf: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)> {
        // Only the handshake reads through here.
        self.inner.recvmsg(buf).await
    }

    /// Kettle neither sends nor accepts file descriptors over D-Bus, so the
    /// connection does not negotiate them and the bus delivers none.
    fn can_pass_unix_fd(&self) -> bool {
        false
    }

    async fn peer_credentials(&mut self) -> std::io::Result<zbus::fdo::ConnectionCredentials> {
        self.inner.peer_credentials().await
    }

    fn auth_mechanism(&self) -> zbus::AuthMechanism {
        self.inner.auth_mechanism()
    }
}

#[cfg(test)]
mod tests;
