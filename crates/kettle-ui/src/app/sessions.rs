//! Saving and restoring the workspace session.

use super::*;

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
        match session_write_target(
            self.startup.layout.as_deref(),
            self.named_layout_writable,
            should_restore_session(
                self.startup.restore,
                self.cfg.restore_session,
                self.other_builds.running(),
            ),
            s.is_empty(),
        ) {
            SessionWriteTarget::NamedLayout(name) => s.save_layout(name),
            SessionWriteTarget::DefaultSession => s.save(),
            SessionWriteTarget::Skip => {}
        }
    }

    /// Move a session aside when it loaded but could not be restored.
    ///
    /// Everything downstream of a rejected preflight behaves as if there were
    /// no session at all, including the next save, so without this the file is
    /// replaced by a single fresh tab before the user has finished looking at
    /// the window.
    pub(super) fn stash_unrestorable_session(&self, reason: &str) {
        // The path this session was READ from, not a guess at where the next
        // save would go. Those are not the same question: `session_write_target`
        // routes on the snapshot being empty and on whether a named layout is
        // writable, and a `--tab-handoff` restore has no durable file at all.
        // Re-deriving it risked moving a file we never loaded.
        let Some(path) = self.restore_source.as_deref() else {
            log::warn!("session could not be restored ({reason}); it had no file to keep");
            return;
        };
        crate::session::stash_unrestorable_session(path, reason);
    }

    /// Resolve the one startup restore source before window or GPU creation.
    ///
    /// Loading and validating here lets `resumed_inner` seed the first native
    /// window with its final saved geometry. That avoids showing/configuring a
    /// default-sized swapchain and immediately replacing it after PTYs spawn.
    pub(super) fn load_startup_session(&mut self) -> Option<crate::session::Session> {
        // Terminator parity (detachable tabs):
        // --tab-handoff-fd FD wins over the file handoff, named layout, and
        // default session. The option is consumed exactly once because
        // `UnixStream::from_raw_fd` takes ownership of the descriptor.
        if let Some(fd) = self.startup.tab_handoff_fd.take() {
            #[cfg(unix)]
            {
                use std::os::unix::io::FromRawFd;

                // SAFETY: the source kettle process passed this owned
                // descriptor through --tab-handoff-fd and surrendered it
                // across exec. `socket` closes it on every return path.
                let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
                let mut payload = vec![0u8; 1024 * 1024];
                return match crate::fd_transport::recv_fds(&socket, &mut payload, 64) {
                    Ok((n, received_fds)) => {
                        log::info!(
                            "tab-handoff-fd: received {n} bytes + {} fds",
                            received_fds.len()
                        );
                        // Adopting the received PTYs is a separate feature.
                        // Until then, explicitly close every owned descriptor.
                        for received_fd in received_fds {
                            // SAFETY: recv_fds returned ownership of each fd.
                            unsafe {
                                libc::close(received_fd);
                            }
                        }
                        payload.truncate(n);
                        match serde_json::from_slice(&payload) {
                            Ok(session) => Some(session),
                            Err(error) => {
                                log::warn!("tab-handoff-fd session JSON is invalid: {error}");
                                None
                            }
                        }
                    }
                    Err(error) => {
                        log::warn!("tab-handoff-fd recv_fds: {error}");
                        None
                    }
                };
            }
            #[cfg(not(unix))]
            {
                let _ = fd;
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
            crate::session::Session::load_layout(name)
        } else if should_restore_session(
            self.startup.restore,
            self.cfg.restore_session,
            self.other_builds.running(),
        ) {
            self.restore_source = crate::session::Session::path();
            crate::session::Session::load()
        } else {
            None
        }
    }
}
