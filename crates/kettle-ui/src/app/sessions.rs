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

    pub(super) fn save_session(&self, ws: &WindowState) {
        if self.session_load_failed {
            return;
        }
        let target = session_write_target(
            self.startup.layout.as_deref(),
            self.named_layout_writable,
            should_restore_session(
                self.startup.restore,
                self.cfg.restore_session,
                self.other_builds.running(),
            ),
            false,
        );
        if target == SessionWriteTarget::Skip {
            return;
        }
        // Serialize EVERY live window (the checked-out one plus the map),
        // ordered by seq so window 1 stays first. Windows whose mux is already
        // empty (a close_window in flight) are dropped, since there is nothing
        // to restore.
        let mut wins: Vec<(u64, crate::session::SWindow)> =
            vec![(ws.seq, Self::snapshot_window(ws))];
        for w in self.windows.values() {
            wins.push((w.seq, Self::snapshot_window(w)));
        }
        wins.sort_by_key(|(seq, _)| *seq);
        let windows: Vec<crate::session::SWindow> = wins
            .into_iter()
            .map(|(_, w)| w)
            .filter(|w| !w.tabs.is_empty())
            .collect();
        // Downgrade-compat dual-write: mirror window 1 into the legacy
        // top-level fields so an older kettle reading this file still
        // restores its first window.
        let mut s = crate::session::Session {
            tabs: windows.first().map(|w| w.tabs.clone()).unwrap_or_default(),
            active: windows.first().map(|w| w.active).unwrap_or(0),
            theme: None,
            windows,
        };
        // Theme is config-governed (persisted to the config file via
        // `persist_pref`), NOT stored in the session. A session-pinned theme
        // would override the config/compile-time default on restore, so a
        // default change (or a fresh-config user) would silently keep the old
        // theme.
        s.theme = None;
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
        self.session_writer.submit(s, layout);
    }

    /// Move a session aside when it loaded but could not be restored.
    ///
    /// Everything downstream of a rejected preflight behaves as if there were
    /// no session at all, including the next save, so without this the file is
    /// replaced by a single fresh tab before the user has finished looking at
    /// the window.
    pub(super) fn stash_unrestorable_session(&mut self, reason: &str) {
        recover_startup_snapshot(
            self.restore_source.as_deref(),
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
        if let Some(path) = self.startup.tab_handoff.as_deref() {
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
            None
        }
    }
}

pub(super) fn recover_startup_snapshot(
    path: Option<&std::path::Path>,
    identity: Option<&crate::session::SnapshotIdentity>,
    load_failed: &mut bool,
    reason: &str,
) {
    let Some(path) = path else {
        log::warn!("session could not be restored ({reason}); it had no file to keep");
        return;
    };
    // Preserve the exact snapshot read by this launch. If it changed, or the
    // backup failed, even a fresh/partial layout must never replace the file.
    let recovered = identity.is_some_and(|identity| {
        crate::session::stash_unrestorable_snapshot(path, identity, reason).is_ok()
    });
    *load_failed |= !recovered;
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

    /// Runs alone: macOS marks a received descriptor close-on-exec only after
    /// `recvmsg` returns, and a child another test spawns in between would keep
    /// the peer from seeing end of file.
    #[test]
    fn handoff_failures_close_received_fds() {
        kettle_test_support::run_alone_in_child(
            "app::sessions::tests::handoff_failures_close_received_fds",
            handoff_failures_close_received_fds_alone,
        );
    }

    fn handoff_failures_close_received_fds_alone() {
        use std::io::Read;
        for payload in [b"{".to_vec(), vec![b' '; 1024 * 1024 + 1]] {
            let (sender, receiver) = UnixStream::pair().unwrap();
            let (passed, mut peer) = UnixStream::pair().unwrap();
            // Before the far end closes: macOS refuses the option after. End of
            // file comes at once when the descriptor is closed; the wait only
            // bounds a leak, and a loaded macOS runner has taken over 1 s to
            // let the closed descriptor go.
            peer.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            // The descriptor travels with the first byte; macOS refuses one
            // `sendmsg` of over 1 MiB with ancillary data.
            let payload_len = payload.len();
            let writer = std::thread::spawn(move || {
                use std::io::Write as _;
                crate::fd_transport::send_fds(&sender, &payload[..1], &[passed.as_raw_fd()])
                    .unwrap();
                (&sender).write_all(&payload[1..]).unwrap();
                sender.shutdown(std::net::Shutdown::Write).unwrap();
            });
            let result = receive_handoff_session(&receiver);
            writer.join().unwrap();
            assert!(result.is_err());
            let end = peer.read(&mut [0u8]);
            assert!(
                matches!(end, Ok(0)),
                "a {payload_len}-byte handoff kept the received descriptor: {end:?}"
            );
        }
    }
}
