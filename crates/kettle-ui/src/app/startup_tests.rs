#[test]
fn startup_plan_truth_table() {
    use kettle_config::WindowState::{Fullscreen, Hidden, Maximise, Normal};
    for command in [false, true] {
        for cwd in [false, true] {
            for restore_bits in 0u8..32 {
                if !cfg!(unix) && restore_bits & 4 != 0 {
                    continue;
                }
                for state in [Normal, Hidden, Maximise, Fullscreen] {
                    for position in 0u8..4 {
                        let options = crate::Options {
                            command: command.then(|| vec!["/bin/true".into()]),
                            cwd: cwd.then(|| std::path::PathBuf::from("/tmp")),
                            layout: (restore_bits & 1 != 0).then(|| "test".into()),
                            tab_handoff: (restore_bits & 2 != 0).then(|| "/tmp/handoff".into()),
                            tab_handoff_fd: {
                                #[cfg(unix)]
                                {
                                    use std::os::fd::IntoRawFd;
                                    (restore_bits & 4 != 0).then(|| {
                                        let (socket, _peer) =
                                            std::os::unix::net::UnixStream::pair().unwrap();
                                        // SAFETY: into_raw_fd gave up the only other owner.
                                        unsafe { crate::TabHandoff::adopt(socket.into_raw_fd()) }
                                            .unwrap()
                                    })
                                }
                                #[cfg(not(unix))]
                                {
                                    None
                                }
                            },
                            restore: restore_bits & 8 != 0,
                            ..Default::default()
                        };
                        let cfg = kettle_config::Config {
                            restore_session: if restore_bits & 16 != 0 {
                                kettle_config::RestoreSession::Always
                            } else {
                                kettle_config::RestoreSession::Never
                            },
                            window_state: state,
                            window_position_x: (position & 1 != 0).then_some(10),
                            window_position_y: (position & 2 != 0).then_some(10),
                            ..Default::default()
                        };
                        let override_launch = command || cwd;
                        let restore = !override_launch && restore_bits != 0;
                        assert_eq!(
                            super::startup_plan(&options, &cfg, false),
                            super::StartupPlan {
                                launch_override: override_launch,
                                may_restore: restore,
                                pre_launch: !restore
                                    && matches!(state, Normal | Hidden)
                                    && position == 0,
                            },
                            "command={command} cwd={cwd} restore={restore_bits} state={state:?} position={position}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn display_arithmetic_matches_winit_at_one_two_and_three_x() {
    use super::{StartupMonitor, fits_monitor};
    for scale in [1.0, 2.0, 3.0] {
        for (width, height) in [(1920usize, 1080usize), (1371, 773)] {
            let monitor = StartupMonitor::from_display_points(width, height, scale);
            assert_eq!(
                monitor,
                StartupMonitor::from_physical(
                    (width as f64 * scale).round() as u32,
                    (height as f64 * scale).round() as u32,
                    scale
                )
            );
            assert_eq!(monitor.logical, (width as f64, height as f64));
            let limit = (
                ((width as f64 * super::STARTUP_MONITOR_WIDTH_FRACTION).floor() * scale) as u32,
                ((height as f64 * super::STARTUP_MONITOR_HEIGHT_FRACTION).floor() * scale) as u32,
            );
            assert!(fits_monitor(limit, Some(monitor)));
            assert!(fits_monitor((limit.0 + 1, limit.1 + 1), Some(monitor)));
            assert!(!fits_monitor((limit.0 + 2, limit.1), Some(monitor)));
            assert!(!fits_monitor((limit.0, limit.1 + 2), Some(monitor)));
            assert!(!fits_monitor(limit, None));
        }
    }
}

#[test]
fn pre_launch_spawn_sits_between_app_build_and_run_app() {
    let src = super::production_source();
    let run = src
        .split_once("pub fn run_with(")
        .unwrap()
        .1
        .split_once("\n    }\n")
        .unwrap()
        .0;
    let order = [
        run.find("EventLoop::<UserEvent>::with_user_event().build()")
            .unwrap(),
        run.find("let plan = startup_plan(&startup, &initial_cfg, another_build_running);")
            .unwrap(),
        run.find("let mut app = App {").unwrap(),
        run.find("startup_trace::Phase::AppBuilt").unwrap(),
        run.find("app.start_first_pane_before_launch()").unwrap(),
        run.find("event_loop.run_app(&mut app)").unwrap(),
        run.find("app.pasted_images.cleanup();").unwrap(),
        // Copies handed to the image viewer, the document viewer and the
        // video player go on exit too.
        run.find("app.viewer_copies").unwrap(),
        run.find("app.document_copies.close();").unwrap(),
        run.find("app.video_copies.close();\n        if let Err(error) = &result {")
            .unwrap(),
        run.find("app.runtime_tracker.stop();").unwrap(),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]));
    assert!(run.contains("Err(anyhow::anyhow!(error))"));
    assert!(run.contains("launch_override: plan.launch_override,"));
    let early = src
        .split_once("fn start_first_pane_before_launch(")
        .unwrap()
        .1
        .split_once("\n    }\n")
        .unwrap()
        .0;
    assert!(early.contains("#[cfg(not(target_os = \"macos\"))]"));
    assert!(early.contains("if !self.pre_launch_eligible"));
    assert!(early.contains("self.startup_surface(&ws, Some(monitor))"));
    assert!(early.contains("fits_monitor(size, Some(monitor))"));
    assert!(early.contains("self.windows.insert(1, ws);"));
    let flat: String = early.split_whitespace().collect();
    assert!(
        flat.find("self.prepare_startup_fonts(&mutws,monitor.scaleasf32)")
            .unwrap()
            < flat
                .find("self.spawn_first_tab(&mutws,self.launch_override)")
                .unwrap()
    );
}

#[test]
fn resumed_preserves_the_launch_override_and_pre_launch_pane() {
    let src = super::production_source();
    let body = src
        .split_once("fn resumed_inner(")
        .unwrap()
        .1
        .split_once("\n    }\n")
        .unwrap()
        .0;
    assert!(body.contains("let has_launch_override = self.launch_override || self.startup.quick_terminal;"));
    assert!(!body.contains("self.startup.command.is_some()"));
    assert!(!body.contains("self.startup.cwd.is_some()"));
    assert!(body.contains("ws.pre_launch.is_some() || has_launch_override"));
    assert!(body.contains("self.named_layout_writable = !has_launch_override;"));
    assert!(body.contains("crate::startup_trace::note_monitor_match(matched);"));
    let flat: String = body.split_whitespace().collect();
    assert!(
        flat.contains("ifws.pre_launch.is_none(){self.prepare_startup_fonts(ws,startup_scale);}")
    );
    assert!(flat.contains("ifws.pre_launch.is_none()&&spawned_early&&letErr(e)=self.spawn_first_tab(ws,has_launch_override)"));
    assert!(flat.contains("letrestored=ifspawned_early{true}"));
    for message in ["failed to create window: {e}", "renderer init failed: {e}"] {
        let failure = body
            .split_once(message)
            .unwrap()
            .1
            .split_once("return;")
            .unwrap()
            .0;
        assert!(
            failure.find("ws.mux.kill_children();").unwrap()
                < failure.find("event_loop.exit();").unwrap()
        );
    }
    let wait = src
        .split_once("fn about_to_wait(&mut self,")
        .unwrap()
        .1
        .split_once("// C1-DISPATCH-END")
        .unwrap()
        .0;
    assert!(
        wait.find("pane.term.poll_startup_geometry()").unwrap()
            < wait.find("self.about_to_wait_inner(").unwrap()
    );
    assert!(wait.contains("current.min(deadline)"));
}

/// The first window opens at exactly `window-width` x `window-height`
/// cells, whatever the cell size, padding, and chrome. The old 8x16 guess
/// opened a 120x36 request at 123x35.
#[test]
fn a_startup_surface_holds_exactly_the_requested_grid() {
    use kettle_config::{StatusBarMode, TabBarPos};
    let pad = (8.0, 8.0);
    let strip = 180.0;
    for pos in [
        TabBarPos::Top,
        TabBarPos::Bottom,
        TabBarPos::Left,
        TabBarPos::Right,
    ] {
        for status in [
            StatusBarMode::Off,
            StatusBarMode::Top,
            StatusBarMode::Bottom,
        ] {
            for tab_bar_h in [0.0, 25.5] {
                for hundredths in (500..3000).step_by(7) {
                    let cw = hundredths as f32 / 100.0;
                    let ch = cw * 2.1;
                    let bands = super::ContentBands {
                        tab_bar_h,
                        status_bar_h: if matches!(status, StatusBarMode::Off) {
                            0.0
                        } else {
                            ch + 6.0
                        },
                        ..Default::default()
                    };
                    for grid in [(20, 5), (80, 24), (120, 36), (211, 57), (300, 100)] {
                        let surface =
                            super::surface_for_grid(grid, (cw, ch), pad, bands, pos, status, strip);
                        let (_, _, w, h) =
                            super::content_rect_for_with_strip(surface, bands, pos, status, strip);
                        assert_eq!(
                            super::grid_dims_px((w, h), (cw, ch), pad),
                            grid,
                            "cell {cw}x{ch}, {pos:?}, status {status:?}, surface {surface:?}"
                        );
                    }
                }
            }
        }
    }
}

/// The system fonts load on their own thread from the start of
/// `run_with`, so enumerating them overlaps the event loop's start
/// (AppKit's launch, on macOS) instead of delaying the first pane, and
/// window 1 only measures them. The thread starts before anything can
/// fail or read the config, learns the family once the command-line
/// overrides are in, and is joined before the first pane spawns.
#[test]
fn the_fonts_load_while_the_event_loop_starts() {
    let src = super::production_source();
    let run = src
        .split_once("pub fn run_with(mut startup: crate::Options) -> Result<()> {")
        .expect("run_with")
        .1;
    let run = &run[..run.find("event_loop.run_app(&mut app)").expect("run_app")];
    let start = run
        .find("let mut font_preload = crate::font_preload::FontPreload::start();")
        .expect("run_with starts the font preload");
    let trace = run
        .find("let _trace = crate::startup_trace::FlushOnDrop;")
        .expect("trace guard");
    assert!(
        trace < start
            && run[trace..start]
                .lines()
                .skip(1)
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with("//")),
        "the preload starts right after the trace guard"
    );
    for later in [
        "crate::paste_image::sweep_stale();",
        "disable_app_state_restoration();",
        "EventLoop::<UserEvent>::with_user_event().build()",
        "Config::read_from_with_trust(",
    ] {
        let at = run.find(later).unwrap_or_else(|| panic!("missing {later}"));
        assert!(start < at, "the preload starts before {later}");
    }
    let loaded = run
        .find("crate::startup_trace::mark(crate::startup_trace::Phase::ConfigLoaded);")
        .expect("config loaded");
    let request = run
        .find("font_preload.request_family(&initial_cfg.font_family);")
        .expect("run_with sends the configured family");
    assert!(
        loaded < request,
        "the family is final once the overrides are in"
    );
    assert!(run[request..].contains("font_preload: Some(font_preload),"));
    assert_eq!(src.matches("FontPreload::start()").count(), 1);

    // Window 1 takes the preload before it sizes and spawns its first
    // pane; no other window waits for it. The measured fonts wait in
    // `ws.startup_fonts` for the renderer, which would otherwise load
    // them a second time on the main thread.
    assert_eq!(src.matches("self.font_preload.take()").count(), 1);
    let resumed = src
        .split_once("fn resumed_inner(")
        .expect("resumed_inner")
        .1;
    let take = resumed
        .find("self.prepare_startup_fonts(ws, startup_scale);")
        .expect("window 1 measures the preloaded fonts");
    let helper = src
        .split_once("fn prepare_startup_fonts(")
        .unwrap()
        .1
        .split_once("\n    }\n")
        .unwrap()
        .0;
    assert!(helper.contains("self.font_preload.take()"));
    assert!(helper.contains("ws.startup_fonts = Some(fonts);"));
    let spawn = resumed
        .find("self.spawn_first_tab(ws, has_launch_override)")
        .expect("early spawn");
    assert!(take < spawn);
}

/// Startup drift guard: the window is hidden only while renderer init runs,
/// then revealed for every visible startup state. `window_state = hidden`
/// still stays hidden.
#[test]
fn window_revealed_after_renderer_init_for_visible_states_only() {
    use kettle_config::WindowState;
    assert!(should_reveal_after_renderer_init(WindowState::Normal));
    assert!(should_reveal_after_renderer_init(WindowState::Maximise));
    assert!(should_reveal_after_renderer_init(WindowState::Fullscreen));
    assert!(!should_reveal_after_renderer_init(WindowState::Hidden));
}

/// Surface backends such as macOS Metal need the native window ordered on
/// screen before their first drawable can exist. Pin the portable policy
/// independently of the host running this unit test.
#[test]
fn visible_surface_requirement_breaks_the_hidden_first_frame_cycle() {
    use kettle_config::WindowState;
    assert!(should_reveal_before_first_surface_frame(
        WindowState::Normal,
        false,
        true
    ));
    assert!(!should_reveal_before_first_surface_frame(
        WindowState::Hidden,
        false,
        true
    ));
    assert!(!should_reveal_before_first_surface_frame(
        WindowState::Normal,
        true,
        true
    ));
    assert!(!should_reveal_before_first_surface_frame(
        WindowState::Normal,
        false,
        false
    ));
}

#[test]
fn startup_keeps_the_remote_spool_while_another_build_runs() {
    let dir = crate::test_tempdir();
    let path = dir.path().join("remote.cmd");
    std::fs::write(&path, b"new-tab\n").unwrap();
    super::initialize_remote_spool(&path, true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new-tab\n");
    super::initialize_remote_spool(&path, false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"");
}

/// Explicit grids at the 13 px font's 8x16 sizing cell.
#[test]
fn startup_geometry_cells_convert_to_inner_size() {
    let baseline = super::startup_geometry_cell(13.0);
    assert_eq!(baseline, (8.0, 16.0));
    let mut cfg = kettle_config::Config {
        macos_titlebar_style: kettle_config::MacosTitlebarStyle::Native,
        window_width: Some(120),
        window_height: Some(40),
        ..Default::default()
    };
    assert_eq!(
        startup_inner_size_px(&cfg, baseline),
        Some((984, 664)),
        "120x40 cells + default padding; the auto tab bar is hidden"
    );

    cfg.tab_bar = kettle_config::TabBarMode::Auto;
    cfg.status_bar = kettle_config::StatusBarMode::Bottom;
    assert_eq!(
        startup_inner_size_px(&cfg, baseline),
        Some((984, 686)),
        "auto tab bar is hidden for a one-tab startup; status bar reserves its strip"
    );

    cfg.window_height = None;
    cfg.tab_bar = kettle_config::TabBarMode::Always;
    cfg.tab_bar_pos = kettle_config::TabBarPos::Left;
    assert_eq!(
        startup_inner_size_px(&cfg, baseline),
        Some((1164, 766)),
        "missing height comes from the 45-row default grid; vertical tabs reserve width"
    );
    cfg.window_width = None;
    cfg.window_height = Some(30);
    cfg.tab_bar_pos = kettle_config::TabBarPos::Top;
    assert_eq!(
        startup_inner_size_px(&cfg, baseline),
        Some((1304, 554)),
        "missing width comes from the 160-column default grid"
    );
    cfg.window_height = None;
    assert_eq!(
        startup_inner_size_px(&cfg, baseline),
        None,
        "neither side set: the monitor-fitted default takes over"
    );
}

#[test]
fn startup_titlebar_tabs_reserve_the_single_tab_band() {
    let baseline = super::startup_geometry_cell(13.0);
    let mut cfg = kettle_config::Config {
        window_width: Some(120), window_height: Some(40),
        tab_bar: kettle_config::TabBarMode::Auto,
        status_bar: kettle_config::StatusBarMode::Bottom,
        ..Default::default()
    };
    assert_eq!(startup_inner_size_px(&cfg, baseline), Some((984,
        if cfg!(target_os = "macos") { 714 } else { 686 })));
    cfg.macos_titlebar_style = kettle_config::MacosTitlebarStyle::Native;
    assert_eq!(startup_inner_size_px(&cfg, baseline), Some((984, 686)));
    cfg.borderless = true;
    cfg.macos_titlebar_style = kettle_config::MacosTitlebarStyle::Tabs;
    assert_eq!(startup_inner_size_px(&cfg, baseline), Some((984, 686)));
}

/// A fresh window with no configured size opens at the 160x45 default
/// grid (1304x744 logical px at the 13 px font's sizing cell, with a native
/// titlebar and the single-tab strip hidden), fitted to 90 % x 85 % of the
/// monitor. The numbers below are the ones the docs quote; the macOS
/// titlebar-tabs band is covered by
/// `startup_titlebar_tabs_reserve_the_single_tab_band`.
#[test]
fn default_startup_size_targets_the_agent_grid_and_fits_the_monitor() {
    use super::{
        STARTUP_DEFAULT_COLS, STARTUP_DEFAULT_ROWS, STARTUP_GEOMETRY_CELL, StartupMonitor,
        default_startup_inner_size, startup_geometry_cell, startup_inner_size,
        startup_inner_size_px,
    };
    let cfg = kettle_config::Config {
        macos_titlebar_style: kettle_config::MacosTitlebarStyle::Native,
        ..Default::default()
    };
    let baseline = startup_geometry_cell(13.0);
    assert_eq!((STARTUP_DEFAULT_COLS, STARTUP_DEFAULT_ROWS), (160, 45));
    assert_eq!(
        default_startup_inner_size(&cfg, None, baseline),
        (1304.0, 744.0),
        "160x45 cells + 12 px padding with the single-tab strip hidden, no monitor known"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((1920.0, 1080.0)), baseline),
        (1304.0, 744.0),
        "a 1080p monitor fits the whole target (164x44 cells at the 13 px font)"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((1366.0, 768.0)), baseline),
        (1229.0, 652.0),
        "a 1366x768 laptop clamps both axes: 154 columns at the 13 px font"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((3440.0, 1440.0)), baseline),
        (1304.0, 744.0),
        "an ultrawide gets the same terminal-sized window, not a monitor fraction"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((640.0, 480.0)), baseline),
        (576.0, 408.0),
        "a monitor smaller than the target wins outright; there is no floor"
    );

    // The explicit path is honoured as typed and never monitor-fitted.
    let explicit = kettle_config::Config {
        window_width: Some(400),
        window_height: Some(200),
        macos_titlebar_style: kettle_config::MacosTitlebarStyle::Native,
        ..Default::default()
    };
    let tiny = Some(StartupMonitor::from_physical(640, 480, 1.0));
    assert_eq!(
        startup_inner_size_px(&explicit, baseline),
        Some((3224, 3224))
    );
    let (w, h) = startup_inner_size_px(&explicit, STARTUP_GEOMETRY_CELL).unwrap();
    assert_eq!(
        startup_inner_size(&explicit, tiny),
        (f64::from(w), f64::from(h)),
        "this target sizes from its own default font's cell"
    );
    assert_eq!(
        startup_inner_size(&cfg, tiny),
        (576.0, 408.0),
        "the default path is the one that fits the monitor"
    );

    // HiDPI: the size is logical, so a 2x display describes the same
    // logical monitor and gets the same grid as a 1x one.
    let hidpi = StartupMonitor::from_physical(3840, 2160, 2.0);
    assert_eq!(hidpi.logical, (1920.0, 1080.0));
    assert_eq!(hidpi.scale, 2.0);
    assert_eq!(
        startup_inner_size(&cfg, Some(hidpi)),
        startup_inner_size(&cfg, Some(StartupMonitor::from_physical(1920, 1080, 1.0)))
    );
    assert_eq!(
        startup_inner_size(&explicit, Some(hidpi)),
        startup_inner_size(
            &explicit,
            Some(StartupMonitor::from_physical(1920, 1080, 1.0))
        ),
        "an explicit window-width means the same columns on every DPI"
    );
    let degenerate = StartupMonitor::from_physical(1920, 1080, 0.0);
    assert_eq!(
        degenerate.scale, 1.0,
        "a bogus scale factor falls back to 1x"
    );
}

/// The grid a fresh window with no configured size gets when the default
/// font is `font_size` px and the window is sized from `baseline`: the
/// bundled font's measured cell, laid out over the surface
/// `startup_surface_for` gives the default path. Returns the surface too.
fn fresh_window_grid(
    font_size: f32,
    baseline: (f64, f64),
    monitor: (f64, f64),
    scale: f64,
) -> ((u32, u32), (usize, usize)) {
    use super::{
        ContentBands, StartupMonitor, content_rect_for_with_strip, default_startup_inner_size,
        grid_dims_px,
    };
    let cfg = kettle_config::Config {
        font_size,
        macos_titlebar_style: kettle_config::MacosTitlebarStyle::Native,
        ..Default::default()
    };
    let cell = kettle_render::StartupFonts::load(&cfg, scale as f32).cell;
    let monitor = StartupMonitor::from_physical(
        (monitor.0 * scale) as u32,
        (monitor.1 * scale) as u32,
        scale,
    );
    let (w, h) = default_startup_inner_size(&cfg, Some(monitor.logical), baseline);
    let physical = |logical: f64| (logical * monitor.scale).round().max(1.0) as u32;
    let surface = (physical(w), physical(h));
    let area = content_rect_for_with_strip(
        surface,
        ContentBands::default(),
        cfg.tab_bar_pos,
        cfg.status_bar,
        cfg.tab_bar_width,
    );
    let grid = grid_dims_px((area.2, area.3), cell, (cfg.padding_x, cfg.padding_y));
    (surface, grid)
}

/// The grids the docs quote for a fresh window on each platform, measured
/// with the shipped font and padding rather than the sizing baseline. Both
/// get 164x44 cells on a 1080p monitor. On a 1366x768 laptop macOS's 13 px
/// gets 154 columns, past the 144 at which Claude Code's diff panel opens;
/// the 15 px default elsewhere gets 133 there, and needs the window maximized
/// (`the_default_font_fits_144_columns_across_a_1366_px_screen`). A change to
/// either default font or the padding fails here, so the docs move with it.
#[test]
fn a_default_window_at_the_shipped_font_holds_the_documented_grid() {
    use super::{
        ContentBands, STARTUP_GEOMETRY_CELL, StartupMonitor, startup_geometry_cell,
        startup_surface_for,
    };
    use kettle_config::default_font_size;
    for (macos, laptop, hidpi_laptop) in [
        (true, (154, 38), [(155, 38), (156, 39)]),
        (false, (133, 33), [(134, 33), (135, 34)]),
    ] {
        let size = default_font_size(macos);
        let grid = |monitor, scale| {
            fresh_window_grid(size, startup_geometry_cell(size), monitor, scale).1
        };
        assert_eq!(grid((1920.0, 1080.0), 1.0), (164, 44), "{size} px");
        assert_eq!(grid((1366.0, 768.0), 1.0), laptop, "{size} px");
        for (scale, expected) in [1.25, 2.0].into_iter().zip(hidpi_laptop) {
            assert_eq!(grid((1366.0, 768.0), scale), expected, "{size} px at {scale}x");
        }
    }

    // The helper lays out the surface this target's window constructors use.
    let cfg = kettle_config::Config {
        macos_titlebar_style: kettle_config::MacosTitlebarStyle::Native,
        ..Default::default()
    };
    let size = kettle_config::DEFAULT_FONT_SIZE;
    assert_eq!(cfg.font_size, size);
    for monitor in [(1920.0, 1080.0), (1366.0, 768.0)] {
        for scale in [1.0, 2.0] {
            let cell = kettle_render::StartupFonts::load(&cfg, scale as f32).cell;
            let display = StartupMonitor::from_physical(
                (monitor.0 * scale) as u32,
                (monitor.1 * scale) as u32,
                scale,
            );
            assert_eq!(
                startup_surface_for(&cfg, cell, ContentBands::default(), Some(display)),
                fresh_window_grid(size, STARTUP_GEOMETRY_CELL, monitor, scale).0,
                "{monitor:?} at {scale}x"
            );
        }
    }
}

/// 15 px is the largest whole-pixel default whose bundled-font cell still
/// fits the 144 columns Claude Code's diff panel opens at across a 1366 px
/// wide screen (a maximized laptop window) with the default padding: its 9 px
/// cell gives (1366 - 24) / 9 = 149 columns, where 16 px (9.6 px) would give
/// 139. macOS's 13 px (7.8 px) gives 172.
#[test]
fn the_default_font_fits_144_columns_across_a_1366_px_screen() {
    use super::{ContentBands, content_rect_for_with_strip, grid_dims_px};
    use kettle_config::default_font_size;
    let grid = |font_size: f32| {
        let cfg = kettle_config::Config {
            font_size,
            ..Default::default()
        };
        let cell = kettle_render::StartupFonts::load(&cfg, 1.0).cell;
        let area = content_rect_for_with_strip(
            (1366, 768),
            ContentBands::default(),
            cfg.tab_bar_pos,
            cfg.status_bar,
            cfg.tab_bar_width,
        );
        let grid = grid_dims_px((area.2, area.3), cell, (cfg.padding_x, cfg.padding_y));
        (cell, grid.0)
    };
    let (cell, cols) = grid(default_font_size(false));
    assert!(
        (cell.0 - 9.0).abs() < 1e-3 && (cell.1 - 18.75).abs() < 1e-3,
        "the bundled font at 15 px: {cell:?}"
    );
    assert_eq!(cols, 149);
    assert!(
        grid(default_font_size(false) + 1.0).1 < 144,
        "a larger default would lose the panel"
    );
    assert_eq!(grid(default_font_size(true)).1, 172);
}

/// The live default-window smoke mirrors the platform default font sizes to
/// size its expectations; keep its copy in step.
#[test]
fn the_default_window_smoke_mirrors_the_default_font_sizes() {
    use kettle_config::default_font_size;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/check-live-ui-smoke.py");
    let script = std::fs::read_to_string(&path).expect("check-live-ui-smoke.py");
    let mirror = format!(
        "return {:?} if system == \"Darwin\" else {:?}",
        default_font_size(true),
        default_font_size(false)
    );
    assert!(script.contains(&mirror), "{} must say {mirror}", path.display());
}

/// The sizing cell is 8x16 at the 13 px font and grows with a larger
/// default font, so a fresh window keeps aiming at the 160x45 grid. With a
/// fixed 8x16 cell a 15 px default would open a 1080p window at 1304x744 px,
/// 142x38 cells, short of the 144 columns Claude Code's diff panel opens at.
#[test]
fn the_startup_sizing_cell_follows_the_default_font_size() {
    use super::{STARTUP_GEOMETRY_CELL, startup_geometry_cell};
    assert_eq!(startup_geometry_cell(13.0), (8.0, 16.0));
    assert_eq!(
        STARTUP_GEOMETRY_CELL,
        startup_geometry_cell(kettle_config::DEFAULT_FONT_SIZE)
    );
    let fifteen = startup_geometry_cell(15.0);
    let (surface, grid) = fresh_window_grid(15.0, fifteen, (1920.0, 1080.0), 1.0);
    assert_eq!(surface, (1501, 855));
    assert_eq!(grid, (164, 44), "15 px keeps the 13 px font's grid");
    for scale in [1.25, 2.0] {
        let (_, (cols, rows)) = fresh_window_grid(15.0, fifteen, (1920.0, 1080.0), scale);
        assert!(cols >= grid.0 && rows >= grid.1, "{cols}x{rows} at {scale}x");
    }
}

/// Both window constructors must size through the shared startup rule in
/// logical pixels; a `PhysicalSize` at startup would bring back the
/// half-columns-on-HiDPI bug, and skipping the monitor would lose the fit.
#[test]
fn window_constructors_size_through_the_shared_startup_rule() {
    let src = production_source();
    let attrs = src
        .split("fn window_attributes(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("window_attributes body");
    assert!(
        attrs.contains("let (w, h) = startup_inner_size(&self.cfg, monitor);")
            && attrs.contains("attrs.with_inner_size(winit::dpi::LogicalSize::new(w, h));"),
        "window_attributes must apply the shared startup size as a LogicalSize"
    );
    assert!(
        !attrs.contains("PhysicalSize"),
        "startup sizing must not build a PhysicalSize"
    );
    assert_eq!(
        src.matches("self.window_attributes(").count(),
        2,
        "exactly the two window constructors build attributes"
    );
    assert!(
        src.contains(
            "let monitor = startup_monitor(event_loop);\n        let mut attrs = self.window_attributes(state, monitor);"
        ) && src.contains("self.window_attributes(initial_window_state, monitor)"),
        "both constructors must pass the startup monitor"
    );
    assert_eq!(
        src.matches("self.startup_surface(").count(),
        3,
        "pre-launch and both constructors use the exact configured-grid rule"
    );
    assert!(
        src.contains("let (w, h) = startup_inner_size(&self.cfg, monitor);\n            let scale = monitor.map_or(1.0, |m| m.scale);"),
        "the restore planner's fallback surface must be the same rule in physical pixels"
    );
}

/// Regression guard. `resumed_inner` must take ONLY the consumed-once CLI
/// fields (`command`, `cwd`) from `self.startup`. A wholesale
/// `mem::take(&mut self.startup)` silently defaults every later read: the
/// `--tab-handoff` / `--layout` / `--restore` startup gates (which then
/// load nothing, so a 2-tab `--layout` opens 1 tab), the save-session
/// layout/restore gating, and reload_config's launch-override
/// re-application.
#[test]
fn startup_is_not_taken_wholesale() {
    let src = production_source();
    let taken = concat!("std::mem::take", "(&mut self.startup)");
    assert!(
        !src.contains(taken),
        "self.startup must never be wholesale-taken; take the \
             consumed-once fields individually"
    );
    assert!(
        src.contains("let cmd_override = self.startup.command.take();")
            && src.contains("let cwd_override = self.startup.cwd.take();"),
        "the -e/-d overrides are the only consumed-once startup fields"
    );
}

/// AppKit reads `ApplePersistence` when the application launches, and
/// building the event loop launches it. Registered any later, the
/// persistent-UI flush keeps running while idle.
#[test]
fn app_state_restoration_is_off_before_the_event_loop_exists() {
    let src = production_source();
    let run = src
        .split("pub fn run_with(mut startup: crate::Options) -> Result<()> {")
        .nth(1)
        .expect("run_with");
    let disable = run
        .find("disable_app_state_restoration();")
        .expect("run_with must turn off AppKit's persistent UI");
    let build = run
        .find("EventLoop::<UserEvent>::with_user_event().build()")
        .expect("run_with builds the event loop");
    assert!(
        disable < build,
        "ApplePersistence must be registered before the event loop exists"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn app_state_restoration_registers_apple_persistence_off() {
    use objc2_foundation::{NSRegistrationDomain, NSString, NSUserDefaults};

    super::disable_app_state_restoration();
    // SAFETY: plain Foundation reads on this thread's defaults object.
    let registered = unsafe {
        NSUserDefaults::standardUserDefaults().volatileDomainForName(NSRegistrationDomain)
    };
    let value = registered
        .get(&*NSString::from_str("ApplePersistence"))
        .expect("ApplePersistence is registered");
    // SAFETY: the registered value is the NSNumber this module created,
    // and `boolValue` takes no arguments.
    let on: bool = unsafe { objc2::msg_send![value, boolValue] };
    assert!(!on, "persistent UI must be off");
}

/// The join neither crate can check on its own.
///
/// `cache_dir_from_env` decides where kettle puts its `kettle/` directory;
/// `kettle_state::is_kettle_owned_dir_name` decides whether that directory
/// may be repaired to `0700`. kettle-state builds its base list from the
/// environment and cannot depend on kettle-ui to ask where the cache went,
/// so a resolver base missing from that list is invisible to every test in
/// either crate. Such a gap can leave `~/.cache/kettle` at `0775` with
/// `0600` content underneath it.
///
/// Each resolver branch runs in a re-executed child. Environment mutation in
/// this test process would race every other test's `getenv`, and checking
/// only the ambient branch would skip under `env -i` and never exercise XDG
/// on normal CI.
#[test]
fn every_resolved_cache_directory_is_recognized_as_kettles_own() {
    use super::cache_dir_from_env;

    const CHILD: &str = "KETTLE_CACHE_RESOLVER_BRANCH";
    if std::env::var_os(CHILD).is_none() {
        let cases: &[(&str, &[(&str, &str)])] = if cfg!(windows) {
            &[
                ("xdg", &[("XDG_CACHE_HOME", r"C:\cache-xdg")]),
                (
                    "local-app-data",
                    &[("LOCALAPPDATA", r"C:\Users\test\AppData\Local")],
                ),
            ]
        } else {
            &[
                (
                    "xdg",
                    &[("XDG_CACHE_HOME", "/tmp/cache-xdg"), ("HOME", "/tmp/home")],
                ),
                ("home", &[("HOME", "/tmp/home")]),
            ]
        };
        for (case, variables) in cases {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "app::tests::every_resolved_cache_directory_is_recognized_as_kettles_own",
                    "--nocapture",
                ])
                .env(CHILD, case);
            for key in ["XDG_CACHE_HOME", "HOME", "LOCALAPPDATA", "USERPROFILE"] {
                child.env_remove(key);
            }
            for (key, value) in *variables {
                child.env(key, value);
            }
            let status = child.status().expect("re-exec cache resolver test");
            assert!(status.success(), "cache resolver child {case} failed");
        }
        return;
    }

    let cache = cache_dir_from_env(|k| std::env::var(k).ok())
        .expect("the controlled branch must resolve a cache directory");
    assert!(
        kettle_state::is_kettle_owned_dir_name(&cache.join("kettle")),
        "the resolver puts kettle's cache in {}, which the private-path base \
             list does not recognize — kettle cannot repair its own directory \
             there, and every private path under it stays refused",
        cache.join("kettle").display()
    );
}

/// Drift guard. `cache_dir_from_env` probes the XDG /
/// HOME / Windows-ish envs in order. Empty values are treated as
/// unset so a stripped CI container with `XDG_CACHE_HOME=""`
/// falls through to the next probe instead of returning `""`.
#[test]
fn cache_dir_from_env_probes_in_order() {
    use super::cache_dir_from_env;
    // XDG wins when set.
    let f = |k: &str| match k {
        "XDG_CACHE_HOME" => Some("/x/cache".to_string()),
        "HOME" => Some("/h".to_string()),
        _ => None,
    };
    assert_eq!(
        cache_dir_from_env(f).as_deref(),
        Some(std::path::Path::new("/x/cache"))
    );
    // The non-XDG fallback is per-OS, mirroring the
    // config-dir resolver. On Unix, empty XDG falls through to HOME/.cache.
    #[cfg(not(windows))]
    {
        let f = |k: &str| match k {
            "XDG_CACHE_HOME" => Some(String::new()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new("/h/.cache"))
        );
    }
    // On Windows the cache dir is %LOCALAPPDATA%, and a stray HOME is IGNORED
    // (the config-dir split-brain class — a shell launch must not disagree
    // with a Start-menu launch).
    #[cfg(windows)]
    {
        // HOME set but no LOCALAPPDATA → None (HOME must not be used).
        let f = |k: &str| match k {
            "XDG_CACHE_HOME" => Some(String::new()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f),
            None,
            "Windows must not fall back to HOME/.cache"
        );
        // HOME AND LOCALAPPDATA set → LOCALAPPDATA wins, HOME ignored.
        let f = |k: &str| match k {
            "HOME" => Some(r"C:\Users\u".to_string()),
            "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new(r"C:\Users\u\AppData\Local"))
        );
    }
    // None of the env vars set → None (every OS).
    assert!(cache_dir_from_env(|_| None).is_none());
}

#[test]
fn relative_remote_spool_watch_is_normalized_before_parent_selection() {
    // Pure: no directory is created, so the test does not depend on who owns
    // the checkout's ancestors (a CI scratch drive may not be trusted).
    let cwd = std::env::current_dir().unwrap();
    let (parent, name) = super::remote_watch_parts(std::path::Path::new("remote.cmd")).unwrap();
    assert_eq!(parent, cwd, "a bare file name selects the current directory");
    assert_eq!(name, "remote.cmd");
    let nested = std::path::Path::new(".").join("new-spool").join("remote.cmd");
    assert!(nested.is_relative());
    let (parent, name) = super::remote_watch_parts(&nested).unwrap();
    assert!(parent.is_absolute());
    assert!(parent.ends_with("new-spool"));
    assert!(parent.starts_with(&cwd));
    assert_eq!(name, "remote.cmd");
}

#[test]
fn remote_spool_watch_creates_its_private_parent_but_not_the_spool() {
    let root = kettle_test_support::private_tempdir("kettle-remote-parent-");
    let path = root.path().join("new-spool").join("remote.cmd");
    let watched = super::remote_watch_path(&path).unwrap();
    assert!(watched.is_absolute());
    assert!(watched.parent().unwrap().is_dir());
    assert_eq!(watched.file_name().unwrap(), "remote.cmd");
    assert!(!watched.exists(), "the spool need not exist yet");
}

#[cfg(unix)]
#[test]
fn review_regression_relative_remote_watch_resolves_parent_symlinks() {
    let root = tempfile::Builder::new()
        .prefix("kettle-remote-alias-")
        .tempdir_in(".")
        .unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink("real", &alias).unwrap();
    let relative = std::path::Path::new(".")
        .join(root.path().file_name().unwrap())
        .join("alias/spool/remote.cmd");
    let watched = super::remote_watch_path(&relative).unwrap();
    let event_path = std::fs::canonicalize(real.join("spool"))
        .unwrap()
        .join("remote.cmd");
    assert!(!event_path.exists());
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(event_path);
    assert!(super::watcher_event_matches(&event, &watched));
}

#[cfg(target_os = "macos")]
#[test]
fn review_regression_remote_watch_matches_macos_tmp_alias() {
    const CHILD: &str = "KETTLE_REMOTE_NATIVE_WATCH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::tests::review_regression_remote_watch_matches_macos_tmp_alias",
                "--nocapture",
            ])
            .current_dir("/")
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("kettle-remote-native-")
        .tempdir_in("/tmp")
        .unwrap();
    let relative = root
        .path()
        .strip_prefix("/")
        .unwrap()
        .join("spool/remote.cmd");
    let watched = super::remote_watch_path(&relative).unwrap();
    let spool = root.path().join("spool/remote.cmd");
    std::fs::write(&spool, b"new-tab\n").unwrap();
    let event_path = std::fs::canonicalize(spool).unwrap();
    assert!(event_path.starts_with("/private/tmp"));
    let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
        .add_path(event_path);
    assert!(super::watcher_event_matches(&event, &watched));
}

#[test]
fn fatal_startup_failures_are_returned_after_event_loop_exit() {
    let src = production_source();
    let resumed = src.split("fn resumed_inner(").nth(1).unwrap();
    for failure in [
        "log::error!(\"{e}\");",
        "log::error!(\"failed to create window: {e}\");",
        "log::error!(\"renderer init failed: {e}\");",
        "log::error!(\"failed to spawn `-e` command: {e}\");",
        "log::error!(\"failed to spawn shell: {e}\");",
    ] {
        let branch = resumed
            .split(failure)
            .nth(1)
            .unwrap()
            .split("event_loop.exit();")
            .next()
            .unwrap();
        assert!(
            branch.contains("self.startup_error = Some("),
            "fatal startup failure must survive event-loop exit: {failure}"
        );
    }
    let run = src
        .split("let result: Result<()> = match app.start_first_pane_before_launch()")
        .nth(1)
        .unwrap();
    assert!(
        run.split("app.runtime_tracker.set_phase(\"exiting\")")
            .next()
            .unwrap()
            .contains("app.startup_error.take()")
    );
}

#[test]
fn review_startup_titlebar_policy_precedes_exact_grid_and_spawn() {
    let startup = include_str!("startup.rs");
    let initial = startup
        .find("let mut initial_window = WindowState::new(1, mux);")
        .unwrap();
    let inserted = startup[initial..]
        .find("windows.insert(1, initial_window);")
        .unwrap();
    assert!(
        startup[initial..initial + inserted].contains("macos_titlebar_tabs"),
        "initialize the first window's policy before pre-launch sizing and spawning"
    );
    let windowing = include_str!("windowing.rs");
    let probe = windowing
        .find("let mut probe = WindowState::new(0, Mux::new());")
        .unwrap();
    let measured = windowing[probe..]
        .find("self.startup_surface(&probe, monitor)")
        .unwrap();
    assert!(
        windowing[probe..probe + measured].contains("macos_titlebar_tabs"),
        "initialize the additional window probe before exact-grid sizing"
    );
}

#[test]
fn review_exact_startup_surface_retains_forty_rows_with_titlebar_tabs() {
    use super::{
        ContentBands, StartupMonitor, content_rect_for_with_strip, grid_dims_px,
        startup_surface_for,
    };
    let cfg = kettle_config::Config {
        window_width: Some(100),
        window_height: Some(40),
        ..Default::default()
    };
    for scale in [1.0, 2.0] {
        let mut ws = WindowState::new(0, Mux::new());
        ws.macos_titlebar_tabs = true;
        ws.startup_scale = scale;
        let band = App::macos_titlebar_band(&ws).unwrap();
        assert_eq!(band.height, 28.0 * scale);
        let bands = ContentBands {
            tab_bar_h: band.height,
            ..Default::default()
        };
        // Deliberately use measured-cell-like fractional sizes, not the 8x16 baseline.
        let cell = (9.25 * scale, 18.5 * scale);
        let monitor = Some(StartupMonitor::from_physical(
            (1920.0 * scale) as u32,
            (1080.0 * scale) as u32,
            scale as f64,
        ));
        let surface = startup_surface_for(&cfg, cell, bands, monitor);
        let area = content_rect_for_with_strip(
            surface,
            bands,
            cfg.tab_bar_pos,
            cfg.status_bar,
            cfg.tab_bar_width,
        );
        assert_eq!(
            grid_dims_px((area.2, area.3), cell, (cfg.padding_x, cfg.padding_y)),
            (100, 40)
        );
        let old_surface = startup_surface_for(&cfg, cell, ContentBands::default(), monitor);
        let old_area = content_rect_for_with_strip(
            old_surface,
            bands,
            cfg.tab_bar_pos,
            cfg.status_bar,
            cfg.tab_bar_width,
        );
        assert!(
            grid_dims_px(
                (old_area.2, old_area.3),
                cell,
                (cfg.padding_x, cfg.padding_y)
            )
            .1 < 40
        );
    }
}

#[test]
fn only_quick_terminal_launches_read_the_previous_app_at_startup() {
    // Capturing the previous app opens a display-server connection (X11 on
    // Linux). An ordinary launch must not make it.
    let src = production_source();
    let startup = src.split("let quick_previous =").nth(1).unwrap();
    let capture = startup.find("PreviousApp::capture()").unwrap();
    assert!(
        startup[..capture].contains("if startup.quick_terminal"),
        "startup reads the previous app for every launch"
    );
}

#[test]
fn measured_startup_titlebar_correction_precedes_size_read_and_reveal() {
    for (source, name) in [
        (include_str!("startup.rs"), "resumed_inner"),
        (include_str!("windowing.rs"), "open_window"),
    ] {
        let body = finding_body(source, name);
        let created = body.find("event_loop.create_window(attrs)").unwrap();
        let correction = body
            .find("self.correct_startup_titlebar_size(")
            .expect("correct the measured titlebar while the new window is hidden");
        let size = body.find("let size = window.inner_size();").unwrap();
        let reveal = body.find("set_visible(true)").unwrap();
        assert!(created < correction && correction < size && correction < reveal);
        assert_eq!(
            body.matches("self.correct_startup_titlebar_size(").count(),
            1
        );
    }
    let helper = finding_body(include_str!("titlebar.rs"), "correct_startup_titlebar_size");
    let measured = helper.find("native::measure(window)").unwrap();
    let corrected = helper
        .find("startup_surface_with_measured_titlebar(")
        .unwrap();
    let requested = helper.find("request_inner_size(").unwrap();
    assert!(measured < corrected && corrected < requested);
    assert_eq!(helper.matches("request_inner_size(").count(), 1);
}

#[test]
fn measured_startup_titlebar_explicit_grid_keeps_content_and_width() {
    use super::{
        ContentBands, StartupMonitor, content_rect_for_with_strip, grid_dims_px,
        startup_surface_for, startup_surface_with_measured_titlebar,
    };
    use kettle_config::Config;
    for scale in [1.0, 2.0] {
        // A small monitor must not clamp an explicitly requested grid.
        let monitor = Some(StartupMonitor::from_display_points(640, 480, scale));
        let cell = (9.25 * scale as f32, 18.5 * scale as f32);
        for (width, height) in [(Some(80), Some(25)), (Some(80), None), (None, Some(25))] {
            let cfg = Config {
                window_width: width,
                window_height: height,
                ..Default::default()
            };
            for (estimated, measured) in [(28.0, 32.0), (32.0, 28.0), (32.0, 32.0)] {
                let estimated = estimated * scale as f32;
                let measured = measured * scale as f32;
                let bands = ContentBands {
                    tab_bar_h: estimated,
                    ..Default::default()
                };
                let before = startup_surface_for(&cfg, cell, bands, monitor);
                let after = startup_surface_with_measured_titlebar(
                    &cfg, before, estimated, measured, monitor,
                );
                assert_eq!(after.0, before.0);
                assert_eq!(
                    after.1 as i64 - before.1 as i64,
                    (measured - estimated) as i64
                );
                let area = |surface, tab_bar_h| {
                    content_rect_for_with_strip(
                        surface,
                        ContentBands { tab_bar_h, ..bands },
                        cfg.tab_bar_pos,
                        cfg.status_bar,
                        cfg.tab_bar_width,
                    )
                };
                let old_area = area(before, estimated);
                let new_area = area(after, measured);
                if width == Some(80) && height == Some(25) && measured > estimated {
                    let short = area(before, measured);
                    assert_eq!(
                        grid_dims_px((short.2, short.3), cell, (cfg.padding_x, cfg.padding_y)),
                        (80, 24),
                        "the uncorrected AppKit band reproduces the reported lost row"
                    );
                }
                assert_eq!((new_area.2, new_area.3), (old_area.2, old_area.3));
                let grid = grid_dims_px(
                    (new_area.2, new_area.3),
                    cell,
                    (cfg.padding_x, cfg.padding_y),
                );
                if let Some(width) = width {
                    assert_eq!(grid.0, width as usize);
                }
                if let Some(height) = height {
                    assert_eq!(grid.1, height as usize);
                }
            }
        }
    }
}

#[test]
fn measured_startup_titlebar_default_grid_corrects_band_before_monitor_fit() {
    use super::{
        ContentBands, StartupMonitor, content_rect_for_with_strip, startup_surface_for,
        startup_surface_with_measured_titlebar,
    };
    use kettle_config::Config;
    let cfg = Config::default();
    for scale in [1.0, 2.0] {
        let cell = (9.25 * scale as f32, 18.5 * scale as f32);
        for monitor in [
            None,
            Some(StartupMonitor::from_display_points(1920, 1080, scale)),
            Some(StartupMonitor::from_display_points(640, 480, scale)),
        ] {
            // Without a monitor, startup_surface_for uses scale 1.
            let actual_scale = monitor.map_or(1.0, |m| m.scale) as f32;
            let estimated = 28.0 * actual_scale;
            for measured in [28.0, 32.0, 24.0] {
                let measured = measured * actual_scale;
                let bands = ContentBands {
                    tab_bar_h: estimated,
                    ..Default::default()
                };
                let before = startup_surface_for(&cfg, cell, bands, monitor);
                let after = startup_surface_with_measured_titlebar(
                    &cfg, before, estimated, measured, monitor,
                );
                assert_eq!(after.0, before.0);
                if monitor.is_some_and(|m| m.logical.1 == 480.0) {
                    assert_eq!(
                        after, before,
                        "the monitor cap still wins in both directions"
                    );
                    assert_eq!(after.1, (408.0 * actual_scale) as u32);
                } else {
                    assert_eq!(
                        after.1 as i64 - before.1 as i64,
                        (measured - estimated) as i64
                    );
                    let content_height = |surface, tab_bar_h| {
                        content_rect_for_with_strip(
                            surface,
                            ContentBands { tab_bar_h, ..bands },
                            cfg.tab_bar_pos,
                            cfg.status_bar,
                            cfg.tab_bar_width,
                        )
                        .3
                    };
                    assert_eq!(
                        content_height(after, measured),
                        content_height(before, estimated)
                    );
                }
            }
        }
    }
}
