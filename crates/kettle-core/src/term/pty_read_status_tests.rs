use super::{PtyReadStatus, pty_read_error_status};

#[cfg(unix)]
#[test]
fn expired_child_exit_deadline_rejects_a_readable_master() {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let (mut writer, reader) = UnixStream::pair().unwrap();
    std::io::Write::write_all(&mut writer, b"pending output").unwrap();
    assert!(
        super::wait_for_master_until(reader.as_raw_fd(), Instant::now() + Duration::from_secs(5),)
            .unwrap()
    );
    assert!(
        !super::wait_for_master_until(
            reader.as_raw_fd(),
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap()
    );
}

#[test]
fn an_unexpected_read_error_is_not_relabelled_as_eof() {
    let error = std::io::Error::other("injected PTY reader failure");
    assert_eq!(pty_read_error_status(&error), PtyReadStatus::Failed);
}

#[cfg(unix)]
#[test]
fn unix_eio_is_the_platforms_orderly_pty_hangup() {
    let error = std::io::Error::from_raw_os_error(libc::EIO);
    assert_eq!(pty_read_error_status(&error), PtyReadStatus::Eof);
}

#[cfg(windows)]
#[test]
fn windows_broken_pipe_is_the_platforms_orderly_conpty_hangup() {
    let error =
        std::io::Error::from_raw_os_error(windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32);
    assert_eq!(pty_read_error_status(&error), PtyReadStatus::Eof);
}
