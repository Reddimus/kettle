//! Who is on the other end of a control connection.
//!
//! The kernel names the connecting process; its first request claims the
//! same pid and start instant. Only when both agree, and the process is
//! still that instance, does [`verify_chain`] walk its ancestors. The App
//! then looks for one of its panes in the chain: a caller is *verified* in a
//! pane only when the pane's own child process is an ancestor of it.
//!
//! Every failure is a fixed [`UnverifiedReason`], never a partial answer.
//! Verification describes the process the kernel names when Kettle accepts
//! the connection, before reading any request, and the first request must
//! claim that process. Linux and Windows name the process that connected.
//! macOS names the last process to use the socket by then, so a descriptor
//! handed on before Kettle accepts binds to the process that received it,
//! which must then claim itself and run in the pane: an outside process
//! reaches a pane only through the cooperation of a process inside it. A
//! hand-off after acceptance leaves the receiver unable to match the claim,
//! and no platform here names the writer of each later frame.

use std::time::Instant;

use crate::process::{InspectError, ProcessIdentity, ProcessSnapshot};
use crate::protocol::PeerClaim;
use crate::transport::CtlStream;

/// Parent links a walk may follow. The caller is at depth 0, so a pane child
/// 64 links above it still verifies and one 65 links above does not.
pub const MAX_PARENT_LINKS: usize = 64;

/// Why a caller is not verified. The wire form is the snake-case name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiedReason {
    /// The connection's first request carried no claim, or no start token.
    MissingClaim,
    /// The first frame was malformed, so it fixed no usable claim.
    InvalidClaim,
    /// A later request claimed a different process than the first.
    ClaimChanged,
    /// The kernel could not name the connecting process.
    PeerUnavailable,
    /// The claim does not match the kernel's view of the connection.
    ClaimMismatch,
    /// The connecting process has exited or been replaced.
    PeerExited,
    /// The process now holding the peer's pid started after the connection.
    PeerStartedAfterAccept,
    /// An ancestor's description made no sense.
    AncestorUnavailable,
    /// A parent started after its child: the pid was reused.
    ParentYoungerThanChild,
    /// A process changed parent or identity while it was being read.
    ChainChanged,
    /// The walk looped.
    Cycle,
    /// No pane within [`MAX_PARENT_LINKS`] links.
    DepthExceeded,
    /// The walk ran out of time.
    DeadlineExceeded,
    /// The chain names no live pane of this Kettle.
    NoPane,
    /// This platform cannot inspect processes.
    Unsupported,
}

impl UnverifiedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingClaim => "missing_claim",
            Self::InvalidClaim => "invalid_claim",
            Self::ClaimChanged => "claim_changed",
            Self::PeerUnavailable => "peer_unavailable",
            Self::ClaimMismatch => "claim_mismatch",
            Self::PeerExited => "peer_exited",
            Self::PeerStartedAfterAccept => "peer_started_after_accept",
            Self::AncestorUnavailable => "ancestor_unavailable",
            Self::ParentYoungerThanChild => "parent_younger_than_child",
            Self::ChainChanged => "chain_changed",
            Self::Cycle => "cycle",
            Self::DepthExceeded => "depth_exceeded",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::NoPane => "no_pane",
            Self::Unsupported => "unsupported",
        }
    }
}

/// "Now" in the unit process start instants use, so a start can be compared
/// with the moment a connection was accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessClock(u64);

impl ProcessClock {
    pub fn now() -> Option<Self> {
        clock::now().map(Self)
    }
}

/// What the kernel said about a connection's peer the moment it was accepted.
#[derive(Debug, Clone, Copy)]
pub struct PeerCapture {
    peer: Result<ProcessIdentity, UnverifiedReason>,
    accepted_at: Option<ProcessClock>,
}

impl PeerCapture {
    /// Capture `stream`'s peer. Call it right after accept, before reading
    /// any request bytes.
    pub fn capture(stream: &CtlStream) -> Self {
        let accepted_at = ProcessClock::now();
        let peer = stream
            .peer_pid()
            .map_err(|_| UnverifiedReason::PeerUnavailable)
            .and_then(|pid| {
                crate::process::identity(pid).map_err(|error| match error {
                    InspectError::Unsupported => UnverifiedReason::Unsupported,
                    InspectError::Unavailable | InspectError::InvalidData => {
                        UnverifiedReason::PeerUnavailable
                    }
                })
            });
        Self { peer, accepted_at }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(peer: ProcessIdentity, accepted_at: u64) -> Self {
        Self {
            peer: Ok(peer),
            accepted_at: Some(ProcessClock(accepted_at)),
        }
    }
}

/// Where a walk reads processes. Production reads the OS; tests script it.
pub(crate) trait ProcessSource {
    fn inspect(&self, pid: u32) -> Result<ProcessSnapshot, InspectError>;
}

struct Os;

impl ProcessSource for Os {
    fn inspect(&self, pid: u32) -> Result<ProcessSnapshot, InspectError> {
        crate::process::inspect(pid)
    }
}

/// Check a connection's claim against its capture, then return the peer's
/// ancestry, nearest first, starting with the peer itself. The walk stops at
/// `stop_at` (this server's own pid, an ancestor of every pane), at a process
/// with no parent, or after [`MAX_PARENT_LINKS`] links.
pub fn verify_chain(
    capture: &PeerCapture,
    claim: &PeerClaim,
    stop_at: u32,
    deadline: Instant,
) -> Result<Vec<ProcessIdentity>, UnverifiedReason> {
    walk(&Os, capture, claim, stop_at, deadline)
}

pub(crate) fn walk(
    source: &impl ProcessSource,
    capture: &PeerCapture,
    claim: &PeerClaim,
    stop_at: u32,
    deadline: Instant,
) -> Result<Vec<ProcessIdentity>, UnverifiedReason> {
    let accepted = capture.peer?;
    let start = claim.start_token.ok_or(UnverifiedReason::MissingClaim)?;
    if claim.pid.get() != accepted.pid() || start.0 != accepted.start() {
        return Err(UnverifiedReason::ClaimMismatch);
    }
    let accepted_at = capture
        .accepted_at
        .ok_or(UnverifiedReason::PeerUnavailable)?;
    if accepted.start() > accepted_at.0 {
        return Err(UnverifiedReason::PeerStartedAfterAccept);
    }
    let fail = |error: InspectError, otherwise: UnverifiedReason| match error {
        InspectError::Unsupported => UnverifiedReason::Unsupported,
        InspectError::Unavailable | InspectError::InvalidData => otherwise,
    };
    let peer = source
        .inspect(accepted.pid())
        .map_err(|error| fail(error, UnverifiedReason::PeerExited))?;
    if peer.identity != accepted || peer.exited {
        return Err(UnverifiedReason::PeerExited);
    }
    match follow_parents(source, peer, Some(stop_at), deadline) {
        (chain, None) => Ok(chain),
        (_, Some(reason)) => Err(reason),
    }
}

/// This process's ancestry, nearest first, starting with itself, by the same
/// rules a server checks a caller with. A client uses it to find the Kettle
/// it runs inside. A link that fails a check ends the chain there: every
/// ancestor before it was checked, so a broken link above the Kettle (a
/// launcher whose pid was reused, say) does not hide that Kettle.
pub fn current_ancestry(deadline: Instant) -> Vec<ProcessIdentity> {
    crate::process::current()
        .ok()
        .map(|me| follow_parents(&Os, me, None, deadline).0)
        .unwrap_or_default()
}

/// Follow `first`'s parents, nearest first, until `stop_at`, a process with
/// no readable parent, or [`MAX_PARENT_LINKS`] links. Returns the checked
/// chain and, when a link failed a check, why the walk stopped there.
fn follow_parents(
    source: &impl ProcessSource,
    first: ProcessSnapshot,
    stop_at: Option<u32>,
    deadline: Instant,
) -> (Vec<ProcessIdentity>, Option<UnverifiedReason>) {
    let fail = |error: InspectError, otherwise: UnverifiedReason| match error {
        InspectError::Unsupported => UnverifiedReason::Unsupported,
        InspectError::Unavailable | InspectError::InvalidData => otherwise,
    };
    let mut chain = vec![first.identity];
    let mut child = first;
    loop {
        if Instant::now() >= deadline {
            return (chain, Some(UnverifiedReason::DeadlineExceeded));
        }
        let Some(parent_pid) = child.parent_pid else {
            return (chain, None);
        };
        if Some(parent_pid) == stop_at {
            return (chain, None);
        }
        if chain.len() > MAX_PARENT_LINKS {
            return (chain, Some(UnverifiedReason::DepthExceeded));
        }
        // A parent this process cannot read (gone, or another user's, such
        // as launchd above every app) ends the chain: nothing above it can be
        // one of this Kettle's panes reached through it.
        let parent = match source.inspect(parent_pid) {
            Ok(parent) if !parent.exited => parent,
            Ok(_) | Err(InspectError::Unavailable) => return (chain, None),
            Err(error) => {
                let reason = fail(error, UnverifiedReason::AncestorUnavailable);
                return (chain, Some(reason));
            }
        };
        if parent.identity.start() > child.identity.start() {
            return (chain, Some(UnverifiedReason::ParentYoungerThanChild));
        }
        // The child must still be the same process with the same parent,
        // or the parent just read may be a stranger that took the pid.
        let again = match source.inspect(child.identity.pid()) {
            Ok(again) => again,
            Err(error) => return (chain, Some(fail(error, UnverifiedReason::ChainChanged))),
        };
        if again.identity != child.identity || again.parent_pid != Some(parent_pid) || again.exited
        {
            return (chain, Some(UnverifiedReason::ChainChanged));
        }
        if chain.contains(&parent.identity) {
            return (chain, Some(UnverifiedReason::Cycle));
        }
        chain.push(parent.identity);
        child = parent;
    }
}

#[cfg(target_os = "linux")]
mod clock {
    /// Clock ticks since boot, the unit of `/proc/<pid>/stat` field 22.
    pub(super) fn now() -> Option<u64> {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is a valid out-parameter for the call.
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) } != 0 {
            return None;
        }
        // SAFETY: plain call.
        let ticks = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
        let seconds = u64::try_from(now.tv_sec).ok()?;
        let nanos = u64::try_from(now.tv_nsec).ok()?;
        seconds
            .checked_mul(ticks)?
            .checked_add(nanos.checked_mul(ticks)? / 1_000_000_000)
    }
}

#[cfg(target_os = "macos")]
mod clock {
    /// Microseconds since the epoch, the unit of `proc_bsdinfo` start times.
    pub(super) fn now() -> Option<u64> {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        u64::try_from(since.as_micros()).ok()
    }
}

#[cfg(windows)]
mod clock {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

    /// A `FILETIME`, the unit of process creation times.
    pub(super) fn now() -> Option<u64> {
        let mut now = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: `now` is a valid out-parameter for the call.
        unsafe { GetSystemTimeAsFileTime(&mut now) };
        Some((u64::from(now.dwHighDateTime) << 32) | u64::from(now.dwLowDateTime))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod clock {
    pub(super) fn now() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::StartToken;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::Duration;

    /// A scripted process table. `reads` counts inspections per pid so a test
    /// can change the table between a parent read and the child's re-read.
    #[derive(Default)]
    struct Table {
        processes: RefCell<HashMap<u32, ProcessSnapshot>>,
        after_reads: RefCell<Vec<(u32, usize, Option<ProcessSnapshot>)>>,
        reads: RefCell<HashMap<u32, usize>>,
    }

    impl Table {
        fn with(processes: &[(u32, u64, Option<u32>)]) -> Self {
            let table = Self::default();
            for &(pid, start, parent) in processes {
                table.set(pid, start, parent);
            }
            table
        }

        fn set(&self, pid: u32, start: u64, parent: Option<u32>) {
            self.processes
                .borrow_mut()
                .insert(pid, snapshot(pid, start, parent));
        }

        /// After `pid` has been read `count` times, replace (or remove) it.
        fn then(&self, pid: u32, count: usize, next: Option<ProcessSnapshot>) {
            self.after_reads.borrow_mut().push((pid, count, next));
        }
    }

    impl ProcessSource for Table {
        fn inspect(&self, pid: u32) -> Result<ProcessSnapshot, InspectError> {
            let result = self
                .processes
                .borrow()
                .get(&pid)
                .copied()
                .ok_or(InspectError::Unavailable);
            let mut reads = self.reads.borrow_mut();
            let count = reads.entry(pid).or_default();
            *count += 1;
            for (target, after, next) in self.after_reads.borrow().iter() {
                if *target == pid && *after == *count {
                    match next {
                        Some(next) => self.processes.borrow_mut().insert(pid, *next),
                        None => self.processes.borrow_mut().remove(&pid),
                    };
                }
            }
            result
        }
    }

    fn snapshot(pid: u32, start: u64, parent: Option<u32>) -> ProcessSnapshot {
        ProcessSnapshot {
            identity: ProcessIdentity::new(pid, start),
            parent_pid: parent,
            exited: false,
        }
    }

    fn claim(pid: u32, start: Option<u64>) -> PeerClaim {
        PeerClaim {
            pid: std::num::NonZeroU32::new(pid).unwrap(),
            start_token: start.map(StartToken),
            pane_hint: None,
            pid_hint: None,
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    const SERVER: u32 = 100;

    /// caller 300 (start 30) → shell 200 (start 20) → server 100.
    fn plain() -> Table {
        Table::with(&[
            (300, 30, Some(200)),
            (200, 20, Some(SERVER)),
            (SERVER, 10, Some(1)),
        ])
    }

    fn chain_pids(chain: &[ProcessIdentity]) -> Vec<u32> {
        chain.iter().map(|identity| identity.pid()).collect()
    }

    #[test]
    fn a_matching_claim_walks_to_the_server() {
        let table = plain();
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        let chain = walk(&table, &capture, &claim(300, Some(30)), SERVER, soon()).unwrap();
        assert_eq!(chain_pids(&chain), [300, 200]);
        assert_eq!(chain[1], ProcessIdentity::new(200, 20));
    }

    #[test]
    fn equal_start_ticks_are_allowed() {
        let table = Table::with(&[(300, 20, Some(200)), (200, 20, Some(SERVER))]);
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 20), 20);
        assert!(walk(&table, &capture, &claim(300, Some(20)), SERVER, soon()).is_ok());
    }

    #[test]
    fn a_pid_alone_or_a_different_claim_never_verifies() {
        let table = plain();
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        for (claimed, expected) in [
            (claim(300, None), UnverifiedReason::MissingClaim),
            (claim(300, Some(29)), UnverifiedReason::ClaimMismatch),
            (claim(301, Some(30)), UnverifiedReason::ClaimMismatch),
        ] {
            assert_eq!(
                walk(&table, &capture, &claimed, SERVER, soon()),
                Err(expected)
            );
        }
    }

    /// S4's failure: the peer exits before accept and its pid is reused by a
    /// process inside a pane. The kernel names the reused pid; the claim
    /// (from the original client) cannot match the newcomer's start.
    #[test]
    fn a_pid_reused_before_accept_is_rejected_by_the_claim() {
        // The newcomer (start 35) lives in pane shell 200.
        let table = Table::with(&[(300, 35, Some(200)), (200, 20, Some(SERVER))]);
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 35), 40);
        // The original client claimed its own start, 30.
        assert_eq!(
            walk(&table, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::ClaimMismatch)
        );
        // A process that starts after the accept is never the peer.
        let late = PeerCapture::for_tests(ProcessIdentity::new(300, 50), 40);
        assert_eq!(
            walk(&table, &late, &claim(300, Some(50)), SERVER, soon()),
            Err(UnverifiedReason::PeerStartedAfterAccept)
        );
    }

    #[test]
    fn a_peer_that_exits_or_is_replaced_after_accept_fails_closed() {
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        let gone = Table::with(&[(200, 20, Some(SERVER))]);
        assert_eq!(
            walk(&gone, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::PeerExited)
        );
        let replaced = Table::with(&[(300, 45, Some(200)), (200, 20, Some(SERVER))]);
        assert_eq!(
            walk(&replaced, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::PeerExited)
        );
        let zombie = plain();
        zombie.processes.borrow_mut().get_mut(&300).unwrap().exited = true;
        assert_eq!(
            walk(&zombie, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::PeerExited)
        );
    }

    #[test]
    fn unstable_parent_links_fail_closed() {
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        // The parent's pid now belongs to a process younger than the child.
        let reused = Table::with(&[(300, 30, Some(200)), (200, 35, Some(SERVER))]);
        assert_eq!(
            walk(&reused, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::ParentYoungerThanChild)
        );
        // A parent that is gone or unreadable ends the chain at the caller,
        // so no pane above it can match.
        let orphaned = Table::with(&[(300, 30, Some(200))]);
        let chain = walk(&orphaned, &capture, &claim(300, Some(30)), SERVER, soon()).unwrap();
        assert_eq!(chain_pids(&chain), [300]);
        let exited = plain();
        exited.processes.borrow_mut().get_mut(&200).unwrap().exited = true;
        let chain = walk(&exited, &capture, &claim(300, Some(30)), SERVER, soon()).unwrap();
        assert_eq!(chain_pids(&chain), [300]);
        // The child was reparented between reading it and its parent.
        let moved = plain();
        moved.then(300, 1, Some(snapshot(300, 30, Some(1))));
        assert_eq!(
            walk(&moved, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::ChainChanged)
        );
        // A loop in the table.
        let looped = Table::with(&[
            (300, 30, Some(200)),
            (200, 20, Some(250)),
            (250, 20, Some(200)),
        ]);
        assert_eq!(
            walk(&looped, &capture, &claim(300, Some(30)), SERVER, soon()),
            Err(UnverifiedReason::Cycle)
        );
    }

    /// 64 links from the caller still reach the pane child; 65 do not.
    #[test]
    fn the_walk_follows_exactly_sixty_four_links() {
        let build = |links: u32| {
            // pid 1000 + depth; depth 0 is the caller, `links` is the pane
            // child, whose parent is the server.
            let mut rows = Vec::new();
            for depth in 0..=links {
                let parent = if depth == links {
                    SERVER
                } else {
                    1000 + depth + 1
                };
                rows.push((1000 + depth, 100 - u64::from(depth), Some(parent)));
            }
            Table::with(&rows)
        };
        let capture = PeerCapture::for_tests(ProcessIdentity::new(1000, 100), 200);
        let reached = walk(
            &build(64),
            &capture,
            &claim(1000, Some(100)),
            SERVER,
            soon(),
        )
        .unwrap();
        assert_eq!(reached.len(), 65);
        assert_eq!(reached.last().unwrap().pid(), 1064);
        assert_eq!(
            walk(
                &build(65),
                &capture,
                &claim(1000, Some(100)),
                SERVER,
                soon()
            ),
            Err(UnverifiedReason::DepthExceeded)
        );
    }

    #[test]
    fn a_spent_deadline_stops_the_walk() {
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        assert_eq!(
            walk(
                &plain(),
                &capture,
                &claim(300, Some(30)),
                SERVER,
                Instant::now()
            ),
            Err(UnverifiedReason::DeadlineExceeded)
        );
    }

    #[test]
    fn a_daemonized_caller_reaches_no_parent_and_ends_its_chain() {
        let table = Table::with(&[(300, 30, Some(1)), (1, 1, None)]);
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        let chain = walk(&table, &capture, &claim(300, Some(30)), SERVER, soon()).unwrap();
        assert_eq!(chain_pids(&chain), [300, 1]);
    }

    /// A client keeps the ancestors it checked before a broken link: the
    /// Kettle below a reused launcher pid is still its Kettle.
    #[test]
    fn a_broken_link_above_keeps_the_checked_ancestors() {
        // me 300 → shell 200 → kettle 100 → "launcher" 50, whose pid now
        // belongs to a process younger than the kettle.
        let table = Table::with(&[
            (300, 30, Some(200)),
            (200, 20, Some(100)),
            (100, 10, Some(50)),
            (50, 99, None),
        ]);
        let start = table.inspect(300).unwrap();
        let (chain, stopped) = follow_parents(&table, start, None, soon());
        assert_eq!(chain_pids(&chain), [300, 200, 100]);
        assert_eq!(stopped, Some(UnverifiedReason::ParentYoungerThanChild));
        // The server path still treats the same break as unverified.
        let capture = PeerCapture::for_tests(ProcessIdentity::new(300, 30), 40);
        assert_eq!(
            walk(&table, &capture, &claim(300, Some(30)), 1, soon()),
            Err(UnverifiedReason::ParentYoungerThanChild)
        );
    }

    #[test]
    fn reasons_have_stable_wire_names() {
        assert_eq!(UnverifiedReason::MissingClaim.as_str(), "missing_claim");
        assert_eq!(UnverifiedReason::NoPane.as_str(), "no_pane");
        assert_eq!(
            UnverifiedReason::PeerStartedAfterAccept.as_str(),
            "peer_started_after_accept"
        );
    }

    const CLIENT_ENV: &str = "KETTLE_CTL_IDENTITY_CLIENT";

    /// Run as a re-executed child: connect to the endpoint in `CLIENT_ENV`,
    /// claim this process's own identity, and wait for the server's go-ahead
    /// before exiting. A no-op in the ordinary test run.
    #[test]
    fn identity_client_helper() {
        use std::io::{Read as _, Write as _};
        let Some(endpoint) = std::env::var_os(CLIENT_ENV) else {
            return;
        };
        let mut stream = crate::transport::connect(endpoint.to_str().unwrap()).expect("connect");
        let me = crate::process::current().unwrap().identity;
        stream
            .write_all(format!("{} {}\n", me.pid(), me.start()).as_bytes())
            .expect("send claim");
        let mut go = [0u8; 1];
        let _ = stream.read(&mut go);
    }

    /// The real OS path through a real intermediate process: a shell starts
    /// the client, and the server, its grandparent, verifies the chain up to
    /// itself. The shell's command ends in a second command, so the shell
    /// cannot `exec` the client away.
    #[test]
    fn a_grandchild_through_a_shell_verifies_up_to_the_server() {
        use std::io::{Read as _, Write as _};
        let endpoint = crate::transport::tests::test_endpoint("identity-grandchild");
        let listener = crate::transport::CtlListener::bind(&endpoint).expect("bind");
        let exe = std::env::current_exe().unwrap();
        let name = "identity::tests::identity_client_helper";
        #[cfg(unix)]
        let mut shell = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "\"$0\" --exact \"$1\" --nocapture --test-threads=1; true",
            ])
            .arg(&exe)
            .arg(name)
            .env(CLIENT_ENV, &endpoint)
            .spawn()
            .expect("spawn shell");
        #[cfg(windows)]
        let mut shell = std::process::Command::new("cmd")
            .arg("/C")
            .arg(&exe)
            .args([
                "--exact",
                name,
                "--nocapture",
                "--test-threads=1",
                "&",
                "ver",
            ])
            .env(CLIENT_ENV, &endpoint)
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn shell");
        let mut conn = listener.accept().expect("accept");
        let capture = PeerCapture::capture(&conn);
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while conn.read(&mut byte).expect("read claim") == 1 && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        let line = String::from_utf8(line).unwrap();
        let (pid, start) = line.split_once(' ').expect("pid and start");
        let (pid, start): (u32, u64) = (pid.parse().unwrap(), start.parse().unwrap());
        let chain = verify_chain(
            &capture,
            &claim(pid, Some(start)),
            std::process::id(),
            soon(),
        );
        conn.write_all(b"g").ok();
        let status = shell.wait().expect("shell exits");
        let chain = chain.expect("the grandchild verifies");
        assert!(status.success());
        assert_eq!(chain.len(), 2, "client, then the shell, then this server");
        assert_eq!(chain[0], ProcessIdentity::new(pid, start));
        assert_eq!(chain[1].pid(), shell.id());
        // A claim for anything but the connecting process fails.
        let other = verify_chain(
            &capture,
            &claim(shell.id(), Some(start)),
            std::process::id(),
            soon(),
        );
        assert_eq!(other, Err(UnverifiedReason::ClaimMismatch));
    }

    /// The real OS path: this process connects to itself and walks up to its
    /// own parent.
    #[test]
    fn a_real_connection_verifies_this_process() {
        let endpoint = crate::transport::tests::test_endpoint("identity-self");
        let listener = crate::transport::CtlListener::bind(&endpoint).expect("bind");
        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept");
            let capture = PeerCapture::capture(&conn);
            (conn, capture)
        });
        let _client = crate::transport::connect(&endpoint).expect("connect");
        let (_conn, capture) = server.join().unwrap();
        let me = crate::process::current().unwrap();
        // Stop at the parent, as a server stops at itself: ancestors above it
        // may belong to other users and cannot be read.
        let parent = me.parent_pid.expect("a test process has a parent");
        let chain = verify_chain(
            &capture,
            &claim(me.identity.pid(), Some(me.identity.start())),
            parent,
            soon(),
        )
        .expect("verify self");
        assert_eq!(chain, [me.identity]);
        assert_eq!(
            verify_chain(
                &capture,
                &claim(me.identity.pid(), Some(me.identity.start() + 1)),
                parent,
                soon()
            ),
            Err(UnverifiedReason::ClaimMismatch)
        );
    }
}
