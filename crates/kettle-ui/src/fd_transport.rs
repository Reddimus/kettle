//! Cycle 399 (Terminator parity, detachable-tabs Bucket-D
//! sub-cycle 3): SCM_RIGHTS file-descriptor passing over Unix
//! sockets. Used by the cross-window tab-handoff path
//! (docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md sub-cycles 7+8):
//!
//!   source process → SerializedTab (cycle 397) + raw PTY fds
//!                 → send_fds over unix socket
//!   target process ← recv_fds over unix socket
//!                 ← deserialize_tab + adopt fds as Pane PTYs
//!
//! Unix-only by design (Linux + macOS + BSDs). Windows + Wayland
//! get the keyboard-driven fallback (`Action::MoveTabToNewWindow`,
//! cycle 384) instead.
//!
//! The actual cross-process IPC handshake + auth + connection
//! lifecycle is sub-cycles 6 + 7 + 8; this module is the pure
//! fd-passing primitive those sub-cycles compose.

#![cfg(unix)]
#![allow(dead_code)]

use std::io;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;

#[cfg(target_os = "linux")]
const SCM_RIGHTS: i32 = 0x01;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
const SCM_RIGHTS: i32 = 0x01;

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
        use std::io::Write;
        let mut s = socket;
        return s.write(payload);
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
    Ok(sent as usize)
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
    let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out: Vec<RawFd> = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                let n_fds = ((*cmsg).cmsg_len as usize - (libc::CMSG_LEN(0) as usize))
                    / std::mem::size_of::<RawFd>();
                for i in 0..n_fds {
                    out.push(*data.add(i));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok((n as usize, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_fds_empty_payload_errors() {
        // Cycle 399 drift guard. SCM_RIGHTS requires ≥1 byte of
        // real data alongside the ancillary cmsg; the kernel
        // rejects an empty iovec. Returns InvalidInput rather
        // than silently sending nothing.
        let (s1, _s2) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let err = send_fds(&s1, &[], &[0, 1, 2]).expect_err("empty payload should error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn send_fds_with_no_fds_falls_through_to_write() {
        // Cycle 399: zero fds skips the SCM_RIGHTS path entirely
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
}
