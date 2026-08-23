//! SCM_RIGHTS file-descriptor passing over Unix sockets.
//!
//! This is what lets a tab move between windows that are different processes.
//! The PTY stays open the whole time; its descriptors are handed across rather
//! than reopened, so the shell never notices.
//!
//!   source process → SerializedTab + raw PTY fds
//!                 → send_fds over unix socket
//!   target process ← recv_fds over unix socket
//!                 ← deserialize_tab + adopt fds as Pane PTYs
//!
//! Unix only, deliberately: Linux, macOS, and the BSDs. Windows and Wayland get
//! the keyboard-driven `Action::MoveTabToNewWindow` instead, which opens a new
//! tab rather than moving the running one.
//!
//! Only the transfer lives here. The handshake, authentication, and connection
//! lifecycle around it are described in
//! `docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md`.

#![allow(dead_code)]

use std::io;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;

// Use `libc::SCM_RIGHTS` directly rather than a hand-rolled
// `0x01`. The literal was only `#[cfg]`'d for linux/macos/freebsd, so on
// NetBSD/OpenBSD/DragonFly/illumos/Android the const was undefined and the
// Unix-only module failed to compile. `libc::SCM_RIGHTS` is correct for
// every Unix target (matching the `libc::SOL_SOCKET` already used beside it).
use libc::SCM_RIGHTS;

/// Send `fds` over the Unix socket along with `payload`. The
/// receiving end will get the duplicated fds via recv_fds.
///
/// Errors: ENOBUFS / EMSGSIZE / EBADF / etc are bubbled up via
/// io::Error. The caller (the detachable-tabs IPC layer) decides
/// how to recover (typically: cancel the drag, restore the source
/// tab).
///
/// Safety: this uses libc::sendmsg with a populated ancillary
/// buffer carrying SCM_RIGHTS. The unsafe block is bounded to
/// the syscall + the iovec/cmsg pointer setup; all pointers come
/// from owned local variables so they outlive the call.
pub fn send_fds(socket: &UnixStream, payload: &[u8], fds: &[RawFd]) -> io::Result<usize> {
    use std::os::unix::io::AsRawFd;
    if payload.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "send_fds: payload must be non-empty (SCM_RIGHTS msgs need ≥1 byte data)",
        ));
    }
    if fds.is_empty() {
        // No fds → fall through to normal write. Saves a cmsg.
        // `write` can short-write (return < payload.len()),
        // and the caller (the detachable-tabs IPC layer) closes the SOURCE tab
        // on a "successful" send — so a partial write silently lost the tail of
        // the serialized tab and the tab vanished. `write_all` loops until the
        // whole payload is delivered (or errors).
        use std::io::Write;
        let mut s = socket;
        s.write_all(payload)?;
        return Ok(payload.len());
    }
    // Build ancillary cmsg buffer carrying the SCM_RIGHTS payload.
    let fd_bytes = std::mem::size_of_val(fds);
    // CMSG_SPACE rounds up to alignment + adds cmsghdr; use a
    // generous bound + fixed cmsghdr size to keep this dep-free.
    let cmsg_len = unsafe { libc::CMSG_LEN(fd_bytes as u32) } as usize;
    let cmsg_space = unsafe { libc::CMSG_SPACE(fd_bytes as u32) } as usize;
    let mut cmsg_buf: Vec<u8> = vec![0u8; cmsg_space];
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;
    // Populate the cmsghdr at the front of cmsg_buf.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(io::Error::other("CMSG_FIRSTHDR null"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = SCM_RIGHTS;
        (*cmsg).cmsg_len = cmsg_len as _;
        let fd_ptr = libc::CMSG_DATA(cmsg) as *mut RawFd;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), fd_ptr, fds.len());
    }
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &msg, 0) };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    // sendmsg can SHORT-write on a stream socket. The fds
    // (ancillary SCM_RIGHTS data) ride with the first delivered byte and must
    // NOT be resent (re-sending the cmsg would duplicate the fds in the
    // receiver). So flush any remaining payload bytes with a plain write_all —
    // the cmsg is already delivered. Without this, a partial send dropped the
    // tail of the serialized tab and the caller, treating the send as complete,
    // closed the source tab → silent tab loss. (payload is non-empty and the
    // socket is blocking, so `sent >= 1` here and the fds were delivered.)
    let sent = sent as usize;
    if sent < payload.len() {
        use std::io::Write;
        let mut s = socket;
        s.write_all(&payload[sent..])?;
    }
    Ok(payload.len())
}

/// Receive bytes + duplicated fds. Caller provides a buffer for
/// the payload; the function returns the byte count + a Vec of
/// the received fds.
///
/// Each returned fd is owned by the caller (must be closed when
/// no longer needed). On error all received fds are closed before
/// returning so callers can't leak.
pub fn recv_fds(
    socket: &UnixStream,
    payload_buf: &mut [u8],
    max_fds: usize,
) -> io::Result<(usize, Vec<RawFd>)> {
    use std::os::unix::io::AsRawFd;
    let cmsg_space =
        unsafe { libc::CMSG_SPACE((max_fds * std::mem::size_of::<RawFd>()) as u32) } as usize;
    let mut cmsg_buf: Vec<u8> = vec![0u8; cmsg_space];
    let mut iov = libc::iovec {
        iov_base: payload_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: payload_buf.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;
    // Receive the fds close-on-exec where the platform
    // offers atomic delivery (Linux/Android `MSG_CMSG_CLOEXEC`); macOS and
    // others lack the flag and fall back to the `fcntl` below. Without CLOEXEC a
    // received PTY-master fd would leak into any shell spawned between this
    // `recvmsg` and adoption.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let recv_flags: libc::c_int = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let recv_flags: libc::c_int = 0;
    let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, recv_flags) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out: Vec<RawFd> = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == SCM_RIGHTS {
                // Guard the length arithmetic. A cmsg whose
                // `cmsg_len < CMSG_LEN(0)` would make the subtraction underflow
                // and the loop read wildly OOB. SCM_RIGHTS headers from the
                // kernel always satisfy this, but it's a cheap check on an
                // `unsafe` path.
                let base = libc::CMSG_LEN(0) as usize;
                if (*cmsg).cmsg_len as usize >= base {
                    let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                    let n_fds = ((*cmsg).cmsg_len as usize - base) / std::mem::size_of::<RawFd>();
                    for i in 0..n_fds {
                        out.push(*data.add(i));
                    }
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    // Belt-and-suspenders CLOEXEC for platforms without the atomic flag (and a
    // harmless no-op where the flag already set it). Best-effort: an fd that's
    // valid enough to adopt is valid enough to keep even if this fails.
    for &fd in &out {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // `MSG_CTRUNC` means the sender's ancillary data didn't fit our control
    // buffer, so the kernel dropped (and closed) the fds that overflowed. A
    // partial fd set would adopt the wrong PTYs — close what we got and surface
    // it rather than silently hand back a truncated handoff.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        for &fd in &out {
            unsafe { libc::close(fd) };
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recv_fds: ancillary data truncated (MSG_CTRUNC); some fds were dropped by the kernel",
        ));
    }
    Ok((n as usize, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_fds_empty_payload_errors() {
        // Drift guard: SCM_RIGHTS requires ≥1 byte of
        // real data alongside the ancillary cmsg; the kernel
        // rejects an empty iovec. Returns InvalidInput rather
        // than silently sending nothing.
        let (s1, _s2) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let err = send_fds(&s1, &[], &[0, 1, 2]).expect_err("empty payload should error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn send_fds_with_no_fds_falls_through_to_write() {
        // Zero fds skips the SCM_RIGHTS path entirely
        // (saves a cmsg). Should write the payload via normal
        // socket write.
        let (s1, s2) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let payload = b"hello";
        let sent = send_fds(&s1, payload, &[]).expect("send");
        assert_eq!(sent, payload.len());
        // Read back.
        use std::io::Read;
        let mut buf = [0u8; 16];
        let n = (&s2).read(&mut buf).expect("read");
        assert_eq!(&buf[..n], payload);
    }

    /// The no-fds send path must deliver the WHOLE payload
    /// even when the kernel send buffer can't hold it in one write — a short
    /// write would lose the tail of the serialized tab, and the caller closes
    /// the source tab on a "successful" send → silent tab loss. Behavioral
    /// test: a tiny SO_SNDBUF + a payload far larger than it forces the
    /// `write_all` loop across many writes; a concurrent reader drains the
    /// socket so the blocking writer makes progress. (Replaces a
    /// source-scan guard that self-matched its own banned-literal and failed
    /// on the unix CI leg where this module actually compiles.)
    #[test]
    fn send_fds_delivers_whole_payload_under_buffer_pressure() {
        use std::io::Read;
        use std::os::unix::io::AsRawFd;
        let (s1, s2) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        // Shrink the send buffer so a large payload can't fit in one write.
        let small: libc::c_int = 4096;
        unsafe {
            libc::setsockopt(
                s1.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &small as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        // 256 KiB — far larger than any rounded-up 4 KiB send buffer, so the
        // write_all loop must iterate many times.
        let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        // Concurrent reader so the blocking writer's write_all can drain.
        let mut reader = s2;
        let handle = std::thread::spawn(move || {
            let mut buf = vec![0u8; expected.len()];
            reader.read_exact(&mut buf).expect("read_exact");
            buf
        });
        let sent = send_fds(&s1, &payload, &[]).expect("send");
        assert_eq!(sent, payload.len(), "send_fds must report the full length");
        let got = handle.join().expect("reader thread");
        assert_eq!(
            got, payload,
            "the whole payload must arrive despite a tiny send buffer (no short-write loss)"
        );
    }

    /// A sent fd round-trips and is delivered close-on-exec
    /// (atomic `MSG_CMSG_CLOEXEC` on Linux, `fcntl` fallback elsewhere), so a
    /// handed-off PTY master can't leak into a later-spawned shell.
    #[test]
    fn recv_fds_round_trips_and_sets_cloexec() {
        let (s1, s2) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        // A concrete fd to pass: the read end of a fresh pipe.
        let mut pipe_fds = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (pipe_fds[0], pipe_fds[1]);

        let sent = send_fds(&s1, b"x", &[rd]).expect("send");
        assert_eq!(sent, 1);

        let mut buf = [0u8; 8];
        let (n, got) = recv_fds(&s2, &mut buf, 4).expect("recv");
        assert_eq!(n, 1, "payload byte received");
        assert_eq!(got.len(), 1, "exactly one fd received");

        let flags = unsafe { libc::fcntl(got[0], libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD on received fd");
        assert!(
            flags & libc::FD_CLOEXEC != 0,
            "received fd must be close-on-exec"
        );

        for fd in [rd, wr, got[0]] {
            unsafe { libc::close(fd) };
        }
    }
}
