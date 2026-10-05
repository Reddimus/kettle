//! Pane-rooted process tree for macOS, read through libproc and `sysctl`.
//!
//! The app polls on redraw, and a blinking cursor redraws about twice a
//! second, so reading the argv and cwd of every process on the machine would
//! make an idle window walk the whole process table. This walk visits only the
//! pane roots and their descendants: one `proc_listchildpids` and a sized
//! `KERN_PROCARGS2` read per process, plus a cwd read for the shell a caller
//! asks about.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use super::{
    MAX_PROC_FILE_BYTES, MAX_PROC_SCAN_DURATION, MAX_PROC_TREE_NODES, MAX_PROC_TREE_TOTAL_BYTES,
    ProcessTree, parse_kern_procargs2,
};

const INITIAL_CHILD_SLOTS: usize = 64;

#[derive(Default)]
pub(crate) struct MacProcessTree {
    entries: HashMap<u32, MacProcessEntry>,
    /// Roots whose subtree holds an argv this walk could not read whole. Only
    /// those panes keep their previous snapshot; the rest still publish.
    pub(crate) partial_roots: HashSet<u32>,
    /// Reused `KERN_PROCARGS2` buffer.
    args: Vec<u8>,
    /// Reused `proc_listchildpids` buffer.
    child_slots: Vec<libc::pid_t>,
    bytes_read: u64,
}

struct MacProcessEntry {
    parent: Option<u32>,
    argv: Option<Vec<String>>,
}

enum ArgvRead {
    Complete(Vec<String>),
    /// The process exited, is a zombie, or belongs to another user. This is
    /// normal during a walk and does not make the snapshot partial.
    Unavailable,
    /// This process's argument area exceeded a per-process limit or changed
    /// between reads, so its pane must not publish a guess.
    Truncated,
    /// The walk's shared byte budget is spent, so the whole scan is partial.
    OverBudget,
}

impl MacProcessTree {
    /// Walk the descendants of `roots`. Returns false when a node, byte, or
    /// time bound cut the walk short; the caller then keeps its last complete
    /// snapshot.
    pub(crate) fn refresh_roots(&mut self, roots: &[u32]) -> bool {
        let deadline = Instant::now() + MAX_PROC_SCAN_DURATION;
        self.entries.clear();
        self.partial_roots.clear();
        let mut queue: VecDeque<_> = roots.iter().copied().map(|pid| (pid, None)).collect();
        let mut scheduled: HashSet<_> = roots.iter().copied().collect();
        let mut root_of = HashMap::new();
        let mut children = Vec::new();
        let mut total_bytes = 0_u64;
        let mut complete = true;
        while let Some((pid, parent)) = queue.pop_front() {
            // Parents are visited first, so every descendant finds its root.
            let root = parent
                .and_then(|parent| root_of.get(&parent).copied())
                .unwrap_or(pid);
            root_of.insert(pid, root);
            if Instant::now() >= deadline {
                complete = false;
                break;
            }
            if self.entries.len() >= MAX_PROC_TREE_NODES {
                complete = false;
                continue;
            }
            let argv = match self.read_argv(pid, &mut total_bytes) {
                ArgvRead::Complete(argv) => Some(argv),
                ArgvRead::Unavailable => None,
                ArgvRead::Truncated => {
                    self.partial_roots.insert(root);
                    None
                }
                ArgvRead::OverBudget => {
                    complete = false;
                    None
                }
            };
            // libproc reports a missing pid as having no children, so only a
            // non-empty list proves the process is structure worth keeping.
            let has_children = match self.list_children(pid, &mut children) {
                Some(within_limits) => {
                    complete &= within_limits;
                    for &child in &children {
                        if scheduled.len() >= MAX_PROC_TREE_NODES {
                            complete = false;
                            break;
                        }
                        if scheduled.insert(child) {
                            queue.push_back((child, Some(pid)));
                        }
                    }
                    !children.is_empty()
                }
                None => false,
            };
            // A vanished requested root has no edge worth keeping; a listed
            // descendant is still structure even when its argv is unreadable.
            if argv.is_none() && !has_children && parent.is_none() {
                continue;
            }
            self.entries.insert(pid, MacProcessEntry { parent, argv });
        }
        self.bytes_read = total_bytes;
        complete
    }

    /// Fill `out` with the direct children of `pid`. Returns whether the list
    /// is known to be whole, or `None` when the kernel cannot list them.
    fn list_children(&mut self, pid: u32, out: &mut Vec<u32>) -> Option<bool> {
        out.clear();
        let pid = libc::pid_t::try_from(pid).ok()?;
        let mut slots = self.child_slots.len().max(INITIAL_CHILD_SLOTS);
        loop {
            self.child_slots.resize(slots, 0);
            let bytes = libc::c_int::try_from(slots * std::mem::size_of::<libc::pid_t>()).ok()?;
            // SAFETY: `child_slots` holds `slots` pids, and `bytes` is exactly
            // its length in bytes, so the kernel cannot write past it.
            let count = unsafe {
                libc::proc_listchildpids(pid, self.child_slots.as_mut_ptr().cast(), bytes)
            };
            // libproc returns a count of pids, and exactly `slots` when the
            // children may not have fit.
            let count = usize::try_from(count).ok()?;
            if count >= slots && slots < MAX_PROC_TREE_NODES {
                slots = (slots * 2).min(MAX_PROC_TREE_NODES);
                continue;
            }
            out.extend(
                self.child_slots[..count.min(slots)]
                    .iter()
                    .filter_map(|&child| u32::try_from(child).ok())
                    .filter(|&child| child > 0),
            );
            return Some(count < slots);
        }
    }

    /// Argv through `KERN_PROCARGS2`. A buffer smaller than the area gets its
    /// tail, not its head, which parses as environment strings, so the read
    /// asks for the exact size first and sizes the buffer one byte larger. A
    /// read that still fills the buffer means the area grew in between (an
    /// `exec`), and the argv is treated as incomplete. Only argv bytes count
    /// toward the walk's byte budget, as on Linux; the environment the kernel
    /// copies alongside is bounded by `MAX_PROC_FILE_BYTES` per process.
    fn read_argv(&mut self, pid: u32, total_bytes: &mut u64) -> ArgvRead {
        let Ok(pid) = libc::c_int::try_from(pid) else {
            return ArgvRead::Unavailable;
        };
        if MAX_PROC_TREE_TOTAL_BYTES.saturating_sub(*total_bytes) == 0 {
            return ArgvRead::OverBudget;
        }
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut needed = 0_usize;
        // SAFETY: `mib` names three integers. A null buffer asks the kernel
        // only for the area's size, which it stores in `needed`.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                std::ptr::null_mut(),
                &mut needed,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return ArgvRead::Unavailable;
        }
        if needed as u64 >= MAX_PROC_FILE_BYTES {
            return ArgvRead::Truncated;
        }
        let capacity = needed + 1;
        if self.args.len() < capacity {
            self.args.resize(capacity, 0);
        }
        let mut size = capacity;
        // SAFETY: `self.args` has at least `size` writable bytes, and the
        // kernel lowers `size` to the bytes it wrote. No new value is set.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                self.args.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return ArgvRead::Unavailable;
        }
        if size >= capacity {
            return ArgvRead::Truncated;
        }
        let Some(parsed) = parse_kern_procargs2(&self.args[..size]) else {
            return ArgvRead::Truncated;
        };
        let argv_bytes: usize = parsed.argv.iter().map(|arg| arg.len() + 1).sum();
        *total_bytes = total_bytes.saturating_add(argv_bytes as u64);
        if *total_bytes > MAX_PROC_TREE_TOTAL_BYTES {
            return ArgvRead::OverBudget;
        }
        if !parsed.complete {
            return ArgvRead::Truncated;
        }
        ArgvRead::Complete(parsed.argv)
    }

    #[cfg(test)]
    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl ProcessTree for MacProcessTree {
    fn refresh(&mut self) {}

    fn parent_of(&self, pid: u32) -> Option<u32> {
        self.entries.get(&pid)?.parent
    }

    fn argv_of(&self, pid: u32) -> Option<Vec<String>> {
        self.entries.get(&pid)?.argv.clone()
    }

    /// Read on demand, like the Linux tree, so only the shell a caller asks
    /// about pays for it.
    fn cwd_of(&self, pid: u32) -> Option<String> {
        self.entries.get(&pid)?;
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
        // SAFETY: `info` is a zeroed `proc_vnodepathinfo` and `size` is its
        // exact size, so the kernel writes only inside it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the kernel filled the whole struct, and it was zeroed first.
        let info = unsafe { info.assume_init() };
        // `vip_path` is a fixed MAXPATHLEN array that the kernel
        // NUL-terminates; reading it as bytes never runs past the array.
        let path: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .iter()
            .flatten()
            .map(|&c| c as u8)
            .take_while(|&byte| byte != 0)
            .collect();
        (!path.is_empty()).then(|| String::from_utf8_lossy(&path).into_owned())
    }

    fn all_pids(&self) -> Vec<u32> {
        self.entries.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Kills the listed pids when a test ends, including on a failed
    /// assertion, so no `sleep` outlives the run.
    struct KillOnDrop(Vec<u32>);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            for &pid in &self.0 {
                if let Ok(pid) = libc::pid_t::try_from(pid) {
                    // SAFETY: signalling a pid has no memory effects; a pid
                    // that already exited returns ESRCH.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }

    /// Spawns `sh -c 'cd DIR && exec sleep 30'` under a fresh parent shell and
    /// walks from the parent, which proves children, argv, and cwd come from
    /// the kernel for exactly that tree.
    #[test]
    fn walks_a_live_tree_and_reads_argv_and_cwd() {
        let dir = std::env::temp_dir().canonicalize().expect("temp dir");
        let mut parent = Command::new("/bin/sh")
            .args([
                "-c",
                "/bin/sh -c 'cd \"$0\" && exec /bin/sleep 30' \"$1\"; wait",
                "kettle-walk-test",
            ])
            .arg(&dir)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn test tree");
        let root = parent.id();
        let mut cleanup = KillOnDrop(vec![root]);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut tree = MacProcessTree::default();
        let found = loop {
            // A descheduled runner can overrun the 25 ms walk deadline, which
            // only means this pass was not published; try again.
            let complete = tree.refresh_roots(&[root]);
            let sleeper = complete
                .then(|| tree.all_pids())
                .into_iter()
                .flatten()
                .find(|&pid| {
                    tree.argv_of(pid)
                        .is_some_and(|argv| argv == ["/bin/sleep", "30"])
                });
            if let Some(pid) = sleeper {
                cleanup.0.push(pid);
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "sleeper never appeared under {root}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };

        assert!(
            tree.parent_of(found).is_some(),
            "walked process has no parent"
        );
        assert_eq!(
            tree.cwd_of(found).map(std::path::PathBuf::from),
            Some(dir),
            "cwd of the walked process"
        );
        assert!(
            tree.all_pids().len() <= 3,
            "walk reached processes outside the test tree: {:?}",
            tree.all_pids()
        );
        assert!(tree.bytes_read() > 0);
        drop(cleanup);
        let _ = parent.wait();
    }

    #[test]
    fn reads_this_process_argv_like_std_does() {
        let mut tree = MacProcessTree::default();
        let mut total = 0;
        let ArgvRead::Complete(argv) = tree.read_argv(std::process::id(), &mut total) else {
            panic!("own argv should be readable");
        };
        let expected: Vec<String> = std::env::args().filter(|arg| !arg.is_empty()).collect();
        assert_eq!(argv, expected);
        assert!(total > 0);
    }

    #[test]
    #[ignore = "run as a subprocess by a_large_environment_does_not_displace_argv"]
    fn argv_probe_sleeper() {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }

    /// The kernel hides the environment of Apple's own binaries from
    /// `KERN_PROCARGS2`, so this uses the test binary itself. A read sized
    /// below its argument area would return the tail, the environment, as argv.
    #[test]
    fn a_large_environment_does_not_displace_argv() {
        let exe = std::env::current_exe().expect("test binary path");
        let args = [
            "macos::tests::argv_probe_sleeper",
            "--ignored",
            "--exact",
            "--test-threads=1",
        ];
        let mut child = Command::new(&exe)
            .args(args)
            .env("KETTLE_ARGV_PROBE_PADDING", "E".repeat(64 * 1024))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn argv probe");
        let _cleanup = KillOnDrop(vec![child.id()]);
        let expected: Vec<String> = std::iter::once(exe.to_string_lossy().into_owned())
            .chain(args.iter().map(|arg| (*arg).to_string()))
            .collect();
        let mut tree = MacProcessTree::default();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut total = 0;
            // Before exec completes the child still carries the test
            // harness's own argv, so only an exact match ends the wait.
            if let ArgvRead::Complete(argv) = tree.read_argv(child.id(), &mut total)
                && argv == expected
            {
                let charged: usize = expected.iter().map(|arg| arg.len() + 1).sum();
                assert_eq!(total, charged as u64, "only argv bytes are charged");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "probe argv never matched {expected:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_long_argv_holds_back_only_its_own_pane() {
        let long = crate::spawn_detached_shell("/bin/sleep 30; true", 300);
        let short = crate::spawn_detached_shell("/bin/sleep 30; true", 0);
        struct Stop(u32, u32);
        impl Drop for Stop {
            fn drop(&mut self) {
                crate::kill_detached_shell(self.0);
                crate::kill_detached_shell(self.1);
            }
        }
        let _cleanup = Stop(long, short);
        let mut tree = MacProcessTree::default();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if tree.refresh_roots(&[long, short])
                && tree.partial_roots.contains(&long)
                && tree.argv_of(short).is_some()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the long argv was never isolated"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!tree.partial_roots.contains(&short));
    }

    #[test]
    fn a_missing_root_publishes_an_empty_complete_tree() {
        let mut tree = MacProcessTree::default();
        // Pids are bounded well below this on macOS (`PID_MAX` is 99998).
        assert!(tree.refresh_roots(&[999_999]));
        assert!(tree.all_pids().is_empty());
    }

    #[test]
    fn another_users_process_is_unavailable_not_incomplete() {
        // launchd is root's and its argv is unreadable to us; the walk must
        // still publish rather than hold the previous snapshot forever.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root can read launchd's argv");
            return;
        }
        let mut tree = MacProcessTree::default();
        let mut children = Vec::new();
        assert!(tree.list_children(1, &mut children).is_some());
        let mut total = 0;
        assert!(matches!(
            tree.read_argv(1, &mut total),
            ArgvRead::Unavailable
        ));
    }
}
