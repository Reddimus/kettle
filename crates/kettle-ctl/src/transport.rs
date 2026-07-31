//! The local-IPC transport.
//!
//! A blocking, thread-per-connection transport with the same surface on every
//! OS: a [`CtlListener`] that `accept()`s [`CtlStream`]s (server), and
//! [`connect`] (client). No tokio — the server runs an accept thread + a
//! reader/writer thread per connection, matching kettle's thread-based model.
//!
//! - **Unix:** a `UnixListener` at a filesystem path, mode `0600`.
//! - **Windows:** a byte-mode named pipe (`\\.\pipe\kettle-ctl-<pid>`) created
//!   with `CreateNamedPipeW` (protected owner/SYSTEM/admin DACL) and
//!   `PIPE_UNLIMITED_INSTANCES`. Both accepted server handles and client
//!   handles use overlapped I/O so blocked writes can honor deadlines and
//!   cancellation.
//!
//! Hand-rolled over the `interprocess` crate (supply-chain leanness; the needed
//! subset is small and matches the windows-sys precedent in the bin crate).

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
#[doc(hidden)]
pub struct UnixStream {
    stream: std::os::unix::net::UnixStream,
    write_gate: std::sync::Arc<std::sync::Mutex<()>>,
}

#[cfg(unix)]
impl UnixStream {
    fn new(stream: std::os::unix::net::UnixStream) -> io::Result<Self> {
        // Keep the shared open-file description nonblocking for its entire
        // lifetime. Read/Write below restore blocking semantics in userspace;
        // unlike a temporary fcntl toggle, a clone can never observe a
        // transient mode change.
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            write_gate: std::sync::Arc::new(std::sync::Mutex::new(())),
        })
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            write_gate: self.write_gate.clone(),
        })
    }
}

#[cfg(windows)]
#[doc(hidden)]
pub struct WindowsStream {
    file: std::fs::File,
    server_end: bool,
}

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
    Unix(UnixStream),
    #[cfg(windows)]
    Windows(WindowsStream),
}

impl Read for CtlStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(stream) => unix_blocking_read(&stream.stream, buf),
            #[cfg(windows)]
            CtlStream::Windows(stream) => windows_io::read(&stream.file, buf),
        }
    }
}

impl Write for CtlStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(stream) => {
                let _gate = stream
                    .write_gate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                unix_blocking_write(&stream.stream, buf)
            }
            #[cfg(windows)]
            CtlStream::Windows(stream) => windows_io::write(&stream.file, buf, None, None),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            CtlStream::Unix(stream) => {
                let mut socket = &stream.stream;
                socket.flush()
            }
            #[cfg(windows)]
            CtlStream::Windows(_) => {
                // A named pipe is a byte stream, not durable storage.
                // FlushFileBuffers on its server end waits until the client
                // drains every buffered byte, bypassing all write deadlines.
                Ok(())
            }
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
            CtlStream::Windows(stream) => Ok(CtlStream::Windows(WindowsStream {
                file: stream.file.try_clone()?,
                server_end: stream.server_end,
            })),
        }
    }

    /// Write an entire protocol frame without allowing a blocked local peer to
    /// outlive the request's deadline. On Windows, both accepted and client
    /// pipe handles use overlapped I/O so cancellation can target the exact
    /// pending operation.
    /// Unix connections are nonblocking for their entire lifetime, with
    /// blocking-compatible `Read`/`Write` wrappers for ordinary callers. A
    /// connection-wide gate keeps complete bounded writes serialized across
    /// [`try_clone`](CtlStream::try_clone) siblings without toggling flags on
    /// their shared open-file description. This retains the fd-level
    /// nonblocking behavior macOS AF_UNIX requires.
    pub fn write_all_until(
        &mut self,
        mut buf: &[u8],
        deadline: Instant,
        cancelled: Option<&AtomicBool>,
    ) -> io::Result<()> {
        #[cfg(unix)]
        let write_gate = {
            let CtlStream::Unix(stream) = &*self;
            stream.write_gate.clone()
        };
        #[cfg(unix)]
        let write_guard = lock_write_gate_until(&write_gate, deadline, cancelled)?;

        let result = (|| {
            while !buf.is_empty() {
                check_write_state(deadline, cancelled)?;
                let written = match self {
                    #[cfg(unix)]
                    CtlStream::Unix(stream) => {
                        use std::os::fd::AsRawFd as _;

                        loop {
                            let flags = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
                            // SAFETY: the stream owns this fd and `buf` is readable
                            // for its full length during the call.
                            let result = unsafe {
                                libc::send(
                                    stream.stream.as_raw_fd(),
                                    buf.as_ptr().cast(),
                                    buf.len(),
                                    flags,
                                )
                            };
                            if result >= 0 {
                                break result as usize;
                            }
                            let error = io::Error::last_os_error();
                            match error.kind() {
                                io::ErrorKind::Interrupted => continue,
                                io::ErrorKind::WouldBlock => {
                                    wait_unix_writable(&stream.stream, deadline, cancelled)?;
                                }
                                _ => return Err(error),
                            }
                        }
                    }
                    #[cfg(windows)]
                    CtlStream::Windows(stream) => {
                        windows_io::write(&stream.file, buf, Some(deadline), cancelled)?
                    }
                };
                if written == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write the control frame",
                    ));
                }
                buf = &buf[written..];
            }
            Ok(())
        })();

        #[cfg(unix)]
        drop(write_guard);
        result
    }

    /// Wait until at least one byte can be read, the peer closes, or `timeout`
    /// elapses. This is deliberately a readiness primitive rather than a
    /// socket read timeout: the Windows transport is a named-pipe `File`, and
    /// both client implementations need the same bounded-frame behavior.
    pub fn wait_readable(&self, timeout: std::time::Duration) -> io::Result<bool> {
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
                        fd: stream.stream.as_raw_fd(),
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
            CtlStream::Windows(stream) => {
                use std::os::windows::io::AsRawHandle as _;

                let deadline = std::time::Instant::now() + timeout;
                loop {
                    let mut available = 0u32;
                    // SAFETY: the file owns a valid named-pipe handle; the
                    // null buffer form queries available bytes without reading.
                    let ok = unsafe {
                        windows_sys::Win32::System::Pipes::PeekNamedPipe(
                            stream.file.as_raw_handle() as _,
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
    /// as this process. Filesystem permissions (Unix mode bits) / the pipe DACL
    /// remain the first boundary; peer credentials close the race where a
    /// socket path is inherited or passed to another local account, AND — on
    /// Windows — the fact that the protected pipe DACL also admits the whole
    /// Builtin-Administrators group for recovery, not only the process owner. Windows'
    /// accepted-side check resolves the connected client's PID at the kernel
    /// level and compares primary-token user SIDs. The client-side check reads
    /// the pipe object's owner SID before any protocol bytes are sent. Pipe
    /// instances are created with this process's exact token-user SID as owner,
    /// including from an elevated process where Windows would otherwise use the
    /// Administrators group as the default owner.
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
                        stream.stream.as_raw_fd(),
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
                let rc = unsafe { libc::getpeereid(stream.stream.as_raw_fd(), &mut uid, &mut gid) };
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
            CtlStream::Unix(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "peer credentials are unavailable on this Unix target",
            )),
            #[cfg(windows)]
            CtlStream::Windows(stream) if stream.server_end => {
                windows_security::client_is_same_user(&stream.file)
            }
            #[cfg(windows)]
            CtlStream::Windows(stream) => {
                windows_security::pipe_owned_by_current_user(&stream.file)
            }
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
                        s.stream.as_raw_fd(),
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
            CtlStream::Windows(stream) => {
                use std::os::windows::io::AsRawHandle;
                let mut avail: u32 = 0;
                // SAFETY: a valid pipe handle we own; a null buffer with zero
                // length is the documented query-only form of PeekNamedPipe.
                let ok = unsafe {
                    windows_sys::Win32::System::Pipes::PeekNamedPipe(
                        stream.file.as_raw_handle() as _,
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

fn check_write_state(deadline: Instant, cancelled: Option<&AtomicBool>) -> io::Result<()> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "control write was cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "control write timed out",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn lock_write_gate_until<'a>(
    gate: &'a std::sync::Mutex<()>,
    deadline: Instant,
    cancelled: Option<&AtomicBool>,
) -> io::Result<std::sync::MutexGuard<'a, ()>> {
    use std::sync::TryLockError;

    loop {
        check_write_state(deadline, cancelled)?;
        match gate.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => {
                // Concurrent complete-frame writes are rare. A short bounded
                // sleep avoids a hot spin while preserving the caller's
                // deadline and prompt cancellation semantics.
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}

#[cfg(unix)]
fn unix_blocking_read(
    stream: &std::os::unix::net::UnixStream,
    buf: &mut [u8],
) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        let mut socket = stream;
        match socket.read(buf) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_unix_event(stream, libc::POLLIN)?;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn unix_blocking_write(stream: &std::os::unix::net::UnixStream, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        let mut socket = stream;
        match socket.write(buf) {
            Ok(written) => return Ok(written),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_unix_event(stream, libc::POLLOUT)?;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn wait_unix_event(
    stream: &std::os::unix::net::UnixStream,
    events: libc::c_short,
) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    loop {
        let mut poll_fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: `poll_fd` is a valid one-element array and the stream owns
        // its fd for the duration of the call. A negative timeout blocks like
        // the public std::io Read/Write contract these wrappers preserve.
        let result = unsafe { libc::poll(&mut poll_fd, 1, -1) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn wait_unix_writable(
    stream: &std::os::unix::net::UnixStream,
    deadline: Instant,
    cancelled: Option<&AtomicBool>,
) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    loop {
        check_write_state(deadline, cancelled)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = if cancelled.is_some() {
            remaining.min(Duration::from_millis(50))
        } else {
            remaining
        };
        let millis = duration_millis_ceil(wait).min(i32::MAX as u128) as i32;
        let mut poll_fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `poll_fd` is a valid one-element array and the stream owns
        // its fd for the duration of the call.
        let result = unsafe { libc::poll(&mut poll_fd, 1, millis) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn duration_millis_ceil(duration: Duration) -> u128 {
    duration
        .as_nanos()
        .saturating_add(999_999)
        .saturating_div(1_000_000)
}

#[cfg(windows)]
mod windows_io {
    use super::*;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_NOT_CONNECTED, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;
    use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

    struct Event(HANDLE);

    impl Event {
        fn new() -> io::Result<Self> {
            // SAFETY: null security attributes/name create an unnamed,
            // non-inheritable event owned by this process.
            let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(handle))
            }
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            // SAFETY: Event exclusively owns this valid handle.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn connect(handle: HANDLE) -> io::Result<()> {
        let event = Event::new()?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: `handle` is an overlapped server pipe and `overlapped` plus
        // its event remain alive through completion.
        let connected = unsafe { ConnectNamedPipe(handle, &mut overlapped) };
        if connected != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            // The client won the CreateNamedPipe -> ConnectNamedPipe race.
            Some(code) if code == ERROR_PIPE_CONNECTED as i32 => return Ok(()),
            Some(code) if code == ERROR_IO_PENDING as i32 => {}
            _ => return Err(error),
        }
        match unsafe { WaitForSingleObject(event.0, INFINITE) } {
            WAIT_OBJECT_0 => {
                completed_result(handle, &mut overlapped, false)?;
                Ok(())
            }
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!(
                "unexpected overlapped connect wait result {other}"
            ))),
        }
    }

    pub(super) fn read(file: &std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len().min(u32::MAX as usize) as u32;
        let result = run(file, None, None, |handle, overlapped| {
            // SAFETY: buffer and OVERLAPPED remain alive until `run` observes
            // completion, including after cancellation.
            unsafe {
                ReadFile(
                    handle,
                    buf.as_mut_ptr(),
                    len,
                    std::ptr::null_mut(),
                    overlapped,
                )
            }
        });
        match result {
            // Named pipes report an orderly peer close as a Win32 error. At
            // the Read boundary this is EOF, matching synchronous File reads
            // and the cross-platform `Read::read` contract.
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code)
                        if code == ERROR_BROKEN_PIPE as i32
                            || code == ERROR_PIPE_NOT_CONNECTED as i32
                ) =>
            {
                Ok(0)
            }
            result => result,
        }
    }

    pub(super) fn write(
        file: &std::fs::File,
        buf: &[u8],
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len().min(u32::MAX as usize) as u32;
        run(file, deadline, cancelled, |handle, overlapped| {
            // SAFETY: buffer and OVERLAPPED remain alive until `run` observes
            // completion, including after cancellation.
            unsafe { WriteFile(handle, buf.as_ptr(), len, std::ptr::null_mut(), overlapped) }
        })
    }

    fn run(
        file: &std::fs::File,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
        start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        let event = Event::new()?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        let handle = file.as_raw_handle() as HANDLE;
        let started = start(handle, &mut overlapped);
        if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
        }

        if started != 0 {
            return completed_result(handle, &mut overlapped, false);
        }

        let stopped = loop {
            let reason = stop_reason(deadline, cancelled);
            if let Some(reason) = reason {
                // A completion can race this cancellation. Either way, wait
                // for the kernel to stop touching OVERLAPPED before it drops.
                // ERROR_NOT_FOUND means the operation already completed.
                let cancelled_ok = unsafe { CancelIoEx(handle, &overlapped) };
                if cancelled_ok == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                        break Err(error);
                    }
                }
                break Err(reason);
            }

            let wait_ms = match deadline {
                None if cancelled.is_none() => INFINITE,
                Some(deadline) => duration_millis(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                ),
                None => 50,
            };
            match unsafe { WaitForSingleObject(event.0, wait_ms) } {
                WAIT_OBJECT_0 => break Ok(()),
                WAIT_TIMEOUT => continue,
                WAIT_FAILED => break Err(io::Error::last_os_error()),
                other => {
                    break Err(io::Error::other(format!(
                        "unexpected overlapped wait result {}",
                        other
                    )));
                }
            }
        };

        if let Err(error) = stopped {
            // Even when waiting itself fails, attempt cancellation and then
            // block until the kernel has stopped referencing `overlapped`.
            // Returning before this drain would leave kernel I/O pointing at
            // stack storage that is about to be freed.
            unsafe { CancelIoEx(handle, &overlapped) };
            let _ = completed_result(handle, &mut overlapped, true);
            return Err(error);
        }
        completed_result(handle, &mut overlapped, false)
    }

    fn completed_result(
        handle: HANDLE,
        overlapped: &mut OVERLAPPED,
        wait: bool,
    ) -> io::Result<usize> {
        let mut transferred = 0u32;
        // SAFETY: the handle and OVERLAPPED belong to the active operation;
        // callers keep all referenced buffers alive through this call.
        let completed =
            unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, i32::from(wait)) };
        if completed == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(transferred as usize)
        }
    }

    fn stop_reason(deadline: Option<Instant>, cancelled: Option<&AtomicBool>) -> Option<io::Error> {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Some(io::Error::new(
                io::ErrorKind::Interrupted,
                "control write was cancelled",
            ));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Some(io::Error::new(
                io::ErrorKind::TimedOut,
                "control write timed out",
            ));
        }
        None
    }

    fn duration_millis(duration: Duration) -> u32 {
        duration_millis_ceil(duration).clamp(1, u32::MAX as u128) as u32
    }
}

#[cfg(windows)]
mod windows_security {
    use super::*;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: this value exclusively owns the valid kernel handle.
            unsafe { CloseHandle(self.0) };
        }
    }

    struct TokenUserSid {
        _buffer: Vec<u64>,
        sid: PSID,
    }

    pub(super) struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl LocalSecurityDescriptor {
        pub(super) fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
            self.0
        }
    }

    impl Drop for LocalSecurityDescriptor {
        fn drop(&mut self) {
            // SAFETY: Windows allocated this descriptor with LocalAlloc.
            unsafe { LocalFree(self.0.cast()) };
        }
    }

    fn token_user_sid(process: HANDLE) -> io::Result<TokenUserSid> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is a valid process or pseudo-handle and `token` is
        // a valid output pointer.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle(token);

        let mut len = 0u32;
        // SAFETY: this is the documented size-query form.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buffer = vec![0u64; words];
        // SAFETY: the aligned buffer contains at least `len` writable bytes.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful TokenUser query populated a TOKEN_USER header
        // whose SID points into `buffer`, retained by TokenUserSid.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        Ok(TokenUserSid {
            _buffer: buffer,
            sid,
        })
    }

    fn current_user_sid() -> io::Result<TokenUserSid> {
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle.
        token_user_sid(unsafe { GetCurrentProcess() })
    }

    fn process_is_current_user(process: HANDLE) -> io::Result<bool> {
        let peer = token_user_sid(process)?;
        let current = current_user_sid()?;
        // SAFETY: both SID pointers remain anchored in live buffers.
        Ok(unsafe { EqualSid(peer.sid, current.sid) } != 0)
    }

    /// Build a protected named-pipe descriptor whose owner is the exact token
    /// user, not the token's possibly group-valued default owner. This keeps an
    /// elevated Kettle interoperable with its unelevated same-user clients
    /// without making "owned by Administrators" acceptable provenance.
    pub(super) fn pipe_security_descriptor() -> io::Result<LocalSecurityDescriptor> {
        struct LocalString(windows_sys::core::PWSTR);

        impl Drop for LocalString {
            fn drop(&mut self) {
                // SAFETY: ConvertSidToStringSidW allocated this with LocalAlloc.
                unsafe { LocalFree(self.0.cast()) };
            }
        }

        let current = current_user_sid()?;
        let mut sid_string = std::ptr::null_mut();
        // SAFETY: `current.sid` is valid and sid_string is an output pointer.
        if unsafe { ConvertSidToStringSidW(current.sid, &mut sid_string) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid_string = LocalString(sid_string);
        let len = unsafe {
            let mut len = 0usize;
            while *sid_string.0.add(len) != 0 {
                len += 1;
            }
            len
        };
        // SAFETY: the API returned a NUL-terminated UTF-16 SID string.
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_string.0, len) })
            .map_err(io::Error::other)?;
        let sddl = format!("O:{sid}D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)");
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and descriptor is a valid output.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalSecurityDescriptor(descriptor))
    }

    pub(super) fn client_is_same_user(file: &std::fs::File) -> io::Result<bool> {
        let mut client_pid = 0u32;
        // SAFETY: `file` owns a connected named-pipe server handle.
        if unsafe { GetNamedPipeClientProcessId(file.as_raw_handle() as HANDLE, &mut client_pid) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the PID came from the pipe kernel object.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, client_pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle(process);
        process_is_current_user(process.0)
    }

    pub(super) fn pipe_owned_by_current_user(file: &std::fs::File) -> io::Result<bool> {
        let mut owner: PSID = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the client handle is valid and all output pointers live for
        // the call. Named-pipe security descriptors are kernel-object security.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let descriptor = LocalSecurityDescriptor(descriptor);
        let current = current_user_sid()?;
        let matches = !owner.is_null() && unsafe { EqualSid(owner, current.sid) } != 0;
        drop(descriptor);
        Ok(matches)
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
            Ok(CtlStream::Unix(UnixStream::new(stream)?))
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
                Ok(s) => return Ok(CtlStream::Unix(super::UnixStream::new(s)?)),
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
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    const PIPE_BUF: u32 = 64 * 1024;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Create one overlapped named-pipe instance for `name`. Its exact owner is
    /// the creator's token-user SID and a protected DACL grants full access to
    /// that owner, SYSTEM, and administrators. `first` adds
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` so creating the FIRST
    /// instance FAILS (ERROR_ACCESS_DENIED) if the name is already taken — this
    /// is the squatting guard: a malicious local process that pre-created the
    /// pipe to intercept the server's clients cannot, because `bind` refuses to
    /// adopt an attacker-owned instance and surfaces the error instead.
    fn create_instance(name_w: &[u16], first: bool) -> io::Result<HANDLE> {
        let open_mode = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        let descriptor = super::windows_security::pipe_security_descriptor()?;
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_ptr(),
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
                // Wait through an explicit OVERLAPPED operation. Passing null
                // here on an overlapped handle can falsely report completion.
                if super::windows_io::connect(handle).is_err() {
                    // Poisoned instance: disconnect + close + recreate, retry.
                    unsafe {
                        DisconnectNamedPipe(handle);
                        CloseHandle(handle);
                    }
                    self.pending.set(create_instance(&self.name_w, false)?);
                    continue;
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
                return Ok(CtlStream::Windows(WindowsStream {
                    file,
                    server_end: true,
                }));
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
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;

        let mut last = None;
        for _ in 0..super::CONNECT_RETRIES {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OVERLAPPED)
                .open(endpoint)
            {
                Ok(file) => {
                    return Ok(CtlStream::Windows(WindowsStream {
                        file,
                        server_end: false,
                    }));
                }
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

pub use imp::CtlListener;

fn authenticate_connected(
    stream: CtlStream,
    verify: impl FnOnce(&CtlStream) -> io::Result<bool>,
) -> io::Result<CtlStream> {
    match verify(&stream)? {
        true => Ok(stream),
        false => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control endpoint is not owned by the current user",
        )),
    }
}

/// Connect to a local endpoint and authenticate its server before returning a
/// stream to protocol code. Unix compares peer credentials; Windows verifies
/// the pipe object's exact token-user owner SID. No request bytes are sent when
/// this check fails.
pub fn connect(endpoint: &str) -> io::Result<CtlStream> {
    let stream = imp::connect(endpoint)?;
    authenticate_connected(stream, CtlStream::peer_is_same_user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead as _;

    #[test]
    fn millisecond_deadlines_round_up_only_sub_millisecond_remainders() {
        assert_eq!(duration_millis_ceil(Duration::ZERO), 0);
        assert_eq!(duration_millis_ceil(Duration::from_nanos(1)), 1);
        assert_eq!(duration_millis_ceil(Duration::from_millis(1)), 1);
        assert_eq!(duration_millis_ceil(Duration::from_micros(1_001)), 2);
        assert_eq!(duration_millis_ceil(Duration::from_millis(1_500)), 1_500);
    }

    /// The shared open-file description stays nonblocking for its full
    /// lifetime, while the public Read API still blocks until data arrives.
    /// This is the clone-safe macOS contract: no bounded write ever toggles a
    /// flag underneath a sibling reader.
    #[cfg(unix)]
    #[test]
    fn unix_stream_mode_is_stable_while_clone_read_remains_blocking() {
        use std::os::fd::AsRawFd as _;

        fn status_flags(stream: &UnixStream) -> libc::c_int {
            // SAFETY: the stream owns this valid fd for the duration of the call.
            let flags = unsafe { libc::fcntl(stream.stream.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0, "read socket status flags");
            flags
        }

        let (stream, mut peer) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let stream = UnixStream::new(stream).expect("wrap stream");
        assert_ne!(
            status_flags(&stream) & libc::O_NONBLOCK,
            0,
            "wrapped connection remains nonblocking at the fd level"
        );

        let mut ctl = CtlStream::Unix(stream);
        let mut reader = ctl.try_clone().expect("clone");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let mut reply = [0_u8; 5];
            done_tx.send(reader.read_exact(&mut reply).map(|()| reply))
        });
        assert!(
            done_rx.recv_timeout(Duration::from_millis(30)).is_err(),
            "blocking-compatible reader returned before data was available"
        );
        peer.write_all(b"reply").expect("write response");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("reader result")
                .expect("reader success"),
            *b"reply"
        );
        reader_thread
            .join()
            .expect("reader thread")
            .expect("send result");

        ctl.write_all_until(b"probe", Instant::now() + Duration::from_secs(1), None)
            .expect("write should succeed on a fresh, empty-buffer socket pair");
        let CtlStream::Unix(stream) = &ctl;
        assert_ne!(
            status_flags(stream) & libc::O_NONBLOCK,
            0,
            "bounded writes must not change the connection's stable mode"
        );
        let mut probe = [0_u8; 5];
        peer.read_exact(&mut probe).expect("read probe");
        assert_eq!(&probe, b"probe");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_writes_from_clones_are_serialized_as_complete_frames() {
        use std::os::fd::AsRawFd as _;

        let (stream, mut peer) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let stream = UnixStream::new(stream).expect("wrap stream");
        let bytes: libc::c_int = 4 * 1024;
        // SAFETY: the stream owns this valid fd and `bytes` is a correctly
        // sized SO_SNDBUF value. A small buffer forces each frame through
        // multiple send/poll iterations, exposing interleaving without a gate.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    stream.stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    std::ptr::addr_of!(bytes).cast(),
                    std::mem::size_of_val(&bytes) as libc::socklen_t,
                )
            },
            0
        );
        let first = CtlStream::Unix(stream);
        let second = first.try_clone().expect("clone");
        let frame_len = 256 * 1024;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let run_writer =
            move |mut stream: CtlStream, byte: u8, barrier: std::sync::Arc<std::sync::Barrier>| {
                std::thread::spawn(move || {
                    let frame = vec![byte; frame_len];
                    barrier.wait();
                    stream
                        .write_all_until(&frame, Instant::now() + Duration::from_secs(5), None)
                        .expect("write complete frame");
                })
            };
        let writer_a = run_writer(first, b'a', barrier.clone());
        let writer_b = run_writer(second, b'b', barrier.clone());
        barrier.wait();
        let mut received = vec![0_u8; frame_len * 2];
        peer.read_exact(&mut received).expect("read both frames");
        writer_a.join().expect("writer a");
        writer_b.join().expect("writer b");

        let split = received
            .windows(2)
            .position(|pair| pair[0] != pair[1])
            .map_or(received.len(), |index| index + 1);
        assert_eq!(split, frame_len, "exactly one complete frame comes first");
        let a_then_b = received[..split].iter().all(|&byte| byte == b'a')
            && received[split..].iter().all(|&byte| byte == b'b');
        let b_then_a = received[..split].iter().all(|&byte| byte == b'b')
            && received[split..].iter().all(|&byte| byte == b'a');
        assert!(
            a_then_b || b_then_a,
            "clone writers must not interleave frame bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_write_waiting_for_clone_gate_observes_deadline_and_cancellation() {
        let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let stream = UnixStream::new(stream).expect("wrap stream");
        let gate = stream.write_gate.clone();
        let _held = gate.lock().expect("hold clone writer gate");
        let mut ctl = CtlStream::Unix(stream);

        let started = Instant::now();
        let error = ctl
            .write_all_until(b"frame", started + Duration::from_millis(30), None)
            .expect_err("gate wait must observe deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "gate wait outlived its deadline: {:?}",
            started.elapsed()
        );

        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let setter = cancelled.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let error = ctl
            .write_all_until(b"frame", started + Duration::from_secs(5), Some(&cancelled))
            .expect_err("gate wait must observe cancellation");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "gate wait ignored cancellation: {:?}",
            started.elapsed()
        );
        canceller.join().expect("canceller thread");
    }

    fn test_endpoint(tag: &str) -> String {
        let pid = std::process::id();
        #[cfg(unix)]
        return std::env::temp_dir()
            .join(format!("kettle-ctl-{tag}-{pid}.sock"))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        return format!(r"\\.\pipe\kettle-ctl-{tag}-{pid}");
    }

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

    /// Audit fix: on Windows `peer_is_same_user()` used to unconditionally
    /// return `Ok(true)` — a no-op that never actually checked anything.
    /// Exercise the real kernel path (PID resolution + token SID compare on
    /// Windows, `SO_PEERCRED`/`getpeereid` on Unix) end to end: a peer
    /// connecting from THIS SAME PROCESS is, on every supported OS, the same
    /// user, so both the server-accepted stream and the connecting client
    /// stream must report `true`. This would not by itself have caught the
    /// audited stub (an always-`Ok(true)` stub also passes this assertion),
    /// but it does pin that the real implementation's kernel calls succeed
    /// and resolve to the expected answer rather than erroring or panicking.
    #[test]
    fn peer_is_same_user_reports_true_for_local_loopback() {
        let endpoint = test_endpoint("peer-same-user");
        let listener = CtlListener::bind(&endpoint).expect("bind");
        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept");
            conn.peer_is_same_user().expect("check server-side peer")
        });
        let client = connect(&endpoint).expect("connect");
        assert!(
            client.peer_is_same_user().expect("check client-side peer"),
            "connecting from this same process must report the same user"
        );
        assert!(
            server.join().expect("server thread"),
            "accepting from this same process must report the same user"
        );
    }

    #[test]
    fn client_authentication_rejects_before_protocol_bytes_are_sent() {
        let endpoint = test_endpoint("peer-rejected");
        let listener = CtlListener::bind(&endpoint).expect("bind");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            accepted_tx.send(()).expect("signal accepted peer");
            let mut byte = [0_u8; 1];
            match conn.read(&mut byte) {
                Ok(0) => true,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    true
                }
                Ok(_) | Err(_) => false,
            }
        });

        // Use the raw platform connector so this test can inject a failed
        // kernel identity decision deterministically on every CI user/OS.
        let stream = imp::connect(&endpoint).expect("raw connect");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server accepted peer before rejection");
        let result = authenticate_connected(stream, |_| Ok(false));
        match result {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied),
            Ok(_) => panic!("a failed server identity check must reject the stream"),
        }
        assert!(
            server.join().expect("server thread"),
            "server received protocol data before client authentication"
        );
    }

    /// Exercise the exact arm implicated by the audit: an accepted server
    /// handle writing more than the pipe/socket buffer to a client that never
    /// reads. Both a deadline and cancellation must interrupt the pending I/O.
    #[test]
    fn accepted_server_write_observes_deadline_and_cancellation() {
        fn run(
            tag: &str,
            cancelled: Option<std::sync::Arc<AtomicBool>>,
        ) -> (io::ErrorKind, Duration) {
            let endpoint = test_endpoint(tag);
            let listener = CtlListener::bind(&endpoint).expect("bind");
            let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let server_cancelled = cancelled.clone();
            let server = std::thread::spawn(move || {
                let mut conn = listener.accept().expect("accept");
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd as _;

                    let CtlStream::Unix(socket) = &conn;
                    let bytes: libc::c_int = 4 * 1024;
                    // SAFETY: `socket` owns this valid fd and `bytes` is a
                    // correctly sized SO_SNDBUF value.
                    assert_eq!(
                        unsafe {
                            libc::setsockopt(
                                socket.stream.as_raw_fd(),
                                libc::SOL_SOCKET,
                                libc::SO_SNDBUF,
                                std::ptr::addr_of!(bytes).cast(),
                                std::mem::size_of_val(&bytes) as libc::socklen_t,
                            )
                        },
                        0,
                        "shrink accepted socket send buffer"
                    );
                }
                accepted_tx.send(()).expect("signal accepted");
                let payload = vec![b'x'; 8 * 1024 * 1024];
                let started = Instant::now();
                let deadline = if server_cancelled.is_some() {
                    started + Duration::from_secs(5)
                } else {
                    started + Duration::from_millis(40)
                };
                let error = conn
                    .write_all_until(&payload, deadline, server_cancelled.as_deref())
                    .expect_err("accepted server write must stop");
                result_tx
                    .send((error.kind(), started.elapsed()))
                    .expect("send write result");
            });

            let client = connect(&endpoint).expect("connect unread client");
            accepted_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("server accepted unread client");
            let canceller = cancelled.map(|flag| {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(40));
                    flag.store(true, Ordering::Release);
                })
            });
            let result = result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("accepted write remained blocked");
            if let Some(canceller) = canceller {
                canceller.join().expect("canceller thread");
            }
            drop(client);
            server.join().expect("server thread");
            result
        }

        let (kind, elapsed) = run("accepted-write-timeout", None);
        assert_eq!(kind, io::ErrorKind::TimedOut);
        assert!(
            elapsed < Duration::from_secs(1),
            "deadline took {elapsed:?}"
        );

        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let (kind, elapsed) = run("accepted-write-cancel", Some(cancelled));
        assert_eq!(kind, io::ErrorKind::Interrupted);
        assert!(
            elapsed < Duration::from_secs(1),
            "cancellation took {elapsed:?}"
        );
    }

    #[test]
    fn blocked_write_observes_deadline_and_cancellation() {
        fn stalled_connection(
            tag: &str,
        ) -> (
            CtlStream,
            std::sync::mpsc::Sender<()>,
            std::thread::JoinHandle<()>,
        ) {
            let endpoint = test_endpoint(tag);
            let listener = CtlListener::bind(&endpoint).expect("bind");
            let (release, released) = std::sync::mpsc::channel();
            // A Windows client can connect before the server's pending
            // ConnectNamedPipe completes. Do not let the timed-out client vanish
            // first: accept would see ERROR_NO_DATA, retry on a fresh instance,
            // and wait forever for a second client this fixture never creates.
            let (accepted, wait_for_accept) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let _conn = listener.accept().expect("accept");
                accepted.send(()).expect("signal accepted peer");
                released.recv().expect("release stalled peer");
            });
            let stream = connect(&endpoint).expect("connect");
            wait_for_accept
                .recv_timeout(Duration::from_secs(1))
                .expect("server accepted stalled peer");

            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd as _;

                let CtlStream::Unix(socket) = &stream;
                let bytes: libc::c_int = 4 * 1024;
                // SAFETY: the socket owns this valid fd and `bytes` is a
                // correctly sized, readable SO_SNDBUF value.
                let result = unsafe {
                    libc::setsockopt(
                        socket.stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        std::ptr::addr_of!(bytes).cast(),
                        std::mem::size_of_val(&bytes) as libc::socklen_t,
                    )
                };
                assert_eq!(result, 0, "shrink test socket send buffer");
            }

            (stream, release, server)
        }

        // Larger than both the Windows PIPE_BUF and ordinary Unix socket send
        // buffers, ensuring the peer's refusal to read creates backpressure.
        let payload = vec![b'x'; 8 * 1024 * 1024];
        let (mut timed, release_timed, timed_server) = stalled_connection("write-timeout");
        let started = Instant::now();
        let error = timed
            .write_all_until(&payload, Instant::now() + Duration::from_millis(30), None)
            .expect_err("blocked write must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(timed);
        release_timed.send(()).expect("release timed peer");
        timed_server.join().expect("server thread");

        let (mut cancellable, release_cancel, cancel_server) = stalled_connection("write-cancel");
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let setter = cancelled.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let error = cancellable
            .write_all_until(
                &payload,
                Instant::now() + Duration::from_secs(2),
                Some(&cancelled),
            )
            .expect_err("blocked write must be cancelled");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
        canceller.join().expect("canceller thread");
        drop(cancellable);
        release_cancel.send(()).expect("release cancelled peer");
        cancel_server.join().expect("server thread");
    }
}
