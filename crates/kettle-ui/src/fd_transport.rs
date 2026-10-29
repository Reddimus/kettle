//! Legacy SCM_RIGHTS transport. Deprecated handoff receivers discard received
//! descriptors; live tab moves transfer ownership within one process.
//! See docs/ARCHITECTURE.md, Detachable tabs.

#![allow(dead_code)]

use std::io;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;

// Use `libc::SCM_RIGHTS` (like the `libc::SOL_SOCKET` beside it) rather than
// a hand-rolled `0x01`. libc defines the correct value for every Unix target,
// including NetBSD, OpenBSD, DragonFly, illumos, and Android.
use libc::SCM_RIGHTS;

/// Send the full payload and duplicate `fds` for the receiver, retaining ownership
/// of the originals. I/O errors propagate; an error may follow a partial send.
pub fn send_fds(socket: &UnixStream, payload: &[u8], fds: &[RawFd]) -> io::Result<usize> {
    use std::os::unix::io::AsRawFd;
    if payload.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "send_fds: payload must be non-empty (SCM_RIGHTS msgs need ≥1 byte data)",
        ));
    }
    if fds.is_empty() {
        // Without ancillary data, write_all still prevents a short payload.
        use std::io::Write;
        let mut s = socket;
        s.write_all(payload)?;
        return Ok(payload.len());
    }
    // Build ancillary cmsg buffer carrying the SCM_RIGHTS payload.
    let fd_bytes = std::mem::size_of_val(fds);
    // CMSG_SPACE (the buffer size) adds the cmsghdr and rounds up to
    // alignment; CMSG_LEN is the unpadded length for `cmsg_len`.
    // SAFETY: CMSG_LEN only computes a size from the byte count; it does not dereference pointers.
    let cmsg_len = unsafe { libc::CMSG_LEN(fd_bytes as u32) } as usize;
    // SAFETY: CMSG_SPACE only computes padded storage size; it does not dereference pointers.
    let cmsg_space = unsafe { libc::CMSG_SPACE(fd_bytes as u32) } as usize;
    let mut cmsg_buf: Vec<u8> = vec![0u8; cmsg_space];
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    // SAFETY: An all-zero msghdr contains valid null pointers and integer fields; buffers are
    // assigned below.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;
    // Populate the cmsghdr at the front of cmsg_buf.
    // SAFETY: Requires cmsghdr/RawFd-aligned storage and a non-truncated fd byte count. Vec<u8>
    // does not guarantee that alignment, and the u32 size cast is unchecked; needs review.
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
    // SAFETY: msg and its iovec, payload and ancillary buffers stay live through this
    // synchronous call.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &msg, 0) };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    // SCM_RIGHTS accompanies the first byte. Finish a short payload without
    // resending ancillary data, which would duplicate the descriptors again.
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
        // SAFETY: CMSG_SPACE computes a padded byte count without dereferencing any pointers.
        unsafe { libc::CMSG_SPACE((max_fds * std::mem::size_of::<RawFd>()) as u32) } as usize;
    let mut cmsg_buf: Vec<u8> = vec![0u8; cmsg_space];
    let mut iov = libc::iovec {
        iov_base: payload_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: payload_buf.len(),
    };
    // SAFETY: All-zero msghdr fields are valid; the writable buffers and lengths are assigned
    // below.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;
    // Linux/Android set CLOEXEC atomically; other Unix platforms use fcntl below.
    // Atomic delivery prevents inheritance by a concurrently spawned child.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let recv_flags: libc::c_int = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let recv_flags: libc::c_int = 0;
    // SAFETY: The iovec and ancillary buffer are exclusively writable for their advertised
    // lengths during the call.
    let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, recv_flags) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out: Vec<RawFd> = Vec::new();
    // SAFETY: Relies on kernel-produced cmsg lengths staying within msg_control and on
    // cmsghdr/RawFd alignment, which Vec<u8> does not guarantee; alignment needs review.
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
    // Best-effort CLOEXEC fallback; the legacy receiver closes every returned fd.
    for &fd in &out {
        // SAFETY: fd is a newly received descriptor still owned by out; F_SETFD takes an
        // integer argument.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // Reject incomplete descriptor sets and close every descriptor we received.
    // The kernel closes descriptors that did not fit the ancillary buffer.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        for &fd in &out {
            // SAFETY: Each received descriptor is owned here and is closed once before
            // returning the error.
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
    /// socket so the blocking writer makes progress.
    #[test]
    fn send_fds_delivers_whole_payload_under_buffer_pressure() {
        use std::io::Read;
        use std::os::unix::io::AsRawFd;
        let (s1, s2) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        // Shrink the send buffer so a large payload can't fit in one write.
        let small: libc::c_int = 4096;
        // SAFETY: s1 owns the socket; small is live c_int storage of the size SO_SNDBUF expects.
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
        // SAFETY: pipe_fds has writable space for the two descriptors pipe returns.
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (pipe_fds[0], pipe_fds[1]);

        let sent = send_fds(&s1, b"x", &[rd]).expect("send");
        assert_eq!(sent, 1);

        let mut buf = [0u8; 8];
        let (n, got) = recv_fds(&s2, &mut buf, 4).expect("recv");
        assert_eq!(n, 1, "payload byte received");
        assert_eq!(got.len(), 1, "exactly one fd received");

        // SAFETY: got owns this received descriptor, which has not yet been closed; F_GETFD
        // takes no extra argument.
        let flags = unsafe { libc::fcntl(got[0], libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD on received fd");
        assert!(
            flags & libc::FD_CLOEXEC != 0,
            "received fd must be close-on-exec"
        );

        for fd in [rd, wr, got[0]] {
            // SAFETY: The two pipe ends and received duplicate are distinct owned descriptors,
            // each closed once.
            unsafe { libc::close(fd) };
        }
    }
}
