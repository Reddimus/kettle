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

    /// Connect a client to `endpoint`.
    pub fn connect(endpoint: &str) -> io::Result<CtlStream> {
        Ok(CtlStream::Unix(std::os::unix::net::UnixStream::connect(
            endpoint,
        )?))
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
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    const PIPE_BUF: u32 = 64 * 1024;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Create one named-pipe instance for `name`. Default security (null SA) =
    /// creator/owner + admins, matching the documented same-local-user threat
    /// model.
    fn create_instance(name_w: &[u16]) -> io::Result<HANDLE> {
        // SAFETY: `name_w` is a valid NUL-terminated wide string; all other
        // args are constants. The returned handle is owned by us.
        let h = unsafe {
            CreateNamedPipeW(
                name_w.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUF,
                PIPE_BUF,
                0,
                ptr::null(),
            )
        };
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
            let pending = create_instance(&name_w)?;
            Ok(Self {
                name_w,
                pending: Cell::new(pending),
            })
        }

        pub fn accept(&self) -> io::Result<CtlStream> {
            let handle = self.pending.get();
            // Block until a client connects to the pending instance.
            // SAFETY: `handle` is a valid pipe handle we own.
            let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
            // ConnectNamedPipe returns 0 with ERROR_PIPE_CONNECTED when a client
            // connected between create + connect — that is success, not failure.
            if connected == 0 {
                let e = unsafe { GetLastError() };
                if e != ERROR_PIPE_CONNECTED {
                    return Err(io::Error::from_raw_os_error(e as i32));
                }
            }
            // Create the NEXT pending instance so the listener always has one
            // waiting, then hand the connected handle to a File.
            let next = create_instance(&self.name_w)?;
            self.pending.set(next);
            // SAFETY: we own `handle` and transfer ownership to the File.
            let file = unsafe { std::fs::File::from_raw_handle(handle as *mut _) };
            Ok(CtlStream::Windows(file))
        }
    }

    impl Drop for CtlListener {
        fn drop(&mut self) {
            // SAFETY: the pending handle is one we own and no longer use.
            unsafe {
                CloseHandle(self.pending.get());
            }
        }
    }

    /// Connect a client to `endpoint` (a `\\.\pipe\…` name). std opens a named
    /// pipe natively; retry briefly on a transient busy/not-found while the
    /// server swaps instances.
    pub fn connect(endpoint: &str) -> io::Result<CtlStream> {
        use std::fs::OpenOptions;
        let mut last = None;
        for _ in 0..50 {
            match OpenOptions::new().read(true).write(true).open(endpoint) {
                Ok(f) => return Ok(CtlStream::Windows(f)),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(20));
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
