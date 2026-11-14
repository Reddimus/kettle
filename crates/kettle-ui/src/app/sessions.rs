//! Saving and restoring the workspace session.

use super::*;

/// Receive one legacy handoff; the sender closes its write half after the JSON.
/// One deadline bounds the entire receive, including fragmented sends.
#[cfg(unix)]
fn receive_handoff_session(
    socket: &std::os::unix::net::UnixStream,
) -> std::io::Result<crate::session::Session> {
    receive_handoff_session_until(
        socket,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
}

#[cfg(unix)]
fn receive_handoff_session_until(
    socket: &std::os::unix::net::UnixStream,
    deadline: std::time::Instant,
) -> std::io::Result<crate::session::Session> {
    const LIMIT: usize = 1024 * 1024;
    // macOS refuses SO_RCVTIMEO after the peer shuts down its write half.
    // Readiness and nonblocking recvmsg keep every fragment on one deadline.
    socket.set_nonblocking(true)?;
    let mut payload = vec![0u8; LIMIT + 1];
    let mut used = 0;
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "tab handoff receive deadline expired",
                )
            })?;
        use std::os::fd::AsRawFd as _;
        let mut readiness = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = (remaining.as_millis() + 1).min(i32::MAX as u128) as i32;
        // SAFETY: readiness describes the live socket for this one poll call.
        let ready = unsafe { libc::poll(&mut readiness, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 || std::time::Instant::now() >= deadline {
            continue;
        }
        let (n, fds) = match crate::fd_transport::recv_fds(socket, &mut payload[used..], 64) {
            Ok(received) => received,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        for fd in fds {
            // SAFETY: recv_fds transferred ownership; PTY adoption is unsupported.
            unsafe {
                libc::close(fd);
            }
        }
        if n == 0 {
            break;
        }
        used += n;
        if used > LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "tab handoff exceeds 1 MiB",
            ));
        }
    }
    serde_json::from_slice(&payload[..used])
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub(super) fn session_windows_for_save(
    mut windows: Vec<(u64, crate::session::SWindow)>,
    focus_order: &VecDeque<u64>,
) -> Vec<crate::session::SWindow> {
    windows.retain(|(_, window)| !window.tabs.is_empty());
    windows.sort_by_key(|(seq, _)| {
        (
            focus_order
                .iter()
                .position(|focused| focused == seq)
                .unwrap_or(usize::MAX),
            *seq,
        )
    });
    if windows.len() > crate::session::MAX_RESTORE_WINDOWS {
        log::debug!(
            "session truncated: retaining {} of {} nonempty windows in focus order",
            crate::session::MAX_RESTORE_WINDOWS,
            windows.len()
        );
        windows.truncate(crate::session::MAX_RESTORE_WINDOWS);
    }
    windows.into_iter().map(|(_, window)| window).collect()
}

pub(super) fn record_window_focus(order: &mut VecDeque<u64>, seq: u64) {
    order.retain(|known| *known != seq);
    order.push_front(seq);
}

impl App {
    /// Snapshot ONE window (its tabs + geometry).
    pub(super) fn snapshot_window(w: &WindowState) -> crate::session::SWindow {
        let s = w.mux.snapshot();
        let geometry = w.window.as_ref().and_then(|win| {
            // A minimized window does not report where it will come back to.
            // Win32 answers `GetWindowRect` with the (-32000, -32000) sentinel
            // and `GetClientRect` with 0x0, and winit passes both through
            // verbatim, so snapshotting one persisted a geometry that the
            // restore clamp could only rescue as far as its 160x120 floor —
            // the window came back as an unusable stub. Saving no geometry
            // restores it at the default size instead, which is the honest
            // answer to "we do not know where this window was".
            //
            // The size check is not redundant with `is_minimized`: it is the
            // observable that actually breaks the restore, and it also covers
            // a window reporting a degenerate size for any other reason.
            if win.is_minimized().unwrap_or(false) {
                return None;
            }
            let size = win.inner_size();
            if size.width == 0 || size.height == 0 {
                return None;
            }
            let pos = win.outer_position().ok()?;
            Some(crate::session::SGeometry {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
            })
        });
        crate::session::SWindow {
            tabs: s.tabs,
            active: s.active,
            geometry,
        }
    }

    pub(super) fn snapshot_session<'a>(
        ws: &'a WindowState,
        others: impl Iterator<Item = &'a WindowState>,
        quick_window: Option<u64>,
        focus_order: &VecDeque<u64>,
    ) -> Option<crate::session::Session> {
        // Include the checked-out window; the save budget prefers recently
        // focused nonempty windows without restricting window creation.
        let mut wins: Vec<(u64, crate::session::SWindow)> = Vec::new();
        for window in std::iter::once(ws).chain(others) {
            if quick_window != Some(window.seq) {
                wins.push((window.seq, Self::snapshot_window(window)));
            }
        }
        if wins.is_empty() {
            return None;
        }
        let windows = session_windows_for_save(wins, focus_order);
        // Mirror the first retained window for older readers.
        let s = crate::session::Session {
            tabs: windows.first().map(|w| w.tabs.clone()).unwrap_or_default(),
            active: windows.first().map(|w| w.active).unwrap_or(0),
            theme: None,
            windows,
        };
        Some(s)
    }

    pub(super) fn clear_disabled_content(&self) {
        if self.other_builds.running() {
            return;
        }
        let disabled = self.cfg.restore_scrollback_lines == 0
            || (self.startup.layout.is_none()
                && !should_restore_session(self.startup.restore, self.cfg.restore_session, false));
        if disabled {
            // Whatever is written next is a fresh snapshot, never a keep.
            self.content_throttle.borrow_mut().reset();
            self.session_writer
                .remove_content(self.startup.layout.clone());
        }
    }

    /// Where automatic saves go right now, before any snapshot is taken.
    pub(super) fn autosave_target(&self) -> SessionWriteTarget<'_> {
        session_write_target(
            self.startup.layout.as_deref(),
            self.named_layout_writable,
            should_restore_session(
                self.startup.restore,
                self.cfg.restore_session,
                self.other_builds.running(),
            ),
            false,
        )
    }

    /// [`autosave_eligible`] for this launch, as the sweep's wake asks it.
    pub(super) fn session_autosave_eligible(&self) -> bool {
        let quick = self.quick_terminal.model.window;
        autosave_eligible(
            self.session_load_failed,
            &self.autosave_target(),
            self.windows
                .values()
                .any(|w| quick != Some(w.seq) && !w.mux.tabs.is_empty()),
        )
    }

    /// Save the session after a gesture or an explicit request.
    pub(super) fn save_session(&self, ws: &WindowState) {
        self.save_session_as(ws, SaveKind::Gesture);
    }

    pub(super) fn save_session_as(&self, ws: &WindowState, kind: SaveKind) {
        self.clear_disabled_content();
        if self.session_load_failed {
            return;
        }
        let target = self.autosave_target();
        if target == SessionWriteTarget::Skip {
            // Nothing is owed while nothing can be written.
            self.content_throttle.borrow_mut().pending = false;
            return;
        }
        let Some(s) = Self::snapshot_session(
            ws,
            self.windows.values(),
            self.quick_terminal.model.window,
            &self.window_focus_order,
        ) else {
            return;
        };
        // When launched with `--layout NAME`, save to the
        // named-layout file instead of the default session.json. Lets
        // the user maintain distinct workspaces ("dev", "ops", "docs")
        // without each one clobbering the others on close.
        // Only write the DEFAULT session.json when this
        // launch is in restore mode — symmetric with the opt-in load gate. A
        // fresh (non-opted-in) window must NOT overwrite the saved layout that
        // `--restore` / `restore-session = true` exists to recover.
        let layout = match target {
            SessionWriteTarget::NamedLayout(name) if !s.is_empty() => Some(name.to_string()),
            SessionWriteTarget::DefaultSession => None,
            _ => return,
        };
        let stale = self.session_writer.take_content_stale();
        let content = if self.cfg.restore_scrollback_lines > 0 {
            let mut windows: Vec<_> = std::iter::once(ws)
                .chain(self.windows.values())
                .filter(|w| {
                    self.quick_terminal.model.window != Some(w.seq) && !w.mux.tabs.is_empty()
                })
                .collect();
            windows.sort_by_key(|w| {
                (
                    self.window_focus_order
                        .iter()
                        .position(|seq| *seq == w.seq)
                        .unwrap_or(usize::MAX),
                    w.seq,
                )
            });
            windows.truncate(crate::session::MAX_RESTORE_WINDOWS);
            // The key is read before any grid, and decides whether to walk
            // them at all.
            let session_hash = crate::session_content::session_hash(&s);
            let key = crate::session_content::ContentKey::new(
                session_hash,
                self.cfg.restore_scrollback_lines,
                self.cfg.restore_scrollback_remote,
                windows
                    .iter()
                    .map(|w| (w.seq, w.mux.content_key_panes()))
                    .collect(),
            );
            let write = self.content_throttle.borrow_mut().decide(
                &key,
                std::time::Instant::now(),
                kind,
                stale,
            );
            if !write {
                crate::session::ContentJob::Keep
            } else {
                let mut budget = crate::session_content::TEXT_BUDGET;
                let snapshot = crate::session_content::ContentSnapshot::bound(
                    session_hash,
                    windows
                        .iter()
                        .map(|w| w.mux.snapshot_content(&self.cfg, &mut budget))
                        .collect(),
                );
                crate::session::ContentJob::Write(snapshot)
            }
        } else {
            self.content_throttle.borrow_mut().reset();
            crate::session::ContentJob::Remove
        };
        self.session_writer.submit_job(crate::session::SaveJob {
            session: Some(s),
            layout,
            content,
        });
    }

    /// Move a session aside when it loaded but could not be restored.
    ///
    /// Everything downstream of a rejected preflight behaves as if there were
    /// no session at all, including the next save, so without this the file is
    /// replaced by a single fresh tab before the user has finished looking at
    /// the window.
    pub(super) fn stash_unrestorable_session(&mut self, reason: &str) {
        recover_startup_snapshot(
            &mut self.restore_source,
            self.restore_identity.as_ref(),
            &mut self.session_load_failed,
            reason,
        );
    }

    /// Resolve the one startup restore source before window or GPU creation.
    ///
    /// Loading and validating here lets `resumed_inner` seed the first native
    /// window with its final saved geometry. That avoids showing/configuring a
    /// default-sized swapchain and immediately replacing it after PTYs spawn.
    pub(super) fn load_startup_session(&mut self) -> Option<crate::session::Session> {
        // Terminator parity (detachable tabs):
        // --tab-handoff-fd FD wins over the file handoff, named layout, and
        // default session. The validated socket is consumed exactly once.
        if let Some(handoff) = self.startup.tab_handoff_fd.take() {
            #[cfg(unix)]
            {
                let socket = handoff.socket;
                return match receive_handoff_session(&socket) {
                    Ok(session) => Some(session),
                    Err(error) => {
                        log::warn!("tab-handoff-fd: {error}");
                        None
                    }
                };
            }
            #[cfg(not(unix))]
            {
                let _ = handoff;
                log::warn!("--tab-handoff-fd unsupported on non-Unix");
                return None;
            }
        }

        // Record where the session came from as it is loaded. A rejected
        // restore has to move that exact file aside, and nothing downstream can
        // work it out afterwards: `session_write_target` routes on things this
        // function never sees, and a one-shot tab handoff has no durable file
        // by the time anyone asks.
        let session = if let Some(path) = self.startup.tab_handoff.as_deref() {
            // Deliberately not recorded: `load_tab_handoff` deletes the file as
            // it reads, so there is nothing left to keep.
            crate::session::Session::load_tab_handoff(path)
        } else if let Some(name) = self.startup.layout.as_deref() {
            self.restore_source = crate::session::Session::path_for_layout(name);
            let (session, identity) = loaded_startup_snapshot(
                crate::session::Session::load_layout_with_identity(name),
                &mut self.session_load_failed,
            )?;
            self.restore_identity = Some(identity);
            Some(session)
        } else if should_restore_session(
            self.startup.restore,
            self.cfg.restore_session,
            self.other_builds.running(),
        ) {
            self.restore_source = crate::session::Session::path();
            let (session, identity) = loaded_startup_snapshot(
                crate::session::Session::load_with_identity(),
                &mut self.session_load_failed,
            )?;
            self.restore_identity = Some(identity);
            Some(session)
        } else {
            if !self.other_builds.running()
                && let Some(path) = crate::session::Session::path()
            {
                crate::session_content::remove(&path);
            }
            None
        };
        if let Some(path) = self.restore_source.as_deref() {
            if self.cfg.restore_scrollback_lines == 0 {
                crate::session_content::remove(path);
            } else if let Some(session) = session.as_ref()
                && let Some(content) = crate::session_content::load(path, session)
            {
                self.restore_content = content.windows.into();
            }
        }
        session
    }
}

pub(super) fn recover_startup_snapshot(
    path: &mut Option<std::path::PathBuf>,
    identity: Option<&crate::session::SnapshotIdentity>,
    load_failed: &mut bool,
    reason: &str,
) {
    let Some(source) = path.as_deref() else {
        log::warn!("session could not be restored ({reason}); it had no file to keep");
        return;
    };
    // Preserve the exact snapshot read by this launch. If it changed, or the
    // backup failed, even a fresh/partial layout must never replace the file.
    let recovered = identity.is_some_and(|identity| {
        crate::session::stash_unrestorable_snapshot(source, identity, reason).is_ok()
    });
    *load_failed |= !recovered;
    if recovered {
        *path = None;
    }
}

pub(super) fn loaded_startup_snapshot(
    result: std::io::Result<Option<(crate::session::Session, crate::session::SnapshotIdentity)>>,
    load_failed: &mut bool,
) -> Option<(crate::session::Session, crate::session::SnapshotIdentity)> {
    match result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            *load_failed = true;
            log::warn!("could not load session: {error}; automatic saving is disabled");
            None
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::receive_handoff_session;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn fragmented_handoff_cannot_extend_receive_deadline() {
        use std::io::Write as _;
        use std::time::{Duration, Instant};
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            for _ in 0..100 {
                if sender.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let started = Instant::now();
        let result =
            super::receive_handoff_session_until(&receiver, started + Duration::from_millis(80));
        let elapsed = started.elapsed();
        drop(receiver);
        writer.join().unwrap();
        assert!(result.is_err());
        assert!(
            elapsed < Duration::from_millis(500),
            "fragments extended the receive to {elapsed:?}"
        );
    }

    #[test]
    fn handoff_receives_fragmented_session() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let small: libc::c_int = 4096;
        assert_eq!(
            // SAFETY: sender owns the live socket and small is readable c_int storage matching
            // SO_SNDBUF and its size.
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &small as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&small) as libc::socklen_t,
                )
            },
            0
        );
        let expected = crate::session::Session {
            active: 7,
            theme: Some("x".repeat(256 * 1024)),
            ..Default::default()
        };
        let payload = serde_json::to_vec(&expected).unwrap();
        let writer = std::thread::spawn(move || {
            let fd = std::fs::File::open("/dev/null").unwrap();
            crate::fd_transport::send_fds(&sender, &payload, &[fd.as_raw_fd()]).unwrap();
            sender.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let session = receive_handoff_session(&receiver).unwrap();
        writer.join().unwrap();
        assert_eq!(session.active, expected.active);
        assert_eq!(session.theme, expected.theme);
    }

    /// Runs alone so other tests cannot change the process descriptor table.
    #[test]
    fn handoff_failures_close_received_fds() {
        kettle_test_support::run_alone_in_child(
            "app::sessions::tests::handoff_failures_close_received_fds",
            handoff_failures_close_received_fds_alone,
        );
    }

    fn handoff_failures_close_received_fds_alone() {
        for payload in [b"{".to_vec(), vec![b' '; 1024 * 1024 + 1]] {
            assert_handoff_failure_closes_received_fds(payload, receive_handoff_session);
        }
    }

    #[test]
    fn handoff_timeouts_leave_no_received_fds() {
        kettle_test_support::run_alone_in_child(
            "app::sessions::tests::handoff_timeouts_leave_no_received_fds",
            || {
                assert_handoff_failure_closes_received_fds(b"{".to_vec(), |socket| {
                    let result =
                        super::receive_handoff_session_until(socket, std::time::Instant::now());
                    assert!(
                        matches!(&result, Err(error) if error.kind() == std::io::ErrorKind::TimedOut)
                    );
                    result
                });
            },
        );
    }

    fn assert_handoff_failure_closes_received_fds(
        payload: Vec<u8>,
        receive: impl FnOnce(&UnixStream) -> std::io::Result<crate::session::Session>,
    ) {
        let before = open_fds();
        let (sender, receiver) = UnixStream::pair().unwrap();
        let (passed, peer) = UnixStream::pair().unwrap();
        // The descriptor travels with the first byte; macOS refuses one
        // `sendmsg` of over 1 MiB with ancillary data.
        let payload_len = payload.len();
        let writer = std::thread::spawn(move || {
            use std::io::Write as _;
            crate::fd_transport::send_fds(&sender, &payload[..1], &[passed.as_raw_fd()]).unwrap();
            (&sender).write_all(&payload[1..]).unwrap();
            sender.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let result = receive(&receiver);
        writer.join().unwrap();
        assert!(result.is_err());
        drop(receiver);
        drop(peer);
        // Queued rights and macOS socket garbage collection can delay peer EOF
        // without retaining a descriptor in this process.
        assert_eq!(
            open_fds(),
            before,
            "a {payload_len}-byte handoff leaked process descriptors"
        );
    }

    fn open_fds() -> std::collections::BTreeSet<i32> {
        let fds: Vec<i32> = std::fs::read_dir("/dev/fd")
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        // The directory iterator has closed its own descriptor before this scan.
        fds.into_iter()
            .filter(|&fd| {
                // SAFETY: F_GETFD only queries the descriptor number, including closed ones.
                unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
            })
            .collect()
    }
}

/// How much of the saved session can be stale before it is written out.
///
/// Most of what `session.json` holds is announced by a gesture that already
/// saves — splitting, closing, opening a tab. Some of it is not: a shell `cd`
/// moves a pane's recorded directory, a divider drag moves a split ratio, a
/// renamed tab moves its title. Without the sweep, each would reach disk only
/// if some *later* gesture happened to save, so restoring a workspace could
/// bring back directories the user left an hour ago.
///
/// Saving on every turn instead would serialize the whole tree at PTY-output
/// rates. So the sweep rides on turns the event loop was running anyway, at
/// most once per interval. That alone leaves the last change before Kettle goes
/// quiet unwritten until something else happens, which on macOS can be the
/// next key press: an idle window there gets no turns at all. So a turn that
/// delivered an event and did not sweep asks for one wake at the end of the
/// interval ([`session_sweep_wake`]). The wake itself delivers no event and so
/// asks for nothing more, and an idle Kettle, which gets no events, never asks:
/// a crash loses at most this much, and idle stays idle. The write itself is
/// skipped when the serialized text matches what is already on disk, so a
/// sweep over an unchanged workspace costs one serialization and no I/O.
pub(super) const SESSION_SWEEP: std::time::Duration = std::time::Duration::from_secs(2);

/// When the session sweep last ran, and whether anything has happened since
/// that it has not seen.
///
/// Only [`SessionSweep::take_due`], the sweep itself, clears the flag.
#[derive(Debug, Default)]
pub(super) struct SessionSweep {
    last: Option<std::time::Instant>,
    touched: bool,
    wake: Option<std::time::Instant>,
}

impl SessionSweep {
    /// Something happened that may have changed what the session holds: any
    /// window event except a redraw, and any user event.
    pub(super) fn touch(&mut self) {
        self.touched = true;
    }

    /// Whether this window's turn sweeps, per [`session_sweep_due`]. A sweep
    /// starts a new interval and has seen everything so far.
    pub(super) fn take_due(&mut self, now: std::time::Instant, window_has_tabs: bool) -> bool {
        if !session_sweep_due(self.last, now, window_has_tabs) {
            return false;
        }
        self.last = Some(now);
        self.touched = false;
        self.wake = None;
        true
    }

    /// The wake this turn should ask for, once every window has had its turn.
    ///
    /// `pending` is a save the sweep owes but put off; it asks for the wake as
    /// a change does. `eligible` (see [`autosave_eligible`]) is asked at most
    /// once per interval, and a sweep that could not save then asks for
    /// nothing. A wake whose time has come without a sweep means no window had
    /// anything to sweep, so it is dropped rather than left in the past, where
    /// it would spin the loop.
    pub(super) fn deadline(
        &mut self,
        now: std::time::Instant,
        pending: bool,
        eligible: impl FnOnce() -> bool,
    ) -> Option<std::time::Instant> {
        if self.wake.is_some_and(|wake| wake <= now) {
            self.wake = None;
            self.touched = false;
        }
        if (self.touched || pending) && self.wake.is_none() {
            self.wake = session_sweep_wake(self.last, now, true, eligible());
            if self.wake.is_none() {
                self.touched = false;
            }
        }
        self.wake
    }
}

/// When to wake for the sweep a change is owed: the end of the current
/// interval, if something has happened since the last sweep (`touched`), this
/// launch saves at all (`eligible`) and the interval is still running. A sweep
/// that is already due runs on the turn it is due; with no sweep yet there is
/// no interval, and the first turn always sweeps.
pub(super) fn session_sweep_wake(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    touched: bool,
    eligible: bool,
) -> Option<std::time::Instant> {
    let due = last
        .filter(|_| touched && eligible)?
        .checked_add(SESSION_SWEEP)?;
    (due > now).then_some(due)
}

/// Whether automatic saving can write anything for this launch: its startup
/// snapshot was read, it has a destination, and some window other than the
/// quick terminal has tabs. The sweep's wake is armed only when it is.
pub(super) fn autosave_eligible(
    load_failed: bool,
    target: &SessionWriteTarget<'_>,
    any_window_with_tabs: bool,
) -> bool {
    !load_failed && *target != SessionWriteTarget::Skip && any_window_with_tabs
}

/// Whether a session sweep is owed as of `now`. The first turn always sweeps,
/// so a workspace that changes and then goes quiet is written once rather than
/// waiting for a second turn that may never come.
///
/// A window with no tabs is never swept, whatever the clock says. A sweep is a
/// background refresh of what is on screen, and an empty window is one that has
/// not opened yet or is on its way out — writing that snapshot would put
/// `{"tabs":[],"windows":[]}` over the saved session the user is about to have
/// restored. The paths that genuinely mean to erase — `close_window`, and the
/// reap that precedes this — still save explicitly, and this does not touch
/// them. Same rule as the named-layout guard in `session_write_target`.
///
/// Returning `false` here deliberately leaves the caller's timestamp alone, so
/// a window that is skipped does not consume the interval for the others.
pub(super) fn session_sweep_due(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    window_has_tabs: bool,
) -> bool {
    window_has_tabs && last.is_none_or(|then| now.saturating_duration_since(then) >= SESSION_SWEEP)
}

/// Why a session save runs, which decides how it treats the output file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveKind {
    /// A gesture, or an explicit save.
    Gesture,
    /// The background sweep (see [`SESSION_SWEEP`]).
    Sweep,
    /// The last save before the process goes. It always takes a fresh
    /// snapshot, which also restores an output file deleted or replaced on
    /// disk; the writer skips the write when the file already holds it.
    Exit,
}

/// While output keeps changing, how often its saved snapshot is replaced.
///
/// An agent pane whose spinner redraws all day, or a long build, changes the
/// output on every sweep. Replacing an up-to-8 MiB file and syncing it twice
/// every 2 seconds for that is about 1,800 writes an hour; every 30 seconds it
/// is 120. A change that went quiet is still written within two sweeps (see
/// [`ContentThrottle::decide`]), and the exit save writes whatever is owed.
pub(super) const CONTENT_STREAM_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// When a save takes a fresh output snapshot, given the last one submitted.
#[derive(Debug, Default)]
pub(super) struct ContentThrottle {
    /// The key and time of the last snapshot handed to the writer. `None`
    /// before the first one, so the first output after launch is written at
    /// the first save rather than 30 seconds in.
    written: Option<(crate::session_content::ContentKey, std::time::Instant)>,
    /// The key the previous sweep saw.
    swept: Option<crate::session_content::ContentKey>,
    /// A snapshot is owed but was put off. Like a change, it asks for the
    /// sweep's wake, so the write it is waiting for happens without another
    /// event.
    pub(super) pending: bool,
}

impl ContentThrottle {
    /// Whether this save writes a fresh snapshot (`true`) or keeps the file.
    ///
    /// In order:
    ///
    /// 1. The exit save writes, and so does a save after the writer found
    ///    the file was not the one it wrote (`stale`).
    /// 2. So does the first save, and one whose [`ContentBinding`] changed:
    ///    the layout the file is bound to, or what the settings let it hold.
    ///    The pair has to stay matched, and a privacy change has to reach the
    ///    disk at once.
    /// 3. A key equal to the one written keeps the file.
    /// 4. A sweep whose key equals the previous sweep's writes: the output
    ///    has gone quiet for a whole interval.
    /// 5. A save 30 seconds or more after the last write writes.
    /// 6. Anything else is put off, and asks for one more sweep.
    ///
    /// A gesture (copy, a font size change) follows the same rules, apart
    /// from rule 4, which only a sweep's fixed interval can judge: a burst of
    /// gestures while output streams cannot bypass the interval.
    ///
    /// [`ContentBinding`]: crate::session_content::ContentBinding
    pub(super) fn decide(
        &mut self,
        key: &crate::session_content::ContentKey,
        now: std::time::Instant,
        kind: SaveKind,
        stale: bool,
    ) -> bool {
        let quiet = kind == SaveKind::Sweep && self.swept.as_ref() == Some(key);
        if kind == SaveKind::Sweep {
            self.swept = Some(key.clone());
        }
        let write = match &self.written {
            _ if kind == SaveKind::Exit || stale => true,
            None => true,
            Some((written, _)) if written.binding() != key.binding() => true,
            Some((written, _)) if written == key => false,
            Some((_, at)) => quiet || now.saturating_duration_since(*at) >= CONTENT_STREAM_INTERVAL,
        };
        if write {
            self.written = Some((key.clone(), now));
        }
        self.pending = !write
            && self
                .written
                .as_ref()
                .is_some_and(|(written, _)| written != key);
        write
    }

    /// Forget what was written: the next save takes a fresh snapshot.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Where a session snapshot should be written, if anywhere.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionWriteTarget<'a> {
    NamedLayout(&'a str),
    DefaultSession,
    Skip,
}

/// Decide where a snapshot goes.
///
/// The rule worth stating out loud is the first arm: **an empty snapshot never
/// overwrites a named layout.** `close_window` deliberately empties the mux
/// before saving, so the SESSION it writes is the empty one ("this window is
/// finished, do not bring it back"). That intent belongs to session.json. Sent
/// to a named layout, the empty snapshot would erase the workspace, and the
/// next `--layout NAME` would open a single default pane. Terminator, whose
/// layouts this mirrors, only ever writes one from an explicit Add/Refresh;
/// launching a layout never modifies it.
///
/// Extracted so the truth table can be driven directly; the arms below are the
/// whole of the routing decision.
pub(super) fn session_write_target(
    layout: Option<&str>,
    named_layout_writable: bool,
    restore_mode: bool,
    snapshot_is_empty: bool,
) -> SessionWriteTarget<'_> {
    match layout {
        // Never clear a named workspace by quitting.
        Some(_) if snapshot_is_empty => SessionWriteTarget::Skip,
        Some(name) if named_layout_writable => SessionWriteTarget::NamedLayout(name),
        // `--layout NAME` given, but a launch override replaced it: leave the
        // file alone rather than overwriting it with a window that was never
        // restored from it.
        Some(_) => SessionWriteTarget::Skip,
        // Only write the DEFAULT session.json when this launch is in restore
        // mode — symmetric with the opt-in load gate. A fresh (non-opted-in)
        // window must NOT overwrite the saved layout that `--restore` /
        // `restore-session = true` exists to recover.
        None if restore_mode => SessionWriteTarget::DefaultSession,
        None => SessionWriteTarget::Skip,
    }
}

/// Is the default last-session (`session.json`) active for THIS launch?
/// Drives BOTH the startup restore gate (whether to `load()`) and the
/// `save_session` gate (whether to `save()` the default session). They MUST
/// agree. If loading is disabled but saving is enabled, a fresh window
/// silently overwrites the saved layout that `--restore` exists to recover.
/// Routing both through this one predicate keeps them symmetric.
/// `--layout NAME` is independent (its own file, explicit intent) and never
/// consults this gate.
///
/// While another build's primary still runs, typically the old version after
/// an update, the session is its: restoring it here would duplicate its open
/// tabs, and saving would overwrite them (#575). Both wait until it exits.
pub(super) fn should_restore_session(
    startup_restore: bool,
    cfg_restore_session: kettle_config::RestoreSession,
    another_build_running: bool,
) -> bool {
    (startup_restore || restore_session_enabled(cfg_restore_session)) && !another_build_running
}

fn restore_session_enabled(policy: kettle_config::RestoreSession) -> bool {
    #[cfg(target_os = "macos")]
    if policy == kettle_config::RestoreSession::Auto {
        use objc2_foundation_03::{NSBundle, NSGlobalDomain, NSNumber, NSString, NSUserDefaults};
        let defaults = NSUserDefaults::standardUserDefaults();
        let key = NSString::from_str("NSQuitAlwaysKeepsWindows");
        let read = |domain: &NSString| {
            let values = defaults.persistentDomainForName(domain)?;
            let value = values.objectForKey(&key)?;
            value
                .downcast_ref::<NSNumber>()
                .map(|n| n.boolValue())
                .or_else(|| value.downcast_ref::<NSString>().map(|s| s.boolValue()))
        };
        let app = NSBundle::mainBundle()
            .bundleIdentifier()
            .and_then(|id| read(&id));
        // SAFETY: Foundation exports this immutable domain name.
        let global = read(unsafe { NSGlobalDomain });
        return policy.resolve(true, app, global);
    }
    // Linux has no system setting for this. Kettle keeps the session, as
    // Ptyxis, GNOME's terminal, does by default.
    #[cfg(target_os = "linux")]
    if policy == kettle_config::RestoreSession::Auto {
        return policy.resolve(true, None, None);
    }
    policy.resolve(false, None, None)
}
