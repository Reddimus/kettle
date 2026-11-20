use super::*;
use futures_lite::future;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

enum Part {
    Bytes(VecDeque<u8>),
    Zeros(u64),
    /// The read that reaches this waits once.
    Stall,
}

/// A scripted socket that records the largest buffer it was asked to fill.
struct Wire {
    parts: VecDeque<Part>,
    /// The most a single read returns.
    chunk: usize,
    largest: Arc<AtomicUsize>,
}

impl std::fmt::Debug for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wire")
    }
}

impl Wire {
    fn new(parts: impl IntoIterator<Item = Part>) -> (Self, Arc<AtomicUsize>) {
        let largest = Arc::new(AtomicUsize::new(0));
        (
            Self {
                parts: parts.into_iter().collect(),
                chunk: usize::MAX,
                largest: largest.clone(),
            },
            largest,
        )
    }
}

#[async_trait::async_trait]
impl ReadHalf for Wire {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)> {
        self.largest.fetch_max(buf.len(), Ordering::AcqRel);
        if matches!(self.parts.front(), Some(Part::Stall)) {
            self.parts.pop_front();
            future::yield_now().await;
        }
        let room = buf.len().min(self.chunk);
        let read = match self.parts.front_mut() {
            None | Some(Part::Stall) => 0,
            Some(Part::Bytes(bytes)) => {
                let read = room.min(bytes.len());
                for (slot, byte) in buf.iter_mut().zip(bytes.drain(..read)) {
                    *slot = byte;
                }
                read
            }
            Some(Part::Zeros(left)) => {
                let read = (room as u64).min(*left) as usize;
                buf[..read].fill(0);
                *left -= read as u64;
                read
            }
        };
        if matches!(self.parts.front(), Some(Part::Bytes(bytes)) if bytes.is_empty())
            || matches!(self.parts.front(), Some(Part::Zeros(0)))
        {
            self.parts.pop_front();
        }
        Ok((read, Vec::new()))
    }
}

fn bytes(bytes: impl Into<Vec<u8>>) -> Part {
    Part::Bytes(bytes.into().into())
}

/// A signal named `member`, as it travels.
fn signal(member: &str, body: &str) -> Vec<u8> {
    zbus::Message::signal("/org/kettle/Test", "org.kettle.Test", member)
        .unwrap()
        .build(&body)
        .unwrap()
        .data()
        .bytes()
        .to_vec()
}

/// A little-endian primary header that declares `body` bytes after it.
fn header(body: u32) -> Vec<u8> {
    let mut header = vec![b'l', 4, 0, 1];
    header.extend(body.to_le_bytes());
    header.extend(1u32.to_le_bytes());
    header.extend(0u32.to_le_bytes());
    header
}

fn receive<R: ReadHalf>(
    reader: &mut R,
    seq: u64,
    already: &mut Vec<u8>,
) -> zbus::Result<zbus::Message> {
    future::block_on(reader.receive_message(seq, already, &mut Vec::new()))
}

fn member(message: &zbus::Message) -> String {
    message.header().member().unwrap().to_string()
}

const LONG: u32 = 8 * 1024 * 1024;

#[test]
fn an_oversized_message_is_dropped_without_a_buffer_of_its_declared_length() {
    let script = || {
        [
            bytes(header(LONG)),
            Part::Zeros(u64::from(LONG)),
            bytes(signal("After", "kept")),
        ]
    };
    // zbus's own reader asks the socket to fill the whole declared length.
    let (mut plain, asked) = Wire::new(script());
    let _ = receive(&mut plain, 1, &mut Vec::new());
    assert_eq!(asked.load(Ordering::Acquire), LONG as usize);

    let (wire, asked) = Wire::new(script());
    let mut reader = Bounded::new(wire);
    let message = receive(&mut reader, 1, &mut Vec::new()).unwrap();
    assert_eq!(member(&message), "After");
    assert_eq!(message.body().deserialize::<String>().unwrap(), "kept");
    assert!(
        asked.load(Ordering::Acquire) as u64 <= MAX_MESSAGE_BYTES,
        "asked the socket for {} bytes",
        asked.load(Ordering::Acquire)
    );
    assert!(matches!(
        receive(&mut reader, 2, &mut Vec::new()),
        Err(zbus::Error::InputOutput(_))
    ));
}

#[test]
fn a_message_at_the_limit_arrives_and_one_past_it_does_not() {
    let sized = |member: &str, len: u64| {
        let mut body = "x".repeat(60_000);
        loop {
            let message = signal(member, &body);
            let now = message.len() as u64;
            if now == len {
                return message;
            }
            let wanted = body.len() as u64 + len - now;
            body = "x".repeat(wanted as usize);
        }
    };
    let (wire, _) = Wire::new([
        bytes(sized("Past", MAX_MESSAGE_BYTES + 1)),
        bytes(sized("AtLimit", MAX_MESSAGE_BYTES)),
    ]);
    let mut reader = Bounded::new(wire);
    let message = receive(&mut reader, 1, &mut Vec::new()).unwrap();
    assert_eq!(member(&message), "AtLimit");
    assert_eq!(message.data().len() as u64, MAX_MESSAGE_BYTES);
}

#[test]
fn bytes_the_handshake_read_ahead_are_bounded_too() {
    let first = signal("First", "one");
    let last = signal("Last", "three");
    let mut stream = first.clone();
    stream.extend(header(100_000));
    stream.extend(vec![0; 100_000]);
    stream.extend(&last);
    // The handshake stopped inside the oversized header, inside its body, or
    // inside the header after it.
    for ahead in [
        first.len() + 7,
        first.len() + PRIMARY_HEADER + 300,
        first.len() + PRIMARY_HEADER + 100_000 + 7,
    ] {
        let mut already = stream.clone();
        let (mut wire, asked) = Wire::new([bytes(already.split_off(ahead))]);
        wire.chunk = 5;
        let mut reader = Bounded::new(wire);
        assert_eq!(
            member(&receive(&mut reader, 1, &mut already).unwrap()),
            "First"
        );
        let message = receive(&mut reader, 2, &mut already).unwrap();
        assert_eq!(member(&message), "Last");
        assert_eq!(message.body().deserialize::<String>().unwrap(), "three");
        assert!(already.is_empty());
        assert!(asked.load(Ordering::Acquire) <= 4096);
    }
}

#[test]
fn a_big_endian_length_is_read_as_one() {
    let mut big = vec![b'B', 4, 0, 1];
    big.extend(70_000u32.to_be_bytes());
    big.extend(1u32.to_be_bytes());
    big.extend(0u32.to_be_bytes());
    let (wire, asked) = Wire::new([bytes(big), Part::Zeros(70_000), bytes(signal("After", ""))]);
    let mut reader = Bounded::new(wire);
    assert_eq!(
        member(&receive(&mut reader, 1, &mut Vec::new()).unwrap()),
        "After"
    );
    assert!(asked.load(Ordering::Acquire) <= 4096);
}

#[test]
fn a_drop_resumes_where_a_cancelled_read_left_it() {
    let (wire, _) = Wire::new([
        bytes(header(150_000)),
        Part::Zeros(60_000),
        Part::Stall,
        Part::Zeros(90_000),
        bytes(signal("After", "")),
    ]);
    let mut reader = Bounded::new(wire);
    let mut already = Vec::new();
    let cancelled = future::block_on(future::poll_once(reader.receive_message(
        1,
        &mut already,
        &mut Vec::new(),
    )));
    assert!(cancelled.is_none());
    assert_eq!(
        member(&receive(&mut reader, 1, &mut already).unwrap()),
        "After"
    );
}

#[test]
fn the_stream_ending_inside_a_dropped_message_is_an_error() {
    let (wire, asked) = Wire::new([bytes(header(100_000)), Part::Zeros(10)]);
    assert!(matches!(
        receive(&mut Bounded::new(wire), 1, &mut Vec::new()),
        Err(zbus::Error::InputOutput(_))
    ));
    assert!(asked.load(Ordering::Acquire) <= 4096);
}

#[test]
fn a_length_past_the_specification_closes_the_connection_unread() {
    let mut long = header(u32::MAX);
    long[12..].copy_from_slice(&u32::MAX.to_le_bytes());
    let (wire, asked) = Wire::new([bytes(long), Part::Zeros(1 << 20)]);
    assert!(matches!(
        receive(&mut Bounded::new(wire), 1, &mut Vec::new()),
        Err(zbus::Error::ExcessData)
    ));
    assert_eq!(asked.load(Ordering::Acquire), PRIMARY_HEADER);
}

#[test]
fn a_bus_that_is_not_the_one_its_address_names_is_refused() {
    let bus = crate::test_bus::Bus::start();
    let open = |address: String| future::block_on(connect(Some(&address), Options::default()));
    let connection = open(bus.address.clone()).unwrap();
    let guid = connection.server_guid().to_string();
    open(format!("{},guid={guid}", bus.address)).unwrap();
    let other = "0123456789abcdef0123456789abcdef";
    assert_ne!(guid, other);
    assert!(matches!(
        open(format!("{},guid={other}", bus.address)),
        Err(zbus::Error::Handshake(_))
    ));
}

#[test]
fn an_abstract_socket_is_reached_and_other_transports_are_refused() {
    let name = format!("kettle-dbus-bound-test-{}", std::process::id());
    let listener = std::os::unix::net::UnixListener::bind_addr(
        &SocketAddr::from_abstract_name(name.as_bytes()).unwrap(),
    )
    .unwrap();
    let (accepted, accept) = std::sync::mpsc::channel();
    std::thread::spawn(move || accepted.send(listener.accept().is_ok()));
    let result = future::block_on(connect(
        Some(&format!("unix:abstract={name}")),
        Options::default(),
    ));
    assert!(
        accept
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    );
    // The listener hangs up instead of authenticating.
    assert!(!matches!(result, Ok(_) | Err(zbus::Error::Unsupported)));

    for address in ["tcp:host=127.0.0.1,port=1", "unix:dir=/tmp"] {
        assert!(matches!(
            future::block_on(connect(Some(address), Options::default())),
            Err(zbus::Error::Unsupported)
        ));
    }
}

#[test]
fn connection_attempts_that_wait_are_capped() {
    static WAITING: AtomicUsize = AtomicUsize::new(0);
    fn refused() -> std::io::Result<UnixStream> {
        Err(std::io::ErrorKind::ConnectionRefused.into())
    }
    let (release, held) = async_channel::bounded::<()>(1);
    let attempts: Vec<_> = (0..MAX_DIALS)
        .map(|_| {
            let held = held.clone();
            dial(&WAITING, move || {
                let _ = held.recv_blocking();
                refused()
            })
            .unwrap()
        })
        .collect();
    assert!(matches!(
        dial(&WAITING, refused),
        Err(zbus::Error::Failure(_))
    ));
    drop(release);
    for attempt in attempts {
        assert!(attempt.recv_blocking().unwrap().is_err());
    }
    // Each thread gives its place back as it ends.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while WAITING.load(Ordering::Acquire) != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(
        dial(&WAITING, refused)
            .unwrap()
            .recv_blocking()
            .unwrap()
            .is_err()
    );
}

/// Every bus connection Kettle opens goes through `connect`: outside tests and
/// `dbus_bound.rs`, nothing in this crate may name zbus's connection builder
/// or one of its connecting constructors.
#[test]
fn no_kettle_connection_is_built_around_the_bound() {
    const NEEDLES: [&str; 12] = [
        "connection::Builder",
        "conn::Builder",
        "Builder::session(",
        "Builder::system(",
        "Builder::address(",
        "Builder::socket(",
        "Builder::unix_stream(",
        "Builder::tcp_stream(",
        "Builder::vsock_stream(",
        "Builder::authenticated_socket(",
        "Connection::session(",
        "Connection::system(",
    ];
    fn visit(dir: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, found);
                continue;
            }
            let name = path.to_string_lossy().into_owned();
            if !name.ends_with(".rs")
                || name.ends_with("tests.rs")
                || name.ends_with("test_bus.rs")
                || path.ends_with("src/dbus_bound.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            // Inline test modules build their fake services by address. One
            // written another way is searched whole, so it fails here instead
            // of hiding anything.
            let production = source.split("\nmod tests {").next().unwrap();
            if NEEDLES.iter().any(|needle| production.contains(needle)) {
                found.push(name);
            }
        }
    }
    let mut found = Vec::new();
    visit(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    assert_eq!(found, Vec::<String>::new());
    for (file, source) in [
        ("portal_settings.rs", include_str!("../portal_settings.rs")),
        (
            "quick_terminal/portal/linux.rs",
            include_str!("../quick_terminal/portal/linux.rs"),
        ),
        (
            "media/external/loupe.rs",
            include_str!("../media/external/loupe.rs"),
        ),
        (
            "notifications/linux.rs",
            include_str!("../notifications/linux.rs"),
        ),
        (
            "media/external/reveal.rs",
            include_str!("../media/external/reveal.rs"),
        ),
    ] {
        assert!(source.contains("crate::dbus_bound::connect("), "{file}");
    }
}
