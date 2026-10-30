//! Config reloads, saved preferences and theme changes from the schedule, the system or Lua.

use super::*;

pub(super) fn read_config_for_reload(
    path: &std::path::Path,
    trust: ConfigTrust,
) -> std::io::Result<Option<kettle_config::LoadedConfig>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    prepare_config_directory(path, trust)?;
    Config::read_from_with_trust(path, trust).map(Some)
}

impl App {
    /// Reconcile the live theme with the theme picker after every event (see
    /// `theme_preview_change`).
    pub(super) fn sync_theme_preview(&mut self, ws: &mut WindowState) {
        if let Some((name, theme)) = theme_preview_change(ws, &self.cfg, &self.ui_text) {
            self.set_runtime_theme_state(ws, name, theme);
        }
    }

    /// Write a `key = value` line to the user's active config file via the
    /// atomic `persist_config_toggle` helper. Resolves the path the same way
    /// `Action::EditConfig` does (`App::config_path`, then
    /// `Config::default_path`). Logs an I/O error so a transient FS issue
    /// doesn't kill the menu dispatch; the in-memory change still applies.
    /// Returns `true` iff the value was written to the config file. Callers
    /// of user-initiated changes (theme picks, Settings) notify the user on
    /// `false` so a change that's live this session but lost on restart isn't
    /// silent.
    pub(super) fn persist_pref(&self, key: &str, value: &str) -> bool {
        let Some(path) = self
            .config_path
            .clone()
            .or_else(kettle_config::Config::default_path)
        else {
            log::warn!(
                "persist_pref: no config path resolved (set $XDG_CONFIG_HOME or pass --config)"
            );
            return false;
        };
        match kettle_config::persist_config_toggle(&path, key, value) {
            Ok(bak) => {
                log::info!(
                    "persist_pref: wrote {key} = {value} to {} (backup at {})",
                    path.display(),
                    bak.display()
                );
                true
            }
            Err(e) => {
                log::warn!(
                    "persist_pref: failed to write {key} = {value} to {}: {e}",
                    path.display()
                );
                false
            }
        }
    }

    /// Whether the configured theme schedule calls for the dark theme now,
    /// or `None` without a schedule.
    pub(super) fn theme_schedule_is_dark(&self) -> Option<bool> {
        let schedule = self.cfg.theme_schedule?;
        Some(match schedule {
            // Clock times are local wall-clock times.
            kettle_config::ThemeSchedule::Clock { .. } => {
                let (h, m, _) = crate::wall_clock::now_local();
                kettle_config::schedule_decision_clock((h, m), schedule)
            }
            kettle_config::ThemeSchedule::SunriseSunset { lat, long } => {
                crate::wall_clock::solar_is_dark(lat, long)
            }
        })
    }

    pub(super) fn poll_theme_schedule(&mut self, ws: &mut WindowState) {
        let Some(is_dark) = self.theme_schedule_is_dark() else {
            return;
        };
        if !theme_schedule_redraw_due(self.last_schedule_decision, Some(is_dark)) {
            return;
        }
        let was_first = self.last_schedule_decision.is_none();
        self.last_schedule_decision = Some(is_dark);
        // Ask `resolve_theme_for_mode`
        // what to switch to. We override the ThemeMode to Light/Dark
        // for the duration of the swap so the helper picks the
        // configured light/dark theme name.
        let target_mode = if is_dark {
            kettle_config::ThemeMode::Dark
        } else {
            kettle_config::ThemeMode::Light
        };
        if let Some(next) = kettle_config::resolve_theme_for_mode(
            target_mode,
            &self.cfg.theme_name,
            &self.cfg.light_theme,
            &self.cfg.dark_theme,
            None,
        ) {
            if was_first {
                self.set_runtime_theme_name(ws, &next);
            } else {
                self.apply_theme_name(ws, "theme-schedule", next);
            }
        }
    }

    pub(super) fn apply_theme_name(&mut self, ws: &mut WindowState, source: &str, next: String) {
        log::info!("{source}: switching to {next}");
        self.set_runtime_theme_name(ws, &next);
        self.save_session(ws);
    }

    /// Update the rendered palette and Lua's read-only view as one state
    /// transition. Every runtime theme mutation, including temporary menu
    /// previews and config reloads, must route through this synchronization
    /// boundary so `kettle.theme()` cannot describe a stale palette.
    pub(super) fn set_runtime_theme_state(
        &mut self,
        ws: &WindowState,
        name: String,
        theme: kettle_config::Theme,
    ) {
        self.cfg.theme_name = name;
        self.cfg.theme = theme;
        self.sync_lua_active_theme();
        self.request_all_window_redraws(ws);
    }

    pub(super) fn set_runtime_theme_name(&mut self, ws: &WindowState, name: &str) {
        self.set_runtime_theme_state(ws, name.to_string(), self.cfg.theme_by_name(name));
    }

    pub(super) fn os_theme_choice(&self, theme: WindowTheme) -> Option<String> {
        if self.cfg.theme_mode != kettle_config::ThemeMode::Auto {
            return None;
        }
        if self.cfg.theme_schedule.is_some() {
            return None;
        }
        let os_dark = matches!(theme, WindowTheme::Dark);
        kettle_config::resolve_theme_for_mode(
            kettle_config::ThemeMode::Auto,
            &self.cfg.theme_name,
            &self.cfg.light_theme,
            &self.cfg.dark_theme,
            Some(os_dark),
        )
    }

    pub(super) fn apply_initial_os_theme_preference(
        &mut self,
        ws: &WindowState,
        theme: WindowTheme,
    ) {
        self.os_theme_dark = Some(matches!(theme, WindowTheme::Dark));
        if let Some(next) = self.os_theme_choice(theme) {
            log::info!("theme-mode=auto: initial OS theme selects {next}");
            self.set_runtime_theme_name(ws, &next);
        }
    }

    pub(super) fn apply_os_theme_preference(&mut self, ws: &mut WindowState, theme: WindowTheme) {
        self.os_theme_dark = Some(matches!(theme, WindowTheme::Dark));
        if let Some(next) = self.os_theme_choice(theme) {
            self.apply_theme_name(ws, "theme-mode=auto", next);
        }
    }

    /// The native window-theme hint for the current config. See
    /// [`native_theme_hint`] for the decision table.
    pub(super) fn native_theme_hint(&self) -> Option<WindowTheme> {
        native_theme_hint(
            self.cfg.theme_mode,
            self.cfg.theme_schedule.is_some(),
            self.cfg.theme.is_dark(),
        )
    }

    /// Push the native theme and compatible live icon to every window when
    /// either changed since the last sync. Called at the top of `redraw` — every
    /// theme mutation ends in a repaint, so one lazy chokepoint covers all
    /// mutation sites (actions, context menu, Lua, settings persist+reload,
    /// schedule, OS auto-switch) without each needing to remember a call.
    /// `ws` is the checked-out window (see the `windows` field contract);
    /// the map holds only the others during dispatch.
    pub(super) fn maybe_sync_native_theme(&mut self, ws: &mut WindowState) {
        let hint = self.native_theme_hint();
        // Tracked per window, not once for the process: on macOS a window can
        // only take the hint after AppKit has built its caption, so windows
        // reach that point at different times and a window opened later still
        // has to be told. See `native_theme_sync_is_due`.
        for state in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
            let Some(w) = state.window.as_ref() else {
                continue;
            };
            if state.native_theme_synced == Some(hint)
                || !native_theme_sync_is_due(
                    state.window_shown && w.has_focus(),
                    state.native_theme_synced.is_some(),
                    cfg!(target_os = "macos"),
                )
            {
                continue;
            }
            w.set_theme(hint);
            state.native_theme_synced = Some(hint);
        }
        let icon_dark = self.cfg.theme.is_dark();
        if self.window_icon_dark_synced != Some(icon_dark) {
            self.window_icon_dark_synced = Some(icon_dark);
            let icon = load_window_icon(icon_dark);
            for w in std::iter::once(&*ws)
                .chain(self.windows.values())
                .filter_map(|s| s.window.as_ref())
            {
                w.set_window_icon(icon.clone());
            }
        }
    }

    /// Tell every window's panes when the theme's colours changed, so a program
    /// following DEC mode 2031 (Claude Code's automatic theme) re-reads the
    /// background. Like `maybe_sync_native_theme`, it runs at every redraw and
    /// every theme mutation ends in one, so no mutation path (actions, menu
    /// previews, Lua, reload, schedule, OS appearance) needs its own call; it
    /// only compares when nothing changed. Hidden windows are synced too,
    /// since their programs still run.
    pub(super) fn maybe_sync_color_scheme(&mut self, ws: &mut WindowState) {
        let scheme = (self.cfg.theme.is_dark(), self.cfg.theme.background);
        for state in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
            if state.color_scheme_synced != Some(scheme) {
                state.color_scheme_synced = Some(scheme);
                state.mux.set_color_scheme(Some(scheme.0));
            }
        }
    }

    /// Make the profile at `path` the active config: what reloads read,
    /// what live reload watches, and what Lua reports.
    pub(super) fn switch_profile(&mut self, ws: &mut WindowState, path: std::path::PathBuf) {
        self.config_path = Some(path);
        self.config_trust = ConfigTrust::VerifyDirectory;
        self.reload_config(ws);
    }

    fn refresh_config_watcher(&mut self) {
        let path = self.config_path.clone().or_else(Config::default_path);
        let Some(path) = path else {
            self._watcher = None;
            self.config_watcher_path = None;
            return;
        };
        let proxy = self.proxy.clone();
        match config_watcher(
            &path,
            self.config_trust,
            self.config_reload_pending.clone(),
            move || proxy.send_event(UserEvent::ReloadConfig).is_ok(),
        ) {
            Ok(watcher) => {
                self._watcher = Some(watcher);
                self.config_watcher_path = Some(path);
            }
            Err(error) => {
                if self.config_watcher_path.as_ref() != Some(&path) {
                    self._watcher = None;
                    self.config_watcher_path = None;
                }
                log::warn!(
                    "config live-reload unavailable for {}: {error}",
                    path.display()
                );
            }
        }
    }

    /// Read and install the process-wide portion of a live configuration once.
    /// Returns whether the configured font size changed, which controls whether
    /// each window keeps its session-local zoom.
    pub(super) fn load_reloaded_config(&mut self) -> bool {
        self.refresh_config_watcher();
        let previous_config_font_size = self.cfg.font_size;
        let path = self.config_path.clone().or_else(Config::default_path);
        if let Some(engine) = &self.lua_engine
            && let Err(error) = engine.set_config_path(path.as_deref())
        {
            log::warn!("Lua config path synchronization failed: {error:#}");
        }
        let (mut new, malformed) = match path {
            Some(path) => match read_config_for_reload(&path, self.config_trust) {
                Ok(None) => (Config::default(), Vec::new()),
                Ok(Some(loaded)) => {
                    if !loaded.unknown_keys.is_empty() {
                        log::warn!(
                            "{}: unrecognized config keys: {}",
                            path.display(),
                            loaded.unknown_keys.join(", ")
                        );
                    }
                    (loaded.config, loaded.malformed_values)
                }
                Err(error) => {
                    log::warn!("config reload refused for {}: {error}", path.display());
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleConfigReloadRefused),
                        &self.ui_text.notify_body_config_reload_refused(
                            &path.display().to_string(),
                            &error.to_string(),
                        ),
                    );
                    return false;
                }
            },
            None => (Config::default(), Vec::new()),
        };
        // Surface malformed values (typos the parser silently ignores, e.g.
        // `cursor-style = beem`) on live reload. `--check-config` already
        // catches these at the CLI, but a live edit otherwise reverted with no
        // feedback. Edge-triggered via `should_notify_malformed` so kettle's
        // own settings-persistence writes don't spam an unchanged warning.
        if should_notify_malformed(&malformed, &self.config_malformed_last) {
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleConfigValuesIgnored),
                &self
                    .ui_text
                    .notify_body_config_values_ignored(&malformed.join("\n")),
            );
        }
        self.config_malformed_last = malformed;
        // Re-compile triggers from the freshly-loaded config
        // BEFORE assigning, while `new` is still owned. Recompile
        // catches added/removed/changed patterns. Clearing the throttle stamp
        // lets a fresh edit fire immediately even mid-throttle.
        self.compiled_triggers = compile_triggers(&new.triggers);
        self.last_trigger_fire = None;
        // The accent seed is a runtime per-window value (not in the
        // config file), so carry it across a reload + re-apply the --accent
        // override (launch-time intent survives a reload).
        new.accent_seed = self.cfg.accent_seed;
        if let Some(rgb) = self.startup.accent_override {
            new.accent_color = Some(rgb);
        }
        // The launch-time window flags are launch-time intent too. Without
        // re-applying them, any live reload (including kettle's own
        // theme/settings persistence writes) would revert a `-T` pinned title
        // to `window-title-format` and drop the -m/-f/-H/-b overrides from
        // `self.cfg`, which save_session and future window ops read.
        if let Some(wstate) = self.startup.window_state_override {
            new.window_state = wstate;
        }
        if let Some(b) = self.startup.borderless_override {
            new.borderless = b;
        }
        if let Some(title) = self.startup.title_override.clone() {
            new.window_title_format = title;
        }
        let font_size_changed = (new.font_size - previous_config_font_size).abs() > f32::EPSILON;
        new.resolve_theme_mode(self.os_theme_dark);
        self.last_schedule_decision = None;
        self.cfg = new;
        self.reconcile_agent_display();
        self.reconcile_claude_plugin();
        self.reconcile_codex_shell();
        crate::dev_record::apply_retention_config(&self.cfg);
        self.sync_lua_active_theme();
        // A reloaded `update-policy` changes what the update notice says, so
        // read its facts now rather than at the next focus or hourly tick.
        self.request_update_facts(true);
        font_size_changed
    }

    pub(super) fn apply_reloaded_config(&mut self, ws: &mut WindowState, font_size_changed: bool) {
        // A config change, including a Settings edit or a menu toggle, is
        // activity: a new blink or timeout value takes effect visibly now.
        self.reset_blink_phase(ws);
        // A changed schedule applies the theme it picks now, not at its next
        // boundary.
        self.poll_theme_schedule(ws);
        if !self.cfg.macos_cursor_blink_layer && !ws.blink_layer.is_active() {
            Self::retire_cursor_layer(ws);
        }
        #[cfg(target_os = "macos")]
        {
            refresh_macos_modifiers(ws, self.cfg.macos_option_as_alt);
            if let Some(window) = &ws.window {
                set_macos_option_as_alt(window, self.cfg.macos_option_as_alt);
            }
        }
        ws.mux.set_osc52_copy_allowed(osc52_copy_is_available(
            self.cfg.osc52,
            self.clipboard.is_some(),
        ));
        ws.mux
            .set_unnegotiated_modified_enter(self.cfg.modify_other_keys);
        ws.mux.autoclean_groups = self.cfg.autoclean_groups;
        // Push `scrollback` and `scrollback-bytes` edits (config file or
        // Settings rows) into live panes too; otherwise only panes opened
        // afterwards would use them.
        ws.mux
            .set_scrollback_limits(self.cfg.scrollback, self.cfg.scrollback_bytes);
        let runtime_font_size = ws.renderer.as_ref().map(|r| r.font_size());
        if let Some(r) = ws.renderer.as_mut() {
            // Family first: the font-size setter re-measures cells and must see
            // the new family. All setters are no-ops when unchanged.
            r.set_font_family(self.cfg.font_family.clone());
            r.set_font_size(reloaded_font_size(
                font_size_changed,
                self.cfg.font_size,
                runtime_font_size,
            ));
            r.set_cell_scale(self.cfg.cell_width, self.cfg.cell_height);
            r.set_background_compositing(&self.cfg);
        }
        // A configured size change invalidates the pre-scaled-zoom baseline;
        // a no-op reload must preserve both it and the live runtime zoom.
        if font_size_changed {
            ws.scaled_zoom_prev_font_size = None;
        }
        self.resize_all(ws);
        let direction_changed = sync_reloaded_search_preferences(
            &mut ws.search,
            self.cfg.search_wrap,
            self.cfg.search_case_sensitive,
            self.cfg.invert_search,
            std::time::Instant::now(),
        );
        if direction_changed
            && let Some(pane_id) = ws.search.target_pane
            && let Some(pane) = ws.mux.panes.get(&pane_id)
            && let Ok(term) = pane.term.term.lock()
        {
            use kettle_core::Dimensions as _;
            let direction = if ws.search.invert {
                kettle_core::SearchDirection::Reverse
            } else {
                kettle_core::SearchDirection::Forward
            };
            ws.search.anchor = Some(search_viewport_anchor(
                term.grid().display_offset(),
                term.screen_lines(),
                term.last_column().0,
                direction,
            ));
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Immediate manual reload. The addressed window is checked out of the
    /// map by the dispatch wrapper, so update it plus every mapped sibling.
    pub(super) fn reload_config(&mut self, ws: &mut WindowState) {
        let font_size_changed = self.load_reloaded_config();
        self.apply_reloaded_config(ws, font_size_changed);
        let seqs: Vec<u64> = self.windows.keys().copied().collect();
        for seq in seqs {
            if let Some(mut sibling) = self.windows.remove(&seq) {
                self.apply_reloaded_config(&mut sibling, font_size_changed);
                self.windows.insert(seq, sibling);
            }
        }
        log::debug!(
            "config reload applied once across {} windows",
            self.windows.len() + 1
        );
    }

    /// Debounced watcher reload when every window is still in the map.
    pub(super) fn reload_config_windows(&mut self) {
        let font_size_changed = self.load_reloaded_config();
        let seqs: Vec<u64> = self.windows.keys().copied().collect();
        for seq in &seqs {
            if let Some(mut ws) = self.windows.remove(seq) {
                self.apply_reloaded_config(&mut ws, font_size_changed);
                self.windows.insert(*seq, ws);
            }
        }
        log::debug!("config reload applied once across {} windows", seqs.len());
    }
}

/// Terminator parity (key_next_profile / key_previous_profile):
/// pick the next profile name after `current`, wrapping at the end.
/// If `current` isn't in the list (e.g. user launched without `--profile`,
/// or with an explicit `--config FILE`), the cycle starts at index 0.
/// Pure — unit-testable without touching disk.
pub(super) fn pick_next_profile(current: Option<&str>, names: &[String], forward: bool) -> String {
    let n = names.len();
    let cur_idx = current
        .and_then(|c| names.iter().position(|x| x == c))
        .unwrap_or(0);
    let next_idx = if forward {
        (cur_idx + 1) % n
    } else {
        (cur_idx + n - 1) % n
    };
    names[next_idx].clone()
}

pub(super) fn pick_light_dark_target(current: &str, light: &str, dark: &str) -> Option<String> {
    let cur = current.trim().to_ascii_lowercase();
    let l = light.trim();
    let d = dark.trim();
    match (l.is_empty(), d.is_empty()) {
        (true, true) => None,
        (false, true) => Some(l.to_string()),
        (true, false) => Some(d.to_string()),
        (false, false) => {
            // Current dark switches to light. Light and any third-party theme
            // switch to dark, so only the dark match needs its own arm.
            if cur == d.to_ascii_lowercase() {
                Some(l.to_string())
            } else {
                Some(d.to_string())
            }
        }
    }
}

/// Decide the native window-theme hint (winit
/// `WindowAttributes::preferred_theme` / `Window::set_theme`) so the OS
/// titlebar — the Windows DWM caption, the Wayland Adwaita CSD frame —
/// matches the palette kettle is actually rendering, instead of staying on
/// the OS-wide setting under a forced dark/light theme.
///
///   - `Auto` without a schedule → `None`: the palette follows the OS, so
///     let the titlebar follow the OS natively too. Forcing a value here
///     would also stop winit's `ThemeChanged` events on some backends,
///     which the palette auto-switcher depends on.
///   - everything else (Explicit / Light / Dark / Auto+schedule) → the
///     titlebar matches the active theme's background darkness.
///
/// Pure — unit-testable without a window.
pub(super) fn native_theme_hint(
    mode: kettle_config::ThemeMode,
    has_schedule: bool,
    theme_is_dark: bool,
) -> Option<WindowTheme> {
    if mode == kettle_config::ThemeMode::Auto && !has_schedule {
        None
    } else if theme_is_dark {
        Some(WindowTheme::Dark)
    } else {
        Some(WindowTheme::Light)
    }
}

pub(super) const CONFIG_RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(75);

/// Should a live config reload fire a "malformed values ignored" notification?
/// Only when the diagnostics are non-empty AND differ from the set surfaced on
/// the previous reload — kettle's own settings-persistence writes (theme /
/// preferences toggles) reload the config, and an unchanged malformed set must
/// not re-notify on every one of them. Pure so the edge-trigger rule is tested.
pub(super) fn should_notify_malformed(new: &[String], last: &[String]) -> bool {
    !new.is_empty() && new != last
}

/// Filesystem watchers observe the containing directory so atomic replacement
/// works across platforms. Only content/name changes to the exact file are
/// actionable. Accepting `Access(Open)` creates a read -> reload -> read
/// feedback loop on Linux inotify; accepting `Modify(Metadata)` creates the
/// same loop on macOS because opening a private state file re-applies its mode.
pub(super) fn watcher_event_matches(event: &notify::Event, watched: &std::path::Path) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;

    if !event.paths.iter().any(|path| path == watched) {
        return false;
    }
    match event.kind {
        EventKind::Any | EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Other => false,
    }
}

/// Queue at most one event until the main thread re-arms the latch. Re-open it
/// when the event loop has already closed so a failed send cannot strand the
/// watcher in a permanently pending state.
pub(super) fn queue_watcher_event(
    pending: &std::sync::atomic::AtomicBool,
    send: impl FnOnce() -> bool,
) -> bool {
    use std::sync::atomic::Ordering;

    if pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    if send() {
        true
    } else {
        pending.store(false, Ordering::Release);
        false
    }
}

pub(super) fn config_watch_paths(
    path: &std::path::Path,
) -> std::io::Result<Vec<std::path::PathBuf>> {
    let requested = std::path::absolute(path)?;
    let mut paths = vec![requested.clone()];
    match std::fs::canonicalize(&requested) {
        Ok(target) if target != requested => paths.push(target),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(paths)
}

pub(super) fn config_watcher(
    path: &std::path::Path,
    trust: ConfigTrust,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    send: impl Fn() -> bool + Send + 'static,
) -> Result<notify::RecommendedWatcher, String> {
    use notify::Watcher as _;

    prepare_config_directory(path, trust).map_err(|error| error.to_string())?;
    let watched = config_watch_paths(path).map_err(|error| error.to_string())?;
    let mut directories = Vec::new();
    for path in &watched {
        if let Some(parent) = path.parent()
            && !directories.iter().any(|directory| directory == parent)
        {
            directories.push(parent.to_path_buf());
        }
    }
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result
            && watched
                .iter()
                .any(|path| watcher_event_matches(&event, path))
        {
            queue_watcher_event(&pending, &send);
        }
    })
    .map_err(|error| error.to_string())?;
    registered_watcher(watcher, |watcher| {
        for directory in directories {
            watcher.watch(&directory, notify::RecursiveMode::NonRecursive)?;
        }
        Ok::<_, notify::Error>(())
    })
    .map_err(|error| error.to_string())
}

pub(super) fn registered_watcher<W, E>(
    mut watcher: W,
    register: impl FnOnce(&mut W) -> Result<(), E>,
) -> Result<W, E> {
    register(&mut watcher)?;
    Ok(watcher)
}

pub(super) fn prepare_config_directory(
    path: &std::path::Path,
    trust: ConfigTrust,
) -> std::io::Result<()> {
    if trust == ConfigTrust::ExplicitPath {
        return Ok(());
    }
    let directory = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("config path has no parent: {}", path.display()),
        )
    })?;
    if let Err(error) = kettle_state::create_private_dirs(directory) {
        log::debug!(
            "config directory repair skipped for {}: {error}",
            directory.display()
        );
    }
    kettle_state::validate_trusted_directory(directory)
}

pub(super) fn reloaded_font_size(
    configured_changed: bool,
    configured: f32,
    runtime: Option<f32>,
) -> f32 {
    if configured_changed {
        configured
    } else {
        runtime.unwrap_or(configured)
    }
}
