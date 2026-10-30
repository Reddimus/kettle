use super::*;

/// Is this argv launching `ssh` (by argv0 basename)? An ssh pane has no
/// local working directory, so the native-cwd poll skips it (a native read would
/// surface kettle's launch dir, not the remote host's). OSC 7 from the remote
/// shell remains the only meaningful cwd there.
fn argv_is_ssh(argv: &[String]) -> bool {
    argv.first()
        .map(|p| {
            let last = p.rsplit(['/', '\\']).next().unwrap_or(p);
            last.eq_ignore_ascii_case("ssh") || last.eq_ignore_ascii_case("ssh.exe")
        })
        .unwrap_or(false)
}

fn update_window_remote_contexts(
    snapshot: &kettle_remote::RemoteProbeSnapshot,
    applied: &mut std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    ws: &mut WindowState,
) -> bool {
    let mut changed = false;
    for (&pane_id, pane) in &mut ws.mux.panes {
        let Some(pid) = pane.term.child_pid() else {
            continue;
        };
        let Some(probe) = snapshot.probes.get(&pid) else {
            continue;
        };
        let context_changed = applied.get(&(pane_id, pid)).is_none_or(|previous| {
            previous.remote != probe.remote
                || previous.native_cwd != probe.native_cwd
                || previous.foreground_shell != probe.foreground_shell
        });
        stage_applied_remote_probe(applied, pane_id, pid, probe);
        if context_changed {
            pane.term.set_native_cwd(probe.native_cwd.clone());
        }
        if probe.remote != pane.remote_context {
            apply_remote_title_transition(
                &mut pane.title,
                &mut pane.title_is_placeholder,
                &mut pane.title_origin,
                &mut pane.title_before_remote,
                probe.remote.as_ref(),
            );
            pane.remote_context = probe.remote.clone();
            changed = true;
        }
        if probe.foreground_process != pane.foreground_process {
            pane.foreground_process = probe.foreground_process.clone();
        }
        // Foreground argv drives input policy but has no visual representation;
        // changing commands must not schedule a title sync and redraw at 5 Hz.
        changed |= context_changed;
    }
    changed
}

pub(super) fn retain_live_remote_probes(
    applied: &mut std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    live_panes: &mut HashSet<(u64, u32)>,
    panes: impl IntoIterator<Item = (u64, Option<u32>)>,
) {
    live_panes.clear();
    live_panes.extend(
        panes
            .into_iter()
            .filter_map(|(pane_id, pid)| pid.map(|pid| (pane_id, pid))),
    );
    applied.retain(|key, _| live_panes.contains(key));
}

pub(super) fn stage_applied_remote_probe(
    applied: &mut std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    pane_id: u64,
    pid: u32,
    probe: &kettle_remote::RemoteProbe,
) -> bool {
    let key = (pane_id, pid);
    if applied.get(&key) == Some(probe) {
        false
    } else {
        applied.insert(key, probe.clone());
        true
    }
}

pub(super) fn argv_is_nonlocal_client(argv: &[String]) -> bool {
    if crate::mux::argv_is_wsl(argv) || argv_is_ssh(argv) {
        return true;
    }
    let Some(argv0) = argv.first() else {
        return false;
    };
    let basename = argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .to_ascii_lowercase();
    let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
    matches!(basename, "docker" | "podman" | "kubectl" | "lxc-attach")
}

fn remote_probe_target(pane: &crate::mux::Pane) -> Option<kettle_remote::RemoteProbeTarget> {
    #[cfg(unix)]
    let foreground_pid = pane.term.foreground_process_group().ok();
    #[cfg(not(unix))]
    let foreground_pid = None;
    Some(kettle_remote::RemoteProbeTarget {
        pid: pane.term.child_pid()?,
        foreground_pid,
        allow_native_cwd: !argv_is_nonlocal_client(&pane.argv),
    })
}

pub(super) fn stage_remote_targets(
    scratch: &mut Vec<kettle_remote::RemoteProbeTarget>,
    checked_out: impl IntoIterator<Item = Option<kettle_remote::RemoteProbeTarget>>,
    stored_windows: impl IntoIterator<Item = Option<kettle_remote::RemoteProbeTarget>>,
) {
    scratch.clear();
    scratch.extend(checked_out.into_iter().flatten());
    scratch.extend(stored_windows.into_iter().flatten());
}

/// The right-click row that reopens a detected remote session. Users, hosts,
/// runtimes and containers are shown as detected.
pub(super) fn remote_reconnect_label(
    ctx: &kettle_remote::RemoteContext,
    tr: &kettle_i18n::Translator,
) -> String {
    match ctx {
        kettle_remote::RemoteContext::Ssh { host, user, .. } => match user {
            Some(user) => tr.menu_reconnect_ssh_user(user, host),
            None => tr.menu_reconnect_ssh(host),
        },
        kettle_remote::RemoteContext::Container {
            runtime, container, ..
        } => tr.menu_reattach(runtime.name(), container),
    }
}

pub(super) fn remote_reconnect_item(
    ctx: &kettle_remote::RemoteContext,
    tr: &kettle_i18n::Translator,
) -> Option<ContextMenuItem> {
    let command = kettle_remote::clone_session_command(ctx);
    let environment_unavailable = matches!(ctx,
        kettle_remote::RemoteContext::Container { options, .. } if options.environment_unavailable);
    if command.is_none() && !environment_unavailable {
        return None;
    }
    Some(ContextMenuItem::ReconnectItem {
        label: remote_reconnect_label(ctx, tr),
        command,
    })
}

pub(super) fn reconnect_remote_session(
    mux: &mut Mux,
    cfg: &Config,
    geometry: PtyGeometry,
    waker: kettle_core::Waker,
    command: Option<kettle_remote::SessionCommand>,
) -> Result<()> {
    let command = command.ok_or_else(|| anyhow::anyhow!(
        "Cannot reconnect: the original client's connection environment could not be read safely"
    ))?;
    let argv = std::iter::once(command.program)
        .chain(command.args)
        .collect();
    mux.split_with_launch(
        Dir::Horizontal,
        false,
        cfg,
        geometry,
        waker,
        argv,
        None,
        Some(&crate::mux::ReconnectLaunch {
            argv0: command.argv0,
            environment: command.environment,
        }),
    )?;
    Ok(())
}

/// How often the process scan may run. It feeds labels and new-pane
/// directories for shells that never send OSC 7.
pub(super) const REMOTE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// `None` to poll now, or when the throttled poll must run instead. Dropping
/// it would leave a `cd` that lands inside the window unseen until a redraw.
pub(super) fn remote_poll_deferred_until(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    let due = last? + REMOTE_POLL_INTERVAL;
    (now < due).then_some(due)
}

impl App {
    /// The shell the focused pane has effectively entered (e.g. the
    /// user opened pwsh then typed `wsl`) — its argv + cwd, so a split can clone
    /// it in the same directory instead of a fresh default shell. Reads the
    /// latest background process snapshot, so a split keystroke never performs
    /// process enumeration on the event-loop thread. `None` before the first
    /// snapshot or for a plain pane; the split then clones the pane's launch
    /// command.
    pub(super) fn focused_foreground_shell(
        &mut self,
        ws: &mut WindowState,
    ) -> Option<kettle_remote::ShellLaunch> {
        let (pane_id, pid, pane_cwd) = {
            let pane_id = ws.mux.active_focus()?;
            let pane = ws.mux.panes.get(&pane_id)?;
            (
                pane_id,
                pane.term.child_pid()?,
                pane.term.reported_current_dir(),
            )
        };
        let mut shell = self
            .remote_applied
            .get(&(pane_id, pid))?
            .foreground_shell
            .clone()?;
        // Assert the interactive-shell contract at the boundary.
        // The detector already rejects one-shot helpers, but re-checking here
        // means a split can never clone a non-interactive argv into a dead pane —
        // `None` routes the caller to `Mux::split`, which clones the pane's own
        // launch shell (falling back to the configured default).
        if !kettle_remote::shell_launch_is_interactive(&shell.argv) {
            return None;
        }
        // Prefer the pane's OSC 7-reported cwd — where the user actually is (e.g.
        // the WSL bash dir) — over the detected process's cwd, which Windows
        // sysinfo typically can't read for `wsl.exe` (it returned None, so the
        // split fell back to WSL's home `~`). `launch_cwd` carries this via
        // `wsl --cd`, which accepts both `/mnt/c/...` and native Linux paths.
        if pane_cwd.is_some() {
            shell.cwd = pane_cwd;
        }
        Some(shell)
    }

    /// Submit coalesced background process scans at most every 200 ms and apply
    /// completed remote-context changes.
    ///
    /// When the worker's snapshot shows a new or changed detection, the
    /// pane's title becomes `format_remote_title(...)`. When the detection
    /// ends (SSH exited), the title from before the remote session is
    /// restored.
    pub(super) fn poll_remote_contexts(&mut self, ws: &mut WindowState) {
        let Some(worker) = self.remote_scan_worker.as_ref() else {
            return;
        };
        let now = std::time::Instant::now();
        if let Some(due) = remote_poll_deferred_until(self.last_remote_poll, now) {
            self.remote_poll_due = Some(due);
            return;
        }
        self.last_remote_poll = Some(now);
        self.remote_poll_due = None;
        stage_remote_targets(
            &mut self.remote_targets_scratch,
            ws.mux.panes.values().map(remote_probe_target),
            self.windows
                .values()
                .flat_map(|window| window.mux.panes.values())
                .map(remote_probe_target),
        );
        worker.submit(self.remote_targets_scratch.clone());
    }

    pub(super) fn apply_remote_scan_snapshot(&mut self, ws: &mut WindowState) {
        let Some(snapshot) = self
            .remote_scan_worker
            .as_ref()
            .and_then(kettle_remote::RemoteScanWorker::take_latest)
        else {
            return;
        };
        retain_live_remote_probes(
            &mut self.remote_applied,
            &mut self.remote_live_panes_scratch,
            ws.mux
                .panes
                .iter()
                .chain(
                    self.windows
                        .values()
                        .flat_map(|window| window.mux.panes.iter()),
                )
                .map(|(&pane_id, pane)| (pane_id, pane.term.child_pid())),
        );

        if update_window_remote_contexts(&snapshot, &mut self.remote_applied, ws) {
            ws.reuse_pane_snapshots_once = false;
            self.sync_window_title(ws);
            self.poll_title_event(ws);
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }

        // `ws` is checked out of this map. Temporarily take the remaining map
        // so title synchronization can borrow the rest of App without aliasing.
        let mut stored_windows = std::mem::take(&mut self.windows);
        for window in stored_windows.values_mut() {
            if update_window_remote_contexts(&snapshot, &mut self.remote_applied, window) {
                window.reuse_pane_snapshots_once = false;
                self.sync_window_title(window);
                self.poll_title_event(window);
                if let Some(handle) = &window.window {
                    handle.request_redraw();
                }
            }
        }
        self.windows = stored_windows;
    }
}
