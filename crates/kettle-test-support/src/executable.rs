//! Unix executable fixtures whose copy completes before another thread executes them.

use std::path::Path;
use std::time::{Duration, Instant};

/// Copy a program to a new fixture path and wait up to five seconds for
/// inherited writable descriptors to close before returning it for execution.
pub fn copy_executable_fixture(source: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut source = std::fs::File::open(source)?;
    let mut writer = std::fs::File::create_new(path)?;
    std::io::copy(&mut source, &mut writer)?;
    writer.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    finish_fixture_copy(writer, path, Duration::from_secs(5))
}

fn finish_fixture_copy(
    writer: std::fs::File,
    path: &Path,
    timeout: Duration,
) -> std::io::Result<()> {
    // A concurrent fork can retain our writable descriptor until exec even
    // after this thread closes it. The lock follows that descriptor; taking
    // it from a new read-only handle waits for every inherited writer to close.
    // https://github.com/rust-lang/rust/issues/114554
    // The writer's own lock waits too: a fork of an earlier handle on the same
    // file may still hold one.
    let deadline = Instant::now() + timeout;
    lock_by(&writer, deadline)?;
    drop(writer);
    let reader = std::fs::File::open(path)?;
    lock_by(&reader, deadline)
}

/// Lock `file` exclusively, waiting for whoever holds it until `deadline`.
fn lock_by(file: &std::fs::File, deadline: Instant) -> std::io::Result<()> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "copied executable still has inherited writable descriptors",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exec regression can fork while the other test holds a locked writer.
    // Its inherited descriptor would invalidate that test's zero-timeout case.
    static DESCRIPTOR_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn fixture_copy_waits_for_inherited_writable_descriptors() {
        let _guard = DESCRIPTOR_TESTS.lock().unwrap();
        let directory = crate::private_tempdir("kettle-worker-copy-");
        let path = directory.path().join("worker");
        let writer = std::fs::File::create_new(&path).unwrap();
        // A duplicated descriptor models the writable handle inherited by a fork.
        let inherited = writer.try_clone().unwrap();
        let error = finish_fixture_copy(writer, &path, Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        drop(inherited);

        // Another test's fork may still hold the descriptor until its exec:
        // waiting it out is what the barrier is for.
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        finish_fixture_copy(writer, &path, Duration::from_secs(5)).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_retained_writer_prevents_execution_until_the_barrier_completes() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let _guard = DESCRIPTOR_TESTS.lock().unwrap();
        let directory = crate::private_tempdir("kettle-copy-exec-");
        let path = directory.path().join("worker");
        let mut writer = std::fs::File::create_new(&path).unwrap();
        writer.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        writer
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let inherited = writer.try_clone().unwrap();
        let error = std::process::Command::new(&path).spawn().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(26)); // Linux ETXTBSY.
        assert_eq!(
            finish_fixture_copy(writer, &path, Duration::ZERO)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        drop(inherited);
        // Another test's fork may still hold the descriptor until its exec.
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        finish_fixture_copy(writer, &path, Duration::from_secs(5)).unwrap();
        assert!(
            std::process::Command::new(&path)
                .status()
                .unwrap()
                .success()
        );
    }
}
