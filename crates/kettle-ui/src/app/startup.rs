use super::*;

/// Locate the XDG cache dir for the current user without
/// pulling in the `dirs` crate. Probes `$XDG_CACHE_HOME` first
/// (the spec-canonical var) then `$HOME/.cache` on Linux/macOS,
/// then `$LOCALAPPDATA` on Windows-ish, returning `None` if none
/// are set (CI / container envs). Pure modulo the env-var reader
/// fn so tests can pin the env.
pub(super) fn cache_dir_from_env<F: Fn(&str) -> Option<String>>(
    get: F,
) -> Option<std::path::PathBuf> {
    let var = |k: &str| get(k).filter(|s| !s.is_empty());
    // XDG_CACHE_HOME is the explicit cross-platform override.
    if let Some(p) = var("XDG_CACHE_HOME") {
        return Some(std::path::PathBuf::from(p));
    }
    // Per-OS fallback, matching `default_path_from`. On
    // Windows the canonical per-user cache dir is `%LOCALAPPDATA%`; a stray
    // `HOME` (git-bash / MSYS / WSL-interop export one) must NOT redirect
    // screenshots / crash logs to `~/.cache` — the same config-dir split-brain
    // that bit the config path (a shell launch vs a Start-menu launch would
    // disagree). On Unix, `HOME/.cache` is the standard XDG fallback.
    if cfg!(windows) {
        var("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        var("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
    }
}

const STARTUP_GEOMETRY_CELL_W: f64 = 8.0;
const STARTUP_GEOMETRY_CELL_H: f64 = 16.0;
const STARTUP_GEOMETRY_TAB_BAR_H: f64 = 24.0;
const STARTUP_GEOMETRY_STATUS_BAR_H: f64 = 22.0;
/// The grid a fresh window aims for when the config sets no size, and the
/// value a half-specified `window-width`/`window-height` fills its other axis
/// from. Sized for agent TUIs: Claude Code's fullscreen diff panel toggles at
/// 110 columns and auto-opens at 144, and Codex CLI lays out two columns
/// above ~120, so the classic 80x24 (or winit's ~800x600, 93 columns at the
/// default font) left every fresh window too narrow for the tools Kettle
/// targets. 160x45 baseline cells are 1296x760 logical px with the default
/// chrome: about 152x41 real cells at the 14 px font on a 1080p monitor.
pub(crate) const STARTUP_DEFAULT_COLS: u32 = 160;
pub(crate) const STARTUP_DEFAULT_ROWS: u32 = 45;
/// The default window never covers more of the monitor than this: a fixed
/// target rather than a monitor fraction keeps a 3440-wide ultrawide from
/// opening a 285-column canvas, and the fit keeps a 1366x768 laptop honest
/// (its 90 % is 1229 px, about 144 columns at the default font). There is no
/// floor on purpose: a monitor smaller than the target wins.
pub(super) const STARTUP_MONITOR_WIDTH_FRACTION: f64 = 0.90;
pub(super) const STARTUP_MONITOR_HEIGHT_FRACTION: f64 = 0.85;

/// Convert a startup grid into an inner-window size in **logical** pixels.
/// This runs before renderer/font metrics exist, so it intentionally uses the
/// same conservative 8x16 baseline as `geometry-hinting`; the first normal
/// layout pass reconciles exact metrics after the renderer starts. Logical
/// rather than physical so a HiDPI display gets the same grid as a 1x one; a
/// physical conversion would give a 2x display half the requested columns.
fn startup_cells_to_logical_px(cfg: &Config, cols: u32, rows: u32) -> (f64, f64) {
    let cols = cols.clamp(
        kettle_config::WINDOW_WIDTH_MIN,
        kettle_config::WINDOW_WIDTH_MAX,
    );
    let rows = rows.clamp(
        kettle_config::WINDOW_HEIGHT_MIN,
        kettle_config::WINDOW_HEIGHT_MAX,
    );

    let mut width = f64::from(cols) * STARTUP_GEOMETRY_CELL_W + f64::from(cfg.padding_x) * 2.0;
    let mut height = f64::from(rows) * STARTUP_GEOMETRY_CELL_H + f64::from(cfg.padding_y) * 2.0;

    if matches!(cfg.tab_bar, TabBarMode::Always) {
        match cfg.tab_bar_pos {
            TabBarPos::Top | TabBarPos::Bottom => height += STARTUP_GEOMETRY_TAB_BAR_H,
            TabBarPos::Left | TabBarPos::Right => {
                width += f64::from(cfg.tab_bar_width.clamp(40.0, 600.0));
            }
        }
    }

    if !matches!(cfg.status_bar, StatusBarMode::Off) {
        height += STARTUP_GEOMETRY_STATUS_BAR_H;
    }

    (width.ceil().max(1.0), height.ceil().max(1.0))
}

/// The inner size an explicit `window-width` / `window-height` asks for, in
/// logical pixels, or `None` when the config sets neither. A missing axis
/// comes from the default grid so `window-width = 200` alone still opens
/// tall enough for an agent TUI. Explicit values are honoured as typed: the
/// monitor fit below applies to the default only.
pub(super) fn startup_inner_size_px(cfg: &Config) -> Option<(u32, u32)> {
    if cfg.window_width.is_none() && cfg.window_height.is_none() {
        return None;
    }
    let (width, height) = startup_cells_to_logical_px(
        cfg,
        cfg.window_width.unwrap_or(STARTUP_DEFAULT_COLS),
        cfg.window_height.unwrap_or(STARTUP_DEFAULT_ROWS),
    );
    Some((width as u32, height as u32))
}

/// The fresh-window default when the config sets no size: the default grid
/// with this config's chrome, fitted to the monitor it will most likely open
/// on. `monitor_logical` is the monitor's logical size; `None` (no monitor
/// enumerable yet) keeps the bare target.
pub(super) fn default_startup_inner_size(
    cfg: &Config,
    monitor_logical: Option<(f64, f64)>,
) -> (f64, f64) {
    let (width, height) =
        startup_cells_to_logical_px(cfg, STARTUP_DEFAULT_COLS, STARTUP_DEFAULT_ROWS);
    let Some((monitor_w, monitor_h)) = monitor_logical else {
        return (width, height);
    };
    let max_w = (monitor_w * STARTUP_MONITOR_WIDTH_FRACTION)
        .floor()
        .max(1.0);
    let max_h = (monitor_h * STARTUP_MONITOR_HEIGHT_FRACTION)
        .floor()
        .max(1.0);
    (width.min(max_w), height.min(max_h))
}

/// The one startup size both window constructors use, in logical pixels:
/// the configured grid when the user set one, else the monitor-fitted default.
pub(super) fn startup_inner_size(cfg: &Config, monitor: Option<StartupMonitor>) -> (f64, f64) {
    match startup_inner_size_px(cfg) {
        Some((w, h)) => (f64::from(w), f64::from(h)),
        None => default_startup_inner_size(cfg, monitor.map(|m| m.logical)),
    }
}

/// What the startup sizing needs to know about the monitor a fresh window
/// will most likely land on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StartupMonitor {
    /// Logical size (physical pixels divided by the scale factor).
    pub(super) logical: (f64, f64),
    pub(super) scale: f64,
}

impl StartupMonitor {
    /// CoreGraphics points converted exactly as winit's macOS monitor size.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn from_display_points(width: usize, height: usize, scale: f64) -> Self {
        let physical = winit::dpi::PhysicalSize::<u32>::from_logical::<_, f64>(
            (width as f64, height as f64),
            scale,
        );
        Self::from_physical(physical.width, physical.height, scale)
    }

    pub(super) fn from_physical(width: u32, height: u32, scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self {
            logical: (f64::from(width) / scale, f64::from(height) / scale),
            scale,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StartupPlan {
    pub(super) launch_override: bool,
    pub(super) may_restore: bool,
    pub(super) pre_launch: bool,
}

pub(super) fn startup_plan(
    options: &crate::Options,
    cfg: &Config,
    another_build_running: bool,
) -> StartupPlan {
    let launch_override = options.command.is_some() || options.cwd.is_some();
    let may_restore = !launch_override
        && (options.tab_handoff_fd.is_some()
            || options.tab_handoff.is_some()
            || options.layout.is_some()
            || should_restore_session(options.restore, cfg.restore_session, another_build_running));
    StartupPlan {
        launch_override,
        may_restore,
        pre_launch: !may_restore
            && matches!(
                cfg.window_state,
                kettle_config::WindowState::Normal | kettle_config::WindowState::Hidden
            )
            && cfg.window_position_x.is_none()
            && cfg.window_position_y.is_none(),
    }
}

pub(super) fn fits_monitor(surface: (u32, u32), monitor: Option<StartupMonitor>) -> bool {
    let (width, height) = surface;
    monitor.is_some_and(|m| {
        f64::from(width) <= (m.logical.0 * STARTUP_MONITOR_WIDTH_FRACTION).floor() * m.scale + 1.0
            && f64::from(height)
                <= (m.logical.1 * STARTUP_MONITOR_HEIGHT_FRACTION).floor() * m.scale + 1.0
    })
}

/// The monitor to fit a fresh window to: the primary monitor where the
/// platform has one, else the largest by area (Wayland has no primary; a
/// mixed multi-monitor setup then gets a deterministic answer instead of
/// whichever output the compositor listed first).
pub(super) fn startup_monitor(event_loop: &ActiveEventLoop) -> Option<StartupMonitor> {
    let describe = |m: winit::monitor::MonitorHandle| {
        let size = m.size();
        StartupMonitor::from_physical(size.width, size.height, m.scale_factor())
    };
    event_loop.primary_monitor().map(describe).or_else(|| {
        event_loop
            .available_monitors()
            .map(describe)
            .max_by(|a, b| {
                let area = |m: &StartupMonitor| m.logical.0 * m.logical.1;
                area(a).total_cmp(&area(b))
            })
    })
}

/// Startup visibility policy. Windows are created hidden while renderer init
/// runs, then visible states are revealed once the wgpu surface is configured.
/// Only `window_state = hidden` remains hidden.
pub(super) fn should_reveal_after_renderer_init(state: kettle_config::WindowState) -> bool {
    !matches!(state, kettle_config::WindowState::Hidden)
}

/// Some surface backends cannot acquire a first frame while their native
/// window is hidden. macOS Metal is one: `CAMetalLayer::nextDrawable` does not
/// make progress until the `NSWindow` can be ordered on screen. In that case a
/// hide-until-first-presentation policy is circular, so reveal after renderer
/// initialization and immediately before the first surface frame instead.
pub(super) fn should_reveal_before_first_surface_frame(
    state: kettle_config::WindowState,
    window_shown: bool,
    surface_requires_visible_window: bool,
) -> bool {
    surface_requires_visible_window && !window_shown && should_reveal_after_renderer_init(state)
}

impl App {
    pub fn run() -> Result<()> {
        Self::run_with(Self::default_startup_options(Config::default_path()))
    }

    pub(super) fn default_startup_options(config: Option<std::path::PathBuf>) -> crate::Options {
        let mut startup = crate::Options {
            config,
            ..crate::Options::default()
        };
        let config = startup
            .config
            .as_deref()
            .filter(|path| path.exists())
            .and_then(|path| {
                prepare_config_directory(path, startup.config_trust)
                    .map(|()| Config::load_from_with_trust(path, startup.config_trust))
                    .ok()
            })
            .unwrap_or_default();
        startup.recording = crate::RecordingPolicy::from_config(&config);
        startup
    }

    pub fn run_with(mut startup: crate::Options) -> Result<()> {
        crate::startup_trace::mark(crate::startup_trace::Phase::RunWith);
        let _trace = crate::startup_trace::FlushOnDrop;
        // Enumerate the system fonts while the event loop starts (AppKit's
        // launch, on macOS) instead of after it.
        let mut font_preload = crate::font_preload::FontPreload::start();
        // Reclaim pasted-image directories from a run that died before its own
        // cleanup. Age-gated, so a sibling instance mid-session is untouched.
        crate::paste_image::sweep_stale();
        #[cfg(target_os = "macos")]
        disable_app_state_restoration();
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        crate::startup_trace::mark(crate::startup_trace::Phase::EventLoopBuilt);
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        // Primaries of other builds, such as the old version's after an
        // update: shared state they own waits until they exit.
        let other_builds = startup
            .activation
            .as_ref()
            .map(kettle_ctl::activation::PrimaryHandle::other_builds)
            .unwrap_or_default();
        let another_build_running = other_builds.running();
        let activation = startup.activation.take().and_then(|primary| {
            match crate::activation_server::ActivationInbox::start(primary, proxy.clone()) {
                Ok(inbox) => Some(inbox),
                Err(error) => {
                    log::warn!("bare-launch activation server unavailable: {error}");
                    None
                }
            }
        });

        // Watch the chosen config file's directory for live reload, but reload
        // only on events whose `paths` contain the config file itself. Other
        // files there change often (session.json saves, palette edits, the
        // theme cache, editor swap files), and each atomic `save_session`
        // alone writes `session.json.tmp.*` then renames it, firing 3+ events.
        let config_reload_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = startup
            .config
            .clone()
            .or_else(Config::default_path)
            .and_then(|path| {
                let p = proxy.clone();
                config_watcher(
                    &path,
                    startup.config_trust,
                    config_reload_pending.clone(),
                    move || p.send_event(UserEvent::ReloadConfig).is_ok(),
                )
                .map_err(|error| {
                    log::warn!(
                        "config live-reload unavailable for {}: {error}",
                        path.display()
                    );
                })
                .ok()
            });

        // Remote-control watcher. Same notify pattern as the
        // config-reload watcher above. When startup.remote_file is
        // Some, watch its parent directory; on a change to the file
        // itself, send UserEvent::RemoteCommand so the main thread
        // reads + dispatches lines.
        let mut remote_watcher = None;
        let mut remote_startup_retry = false;
        let remote_command_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(path) = startup.remote_file.clone()
            && let Some(dir) = path.parent().map(|p| p.to_path_buf())
        {
            let p = proxy.clone();
            let watched = path.clone();
            let pending = remote_command_pending.clone();
            // `remote.cmd` is a command channel into this terminal, so its
            // parent must be private before the lock below touches anything.
            // Repairing here rather than relying on some other subsystem
            // having run first is the difference between the watcher working
            // on this launch and on the next one.
            let _ = kettle_state::create_private_dirs(&dir);
            use notify::Watcher;
            let built = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res
                    && watcher_event_matches(&ev, &watched)
                {
                    queue_watcher_event(&pending, || {
                        p.send_event(UserEvent::RemoteCommand).is_ok()
                    });
                }
            });
            // Same silent constructor as the config watcher above, and the same
            // reason to report it: this one carries remote commands, so a
            // channel that never subscribes is worth a line in the log.
            if let Err(error) = &built {
                log::warn!("remote-command watcher unavailable: {error}");
            }
            if let Ok(mut w) = built {
                // Create or harden the control file before registering its
                // watcher, and truncate through that verified handle so a
                // reparse/path swap cannot redirect startup cleanup. Commands
                // left after a previous crash must not replay into this run.
                let lock_path = kettle_state::remote_command_lock_path(&path);
                match kettle_state::ExclusiveFileLock::try_acquire(&lock_path) {
                    Ok(Some(_lock)) => {
                        // Keep the sender/receiver lock through watcher
                        // registration. Once it is released, every later
                        // append is observed; there is no truncate→watch gap
                        // in which a valid command can be stranded.
                        let initialized = initialize_remote_spool(&path, another_build_running)
                            .and_then(|()| {
                                w.watch(&dir, notify::RecursiveMode::NonRecursive)
                                    .map_err(std::io::Error::other)
                            });
                        match initialized {
                            Ok(()) => remote_watcher = Some(w),
                            Err(error) => log::warn!(
                                "remote-command watcher disabled for {}: {error}",
                                path.display()
                            ),
                        }
                    }
                    Ok(None) => {
                        // A live sender owns the spool. Register the watcher
                        // immediately and claim asynchronously once it releases
                        // the lock; waiting and then truncating would erase the
                        // command that sender just finished appending.
                        match w
                            .watch(&dir, notify::RecursiveMode::NonRecursive)
                            .map_err(std::io::Error::other)
                        {
                            Ok(()) => {
                                log::warn!(
                                    "remote-command startup lock at {} is busy; \
                                     watcher is active and the spool will be claimed asynchronously",
                                    lock_path.display()
                                );
                                remote_startup_retry = true;
                                remote_watcher = Some(w);
                            }
                            Err(watch_error) => log::warn!(
                                "remote-command watcher disabled for {} while its startup lock \
                                 was busy: {watch_error}",
                                path.display()
                            ),
                        }
                    }
                    Err(error) => log::warn!(
                        "remote-command watcher disabled for {}: {error}",
                        path.display()
                    ),
                }
            }
        }

        // Load the config into a local so the trigger compile and the `cfg`
        // field below can both use it; inside the struct init, a bare `cfg.…`
        // would hit the `cfg!()` macro instead.
        let initial_path = startup.config.clone().or_else(Config::default_path);
        let mut initial_cfg = match initial_path.as_deref().filter(|path| path.exists()) {
            Some(path) => match prepare_config_directory(path, startup.config_trust)
                .and_then(|()| Config::read_from_with_trust(path, startup.config_trust))
            {
                Ok(loaded) => {
                    if !loaded.unknown_keys.is_empty() {
                        log::warn!(
                            "{}: unrecognized config keys: {}",
                            path.display(),
                            loaded.unknown_keys.join(", ")
                        );
                    }
                    if !loaded.malformed_values.is_empty() {
                        log::warn!(
                            "{}: malformed values (ignored): {}",
                            path.display(),
                            loaded.malformed_values.join(", ")
                        );
                    }
                    loaded.config
                }
                Err(error) => {
                    log::warn!("config {} ignored: {error}", path.display());
                    // No `language` to read, so the notice follows the OS.
                    let tr = kettle_i18n::Translator::new(
                        kettle_config::LanguagePreference::Auto.resolve(kettle_i18n::os_locale),
                    );
                    fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitleConfigIgnored),
                        &tr.notify_body_config_ignored(
                            &path.display().to_string(),
                            &error.to_string(),
                        ),
                    );
                    Config::default()
                }
            },
            None => Config::default(),
        };
        // The process's UI language, fixed for its lifetime: a config reload
        // keeps it, and Settings says a change applies on restart.
        let ui_text =
            kettle_i18n::Translator::new(initial_cfg.language.resolve(kettle_i18n::os_locale));
        // `EventLoop::build` registers and installs winit's application
        // delegate, so the Dock menu can only be grafted on after it; its rows
        // need the UI language, so after the config is read too.
        crate::macos_dock::install(proxy.clone(), &ui_text);
        // Peacock parity: the --accent CLI flag wins over the config
        // `accent-color` key. Applied here at startup and again on every
        // config reload, since CLI flags are launch-time intent.
        if let Some(rgb) = startup.accent_override {
            initial_cfg.accent_color = Some(rgb);
        }
        // Terminator parity: launch-time window-state CLI flags
        // (`-m/-f/-H/-b/-T`) override the config for THIS launch — same
        // "CLI flags are launch-time intent" precedent as --accent above.
        if let Some(wstate) = startup.window_state_override {
            initial_cfg.window_state = wstate;
        }
        if let Some(b) = startup.borderless_override {
            initial_cfg.borderless = b;
        }
        if let Some(title) = startup.title_override.clone() {
            // A literal title (no `{title}` placeholder) renders verbatim.
            initial_cfg.window_title_format = title;
        }
        // Peacock: seed the per-window accent variation from this
        // window's working directory, so `accent-color = auto` gives a window
        // in a different project a different (but per-project stable) accent.
        initial_cfg.accent_seed = accent_seed_from_cwd(startup.cwd.as_deref());
        crate::startup_trace::mark(crate::startup_trace::Phase::ConfigLoaded);
        font_preload.request_family(&initial_cfg.font_family);
        let initial_triggers = compile_triggers(&initial_cfg.triggers);
        // Dropdown-parity: capture before `startup` moves into the App.
        let startup_version = startup
            .version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let runtime_tracker = crate::runtime_diagnostics::RuntimeTracker::start(
            cache_dir_from_env(|key| std::env::var(key).ok()),
            startup_version.clone(),
        );
        // Lua scripting foundation. If `--lua-script PATH`
        // was set, init a LuaEngine + run the script once. Failures
        // log::warn but don't block the launch (same shape as
        // `compile_triggers`'s per-pattern warn-and-skip fallthrough).
        //
        // Also drain pending side-effect commands queued
        // by Lua and stash them on App so the first focused pane
        // gets them written to its PTY once it's ready (the pane
        // doesn't exist yet at this point in `run_with`).
        let mut pending_lua_commands = PendingLuaCommands::default();
        let mut initial_lua_queue_rejection_reported = false;
        // Keep the LuaEngine alive on App so kettle.on(...) registrations
        // survive startup and can be fire_event'd from emission sites. With no
        // --lua-script and no `<config-dir>/init.lua`, engine init is skipped
        // so non-Lua runs stay zero-cost.
        //
        // Script resolution:
        //   1. --lua-script PATH (explicit; overrides)
        //   2. <config-dir>/init.lua  (auto-loaded; default for plugins)
        // where <config-dir> is the parent of Config::default_path().
        let init_lua = resolved_init_lua_script(
            startup.lua_script.clone(),
            kettle_config::Config::default_path(),
        );
        let mut lua_engine: Option<crate::LuaEngine> = None;
        if let Some((script, trust)) = &init_lua {
            match crate::LuaEngine::new_with_config_path(
                &initial_cfg.theme_name,
                initial_cfg.lua_sandbox,
                initial_path.as_deref(),
            ) {
                Ok(eng) => {
                    let executed = match trust {
                        ConfigTrust::ExplicitPath => eng.exec_file(script),
                        ConfigTrust::VerifyDirectory => eng.exec_trusted_file(script),
                    };
                    if let Err(e) = executed {
                        log::warn!("lua script {}: {e:#}", script.display());
                    } else {
                        log::info!("lua script {}: executed", script.display());
                    }
                    for cmd in eng.drain_commands() {
                        let result = match cmd {
                            crate::LuaCommand::SendText(s) => {
                                pending_lua_commands.push(PendingLuaCommand::SendText {
                                    origin_window: 1,
                                    target_pane: None,
                                    bytes: Arc::from(s.into_bytes()),
                                })
                            }
                            crate::LuaCommand::ExecAction(name) => {
                                if let Some(a) = kettle_config::Action::from_name(&name) {
                                    pending_lua_commands.push(PendingLuaCommand::ExecAction {
                                        origin_window: 1,
                                        action: a,
                                    })
                                } else {
                                    log::warn!(
                                        "lua kettle.exec_action: unknown action name {name:?}"
                                    );
                                    PaneInputResult::Queued
                                }
                            }
                            crate::LuaCommand::Notify { title, body } => {
                                pending_lua_commands.push(PendingLuaCommand::Notify {
                                    origin_window: 1,
                                    title,
                                    body,
                                })
                            }
                            crate::LuaCommand::CallbackStopped { event, dropped } => {
                                pending_lua_commands.push(PendingLuaCommand::CallbackStopped {
                                    origin_window: 1,
                                    event,
                                    dropped,
                                })
                            }
                            crate::LuaCommand::SetTheme(name) => {
                                pending_lua_commands.push(PendingLuaCommand::SetTheme {
                                    origin_window: 1,
                                    name,
                                })
                            }
                        };
                        if result != PaneInputResult::Queued
                            && !initial_lua_queue_rejection_reported
                            && let Some(message) = input_rejection_message(result)
                        {
                            log::warn!("lua startup command rejected ({result:?}): {message}");
                            fire_notify(
                                ui_text.text(kettle_i18n::Text::NotifyTitleLuaNotQueued),
                                input_rejection_text(result, &ui_text).unwrap_or(message),
                            );
                            initial_lua_queue_rejection_reported = true;
                        }
                    }
                    lua_engine = Some(eng);
                }
                Err(e) => {
                    log::warn!("lua engine init failed: {e:#}");
                }
            }
        }
        // If a LuaEngine is active,
        // the Mux must subscribe to per-PTY output bytes so the
        // App can fire LuaEvent::Output. Set before the Mux moves
        // into the struct.
        let lua_output_subscribed = lua_engine.is_some();
        // Recording policy is fixed before activation, including an explicit
        // disabled state. Config reload still updates retention budgets only;
        // it does not restart or reconfigure the active recorder.
        let recording_requested =
            matches!(startup.recording, crate::RecordingPolicy::Enabled { .. });
        // Retention budgets are process-wide in kettle-core, so publish them
        // before any recorder can be constructed.
        crate::dev_record::apply_retention_config(&initial_cfg);
        let lua_output_subscribed = lua_output_subscribed || recording_requested;
        // Broadcast always starts off; `broadcast-default` only picks the
        // scope the toggle chord turns on (`broadcast_scope_for_default`).
        // Resolve clipboard availability before constructing the first Mux:
        // DA1 extension 52 is a runtime capability, not merely compiled code,
        // and must not be advertised when policy or the platform denies writes.
        let clipboard = match arboard::Clipboard::new() {
            Ok(cb) => Some(cb),
            Err(e) => {
                log::warn!(
                    "clipboard unavailable ({e}); copy/paste and OSC 52 \
                     will no-op — no DISPLAY/Wayland, headless SSH, or a \
                     sandbox without clipboard-portal access?"
                );
                None
            }
        };
        let osc52_copy_allowed = osc52_copy_is_available(initial_cfg.osc52, clipboard.is_some());

        // Per-window state lives in a WindowState; the first window is seq 1.
        // The Mux is built here because its construction flags
        // (`lua_output_subscribed`, `record_lossless`) are process-global
        // decisions.
        let mux = {
            let mut m = Mux::new();
            m.lua_output_subscribed = lua_output_subscribed;
            // A recording needs a lossless output channel so the asciicast
            // trace can't drop chunks under a fast burst (CLI/env target or the
            // `record = on` config key — see `recording_requested` above).
            {
                m.record_lossless = recording_requested;
            }
            m.osc52_copy_allowed = osc52_copy_allowed;
            m.autoclean_groups = initial_cfg.autoclean_groups;
            m
        };
        let mut windows = std::collections::BTreeMap::new();
        let mut initial_window = WindowState::new(1, mux);
        initial_window.cursor_exit_log = crate::cursor_exit_log::CursorExitLog::from_env(1);
        windows.insert(1, initial_window);
        let remote_proxy = proxy.clone();
        let remote_scan_worker =
            match kettle_remote::RemoteScanWorker::spawn_with_notifier(move || {
                let _ = remote_proxy.send_event(UserEvent::RemoteScanReady);
            }) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    log::warn!(
                        "remote-context detection disabled: cannot start scanner worker: {error}"
                    );
                    None
                }
            };
        let video_previewer =
            crate::video_preview::VideoPreviewer::new(proxy.clone(), startup.media.clone());
        let plan = startup_plan(&startup, &initial_cfg, another_build_running);
        let ctl_policy = ctl_overrides(&startup).resolve(initial_cfg.ctl_policy());
        let mut app = App {
            pending_shell_action: PendingShellAction::default(),
            #[cfg(target_os = "linux")]
            wayland: None,
            #[cfg(target_os = "linux")]
            wayland_tab_drag: None,
            #[cfg(target_os = "linux")]
            wayland_file_drops: std::collections::BTreeMap::new(),
            restore_source: None,
            restore_identity: None,
            session_load_failed: false,
            session_writer: crate::session::SessionWriter::new()?,
            cfg: initial_cfg,
            // Assume writable; `resumed` clears it when a launch override
            // means the named layout was never loaded.
            named_layout_writable: true,
            windows,
            accent_registry: crate::window_accent::AccentRegistry::default(),
            focused_seq: 1,
            next_window_seq: 2,
            gpu: None,
            font_preload: Some(font_preload),
            launch_override: plan.launch_override,
            pre_launch_eligible: plan.pre_launch && !plan.may_restore,
            gpu_choices: Vec::new(),
            ui_text,
            gpu_recovery: RecoveryState::default(),
            gpu_incident: None,
            gpu_incident_started_for_loss: false,
            gpu_software_fallback: false,
            window_icon_dark_synced: None,
            pending_window_closes: std::collections::BTreeSet::new(),
            quit_requested: false,
            recorder: None,
            recording_start_failed: false,
            recording_error_reported: false,
            last_input_rejection_notice: None,
            system_motion: std::sync::OnceLock::new(),
            #[cfg(target_os = "linux")]
            agent_launches: kettle_ctl::agent_launch::LaunchScopes::default(),
            proxy,
            activation,
            runtime_tracker,
            wake_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            config_reload_pending,
            config_reload_deadline: None,
            remote_command_pending,
            pending_remote_commands: VecDeque::new(),
            remote_spool_claim_pending: remote_startup_retry,
            remote_command_retry: {
                let mut retry = AutomationRetry::default();
                if remote_startup_retry {
                    retry.expedite_if_idle(std::time::Instant::now());
                }
                retry
            },
            // Server is started later in `resumed`
            // (needs the pid + a live event-loop proxy for the waker).
            ctl: None,
            ctl_policy,
            ctl_start: CtlStart::NotReady,
            pending_runs: std::collections::HashMap::new(),
            media: crate::media::MediaService::default(),
            cards_tip: crate::media::CardsTip::load(),
            clipboard,
            pasted_images: crate::paste_image::PastedImages::new(crate::paste_image::PASTED),
            png_encoder: None,
            png_jobs: 0,
            queued_pastes: std::collections::VecDeque::new(),
            viewer_copies: std::sync::Arc::new(std::sync::Mutex::new(
                crate::paste_image::PastedImages::new(crate::paste_image::OPENED),
            )),
            video_copies: std::sync::Arc::new(crate::media::PrivateCopies::new(
                crate::paste_image::VIDEOS,
            )),
            document_copies: std::sync::Arc::new(crate::media::PrivateCopies::new(
                crate::paste_image::DOCUMENTS,
            )),
            previews_ready: Vec::new(),
            ctl_driving: false,
            lua_driving: false,
            crop_tickets: 0,
            page_tickets: 0,
            preview_tickets: 0,
            preview_copy: None,
            video_previewer,
            next_video_preview_generation: 1,
            compiled_triggers: initial_triggers,
            last_trigger_fire: None,
            remote_scan_worker,
            remote_applied: std::collections::HashMap::new(),
            remote_live_panes_scratch: std::collections::HashSet::new(),
            remote_targets_scratch: Vec::new(),
            last_remote_poll: None,
            remote_poll_due: None,
            last_schedule_decision: None,
            os_theme_dark: None,
            config_path: startup.config.clone(),
            config_trust: startup.config_trust,
            config_malformed_last: Vec::new(),
            config_watcher_path: watcher
                .as_ref()
                .and_then(|_| startup.config.clone().or_else(Config::default_path)),
            startup,
            _watcher: watcher,
            _remote_watcher: remote_watcher,
            pending_lua_commands,
            lua_command_retry: AutomationRetry::default(),
            lua_engine,
            lua_startup_fired: false,
            update_facts: crate::update_notice::UpdateFacts::default(),
            update_local_dismissals: crate::update_notice::LocalDismissals::default(),
            update_restart_dismissed: false,
            update_toast: None,
            update_toast_receipts: None,
            update_toast_claiming: None,
            update_toast_handled: None,
            update_toast_unavailable: None,
            update_toast_retry: None,
            update_toast_focus: None,
            update_facts_requested: None,
            update_facts_refresh: Default::default(),
            logged_restart_pending: None,
            other_builds,
            executable: std::env::current_exe().and_then(std::fs::canonicalize).ok(),
            version_line: startup_version,
            torn_drag: None,
            last_session_sweep: None,
        };
        app.runtime_tracker.set_window_count(app.windows.len());
        // Before the first pane, which may start Claude Code or Codex.
        app.reconcile_claude_plugin();
        app.reconcile_codex_shell();
        crate::startup_trace::mark(crate::startup_trace::Phase::AppBuilt);
        let result: Result<()> = match app.start_first_pane_before_launch() {
            Ok(_) => event_loop.run_app(&mut app).map_err(Into::into),
            Err(error) => {
                log::error!("{error}");
                Err(anyhow::anyhow!(error))
            }
        };
        app.runtime_tracker.set_phase("exiting");
        // Pasted screenshots are captured screen content, so they do not outlive
        // the session that produced them. Runs on the error path too — an exit
        // caused by a failure is exactly when leftovers would go unnoticed.
        // PNGs still being encoded are stopped and kept first, so cleanup
        // takes them too.
        if let Some(encoder) = app.png_encoder.take() {
            for encoded in encoder.close() {
                let _ = app.pasted_images.keep_png(encoded);
            }
        }
        app.pasted_images.cleanup();
        // So are the copies handed to the image viewer, and the store closes
        // so an open still on its way cannot leave a new one.
        app.viewer_copies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .close();
        // And the copies handed to the document viewer and the video
        // player; a copy on its way stops first.
        app.document_copies.close();
        app.video_copies.close();
        if let Err(error) = &result {
            app.runtime_tracker.record_exit(&error.to_string());
        }
        app.runtime_tracker.stop();
        crate::notifications::flush_desktop_notifications(std::time::Duration::from_millis(250));
        result
    }

    /// The first window's surface in physical pixels, before the renderer
    /// exists. An axis `window-width` or `window-height` sets holds exactly
    /// that many cells at the startup cell size. An axis the config leaves
    /// alone keeps the default window's pixel size, fitted to the monitor
    /// when neither is set. `None` before the cell is measured.
    pub(super) fn startup_surface(
        &self,
        ws: &WindowState,
        monitor: Option<StartupMonitor>,
    ) -> Option<(u32, u32)> {
        let cell = Self::cell_size(ws)?;
        let scale = monitor.map_or(1.0, |m| m.scale);
        let (default_w, default_h) = startup_inner_size(&self.cfg, monitor);
        let physical = |logical: f64| (logical * scale).round().max(1.0) as u32;
        let cols = self.cfg.window_width.map(|cols| {
            cols.clamp(
                kettle_config::WINDOW_WIDTH_MIN,
                kettle_config::WINDOW_WIDTH_MAX,
            )
        });
        let rows = self.cfg.window_height.map(|rows| {
            rows.clamp(
                kettle_config::WINDOW_HEIGHT_MIN,
                kettle_config::WINDOW_HEIGHT_MAX,
            )
        });
        let (exact_w, exact_h) = surface_for_grid(
            (
                cols.unwrap_or(STARTUP_DEFAULT_COLS) as usize,
                rows.unwrap_or(STARTUP_DEFAULT_ROWS) as usize,
            ),
            cell,
            (self.cfg.padding_x, self.cfg.padding_y),
            self.content_bands(ws),
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
            self.cfg.tab_bar_width,
        );
        Some((
            if cols.is_some() {
                exact_w
            } else {
                physical(default_w)
            },
            if rows.is_some() {
                exact_h
            } else {
                physical(default_h)
            },
        ))
    }

    fn prepare_startup_fonts(&mut self, ws: &mut WindowState, startup_scale: f32) {
        let fonts = match ws.startup_fonts.take() {
            Some(fonts) if fonts.matches(&self.cfg, startup_scale) => fonts,
            Some(fonts) => fonts.remeasure(&self.cfg, startup_scale),
            None => match self.font_preload.take() {
                Some(preload) => preload.finish(&self.cfg, startup_scale),
                None => kettle_render::StartupFonts::load(&self.cfg, startup_scale),
            },
        };
        ws.startup_cell = Some(fonts.cell);
        ws.startup_fonts = Some(fonts);
    }

    fn start_first_pane_before_launch(&mut self) -> Result<bool, String> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.pre_launch_eligible;
            Ok(false)
        }
        #[cfg(target_os = "macos")]
        {
            let decline = crate::startup_trace::note_pre_launch_decline;
            if !self.pre_launch_eligible {
                decline("ineligible".to_owned());
                return Ok(false);
            }
            let Some(monitor) = crate::macos_display::startup_display() else {
                decline("display".to_owned());
                return Ok(false);
            };
            crate::startup_trace::mark(crate::startup_trace::Phase::DisplayRead);
            let Some(mut ws) = self.windows.remove(&1) else {
                decline("window".to_owned());
                return Ok(false);
            };
            self.prepare_startup_fonts(&mut ws, monitor.scale as f32);
            let surface = self.startup_surface(&ws, Some(monitor));
            let result = if surface.is_some_and(|size| fits_monitor(size, Some(monitor))) {
                ws.startup_surface = surface;
                self.spawn_first_tab(&mut ws, self.launch_override)
                    .map(|()| {
                        ws.pre_launch = Some(monitor);
                        crate::startup_trace::mark(crate::startup_trace::Phase::PaneSpawned);
                        crate::startup_trace::note_path(
                            crate::startup_trace::StartupPath::PreLaunch,
                        );
                        true
                    })
            } else {
                // Keep the font system for the resumed path's measurement.
                ws.startup_cell = None;
                decline(match surface {
                    Some((width, height)) => format!(
                        "fit surface={width}x{height} monitor={:.0}x{:.0} scale={}",
                        monitor.logical.0, monitor.logical.1, monitor.scale
                    ),
                    None => "fit surface=none".to_owned(),
                });
                Ok(false)
            };
            self.windows.insert(1, ws);
            result
        }
    }

    /// Start the first tab at the current startup grid: the `-e` command and
    /// working directory when given, else the shell.
    fn spawn_first_tab(
        &mut self,
        ws: &mut WindowState,
        launch_override: bool,
    ) -> Result<(), String> {
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
        if launch_override {
            let argv = self.startup.command.take().unwrap_or_default();
            let cwd = self
                .startup
                .cwd
                .take()
                .map(|p| p.to_string_lossy().into_owned());
            ws.mux
                .new_tab_with_geometry(&self.cfg, geometry, self.waker(), &argv, cwd.as_deref())
                .map_err(|e| format!("failed to spawn `-e` command: {e}"))
        } else {
            ws.mux
                .new_tab_geometry(&self.cfg, geometry, self.waker())
                .map_err(|e| format!("failed to spawn shell: {e}"))
        }
    }

    pub(super) fn resumed_inner(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {
        if ws.window.is_some() {
            return;
        }
        crate::startup_trace::mark(crate::startup_trace::Phase::Resumed);
        // Explicit command/cwd launches never restore a saved workspace.
        // Preserve the fields until the existing consumed-once spawn path
        // below, but decide and preflight the restore before native resources
        // are allocated.
        let has_launch_override = self.launch_override;
        // A launch override means the named layout is not loaded, so it must
        // not be written back either — otherwise the flag that names a layout
        // is the same flag that destroys it.
        self.named_layout_writable = !has_launch_override;
        let loaded_session = if ws.pre_launch.is_some() || has_launch_override {
            None
        } else {
            self.load_startup_session()
        };
        let live_monitors = monitor_rects(event_loop);
        // The size a saved window without geometry gets, in physical pixels
        // like the monitor rects it is validated against: the same startup
        // rule a fresh window uses, so a restore and a first launch agree.
        let monitor = startup_monitor(event_loop);
        if let Some(pre_launch) = ws.pre_launch {
            let matched = monitor == Some(pre_launch);
            crate::startup_trace::note_monitor_match(matched);
            if !matched {
                log::warn!("startup display changed after the pre-launch spawn");
            }
        }
        let default_surface = {
            let (w, h) = startup_inner_size(&self.cfg, monitor);
            let scale = monitor.map_or(1.0, |m| m.scale);
            (
                (w * scale).round().max(1.0) as u32,
                (h * scale).round().max(1.0) as u32,
            )
        };
        let restore_plan = loaded_session.as_ref().and_then(|session| {
            if session.is_empty() {
                return None;
            }
            let windows = match session.validated_restore_windows() {
                Ok(windows) => windows,
                Err(error) => {
                    self.stash_unrestorable_session(&error);
                    return None;
                }
            };
            let geometries = match crate::session::validated_restore_surface_geometries(
                &windows,
                &live_monitors,
                default_surface,
            ) {
                Ok(geometries) => geometries,
                Err(error) => {
                    self.stash_unrestorable_session(&error);
                    return None;
                }
            };
            Some((windows, geometries))
        });

        // Measure the fonts before the window. A pane's grid needs only the
        // cell, so a fresh window can size itself to exactly the configured
        // grid and start its first pane while the window and GPU initialize.
        // The system fonts have been loading on their own thread since
        // `run_with` began, and only the measurement needs this scale. The
        // renderer takes the fonts over.
        let startup_scale = monitor.map_or(1.0, |m| m.scale) as f32;
        if ws.pre_launch.is_none() {
            self.prepare_startup_fonts(ws, startup_scale);
        }
        // A restored window keeps its saved geometry, and a maximized or
        // full-screen one gets its size from the OS after creation. Only a
        // fresh normal or hidden window knows its size before it exists.
        let early_start = restore_plan.is_none()
            && matches!(
                self.cfg.window_state,
                kettle_config::WindowState::Normal | kettle_config::WindowState::Hidden
            );
        let startup_surface = if ws.pre_launch.is_some() {
            ws.startup_surface
        } else if early_start {
            self.startup_surface(ws, monitor)
        } else {
            None
        };
        ws.startup_surface = startup_surface;
        // The pane starts before the window only when the window will open at
        // this scale and size: on the monitor the cell was measured for, which
        // a configured position could change, and within it, where the OS
        // would shrink it. Otherwise it starts after the renderer, at the
        // window's real size.
        let spawned_early = ws.pre_launch.is_some()
            || startup_surface.is_some_and(|size| fits_monitor(size, monitor))
                && self.cfg.window_position_x.is_none()
                && self.cfg.window_position_y.is_none();
        if ws.pre_launch.is_none()
            && spawned_early
            && let Err(e) = self.spawn_first_tab(ws, has_launch_override)
        {
            log::error!("{e}");
            event_loop.exit();
            return;
        }
        if spawned_early && ws.pre_launch.is_none() {
            crate::startup_trace::mark(crate::startup_trace::Phase::PaneSpawned);
            crate::startup_trace::note_path(crate::startup_trace::StartupPath::ResumedEarly);
        }

        // Create the window hidden while renderer init runs on the event-loop
        // thread. The first successful paint reveals it; `window_state =
        // hidden` remains hidden.
        ws.window_shown = !should_reveal_after_renderer_init(self.cfg.window_state);
        ws.startup_window = true;
        let mut attrs = self.window_attributes(self.cfg.window_state, monitor);
        if let Some((width, height)) = startup_surface {
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(width, height));
        }
        if let Some(surface) = restore_plan
            .as_ref()
            .and_then(|(_, surfaces)| surfaces.first())
        {
            let (width, height) = surface.size;
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(width, height));
            if let Some((x, y)) = surface.position {
                attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
            }
        }
        // Startup is measured, not guessed: the window is created HIDDEN and
        // revealed at the first paint, so everything between here and that
        // reveal is time the user spends looking at nothing. A comparator
        // benchmark can only report the total.
        let t_startup = std::time::Instant::now();
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                ws.mux.kill_children();
                event_loop.exit();
                return;
            }
        };
        self.apply_post_create(&window);
        ws.native_fullscreen = Some(crate::native_fullscreen::NativeFullscreen::install(
            &window,
            ws.seq,
            self.proxy.clone(),
        ));
        ws.native_caption_drag = Some(crate::native_drag::NativeCaptionDrag::install(&window));
        ws.native_material = Some(crate::native_material::NativeMaterial::install(
            &window,
            &self.cfg,
            self.proxy.clone(),
        ));
        crate::startup_trace::mark(crate::startup_trace::Phase::WindowCreated);
        // Measured AFTER apply_post_create because the gpu figure below is
        // derived by subtraction; stopping the clock earlier would charge that
        // setup to the GPU.
        let create_window_ms = t_startup.elapsed().as_secs_f64() * 1000.0;
        let t_a11y = std::time::Instant::now();
        let accessibility = Self::new_accessibility_adapter(
            event_loop,
            &window,
            ws.seq,
            self.proxy.clone(),
            &self.ui_text,
        );
        let a11y_ms = t_a11y.elapsed().as_secs_f64() * 1000.0;

        // Reveal AFTER the accessibility adapter. AccessKit's winit adapter
        // panics if the window has already been shown when it is constructed
        // ("must be created before the window is shown (made visible) for the
        // first time"). The reveal has to sit between the adapter and renderer
        // init, and this ordering is load-bearing.
        //
        // Reveal before the GPU is ready only when the background is close
        // enough to black (see `background_is_dark_enough_to_reveal_early`).
        // Renderer init is ~730ms of startup, and on Windows an unpainted
        // window is black, so a near-black window can appear that much sooner
        // with no visible flash. Otherwise `window_shown` stays false and the
        // first paint reveals the window.
        //
        // The class brush is set to the terminal background as a best effort
        // only. The reveal does not depend on it, because winit owns
        // WM_ERASEBKGND and a light-background probe showed the pre-paint
        // window is black whatever the class brush says.
        if let Some(h) = window_hwnd(&window) {
            set_window_background_brush(h, self.cfg.theme.background);
        }
        let early_reveal = should_reveal_after_renderer_init(self.cfg.window_state)
            && background_is_dark_enough_to_reveal_early(self.cfg.theme.background);
        if early_reveal {
            window.set_visible(true);
            ws.window_shown = true;
            crate::startup_trace::mark(crate::startup_trace::Phase::WindowRevealed);
        }
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        // Guard the synchronous GPU init against a hung
        // graphics driver. `Renderer::new` block_on's wgpu's adapter+device
        // requests on this (event-loop) thread; a wedged driver or GPU reset
        // can make those never return, leaving kettle stuck on an invisible
        // window (kept hidden until the first paint) with no
        // diagnostic — indistinguishable from a crash. A watchdog thread (which
        // only ever touches an `AtomicBool`, so there's no Send/thread-affinity
        // hazard with the GPU objects) turns that infinite hang into a clean,
        // actionable exit. `done` is set the instant init returns — success OR
        // a quick failure — so only a true never-returns hang trips it.
        let gpu_init_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let gpu_init_done = gpu_init_done.clone();
            // If the watchdog thread can't be spawned, proceed without it
            // rather than failing init.
            let _ = std::thread::Builder::new()
                .name("kettle-gpu-init-watchdog".into())
                .spawn(move || {
                    if gpu_init_timed_out(
                        &gpu_init_done,
                        std::time::Duration::from_secs(GPU_INIT_TIMEOUT_SECS),
                        std::time::Duration::from_millis(100),
                    ) {
                        log::error!(
                            "GPU initialization did not finish within {GPU_INIT_TIMEOUT_SECS}s — \
                             your graphics driver may be hung or unresponsive. Try updating the \
                             GPU driver; on Linux you can force software rendering with \
                             `LIBGL_ALWAYS_SOFTWARE=1` or pick another backend via \
                             `WGPU_BACKEND=gl`. Exiting so kettle doesn't hang invisibly."
                        );
                        // exit() skips destructors, so run_with's guard never
                        // flushes this startup's trace.
                        crate::startup_trace::flush();
                        std::process::exit(1);
                    }
                });
        }
        let init_result = {
            let runtime_tracker = self.runtime_tracker.clone();
            let _phase = runtime_tracker.enter("gpu_init");
            pollster::block_on(Renderer::new_with_display_handle(
                window.clone(),
                event_loop.owned_display_handle(),
                size.width.max(1),
                size.height.max(1),
                scale,
                &self.cfg,
                ws.startup_fonts.take(),
            ))
        };
        // Disarm the watchdog the moment init returns, on BOTH the success and
        // the quick-failure path, so a clean renderer-init error below never
        // gets a spurious "timed out" report behind it.
        gpu_init_done.store(true, std::sync::atomic::Ordering::Release);
        let renderer = match init_result {
            Ok(r) => r,
            Err(e) => {
                log::error!("renderer init failed: {e}");
                ws.mux.kill_children();
                event_loop.exit();
                return;
            }
        };
        crate::startup_trace::mark(crate::startup_trace::Phase::GpuReady);
        log::info!(
            "startup: working set after gpu init {:.1} MiB",
            process_working_set_mb()
        );
        log::info!(
            "startup: window create+setup {create_window_ms:.1}ms, accessibility {a11y_ms:.1}ms,              gpu {:.1}ms (cumulative {:.1}ms)",
            t_startup.elapsed().as_secs_f64() * 1000.0 - create_window_ms - a11y_ms,
            t_startup.elapsed().as_secs_f64() * 1000.0
        );
        // Cache the shared GPU context for open_window (windows 2..N).
        let gpu = renderer.gpu().clone();
        let recovery_proxy = self.proxy.clone();
        gpu.set_recovery_wake(kettle_render::ScreenshotRecoveryWake::new(move || {
            let _ = recovery_proxy.send_event(UserEvent::Wakeup);
        }));
        self.gpu = Some(gpu);
        ws.renderer = Some(renderer);
        ws.startup_cell = None;
        ws.startup_surface = None;
        ws.accessibility = Some(accessibility);
        ws.window = Some(window);
        self.sync_output_frame_budget(ws, true);
        if let Some(theme) = ws.window.as_ref().and_then(|w| w.theme()) {
            self.apply_initial_os_theme_preference(ws, theme);
        }

        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);

        // CLI `-e cmd` / `-d dir` are consumed ONCE (they seed this window's
        // first tab and must not respawn on restore); take exactly those two
        // fields. The REST of `self.startup` stays intact for the whole
        // session — the explicit-restore gates below (`--tab-handoff` /
        // `--layout` / `--restore`), the save-session layout/restore gating,
        // and reload_config's launch-override re-application all
        // read it later.
        //
        // A wholesale `mem::take(&mut self.startup)` would silently default
        // those later reads, so `--layout` / `--restore` / `--tab-handoff`
        // would never load at startup and live reloads would drop the
        // `-m/-f/-b/-H/-T` overrides. Pinned by
        // `startup_is_not_taken_wholesale` below.
        let cmd_override = self.startup.command.take();
        let cwd_override = self.startup.cwd.take();
        let dev_record = match self.startup.recording.clone() {
            crate::RecordingPolicy::Enabled { target, raw_input } => Some((target, raw_input)),
            crate::RecordingPolicy::Disabled => None,
        };
        let restored = if spawned_early {
            true
        } else if has_launch_override {
            let argv = cmd_override.unwrap_or_default();
            let cwd = cwd_override
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            if let Err(e) = ws.mux.new_tab_with_geometry(
                &self.cfg,
                geometry,
                self.waker(),
                &argv,
                cwd.as_deref(),
            ) {
                log::error!("failed to spawn `-e` command: {e}");
                event_loop.exit();
                return;
            }
            true
        } else {
            match restore_plan.as_ref() {
                Some((wins, _)) => {
                    // The theme is config-governed (the config `theme =` line,
                    // with the compile-time default as fallback) and persisted
                    // via `persist_pref`. Restore never applies a
                    // session-stored theme, which would override the
                    // config/default on every restore. The session's `theme`
                    // field is ignored (kept only for back-compat parsing of
                    // older session.json files).
                    // `self.waker()` is the latched waker constructor; a
                    // hand-rolled closure would bypass the wakeup-dedup latch
                    // for every restored pane.
                    // Window 1 of the session restores into THIS window; each
                    // additional saved window opens via open_window(Restore),
                    // which works here because the GPU context was cached just
                    // above. A legacy single-window file normalizes to one
                    // entry.
                    if let Some((first, _)) = wins.split_first() {
                        let first_session = crate::session::Session {
                            tabs: first.tabs.to_vec(),
                            active: first.active,
                            theme: None,
                            windows: Vec::new(),
                        };
                        let geometries = self.restore_geometries(ws, &first_session);
                        let mk = || self.waker();
                        let outcome =
                            ws.mux
                                .restore_geometry(&first_session, &self.cfg, &geometries, &mk);
                        // A tab that could not be rebuilt is gone from the live
                        // mux, and the next save rewrites the file from it. Keep
                        // the original so a transient failure — a command not on
                        // PATH yet, a fork refused under a process limit — costs
                        // nothing permanent.
                        if outcome.skipped > 0 {
                            self.stash_unrestorable_session(&format!(
                                "{} tab(s) could not be rebuilt",
                                outcome.skipped
                            ));
                        }
                        outcome.restored_any
                    } else {
                        false
                    }
                }
                _ => false,
            }
        };
        if !restored && let Err(e) = ws.mux.new_tab_geometry(&self.cfg, geometry, self.waker()) {
            log::error!("failed to spawn shell: {e}");
            event_loop.exit();
            return;
        }
        if !spawned_early {
            crate::startup_trace::mark(crate::startup_trace::Phase::PaneSpawned);
            crate::startup_trace::note_path(crate::startup_trace::StartupPath::AfterRenderer);
        }
        if ws.cursor_exit_log.enabled
            && let Some(pane_id) = ws.mux.tabs.get(ws.mux.active).map(|tab| tab.focus)
            && let Some(window) = ws.window.as_deref()
        {
            ws.cursor_exit_log.bind(ws.seq, pane_id, window);
        }
        self.resize_all(ws);
        // Use the restored layout for the header before sibling windows can drain output.
        if let Some((target, raw)) = dev_record {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            match crate::dev_record::Recorder::start_target_async(
                &target,
                cols as u16,
                rows as u16,
                raw,
            ) {
                Ok(mut rec) => {
                    log::info!("dev-record: queued asynchronous start for {target:?}");
                    let proxy = self.proxy.clone();
                    rec.set_failure_waker(Arc::new(move || {
                        let _ = proxy.send_event(UserEvent::Wakeup);
                    }));
                    self.recording_start_failed = false;
                    self.recording_error_reported = false;
                    self.recorder = Some(rec);
                    self.sync_recording_state(ws, "");
                }
                Err(e) => {
                    log::warn!("dev-record: could not start recorder for {target:?}: {e}");
                    self.recording_start_failed = true;
                    let body = self
                        .ui_text
                        .notify_body_recording_start_failed(&e.to_string());
                    self.sync_recording_state(ws, &body);
                }
            }
        }
        if !spawned_early
            && !has_launch_override
            && let Some((wins, surfaces)) = restore_plan.as_ref()
            && let Some((_, rest)) = wins.split_first()
            && !rest.is_empty()
        {
            for (sw, surface) in rest.iter().zip(&surfaces[1..]) {
                let sw = crate::session::SWindow {
                    tabs: sw.tabs.to_vec(),
                    active: sw.active,
                    geometry: surface.geometry(),
                };
                let size = winit::dpi::PhysicalSize::new(surface.size.0, surface.size.1);
                if self
                    .open_window(event_loop, WindowOpen::Restore(sw), None, Some(size))
                    .is_err()
                {
                    self.stash_unrestorable_session("could not open an additional window");
                    log::warn!("session restore: could not open an additional window");
                }
            }
            // Focus comes home to window 1 (open_window moves
            // focused_seq to each window it opens).
            self.focused_seq = ws.seq;
        }
        // Start the control server right after the first pane exists and BEFORE
        // the first GPU paint, which can take several seconds on a cold shader
        // cache. The server only needs the pid + a live proxy for its waker, so
        // binding it here makes the agent surface available as soon as the
        // window comes up. A re-resume does not start it again.
        if self.ctl_start == CtlStart::NotReady {
            self.ctl_start = CtlStart::Ready;
        }
        self.ensure_ctl_started();
        // Terminator plugin parity: fire
        // LuaEvent::Startup the first time we have an alive window
        // + at least one pane. Subsequent resumed() calls (Wayland
        // can re-emit) get short-circuited by lua_startup_fired.
        // Drains any LuaCommand the callbacks queued so a
        // `kettle.on('startup', function() kettle.send_text(...) end)`
        // takes effect immediately.
        if !self.lua_startup_fired && self.lua_engine.is_some() && ws.mux.focused().is_some() {
            self.fire_lua_event(ws, crate::LuaEvent::Startup, "startup hook");
            self.lua_startup_fired = true;
        }
        // Top-level script commands precede startup-hook commands in the same
        // FIFO. Run one bounded slice before the first paint; a stalled child
        // retains the exact head message and about_to_wait drives retries.
        let _ = self.poll_pending_lua_commands(ws, event_loop, std::time::Instant::now());
        if should_reveal_before_first_surface_frame(
            self.cfg.window_state,
            ws.window_shown,
            cfg!(target_os = "macos"),
        ) {
            if let Some(w) = &ws.window {
                w.set_visible(true);
            }
            ws.window_shown = true;
            crate::startup_trace::mark(crate::startup_trace::Phase::WindowRevealed);
            log::info!(
                "startup: window revealed before first macOS surface frame, working set {:.1} MiB",
                process_working_set_mb()
            );
        }
        self.start_window_frames(ws);
        // A window that starts hidden presents nothing until it is shown, so
        // its startup trace ends here, without a first frame. Any other
        // window's trace is printed by the first frame it presents.
        if !should_reveal_after_renderer_init(self.cfg.window_state) {
            crate::startup_trace::flush();
        }
        // The first frame is scheduled. The signed-feed worker skips the first
        // launch, applies the shared per-interval throttle, and honors
        // off/notify/auto. A recurring timer re-checks a long-lived window.
        crate::update_check::maybe_spawn_check(
            self.proxy.clone(),
            env!("CARGO_PKG_VERSION"),
            self.startup.update_install.clone(),
            self.cfg.update_policy,
            self.cfg.update_check_interval_hours,
        );
        crate::update_check::spawn_update_check_timer(self.proxy.clone());
        self.request_update_facts(true);
        // Warm the shell-detection cache (wsl.exe /
        // vswhere probes, bounded ~2s each) off the UI thread so the first
        // dropdown open / Ctrl+Shift+N press doesn't pay it.
        let proxy = self.proxy.clone();
        kettle_core::term::prewarm_shell_detection(Arc::new(move || {
            let _ = proxy.send_event(UserEvent::ShellsDetected);
        }));
    }
}
