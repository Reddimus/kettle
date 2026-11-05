use super::{PtyReadStatus, pty_read_error_status};

#[cfg(unix)]
#[test]
fn downstream_backpressure_does_not_consume_the_eof_deadline() {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let (mut writer, mut reader) = UnixStream::pair().unwrap();
    writer.write_all(b"unread tail").unwrap();
    drop(writer);
    let (raw_tx, raw_rx) = crossbeam_channel::bounded(0);
    let (started_tx, started_rx) = crossbeam_channel::bounded(0);
    let pump = std::thread::spawn(move || {
        let mut deadline = Some(Instant::now() + super::PTY_CHILD_EXIT_EOF_TIMEOUT);
        started_tx.send(()).unwrap();
        let result = super::forward_pty_buffer_or_drain(
            &raw_tx,
            &AtomicBool::new(false),
            1,
            b"delivered".to_vec(),
            9,
            &mut deadline,
        );
        assert!(matches!(result, super::PtyPumpSend::Forwarded));
        deadline.unwrap()
    });
    started_rx.recv().unwrap();
    std::thread::sleep(super::PTY_CHILD_EXIT_EOF_TIMEOUT + Duration::from_millis(50));
    assert_eq!(raw_rx.recv().unwrap().unwrap().1, b"delivered");
    let deadline = pump.join().unwrap();
    assert!(
        super::wait_for_master_until(reader.as_raw_fd(), deadline).unwrap(),
        "downstream delivery must leave time to drain output and observe EOF"
    );
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, b"unread tail");
    assert!(super::wait_for_master_until(reader.as_raw_fd(), deadline).unwrap());
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
}

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
