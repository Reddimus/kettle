use super::PtyStdinLeasePhase;

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use super::{UnixPtyStdinLease, fcntl_retry};

#[test]
fn lease_is_exclusive_until_restoration_finishes() {
    let mut phase = PtyStdinLeasePhase::Available;

    phase.try_begin().expect("first lease is available");
    assert_eq!(
        phase.try_begin(),
        Err("a PTY stdin nonblocking lease is already active")
    );

    phase.finish(true);
    phase.try_begin().expect("restored lease is reusable");
}

#[test]
fn failed_setup_releases_the_reserved_lease() {
    let mut phase = PtyStdinLeasePhase::Available;

    phase.try_begin().expect("lease reservation succeeds");
    phase.abort_begin();

    phase
        .try_begin()
        .expect("a setup failure must not permanently consume the lease");
}

#[test]
fn failed_restoration_latches_the_lease_closed() {
    let mut phase = PtyStdinLeasePhase::Available;

    phase.try_begin().expect("lease reservation succeeds");
    phase.finish(false);

    assert_eq!(
        phase.try_begin(),
        Err("the previous PTY stdin lease could not restore file status flags")
    );
}

#[cfg(unix)]
#[test]
fn unix_lease_sets_exclusive_nonblocking_status_and_restores_it() {
    let mut descriptors = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe(descriptors.as_mut_ptr()) },
        0,
        "pipe fixture creation failed: {}",
        std::io::Error::last_os_error()
    );
    let _read_end = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let phase = Arc::new(Mutex::new(PtyStdinLeasePhase::Available));
    let original =
        fcntl_retry(write_end.as_raw_fd(), libc::F_GETFL, 0).expect("read original pipe status");

    let lease = UnixPtyStdinLease::acquire(write_end.as_raw_fd(), Arc::clone(&phase))
        .expect("first lease succeeds");
    let active =
        fcntl_retry(write_end.as_raw_fd(), libc::F_GETFL, 0).expect("read active pipe status");
    assert_ne!(active & libc::O_NONBLOCK, 0);
    assert!(
        UnixPtyStdinLease::acquire(write_end.as_raw_fd(), Arc::clone(&phase)).is_err(),
        "a second live lease must be rejected"
    );

    drop(lease);
    assert_eq!(
        fcntl_retry(write_end.as_raw_fd(), libc::F_GETFL, 0).expect("read restored pipe status"),
        original
    );

    drop(
        UnixPtyStdinLease::acquire(write_end.as_raw_fd(), phase)
            .expect("the restored lease is reusable"),
    );
}
