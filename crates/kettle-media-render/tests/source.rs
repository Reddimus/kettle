//! The source loader: what a path may be, and what is refused.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use kettle_media::{FailureCode, GuiActionWitness, NativePath, Source};
use kettle_media_render::source::load;

fn user_pull(path: &Path) -> Source {
    use std::os::unix::ffi::OsStrExt as _;
    Source::user_pull(
        NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
        GuiActionWitness::from_explicit_gui_action(),
    )
}

#[test]
fn umask_002_regular_file_is_accepted() {
    // Group-writable media, as a umask of 002 leaves it, is ordinary.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.png");
    std::fs::write(&path, b"bytes").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert_eq!(&*load(&user_pull(&path), 1024).unwrap().bytes, b"bytes");
}

#[test]
fn symlink_leaf_is_accepted() {
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real.png");
    let link = directory.path().join("link.png");
    std::fs::write(&real, b"through the link").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        &*load(&user_pull(&link), 1024).unwrap().bytes,
        b"through the link"
    );
}

#[test]
fn fifo_is_refused_without_blocking() {
    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("pipe.png");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    // No writer will ever come: a blocking open would hang, so the load runs
    // on its own thread and a hang fails the test instead of stalling it.
    let source = user_pull(&fifo);
    let (sent, answer) = std::sync::mpsc::channel();
    let loader = std::thread::spawn(move || {
        // The receiver lives until this thread is joined: the send cannot fail.
        let _ = sent.send(load(&source, 1024).map(|_| ()));
    });
    match answer.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => {
            loader.join().unwrap();
            assert_eq!(result.unwrap_err(), FailureCode::FileNotRegular);
        }
        Err(_) => {
            // Opening the write end releases the blocked reader.
            drop(std::fs::OpenOptions::new().write(true).open(&fifo));
            loader.join().unwrap();
            panic!("opening a FIFO with no writer blocked");
        }
    }
}

#[test]
fn directory_and_device_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    for path in [directory.path(), Path::new("/dev/null")] {
        assert_eq!(
            load(&user_pull(path), 1024).unwrap_err(),
            FailureCode::FileNotRegular,
            "{}",
            path.display()
        );
    }
}

#[test]
fn source_cap_plus_one_is_refused() {
    // Inline: exactly the cap is fine, one byte more is not.
    assert!(load(&Source::Bytes(vec![0; 64]), 64).is_ok());
    assert_eq!(
        load(&Source::Bytes(vec![0; 65]), 64).unwrap_err(),
        FailureCode::TooLarge
    );
    // A file likewise.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("big.png");
    std::fs::write(&path, vec![0; 64]).unwrap();
    assert_eq!(load(&user_pull(&path), 64).unwrap().bytes.len(), 64);
    std::fs::write(&path, vec![0; 65]).unwrap();
    assert_eq!(
        load(&user_pull(&path), 64).unwrap_err(),
        FailureCode::FileTooLarge
    );
}

#[test]
fn missing_and_unreadable_files_have_fixed_failures() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        load(&user_pull(&directory.path().join("absent.png")), 64).unwrap_err(),
        FailureCode::FileNotFound
    );
    let path = directory.path().join("private.png");
    std::fs::write(&path, b"x").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads it anyway.
    if std::fs::read(&path).is_err() {
        assert_eq!(
            load(&user_pull(&path), 64).unwrap_err(),
            FailureCode::FilePermission
        );
    }
}
