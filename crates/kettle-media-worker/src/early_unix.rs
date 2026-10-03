//! The worker's first act, and its resource limits.
//!
//! Closing raw inherited descriptors is deliberately unsafe. It is sound here
//! because it runs first in `main`: the process has one thread, no Rust value
//! owns a descriptor above stderr yet (so no later drop can close a reused
//! number), and stdin, stdout and stderr stay open. The guarantee starts in
//! Rust's `main`, not before the dynamic loader and libc start up.

use std::io;

use libc::c_int;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type Resource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type Resource = c_int;

/// Close every descriptor above stderr and turn off core dumps; on Linux the
/// process also becomes non-dumpable, which keeps same-user debuggers out.
pub(crate) fn sweep_fds_and_disable_dumping() -> io::Result<()> {
    close_above_stderr()?;
    set_limit(libc::RLIMIT_CORE, 0)?;
    #[cfg(target_os = "linux")]
    {
        let off: libc::c_ulong = 0;
        // SAFETY: PR_SET_DUMPABLE reads only its integer arguments, passed at
        // the width the kernel reads.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, off, off, off, off) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The worker's resource limits: CPU time, no regular-file growth, few
/// descriptors, and on Linux an address-space ceiling (macOS does not enforce
/// one). Each is lowered to its value, or to an inherited hard limit that is
/// already lower, never raised; failing to set one refuses setup.
pub(crate) fn install_limits() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    set_limit(libc::RLIMIT_AS, 1 << 30)?;
    set_limit(libc::RLIMIT_CPU, 5)?;
    set_limit(libc::RLIMIT_FSIZE, 0)?;
    set_limit(libc::RLIMIT_NOFILE, 32)?;
    Ok(())
}

pub(crate) fn set_limit(resource: Resource, value: libc::rlim_t) -> io::Result<()> {
    let mut inherited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `inherited` is a valid rlimit for the call to fill.
    if unsafe { libc::getrlimit(resource, &mut inherited) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = value.min(inherited.rlim_max);
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` is a valid rlimit the call only reads.
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Close `fd`, which no Rust value owns; one that is not open is fine.
fn close_unowned(fd: c_int) -> io::Result<()> {
    // SAFETY: see the module comment; nothing owns `fd` or will use it again.
    if unsafe { libc::close(fd) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBADF) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(target_os = "linux")]
fn close_above_stderr() -> io::Result<()> {
    let (first, last, flags): (libc::c_ulong, libc::c_ulong, libc::c_ulong) =
        (3, libc::c_ulong::from(u32::MAX), 0);
    // SAFETY: close_range reads only its integer arguments; closing the
    // unowned descriptors is sound for the reasons in the module comment.
    if unsafe { libc::syscall(libc::SYS_close_range, first, last, flags) } == 0 {
        return Ok(());
    }
    // Kernels before 5.9 lack close_range, and some seccomp policies refuse
    // it: close each number below the hard descriptor limit instead.
    close_each_below(descriptor_ceiling()?)
}

/// The most descriptors to sweep one by one. A higher or unlimited hard limit
/// refuses setup rather than leave descriptors open.
#[cfg(target_os = "linux")]
const MAX_SWEPT_DESCRIPTORS: libc::rlim_t = 1 << 20;

#[cfg(target_os = "linux")]
fn descriptor_ceiling() -> io::Result<c_int> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid rlimit for the call to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    sweep_ceiling(limit.rlim_max)
}

/// How many descriptor numbers to sweep for a hard limit of `hard`.
#[cfg(target_os = "linux")]
pub(crate) fn sweep_ceiling(hard: libc::rlim_t) -> io::Result<c_int> {
    if hard == libc::RLIM_INFINITY || hard > MAX_SWEPT_DESCRIPTORS {
        return Err(io::Error::other("descriptor limit too high to sweep"));
    }
    c_int::try_from(hard).map_err(io::Error::other)
}

#[cfg(target_os = "linux")]
pub(crate) fn close_each_below(ceiling: c_int) -> io::Result<()> {
    (3..ceiling).try_for_each(close_unowned)
}

/// macOS has no close_range, but the kernel lists a process's descriptors.
#[cfg(target_os = "macos")]
fn close_above_stderr() -> io::Result<()> {
    open_descriptors()?
        .into_iter()
        .filter(|&fd| fd > 2)
        .try_for_each(close_unowned)
}

/// The descriptors this process holds, from the kernel's list. A list that
/// fills its buffer may be cut short, so that refuses rather than guesses.
#[cfg(target_os = "macos")]
fn open_descriptors() -> io::Result<Vec<c_int>> {
    let pid = c_int::try_from(std::process::id()).map_err(io::Error::other)?;
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    // SAFETY: with a null buffer the call only reports the size it needs.
    let needed =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let needed = usize::try_from(needed)
        .ok()
        .filter(|&needed| needed > 0)
        .ok_or_else(io::Error::last_os_error)?;
    let capacity = needed / entry + 16;
    let mut list = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        capacity
    ];
    let bytes = c_int::try_from(capacity * entry).map_err(io::Error::other)?;
    // SAFETY: `list` is writable for `bytes` bytes of proc_fdinfo records.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            list.as_mut_ptr().cast(),
            bytes,
        )
    };
    let count = usize::try_from(filled)
        .ok()
        .filter(|&filled| filled > 0)
        .ok_or_else(io::Error::last_os_error)?
        / entry;
    if count >= capacity {
        return Err(io::Error::other("descriptor list did not fit"));
    }
    Ok(list[..count].iter().map(|info| info.proc_fd).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `test` in a child of this test binary, where changing the
    /// process's limits and descriptors cannot disturb other tests.
    fn in_child(test: &str) -> bool {
        const CHILD_ENV: &str = "KETTLE_MEDIA_WORKER_EARLY_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            return true;
        }
        // Pipes, not the caller's stdout: the child's file-size limit of 0
        // would stop its writes to a regular file.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    /// A pipe end at `fd`, without close-on-exec, as a careless parent would
    /// leave it.
    fn inherited_at(fd: c_int) {
        let mut ends = [0; 2];
        // SAFETY: `ends` has room for the two descriptors.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        // SAFETY: plain descriptor numbers; the test owns both ends.
        assert_eq!(unsafe { libc::dup2(ends[1], fd) }, fd);
    }

    fn is_open(fd: c_int) -> bool {
        // SAFETY: F_GETFD only reads the descriptor table.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        flags != -1
    }

    fn limit(resource: Resource) -> libc::rlimit {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid rlimit for the call to fill.
        assert_eq!(unsafe { libc::getrlimit(resource, &mut limit) }, 0);
        limit
    }

    #[test]
    fn the_sweep_closes_everything_above_stderr() {
        if !in_child("early_unix::tests::the_sweep_closes_everything_above_stderr") {
            return;
        }
        for fd in [3, 17, 200] {
            inherited_at(fd);
        }
        sweep_fds_and_disable_dumping().unwrap();
        for fd in [3, 17, 200] {
            assert!(!is_open(fd), "{fd} is still open");
        }
        for fd in 0..=2 {
            assert!(is_open(fd), "stdio {fd} was closed");
        }
        assert_eq!(limit(libc::RLIMIT_CORE).rlim_max, 0);
        #[cfg(target_os = "linux")]
        // SAFETY: PR_GET_DUMPABLE reads nothing.
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE) }, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_fallback_sweep_closes_everything_below_the_limit() {
        if !in_child("early_unix::tests::the_fallback_sweep_closes_everything_below_the_limit") {
            return;
        }
        inherited_at(40);
        close_each_below(descriptor_ceiling().unwrap()).unwrap();
        assert!(!is_open(40));
        assert!(is_open(2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_unbounded_descriptor_limit_refuses_the_fallback_sweep() {
        assert_eq!(sweep_ceiling(4096).unwrap(), 4096);
        assert_eq!(sweep_ceiling(1 << 20).unwrap(), 1 << 20);
        assert!(sweep_ceiling((1 << 20) + 1).is_err());
        assert!(sweep_ceiling(libc::RLIM_INFINITY).is_err());
    }

    #[test]
    fn limits_are_lowered_never_raised() {
        if !in_child("early_unix::tests::limits_are_lowered_never_raised") {
            return;
        }
        // An inherited hard limit below the worker's own stays.
        set_limit(libc::RLIMIT_NOFILE, 20).unwrap();
        install_limits().unwrap();
        let expect = |resource: Resource, value: libc::rlim_t| {
            let limit = limit(resource);
            assert_eq!((limit.rlim_cur, limit.rlim_max), (value, value));
        };
        expect(libc::RLIMIT_NOFILE, 20);
        expect(libc::RLIMIT_CPU, 5);
        expect(libc::RLIMIT_FSIZE, 0);
        #[cfg(target_os = "linux")]
        expect(libc::RLIMIT_AS, 1 << 30);
    }
}
