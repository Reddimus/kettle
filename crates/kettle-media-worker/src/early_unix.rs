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
/// one). Each is lowered to its value, or to an inherited soft or hard limit
/// that is already lower, never raised; failing to set one refuses setup.
pub(crate) fn install_limits() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    set_limit(libc::RLIMIT_AS, 1 << 30)?;
    set_limits(libc::RLIMIT_CPU, JOB_CPU_SECONDS, SESSION_CPU_CEILING)?;
    set_limit(libc::RLIMIT_FSIZE, 0)?;
    set_limit(libc::RLIMIT_NOFILE, 32)?;
    Ok(())
}

/// Processor time: a job's, soft from the start and hard once it arrives;
/// a playback session's most, which it lowers to what its video needs once
/// open: half a minute and a second for each second left to play.
const JOB_CPU_SECONDS: libc::rlim_t = 5;
const SESSION_CPU_CEILING: libc::rlim_t = 12 * 60 * 60;
const SESSION_CPU_SECONDS: u64 = 30;

/// Pin a job's processor time, soft and hard, before its input is read.
pub(crate) fn limit_job_cpu() -> io::Result<()> {
    set_limit(libc::RLIMIT_CPU, JOB_CPU_SECONDS)
}

/// Pin a session's processor time to what `left_ms` of video needs, soft
/// and hard, within the hard limit the start left (never more than the
/// soft limit inherited).
pub(crate) fn limit_session_cpu(left_ms: u64) -> io::Result<()> {
    let seconds = SESSION_CPU_SECONDS.saturating_add(left_ms.div_ceil(1000));
    let seconds = libc::rlim_t::try_from(seconds).unwrap_or(libc::rlim_t::MAX);
    let current = limit_of(libc::RLIMIT_CPU)?;
    set(
        libc::RLIMIT_CPU,
        seconds.min(current.rlim_max),
        seconds.min(current.rlim_max),
    )
}

/// Set `resource` to `soft` under `hard`, neither above the soft or hard
/// limit inherited: so a soft limit the worker raises later, up to its
/// hard one, still never passes what it was given.
fn set_limits(resource: Resource, soft: libc::rlim_t, hard: libc::rlim_t) -> io::Result<()> {
    let inherited = limit_of(resource)?;
    let hard = hard.min(inherited.rlim_cur).min(inherited.rlim_max);
    set(resource, soft.min(hard), hard)
}

fn limit_of(resource: Resource) -> io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(resource, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(limit)
}

fn set(resource: Resource, soft: libc::rlim_t, hard: libc::rlim_t) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
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
    // Never above either inherited limit: a lower soft limit stays too.
    let value = value.min(inherited.rlim_cur).min(inherited.rlim_max);
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
    // it: close each descriptor /proc lists instead. Not each number below
    // the descriptor limit, which a parent can lower below a descriptor it
    // already holds.
    close_listed()
}

#[cfg(target_os = "linux")]
pub(crate) fn close_listed() -> io::Result<()> {
    open_descriptors()?
        .into_iter()
        .filter(|&fd| fd > 2)
        .try_for_each(close_unowned)
}

/// The descriptors /proc lists for this process. The listing's own
/// descriptor is among them; it is closed again before they are, and then
/// reads as not open.
#[cfg(target_os = "linux")]
fn open_descriptors() -> io::Result<Vec<c_int>> {
    let mut listed = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let name = entry?.file_name();
        let fd = name
            .to_str()
            .and_then(|name| name.parse::<c_int>().ok())
            .ok_or_else(|| io::Error::other("unexpected /proc/self/fd entry"))?;
        listed.push(fd);
    }
    Ok(listed)
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
    fn the_fallback_sweep_closes_descriptors_above_the_limit() {
        if !in_child("early_unix::tests::the_fallback_sweep_closes_descriptors_above_the_limit") {
            return;
        }
        // A descriptor the parent opened before lowering its limit below it.
        inherited_at(200);
        set_limit(libc::RLIMIT_NOFILE, 32).unwrap();
        close_listed().unwrap();
        assert!(!is_open(200));
        for fd in 0..=2 {
            assert!(is_open(fd), "stdio {fd} was closed");
        }
    }

    /// With room to spare, a job's processor time is 5 s and a session's
    /// half a minute and a second for each second of video left.
    #[test]
    fn processor_time_follows_what_the_worker_is_for() {
        if !in_child("early_unix::tests::processor_time_follows_what_the_worker_is_for") {
            return;
        }
        install_limits().unwrap();
        let cpu = |at: &str| {
            let limit = limit(libc::RLIMIT_CPU);
            assert!(limit.rlim_max >= 3600, "{at}: the test needs room");
            (limit.rlim_cur, limit.rlim_max)
        };
        assert_eq!(cpu("installed").0, 5);
        limit_session_cpu(90_500).unwrap();
        let limit = limit(libc::RLIMIT_CPU);
        assert_eq!((limit.rlim_cur, limit.rlim_max), (121, 121));
    }

    #[test]
    fn job_processor_time_is_pinned_at_five_seconds() {
        if !in_child("early_unix::tests::job_processor_time_is_pinned_at_five_seconds") {
            return;
        }
        install_limits().unwrap();
        limit_job_cpu().unwrap();
        let limit = limit(libc::RLIMIT_CPU);
        assert_eq!((limit.rlim_cur, limit.rlim_max), (5, 5));
    }

    #[test]
    fn limits_are_lowered_never_raised() {
        if !in_child("early_unix::tests::limits_are_lowered_never_raised") {
            return;
        }
        // An inherited hard limit below the worker's own stays, and so does
        // a lower soft limit under a higher hard one.
        set_limit(libc::RLIMIT_NOFILE, 20).unwrap();
        let cpu = limit(libc::RLIMIT_CPU);
        let lower = libc::rlimit {
            rlim_cur: 2,
            rlim_max: cpu.rlim_max,
        };
        // SAFETY: `lower` is a valid rlimit the call only reads.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CPU, &lower) }, 0);
        install_limits().unwrap();
        let expect = |resource: Resource, value: libc::rlim_t| {
            let limit = limit(resource);
            assert_eq!((limit.rlim_cur, limit.rlim_max), (value, value));
        };
        expect(libc::RLIMIT_NOFILE, 20);
        expect(libc::RLIMIT_CPU, 2);
        // A session's limit, set later, does not pass it either.
        limit_session_cpu(100_000).unwrap();
        expect(libc::RLIMIT_CPU, 2);
        expect(libc::RLIMIT_FSIZE, 0);
        #[cfg(target_os = "linux")]
        expect(libc::RLIMIT_AS, 1 << 30);
    }
}
