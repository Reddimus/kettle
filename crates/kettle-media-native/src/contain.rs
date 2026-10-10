//! What a decoder process may do beyond decoding: start no process of its
//! own. It runs in the worker's process group, which Kettle kills when the
//! worker ends, and counts toward the group's memory limit, so a process it
//! started in another group would escape both.
//!
//! On macOS the decoder's process limit is set to one, so every fork or
//! spawn it attempts fails: the limit counts the user's processes, and the
//! decoder is one of them. Threads are not processes and still work. It
//! could still call `setsid` or `setpgid` to leave the group itself; only
//! the self-sandbox planned for the worker can take that away.
//!
//! On Linux, where threads count against the process limit, a seccomp
//! filter refuses `fork`, `vfork`, a `clone` that is not a thread, `setsid`
//! and `setpgid` with `EPERM`, and `clone3` with `ENOSYS`, which C libraries
//! answer by creating threads with `clone`. The filter needs no privilege
//! (`PR_SET_NO_NEW_PRIVS` comes first), is inherited across `exec`, and a
//! system call made under another architecture's numbering ends the process.
//!
//! The hook runs in the child between fork and exec and makes only
//! async-signal-safe calls on values built before the fork. If it cannot
//! install the limit or the filter, the spawn fails: a decoder never runs
//! uncontained.

use std::process::Command;

/// Make `command`'s process unable to start another, or (on Linux) to leave
/// its process group.
pub(crate) fn contain(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    #[cfg(target_os = "linux")]
    let filter = linux::filter();
    let hook = move || -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        linux::install(&filter)?;
        #[cfg(target_os = "macos")]
        macos::no_processes()?;
        Ok(())
    };
    // SAFETY: the hook runs in the forked child before exec. It allocates
    // nothing and takes no lock: it reads a filter built before the fork and
    // makes only prctl and setrlimit calls, which are async-signal-safe.
    unsafe { command.pre_exec(hook) };
}

#[cfg(target_os = "macos")]
mod macos {
    /// Lower the process limit to one, soft and hard.
    pub(super) fn no_processes() -> std::io::Result<()> {
        let one = libc::rlimit {
            rlim_cur: 1,
            rlim_max: 1,
        };
        // SAFETY: `one` is a valid rlimit that outlives the call.
        if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &one) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use libc::sock_filter;

    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e; // AUDIT_ARCH_X86_64
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7; // AUDIT_ARCH_AARCH64
    /// x32 system calls carry this bit in their number on x86_64.
    #[cfg(target_arch = "x86_64")]
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;

    /// Offsets into `struct seccomp_data`: the call's number, the
    /// architecture, and the low half of its first argument (both supported
    /// architectures are little-endian).
    const NR: u32 = 0;
    const ARCH_OFFSET: u32 = 4;
    const ARG0_LOW: u32 = 16;

    const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
    const EPERM: u32 = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
    const ENOSYS: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
    const KILL: u32 = libc::SECCOMP_RET_KILL_PROCESS;

    fn load(offset: u32) -> sock_filter {
        stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, offset)
    }

    fn ret(action: u32) -> sock_filter {
        stmt(libc::BPF_RET | libc::BPF_K, action)
    }

    fn stmt(code: u32, k: u32) -> sock_filter {
        sock_filter {
            code: code as u16,
            jt: 0,
            jf: 0,
            k,
        }
    }

    fn jump(code: u32, k: u32, jt: u8, jf: u8) -> sock_filter {
        sock_filter {
            code: (libc::BPF_JMP | code | libc::BPF_K) as u16,
            jt,
            jf,
            k,
        }
    }

    /// The system calls refused outright, with the answer each gets.
    fn refused() -> Vec<(i64, u32)> {
        let common = [
            (libc::SYS_clone3, ENOSYS),
            (libc::SYS_setsid, EPERM),
            (libc::SYS_setpgid, EPERM),
        ];
        // aarch64 has no fork or vfork call: its C library forks with clone.
        #[cfg(target_arch = "x86_64")]
        let forks = [(libc::SYS_fork, EPERM), (libc::SYS_vfork, EPERM)];
        #[cfg(not(target_arch = "x86_64"))]
        let forks: [(i64, u32); 0] = [];
        common.into_iter().chain(forks).collect()
    }

    /// The filter, built before the fork. Its shape: check the architecture,
    /// load the call number, refuse the x32 range on x86_64, send `clone` to
    /// the thread check, answer each refused call, allow the rest; the
    /// thread check allows a `clone` with `CLONE_THREAD` and refuses others.
    pub(super) fn filter() -> Vec<sock_filter> {
        let refused = refused();
        let mut program = vec![
            load(ARCH_OFFSET),
            jump(libc::BPF_JEQ, ARCH, 1, 0),
            ret(KILL),
            load(NR),
        ];
        #[cfg(target_arch = "x86_64")]
        {
            // Over the x32 range to the refusal just past the main table.
            let to_eperm = (1 + 2 * refused.len() + 1) as u8;
            program.push(jump(libc::BPF_JGE, X32_SYSCALL_BIT, to_eperm, 0));
        }
        // `clone` jumps past the table: one pair per refused call, then the
        // allow, the x86_64 refusal, and lands on the thread check.
        let past_table = (2 * refused.len() + 1 + usize::from(cfg!(target_arch = "x86_64"))) as u8;
        program.push(jump(libc::BPF_JEQ, libc::SYS_clone as u32, past_table, 0));
        for (number, answer) in &refused {
            program.push(jump(libc::BPF_JEQ, *number as u32, 0, 1));
            program.push(ret(*answer));
        }
        program.push(ret(ALLOW));
        #[cfg(target_arch = "x86_64")]
        program.push(ret(EPERM));
        program.extend([
            load(ARG0_LOW),
            jump(libc::BPF_JSET, libc::CLONE_THREAD as u32, 0, 1),
            ret(ALLOW),
            ret(EPERM),
        ]);
        program
    }

    /// Install `filter` on the calling process, after giving up new
    /// privileges, which an unprivileged filter requires.
    pub(super) fn install(filter: &[sock_filter]) -> std::io::Result<()> {
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr().cast_mut(),
        };
        let one: libc::c_ulong = 1;
        let zero: libc::c_ulong = 0;
        // SAFETY: PR_SET_NO_NEW_PRIVS reads only its integer arguments,
        // passed at the width the kernel reads.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, one, zero, zero, zero) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `program` points at `filter`, which outlives the call; the
        // kernel copies the program before it returns.
        let mode = libc::SECCOMP_MODE_FILTER as libc::c_ulong;
        if unsafe { libc::prctl(libc::PR_SET_SECCOMP, mode, &program as *const _, zero, zero) } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every jump lands inside the program, and the program ends in a
        /// return, so the kernel cannot run off its end.
        #[test]
        fn every_jump_lands_inside_the_filter() {
            let program = filter();
            for (at, instruction) in program.iter().enumerate() {
                if u32::from(instruction.code) & 0x07 == libc::BPF_JMP {
                    for offset in [instruction.jt, instruction.jf] {
                        assert!(at + 1 + usize::from(offset) < program.len(), "{at}");
                    }
                }
            }
            assert_eq!(
                u32::from(program.last().unwrap().code),
                libc::BPF_RET | libc::BPF_K
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// Set, to the check to make, when this test binary is started again as
    /// the child.
    const CHILD: &str = "KETTLE_CONTAIN_TEST_CHILD";

    /// The child half of the tests below, run in a copy of this test binary:
    /// it reports whether the one thing it was asked to make could be made.
    #[test]
    #[ignore = "run by the tests below as their child"]
    fn contained_child() {
        let Some(check) = std::env::var_os(CHILD) else {
            return;
        };
        let made = match check.to_str() {
            Some("thread") => std::thread::spawn(|| 7).join().ok() == Some(7),
            Some("process") => Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .stdin(Stdio::null())
                .status()
                .is_ok(),
            // SAFETY: setsid takes no arguments. The child is not a group
            // leader, so only a filter refuses it.
            Some("session") => (unsafe { libc::setsid() }) != -1,
            // SAFETY: setpgid takes no pointers.
            Some("group") => (unsafe { libc::setpgid(0, 0) }) == 0,
            _ => false,
        };
        println!("made={made}");
    }

    /// Whether the child could make `check`, contained or not.
    fn child_made(check: &str, contained: bool) -> bool {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "contain::tests::contained_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, check)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        if contained {
            contain(&mut command);
        }
        let output = command.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        // The harness prints its own text before the child's on that line.
        let report = text
            .lines()
            .find_map(|line| line.split_once("made=").map(|(_, made)| made.trim()));
        match report {
            Some("true") => true,
            Some("false") => false,
            _ => panic!("no report from the {check} child: {text}"),
        }
    }

    /// A contained process can make threads but cannot start a process; on
    /// Linux it also cannot leave its session or group, which macOS cannot
    /// prevent without a sandbox. Uncontained, the same child does all four,
    /// so nothing else is refusing them.
    #[test]
    fn contained_processes_start_threads_but_no_process() {
        let linux = cfg!(target_os = "linux");
        for (check, contained) in [
            ("thread", true),
            ("process", false),
            ("session", !linux),
            ("group", !linux),
        ] {
            assert_eq!(child_made(check, true), contained, "{check}, contained");
            assert!(child_made(check, false), "{check}, uncontained");
        }
    }
}
