//! Cycle 926 (agent-first A2): the local-IPC transport.
//!
//! A blocking, thread-per-connection transport with the same surface on every
//! OS: a [`CtlListener`] that `accept()`s [`CtlStream`]s (server), and
//! [`connect`] (client). No tokio — the server runs an accept thread + a
//! reader/writer thread per connection, matching kettle's thread-based model.
//!
//! - **Unix:** a `UnixListener` at a filesystem path, mode `0600`.
//! - **Windows:** a byte-mode named pipe (`\\.\pipe\kettle-ctl-<pid>`) created
//!   with `CreateNamedPipeW` (default DACL = creator/owner + admins) and
//!   `PIPE_UNLIMITED_INSTANCES`, wrapped via `File::from_raw_handle` so each
//!   connection is plain `Read`/`Write`. The *client* needs no platform code —
//!   `std::fs::OpenOptions` opens a named pipe natively.
//!
//! Hand-rolled over the `interprocess` crate (supply-chain leanness; the needed
//! subset is small and matches the windows-sys precedent in the bin crate).

use std::io::{self, Read, Write};

/// Shared client-connect retry policy, referenced by BOTH platform `connect`
/// impls so the Unix socket and Windows named-pipe legs stay in lockstep. A
/// *missing* endpoint (`NotFound`) is never retried (a dead server) — these
/// bound only genuinely-transient failures (the server mid-accept or swapping
/// instances): `CONNECT_RETRIES` attempts, `CONNECT_BACKOFF` between each, so
/// the worst case is ~`CONNECT_RETRIES * CONNECT_BACKOFF` before giving up.
const CONNECT_RETRIES: u32 = 50;
const CONNECT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(20);

/// One accepted connection — a bidirectional byte stream. `Read`/`Write` go to
/// the peer; [`try_clone`](CtlStream::try_clone) splits read/write across the
/// per-connection reader + writer threads.
pub enum CtlStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(windows)]
    Windows(std::fs::File),
}

impl Read for CtlStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(s) => s.read(buf),
            #[cfg(windows)]
            CtlStream::Windows(f) => f.read(buf),
        }
    }
}

impl Write for CtlStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(s) => s.write(buf),
            #[cfg(windows)]
            CtlStream::Windows(f) => f.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(s) => s.flush(),
            #[cfg(windows)]
            CtlStream::Windows(f) => f.flush(),
        }
    }
}

impl CtlStream {
    /// Clone the underlying handle so one half can read while the other writes.
    pub fn try_clone(&self) -> io::Result<CtlStream> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(s) => Ok(CtlStream::Unix(s.try_clone()?)),
            #[cfg(windows)]
            CtlStream::Windows(f) => Ok(CtlStream::Windows(f.try_clone()?)),
        }
    }

    /// Wait until at least one byte can be read, the peer closes, or `timeout`
    /// elapses. This is deliberately a readiness primitive rather than a
    /// socket read timeout: the Windows transport is a named-pipe `File`, and
    /// both client implementations need the same bounded-frame behavior.
    pub(crate) fn wait_readable(&self, timeout: std::time::Duration) -> io::Result<bool> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(stream) => {
                use std::os::fd::AsRawFd as _;

                let started = std::time::Instant::now();
                loop {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    let millis = remaining
                        .as_millis()
                        .saturating_add(u128::from(remaining.subsec_nanos() > 0))
                        .min(i32::MAX as u128) as i32;
                    let mut poll_fd = libc::pollfd {
                        fd: stream.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: `poll_fd` is a valid one-element poll array and
                    // the stream owns its fd for the duration of the call.
                    let result = unsafe { libc::poll(&mut poll_fd, 1, millis) };
                    if result > 0 {
                        return Ok(true);
                    }
                    if result == 0 {
                        return Ok(false);
                    }
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                    if started.elapsed() >= timeout {
                        return Ok(false);
                    }
                }
            }
            #[cfg(windows)]
            CtlStream::Windows(file) => {
                use std::os::windows::io::AsRawHandle as _;

                let deadline = std::time::Instant::now() + timeout;
                loop {
                    let mut available = 0u32;
                    // SAFETY: the file owns a valid named-pipe handle; the
                    // null buffer form queries available bytes without reading.
                    let ok = unsafe {
                        windows_sys::Win32::System::Pipes::PeekNamedPipe(
                            file.as_raw_handle() as _,
                            std::ptr::null_mut(),
                            0,
                            std::ptr::null_mut(),
                            &mut available,
                            std::ptr::null_mut(),
                        )
                    };
                    if ok == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if available > 0 {
                        return Ok(true);
                    }
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Ok(false);
                    }
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(now)
                            .min(std::time::Duration::from_millis(10)),
                    );
                }
            }
        }
    }

    /// Verify that an accepted local transport peer has the same effective user
    /// as this process. Filesystem permissions remain the first boundary; peer
    /// credentials close the race where a socket path is inherited or passed to
    /// another local account. Platforms without a peer-credential API retain
    /// the private endpoint/DACL boundary.
    pub fn peer_is_same_user(&self) -> io::Result<bool> {
        match self {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            CtlStream::Unix(stream) => {
                use std::os::fd::AsRawFd as _;
                let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
                let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
                // SAFETY: `credentials` and `len` are valid writable buffers and
                // the stream fd remains alive for the duration of the call.
                let rc = unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_PEERCRED,
                        std::ptr::addr_of_mut!(credentials).cast(),
                        &mut len,
                    )
                };
                if rc != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(credentials.uid == unsafe { libc::geteuid() })
            }
            #[cfg(any(
                target_os = "macos",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            CtlStream::Unix(stream) => {
                use std::os::fd::AsRawFd as _;
                let mut uid: libc::uid_t = 0;
                let mut gid: libc::gid_t = 0;
                // SAFETY: uid/gid are valid output pointers and the stream fd
                // remains alive for the call.
                let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
                if rc != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(uid == unsafe { libc::geteuid() })
            }
            #[cfg(all(
                unix,
                not(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "macos",
                    target_os = "freebsd",
                    target_os = "openbsd",
                    target_os = "netbsd",
                    target_os = "dragonfly"
                ))
            ))]
            CtlStream::Unix(_) => Ok(true),
            #[cfg(windows)]
            CtlStream::Windows(_) => Ok(true),
        }
    }

    /// v2.20.0 (review fix): has the peer hung up? Non-destructive and
    /// non-blocking (a zero-byte peek). Lets `wait_for`'s poll loop notice a
    /// vanished client instead of pinning one of the MAX_CONNECTIONS slots —
    /// and hammering the UI thread with probes — for up to the full timeout.
    /// This does NOT violate the one-thread sequential read→write rule: the
    /// caller IS the connection thread, with no other I/O outstanding on the
    /// handle. Errs toward "alive" on anything ambiguous (a false `dead`
    /// would cut short a legitimate wait).
    pub fn peer_disconnected(&self) -> bool {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(s) => {
                use std::os::unix::io::AsRawFd;
                let mut probe = [0u8; 1];
                // recv with MSG_PEEK|MSG_DONTWAIT (UnixStream::peek is still
                // unstable on the MSRV): 0 = orderly EOF (dead); >0 = request
                // bytes pending (alive); EWOULDBLOCK/EINTR = idle (alive);
                // anything else = reset (dead). Never consumes, never blocks.
                // SAFETY: the fd is valid for the stream's lifetime; the
                // buffer outlives the call.
                let n = unsafe {
                    libc::recv(
                        s.as_raw_fd(),
                        probe.as_mut_ptr() as *mut libc::c_void,
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                match n {
                    0 => true,
                    n if n > 0 => false,
                    _ => {
                        let e = io::Error::last_os_error();
                        !matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        )
                    }
                }
            }
            #[cfg(windows)]
            CtlStream::Windows(f) => {
                use std::os::windows::io::AsRawHandle;
                let mut avail: u32 = 0;
                // SAFETY: a valid pipe handle we own; a null buffer with zero
                // length is the documented query-only form of PeekNamedPipe.
                let ok = unsafe {
                    windows_sys::Win32::System::Pipes::PeekNamedPipe(
                        f.as_raw_handle() as _,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut avail,
                        std::ptr::null_mut(),
                    )
                };
                if ok != 0 {
                    return false;
                }
                // Only definitive hangup codes count as dead.
                const ERROR_BROKEN_PIPE: u32 = 109;
                const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
                const ERROR_INVALID_HANDLE: u32 = 6;
                matches!(
                    unsafe { windows_sys::Win32::Foundation::GetLastError() },
                    ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_INVALID_HANDLE
                )
            }
        }
    }
}

// ===================== Unix =====================

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    /// A server listener bound to `endpoint` (a socket path on Unix).
    pub struct CtlListener {
        inner: UnixListener,
        path: std::path::PathBuf,
    }

    impl CtlListener {
        pub fn bind(endpoint: &str) -> io::Result<Self> {
            // Clear a stale socket file from a previous (dead) instance.
            let _ = std::fs::remove_file(endpoint);
            if let Some(parent) = std::path::Path::new(endpoint).parent() {
                std::fs::create_dir_all(parent)?;
                // Best-effort 0700 on the parent dir.
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
            let inner = UnixListener::bind(endpoint)?;
            std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self {
                inner,
                path: endpoint.into(),
            })
        }

        pub fn accept(&self) -> io::Result<CtlStream> {
            let (stream, _addr) = self.inner.accept()?;
            Ok(CtlStream::Unix(stream))
        }
    }

    impl Drop for CtlListener {
        fn drop(&mut self) {
            // Remove the socket file so the registry never points at a dead one.
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Connect a client to `endpoint`. v2.27.0 (audit): retry briefly on a
    /// transient `ConnectionRefused` (the server may be mid-accept or swapping
    /// the socket) before giving up — mirroring the Windows named-pipe retry — so
    /// a transient failure doesn't make `client::discover` permanently prune a
    /// live server. `NotFound` (the socket file is gone) is definitive → bail at
    /// once so a truly-dead entry is still pruned promptly.
    pub fn connect(endpoint: &str) -> io::Result<CtlStream> {
        use std::os::unix::net::UnixStream;
        let mut last = None;
        for _ in 0..super::CONNECT_RETRIES {
            match UnixStream::connect(endpoint) {
                Ok(s) => return Ok(CtlStream::Unix(s)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(super::CONNECT_BACKOFF);
                }
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("unix socket connect failed")))
    }
}

// ===================== Windows =====================

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::Cell;
    use std::os::windows::io::FromRawHandle;
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    const PIPE_BUF: u32 = 64 * 1024;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Create one named-pipe instance for `name`. A protected DACL grants full
    /// access only to the object owner, SYSTEM, and administrators. `first` adds
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` so creating the FIRST
    /// instance FAILS (ERROR_ACCESS_DENIED) if the name is already taken — this
    /// is the squatting guard: a malicious local process that pre-created the
    /// pipe to intercept the server's clients cannot, because `bind` refuses to
    /// adopt an attacker-owned instance and surfaces the error instead.
    fn create_instance(name_w: &[u16], first: bool) -> io::Result<HANDLE> {
        let open_mode = PIPE_ACCESS_DUPLEX
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        let sddl = wide("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)");
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: SDDL is NUL-terminated and descriptor is a valid output
        // pointer. LocalFree below releases the returned allocation.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: `name_w` is a valid NUL-terminated wide string and attrs owns
        // a valid security descriptor for this call. The returned pipe handle
        // is owned by us.
        let h = unsafe {
            CreateNamedPipeW(
                name_w.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUF,
                PIPE_BUF,
                0,
                &attrs,
            )
        };
        unsafe {
            LocalFree(descriptor);
        }
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(h)
    }

    /// A server listener owning a pending named-pipe instance. There is always
    /// exactly one instance waiting for `ConnectNamedPipe`, so a client never
    /// races a window where no instance exists. `pending` uses `Cell` for the
    /// single-threaded interior mutability `accept(&self)` needs.
    pub struct CtlListener {
        name_w: Vec<u16>,
        pending: Cell<HANDLE>,
    }

    // The raw HANDLE is owned solely by this listener and never shared, so it is
    // sound to move the listener to the accept thread. (`Cell<HANDLE>` is not
    // `Send` by default because `HANDLE` is a raw pointer.)
    unsafe impl Send for CtlListener {}

    impl CtlListener {
        pub fn bind(endpoint: &str) -> io::Result<Self> {
            let name_w = wide(endpoint);
            // FIRST instance: fails if the name is squatted (the security guard).
            let pending = create_instance(&name_w, true)?;
            Ok(Self {
                name_w,
                pending: Cell::new(pending),
            })
        }

        pub fn accept(&self) -> io::Result<CtlStream> {
            // Retry-bounded so a transient ConnectNamedPipe failure on one
            // instance (e.g. a client that connected then vanished) tears that
            // instance down + recreates a fresh one instead of poisoning the
            // accept loop forever.
            for _ in 0..16 {
                // Ensure a pending instance exists (a prior accept may have left
                // none if creating the next one failed).
                let mut handle = self.pending.get();
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    handle = create_instance(&self.name_w, false)?;
                    self.pending.set(handle);
                }
                // Block until a client connects.
                // SAFETY: `handle` is a valid pipe handle we own.
                let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
                if connected == 0 {
                    let e = unsafe { GetLastError() };
                    // ERROR_PIPE_CONNECTED = a client connected between create +
                    // connect — success, not failure.
                    if e != ERROR_PIPE_CONNECTED {
                        // Poisoned instance: disconnect + close + recreate, retry.
                        unsafe {
                            DisconnectNamedPipe(handle);
                            CloseHandle(handle);
                        }
                        self.pending.set(create_instance(&self.name_w, false)?);
                        continue;
                    }
                }
                // Connected. Create the NEXT pending instance; if that fails,
                // STILL return this (valid) client and leave `pending` invalid so
                // the next accept() recreates it — don't abandon a live client.
                match create_instance(&self.name_w, false) {
                    Ok(next) => self.pending.set(next),
                    Err(_) => self.pending.set(INVALID_HANDLE_VALUE),
                }
                // SAFETY: we own `handle` and transfer ownership to the File.
                let file = unsafe { std::fs::File::from_raw_handle(handle as *mut _) };
                return Ok(CtlStream::Windows(file));
            }
            Err(io::Error::other("accept retries exhausted"))
        }
    }

    impl Drop for CtlListener {
        fn drop(&mut self) {
            let h = self.pending.get();
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                return; // no live pending instance (accept left none)
            }
            // SAFETY: the pending handle is one we own and no longer use.
            unsafe {
                CloseHandle(h);
            }
        }
    }

    /// Connect a client to `endpoint` (a `\\.\pipe\…` name). std opens a named
    /// pipe natively; retry briefly on a transient busy while the server swaps
    /// instances. Mirrors the Unix early-out: `NotFound` (the pipe doesn't
    /// exist) is definitive — a dead server — so bail at once rather than
    /// spinning the full retry budget (~1s) per dead registry entry, which
    /// would otherwise make `client::discover` hang scanning stale entries.
    pub fn connect(endpoint: &str) -> io::Result<CtlStream> {
        use std::fs::OpenOptions;
        let mut last = None;
        for _ in 0..super::CONNECT_RETRIES {
            match OpenOptions::new().read(true).write(true).open(endpoint) {
                Ok(f) => return Ok(CtlStream::Windows(f)),
                // A missing pipe = a dead server: no point retrying. (A pipe
                // that exists but is momentarily busy reports ERROR_PIPE_BUSY,
                // not NotFound, and is still retried below.)
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(super::CONNECT_BACKOFF);
                }
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("pipe connect failed")))
    }
}

pub use imp::{CtlListener, connect};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead as _;

    /// Loopback: bind a listener, connect a client, round-trip a line both
    /// ways. Runs on all three CI OSes (the Windows leg exercises the named
    /// pipe). Uses a unique endpoint derived from the pid (no clock/rand).
    #[test]
    fn loopback_round_trip() {
        let pid = std::process::id();
        #[cfg(unix)]
        let endpoint = std::env::temp_dir()
            .join(format!("kettle-ctl-test-{pid}.sock"))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\kettle-ctl-test-{pid}");

        let listener = CtlListener::bind(&endpoint).expect("bind");
        let ep = endpoint.clone();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            // Echo one line back, prefixed.
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).expect("read");
            let got = String::from_utf8_lossy(&buf[..n]).into_owned();
            conn.write_all(format!("echo:{got}").as_bytes())
                .expect("write");
            conn.flush().ok();
        });

        // Give the listener a beat to be ready (Unix bind is sync; Windows the
        // first instance exists after bind()).
        let mut client = connect(&ep).expect("connect");
        client.write_all(b"ping\n").expect("client write");
        client.flush().ok();
        let mut reply = [0u8; 64];
        let n = client.read(&mut reply).expect("client read");
        let reply = String::from_utf8_lossy(&reply[..n]);
        assert!(reply.starts_with("echo:ping"), "got: {reply:?}");
        server.join().expect("server thread");
    }

    /// A connect to an endpoint that does not exist must fail FAST (a missing
    /// socket / pipe = a dead server), not spin the full retry budget. Both
    /// platform impls early-out on `NotFound`; assert the call returns well
    /// under the worst-case `CONNECT_RETRIES * CONNECT_BACKOFF` budget so a
    /// stale registry entry can't hang `client::discover`.
    #[test]
    fn connect_to_missing_endpoint_fails_fast() {
        let pid = std::process::id();
        #[cfg(unix)]
        let endpoint = std::env::temp_dir()
            .join(format!("kettle-ctl-absent-{pid}.sock"))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\kettle-ctl-absent-{pid}");

        let start = std::time::Instant::now();
        let res = connect(&endpoint);
        let elapsed = start.elapsed();
        // Match on the Err directly — CtlStream (the Ok type) isn't Debug, so
        // unwrap_err / is_err+assert_eq would require it.
        match res {
            Err(e) => assert_eq!(
                e.kind(),
                io::ErrorKind::NotFound,
                "a missing endpoint surfaces NotFound"
            ),
            Ok(_) => panic!("connecting to a missing endpoint must fail"),
        }
        // The full retry budget would be CONNECT_RETRIES * CONNECT_BACKOFF
        // (~1s); the early-out should be near-instant. Allow generous slack for
        // a loaded CI box but well below even a single backoff iteration's
        // worth of the full budget.
        let budget = CONNECT_RETRIES * CONNECT_BACKOFF;
        assert!(
            elapsed < budget / 2,
            "early-out took {elapsed:?}, expected well under {:?}",
            budget / 2
        );
    }

    /// The REAL server/client usage: each side splits its connection into a
    /// reader thread + a writer thread over `try_clone`d handles, with
    /// concurrent read+write. This is what the control server does; it caught a
    /// Windows named-pipe ERROR_NO_DATA where writing on one split handle while
    /// reading on the other closed the pipe. Pins the split pattern works.
    #[test]
    fn split_handle_concurrent_read_write() {
        let pid = std::process::id();
        #[cfg(unix)]
        let endpoint = std::env::temp_dir()
            .join(format!("kettle-ctl-split-{pid}.sock"))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\kettle-ctl-split-{pid}");

        let listener = CtlListener::bind(&endpoint).expect("bind");
        // Connect the client FIRST — before the server calls accept()
        // (ConnectNamedPipe). On Windows this exercises the ERROR_PIPE_CONNECTED
        // path (client connected to an instance that exists from bind() but has
        // no pending ConnectNamedPipe yet) — the real-world timing a fast
        // `kettle ctl` hits, and the suspected ERROR_NO_DATA trigger.
        let stream = connect(&endpoint).expect("connect");
        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept");
            // Mimic the real accept loop: a background thread blocks accepting
            // the NEXT connection (ConnectNamedPipe on instance #2) while we
            // serve conn #1.
            let next_accept = std::thread::spawn(move || {
                let _ = listener.accept();
            });
            // Split: reader thread reads a request line; writer (this thread)
            // writes a response on the OTHER handle while the reader blocks.
            let read_half = conn.try_clone().expect("clone");
            let mut writer = conn;
            let (gtx, grx) = std::sync::mpsc::channel::<String>();
            let rh = std::thread::spawn(move || {
                let mut r = std::io::BufReader::new(read_half);
                let mut line = String::new();
                r.read_line(&mut line).expect("read");
                gtx.send(line.trim_end().to_string()).ok();
            });
            let got = grx.recv().expect("req");
            writer
                .write_all(format!("resp:{got}\n").as_bytes())
                .expect("write resp");
            writer.flush().expect("flush resp");
            rh.join().ok();
            // Don't join next_accept (it blocks forever waiting for conn #2);
            // detach it — the listener drop will tear it down.
            drop(next_accept);
        });

        let read_half = stream.try_clone().expect("client clone");
        let mut writer = stream;
        let mut reader = std::io::BufReader::new(read_half);
        writer.write_all(b"req-1\n").expect("client write");
        writer.flush().ok();
        let mut resp = String::new();
        reader.read_line(&mut resp).expect("client read");
        assert_eq!(resp.trim_end(), "resp:req-1", "got: {resp:?}");
        server.join().expect("server thread");
    }
}
