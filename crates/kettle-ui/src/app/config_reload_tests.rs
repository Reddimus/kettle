#[cfg(unix)]
#[test]
fn reload_metadata_errors_are_not_missing_configs() {
    use std::os::unix::fs::symlink;

    let dir = kettle_test_support::private_tempdir("kettle-reload-metadata-");
    let path = dir.join("config");
    assert!(
        super::config_reload::read_config_for_reload(
            &path,
            kettle_config::ConfigTrust::ExplicitPath,
        )
        .unwrap()
        .is_none()
    );
    symlink("config", &path).unwrap();
    assert!(!path.exists());
    assert!(path.try_exists().is_err());
    assert!(
        super::config_reload::read_config_for_reload(
            &path,
            kettle_config::ConfigTrust::ExplicitPath,
        )
        .is_err()
    );
}

/// A config reload has to push the scrollback budget into panes that are
/// already open. Otherwise editing `scrollback` or `scrollback-bytes`,
/// through the file or the Settings overlay's two rows, reloads the value
/// and changes nothing visible.
#[test]
fn a_reload_carries_the_scrollback_budget_into_live_panes() {
    let src = production_source();
    let apply = src
        .split("fn apply_reloaded_config(")
        .nth(1)
        .expect("apply_reloaded_config present");
    let end = apply.find("\n    }\n").unwrap_or(apply.len());
    assert!(
        apply[..end]
            .contains("set_scrollback_limits(self.cfg.scrollback, self.cfg.scrollback_bytes)"),
        "the reload must hand both scrollback settings to the live panes, \
             or the Settings rows are inert until restart"
    );
}

#[test]
fn watcher_accepts_content_and_name_changes_to_exact_path_only() {
    use notify::EventKind;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };

    let watched = std::path::Path::new("/tmp/kettle-config.toml");
    let event = |kind| notify::Event::new(kind).add_path(watched.to_path_buf());
    for kind in [
        EventKind::Any,
        EventKind::Create(CreateKind::File),
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
        EventKind::Remove(RemoveKind::File),
    ] {
        assert!(super::watcher_event_matches(&event(kind), watched));
    }
    for kind in [
        EventKind::Access(AccessKind::Any),
        EventKind::Access(AccessKind::Read),
        EventKind::Access(AccessKind::Open(AccessMode::Read)),
        EventKind::Access(AccessKind::Close(AccessMode::Read)),
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions)),
        EventKind::Other,
    ] {
        assert!(!super::watcher_event_matches(&event(kind), watched));
    }
    let unrelated = notify::Event::new(EventKind::Modify(ModifyKind::Any))
        .add_path(std::path::PathBuf::from("/tmp/session.json"));
    assert!(!super::watcher_event_matches(&unrelated, watched));
}

#[cfg(target_os = "linux")]
#[test]
fn profile_switches_retarget_live_reload() {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::sync::{Arc, atomic::AtomicBool, mpsc};
    use std::time::Duration;

    let source = production_source();
    let refresh = source
        .split_once("fn refresh_config_watcher(")
        .unwrap()
        .1
        .split_once("/// Read and install")
        .unwrap()
        .0;
    assert!(refresh.contains("self.config_path.clone()"));
    assert!(!refresh.contains("self.startup.config"));
    assert_eq!(source.matches("self.switch_profile(ws, p);").count(), 2);

    let root = kettle_test_support::private_tempdir("kettle-profile-watch-");
    let original = root.join("config");
    let profiles = root.join("profiles");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&profiles)
        .unwrap();
    let profile = profiles.join("work.config");
    std::fs::write(&original, "").unwrap();
    std::fs::write(&profile, "").unwrap();
    let (tx, rx) = mpsc::channel();
    let _watcher = super::config_watcher(
        &profile,
        kettle_config::ConfigTrust::VerifyDirectory,
        Arc::new(AtomicBool::new(false)),
        move || tx.send(()).is_ok(),
    )
    .unwrap();
    std::fs::write(&original, "font-size = 13\n").unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    std::fs::write(&profile, "font-size = 14\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[cfg(unix)]
#[test]
fn config_watch_paths_follow_leaf_symlink_retargets() {
    use std::os::unix::fs::symlink;

    let root = kettle_test_support::private_tempdir("kettle-config-watch-link-");
    let config_dir = root.join("config-dir");
    let dotfiles = root.join("dotfiles");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::create_dir(&dotfiles).unwrap();
    let first = dotfiles.join("first");
    let second = dotfiles.join("second");
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    let link = config_dir.join("config");
    symlink(&first, &link).unwrap();
    for target in [&first, &second] {
        let paths = super::config_watch_paths(&link).unwrap();
        assert!(paths.contains(&std::path::absolute(&link).unwrap()));
        assert!(paths.contains(&std::fs::canonicalize(target).unwrap()));
        let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(std::fs::canonicalize(target).unwrap());
        assert!(
            paths
                .iter()
                .any(|path| super::watcher_event_matches(&event, path))
        );
        if target == &first {
            std::fs::remove_file(&link).unwrap();
            symlink(&second, &link).unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn symlinked_config_target_events_reload() {
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, atomic::AtomicBool, mpsc};
    use std::time::Duration;

    let root = kettle_test_support::private_tempdir("kettle-config-watch-target-");
    let links = root.join("links");
    let targets = root.join("targets");
    std::fs::create_dir(&links).unwrap();
    std::fs::create_dir(&targets).unwrap();
    let target = targets.join("config");
    let link = links.join("config");
    std::fs::write(&target, "").unwrap();
    symlink(&target, &link).unwrap();
    let (tx, rx) = mpsc::channel();
    let _watcher = super::config_watcher(
        &link,
        kettle_config::ConfigTrust::ExplicitPath,
        Arc::new(AtomicBool::new(false)),
        move || tx.send(()).is_ok(),
    )
    .unwrap();
    std::fs::write(&target, "font-size = 15\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn watcher_handle_survives_only_a_successful_registration() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Candidate(Arc<AtomicUsize>);
    impl Drop for Candidate {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let dropped = Arc::new(AtomicUsize::new(0));
    let kept = super::registered_watcher(Candidate(dropped.clone()), |_| Ok::<_, ()>(()));
    assert!(kept.is_ok());
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    drop(kept);
    assert_eq!(dropped.load(Ordering::Relaxed), 1);

    let refused = super::registered_watcher(Candidate(dropped.clone()), |_| Err::<(), _>(()));
    assert!(refused.is_err());
    assert_eq!(
        dropped.load(Ordering::Relaxed),
        2,
        "a failed subscription retained its unusable handle"
    );
}

#[test]
fn watcher_gate_coalesces_rearms_and_recovers_from_send_failure() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let pending = AtomicBool::new(false);
    let sends = AtomicUsize::new(0);
    assert!(super::queue_watcher_event(&pending, || {
        sends.fetch_add(1, Ordering::Relaxed);
        true
    }));
    assert!(!super::queue_watcher_event(&pending, || {
        sends.fetch_add(1, Ordering::Relaxed);
        true
    }));
    assert_eq!(sends.load(Ordering::Relaxed), 1);

    pending.store(false, Ordering::Release);
    assert!(!super::queue_watcher_event(&pending, || false));
    assert!(!pending.load(Ordering::Acquire));
    assert!(super::queue_watcher_event(&pending, || true));
}

#[test]
fn watcher_gate_allows_only_one_racing_sender() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    let pending = Arc::new(AtomicBool::new(false));
    let sends = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let pending = pending.clone();
            let sends = sends.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                super::queue_watcher_event(&pending, || {
                    sends.fetch_add(1, Ordering::Relaxed);
                    true
                });
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(sends.load(Ordering::Relaxed), 1);
}

#[test]
fn config_reload_font_policy_preserves_runtime_zoom() {
    assert_eq!(super::reloaded_font_size(false, 13.0, Some(19.0)), 19.0);
    assert_eq!(super::reloaded_font_size(false, 13.0, None), 13.0);
    assert_eq!(super::reloaded_font_size(true, 15.0, Some(19.0)), 15.0);
    assert_eq!(
        super::CONFIG_RELOAD_DEBOUNCE,
        std::time::Duration::from_millis(75)
    );
}

/// Turning updates off in Settings, or any other reload, refreshes the update
/// notice at once. It once waited for the next focus change or hourly tick,
/// so a focused window kept its chip for up to an hour.
#[test]
fn a_config_reload_refreshes_the_update_notice() {
    let src = production_source();
    let body = src
        .split("fn load_reloaded_config(&mut self) -> bool {")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("load_reloaded_config body");
    let assigned = body
        .find("self.cfg = new;")
        .expect("the new config is applied");
    let refreshed = body
        .find("self.request_update_facts(true);")
        .expect("a reload refreshes the update notice");
    assert!(assigned < refreshed, "facts are read with the new policy");
}

#[test]
fn watched_reload_loads_once_then_applies_every_window() {
    let src = production_source();
    let body = src
        .split("fn reload_config_windows(&mut self)")
        .nth(1)
        .and_then(|body| body.split("/// Scan every pane's recent output").next())
        .expect("reload_config_windows body");
    assert_eq!(body.matches("self.load_reloaded_config()").count(), 1);
    assert!(body.contains("self.windows.keys().copied().collect()"));
    assert!(body.contains("self.apply_reloaded_config(&mut ws"));
}

#[test]
fn a_flipped_theme_schedule_asks_for_one_redraw() {
    assert!(super::theme_schedule_redraw_due(None, Some(true)));
    assert!(super::theme_schedule_redraw_due(None, Some(false)));
    assert!(!super::theme_schedule_redraw_due(None, None));
    assert!(!super::theme_schedule_redraw_due(Some(true), Some(true)));
    assert!(!super::theme_schedule_redraw_due(Some(false), None));
    assert!(super::theme_schedule_redraw_due(Some(true), Some(false)));
    assert!(super::theme_schedule_redraw_due(Some(false), Some(true)));
    let wait = super::until_next_wall_minute();
    assert!(wait > std::time::Duration::ZERO && wait <= std::time::Duration::from_secs(60));
}

#[test]
fn a_theme_schedule_redraws_whoever_blinks_the_cursor() {
    // With the cursor blinking on the window server's layer, no blink
    // frame comes to apply a scheduled theme, so the wait loop must check
    // the schedule itself, at every minute, whatever the blink is doing.
    let src = production_source();
    let body = src
        .split_once("fn about_to_wait_inner(")
        .expect("about_to_wait_inner")
        .1;
    let due = body
        .split_once("let theme_schedule_due =")
        .expect("theme schedule check")
        .1
        .split_once(';')
        .expect("statement")
        .0;
    assert!(due.contains("theme_schedule_redraw_due("));
    assert!(
        !due.contains("blink"),
        "the check must not depend on the blink"
    );
    assert!(
        body.contains("|| theme_schedule_due)"),
        "a flip must request a redraw"
    );
    assert!(
        body.contains("if let Some(next) = theme_schedule_wait {"),
        "the loop must wake for the schedule"
    );
    assert!(body.contains(".then(until_next_wall_minute)"));
}

/// Every theme change reaches the panes' colour scheme (DEC mode 2031)
/// through the redraw-time sync, which fires on a new background even when
/// the theme stays dark, and a new pane spawns with the window's scheme.
#[test]
fn theme_changes_reach_every_panes_color_scheme() {
    let src = production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_string()
    };
    assert!(
        body("redraw").contains(
            "self.maybe_sync_native_theme(ws);\n        self.maybe_sync_color_scheme(ws);"
        ),
        "the colour scheme must sync at every redraw, after the native theme"
    );
    let sync = body("maybe_sync_color_scheme");
    assert!(sync.contains("(self.cfg.theme.is_dark(), self.cfg.theme.background)"));
    assert!(sync.contains("std::iter::once(&mut *ws).chain(self.windows.values_mut())"));
    assert!(sync.contains("state.mux.set_color_scheme(Some(scheme.0));"));
    let mux = include_str!("../mux.rs");
    assert!(
        mux.contains("color_scheme_dark: self.color_scheme_dark,"),
        "a new pane must spawn with its window's scheme"
    );
}

/// Drift guard for `theme-mode = auto` / `system` / `follow-system`.
/// OS appearance following must apply both the initial window theme and
/// live `WindowEvent::ThemeChanged` updates, while `theme-schedule` remains
/// the explicit owner when configured.
#[test]
fn system_theme_following_is_wired() {
    let src = production_source();
    assert!(
        src.contains(concat!(
            "WindowEvent::ThemeChanged",
            "(theme) => {\n                self.apply_os_theme_preference(ws, theme);"
        )),
        "winit ThemeChanged events must feed theme-mode=auto"
    );
    assert!(
        src.contains(concat!("ws.window.as_ref().and_then", "(|w| w.theme())")),
        "startup must apply the platform's current window theme when winit reports one"
    );
    assert!(
        src.contains(concat!(
            "self.apply_initial_os_theme_preference",
            "(ws, theme);"
        )),
        "startup OS theme application must use the pre-session-save path"
    );
    let initial_body = src
        .split(concat!("fn apply_initial_os_theme_preference", "("))
        .nth(1)
        .and_then(|s| s.split(concat!("fn apply_os_theme_preference", "(")).next())
        .expect("initial OS theme helper exists");
    assert!(
        !initial_body.contains(concat!("save_", "session")),
        "initial OS theme application must not save an empty startup session"
    );
    assert!(
        src.contains(concat!(
            "if self.cfg.theme_schedule.is_some",
            "() {\n            return None;\n        }"
        )),
        "theme-schedule must not fight OS appearance following"
    );
}

#[test]
fn runtime_theme_changes_synchronize_lua_and_redraw_every_window() {
    let src = production_source();
    let state_body = src
        .split(concat!("fn set_runtime_theme_state", "("))
        .nth(1)
        .and_then(|body| body.split(concat!("fn set_runtime_theme_name", "(")).next())
        .expect("runtime theme state helper exists");
    assert!(
        state_body.contains(concat!("self.cfg.", "theme_name = name;"))
            && state_body.contains(concat!("self.cfg.", "theme = theme;"))
            && state_body.contains(concat!("self.", "sync_lua_active_theme();"))
            && state_body.contains(concat!("self.", "request_all_window_redraws(ws);")),
        "palette, Lua introspection, and redraw invalidation must be one transition"
    );

    let theme_assignment = concat!("self.cfg.", "theme_name = name;");
    assert_eq!(
        src.matches(theme_assignment).count(),
        1,
        "runtime code must not bypass the synchronized theme transition"
    );

    let redraw_body = src
        .split(concat!("fn request_all_window_redraws", "("))
        .nth(1)
        .and_then(|body| body.split(concat!("fn sync_lua_active_theme", "(")).next())
        .expect("all-window redraw helper exists");
    assert!(
        redraw_body.contains(concat!("for (&seq, window) in &self.", "windows"))
            && redraw_body.contains("seq != ws.seq")
            && redraw_body.contains(concat!("native.", "request_redraw();")),
        "a global theme change must invalidate every mapped sibling window"
    );
}

/// Drift guard. `pick_next_profile` is the pure
/// helper behind `Action::NextProfile`/`PrevProfile`. Cycles
/// the sorted profile list with wrap-around; starts at index 0
/// when current isn't a known profile (e.g. --config FILE or
/// default config).
#[test]
fn pick_next_profile_wraps_and_starts_at_index_0() {
    use super::pick_next_profile;
    let names: Vec<String> = vec!["dark".into(), "dev".into(), "light".into()];
    // Forward through the list, wrapping at the end.
    assert_eq!(pick_next_profile(Some("dark"), &names, true), "dev");
    assert_eq!(pick_next_profile(Some("dev"), &names, true), "light");
    assert_eq!(pick_next_profile(Some("light"), &names, true), "dark");
    // Backward, wrapping at the start.
    assert_eq!(pick_next_profile(Some("dark"), &names, false), "light");
    assert_eq!(pick_next_profile(Some("dev"), &names, false), "dark");
    assert_eq!(pick_next_profile(Some("light"), &names, false), "dev");
    // Current is None or an unknown name: start the cycle at idx 0.
    // Forward → idx 1 (the second name); backward → last name.
    assert_eq!(pick_next_profile(None, &names, true), "dev");
    assert_eq!(pick_next_profile(None, &names, false), "light");
    assert_eq!(pick_next_profile(Some("missing"), &names, true), "dev");
    // Single profile: always returns itself (n=1, both arms ⇒ idx 0).
    let single = vec!["only".to_string()];
    assert_eq!(pick_next_profile(Some("only"), &single, true), "only");
    assert_eq!(pick_next_profile(Some("only"), &single, false), "only");
}

/// Drift guard. `pick_light_dark_target` is the
/// pure helper behind `Action::ToggleLightDark` — the policy
/// must round-trip current ↔ {light, dark} cleanly, default
/// to `dark` on a third-party current theme, and silently
/// no-op when neither config key is set.
#[test]
fn pick_light_dark_target_round_trips() {
    use super::pick_light_dark_target;
    // Round-trip: current==dark → switch to light.
    assert_eq!(
        pick_light_dark_target("TokyoNight Night", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Day"),
    );
    // Round-trip: current==light → switch to dark.
    assert_eq!(
        pick_light_dark_target("TokyoNight Day", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Night"),
    );
    // Case-insensitive match on `current`.
    assert_eq!(
        pick_light_dark_target("tokyonight night", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Day"),
    );
    // Current is a third-party theme: default to dark.
    assert_eq!(
        pick_light_dark_target("Catppuccin Mocha", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Night"),
    );
    // Only light set: one-way switch to light.
    assert_eq!(
        pick_light_dark_target("Catppuccin Mocha", "TokyoNight Day", "").as_deref(),
        Some("TokyoNight Day"),
    );
    // Only dark set: one-way switch to dark.
    assert_eq!(
        pick_light_dark_target("Catppuccin Latte", "", "TokyoNight Night").as_deref(),
        Some("TokyoNight Night"),
    );
    // Neither set: no-op.
    assert_eq!(pick_light_dark_target("anything", "", ""), None);
}

#[test]
fn only_one_window_can_own_the_theme_preview() {
    use kettle_config::Config;
    let mut windows = std::collections::BTreeMap::new();
    let mut first = WindowState::new(1, Mux::new());
    let mut second = WindowState::new(2, Mux::new());
    let baseline = (
        "TokyoNight Night".to_string(),
        kettle_config::Theme::by_name("TokyoNight Night"),
    );
    super::begin_theme_picker_session(&mut first, &mut windows, baseline.clone());
    first.theme_picker_input.as_mut().unwrap().selected += 1;
    let live = super::theme_preview_change(
        &mut first,
        &Config {
            theme_name: baseline.0.clone(),
            ..Default::default()
        },
        &kettle_i18n::Translator::default(),
    )
    .unwrap();
    windows.insert(1, first);
    let restored = super::begin_theme_picker_session(&mut second, &mut windows, live);
    assert!(
        windows[&1].theme_picker_input.is_none(),
        "opening another picker must retire the previous preview owner"
    );
    assert!(windows[&1].theme_preview.is_none());
    assert_eq!(
        restored.as_ref().map(|state| state.0.as_str()),
        Some(baseline.0.as_str())
    );
    second.theme_picker_input.as_mut().unwrap().query = "Dracula".into();
    second.theme_picker_input.as_mut().unwrap().selected = 0;
    let preview = super::theme_preview_change(
        &mut second,
        &Config {
            theme_name: baseline.0.clone(),
            ..Default::default()
        },
        &kettle_i18n::Translator::default(),
    )
    .unwrap();
    assert!(
        super::theme_preview_change(
            windows.get_mut(&1).unwrap(),
            &Config {
                theme_name: preview.0.clone(),
                ..Default::default()
            },
            &kettle_i18n::Translator::default()
        )
        .is_none()
    );
    second.theme_picker_input = None;
    let restored = super::theme_preview_change(
        &mut second,
        &Config {
            theme_name: preview.0.clone(),
            ..Default::default()
        },
        &kettle_i18n::Translator::default(),
    )
    .unwrap();
    assert_eq!(restored.0, baseline.0);
    assert_eq!(restored.1.background, baseline.1.background);
}

#[test]
fn theme_previews_preserve_configured_color_overrides() {
    use kettle_config::Config;
    let cfg = Config::parse_text(
        "background = #010203\nforeground = #040506\ncursor-color = #070809\npalette = 4=#123456\n",
    );
    let baseline = cfg.theme.clone();
    let mut ws = WindowState::new(1, Mux::new());
    super::begin_theme_picker(&mut ws, (cfg.theme_name.clone(), baseline.clone()));
    let picker = ws.theme_picker_input.as_mut().unwrap();
    picker.query = "Dracula".into();
    picker.selected = 0;
    let (name, preview) =
        super::theme_preview_change(&mut ws, &cfg, &kettle_i18n::Translator::default()).unwrap();
    assert_eq!(
        preview.background,
        kettle_config::Rgb::parse("#010203").unwrap(),
        "preview must retain configured background overrides"
    );
    assert_eq!(
        preview.foreground,
        kettle_config::Rgb::parse("#040506").unwrap()
    );
    assert_eq!(
        preview.cursor,
        kettle_config::Rgb::parse("#070809").unwrap()
    );
    assert_eq!(
        preview.palette[4],
        kettle_config::Rgb::parse("#123456").unwrap()
    );
    let committed = cfg.theme_by_name(&name);
    assert_eq!(preview.background, committed.background);
    assert_eq!(preview.palette, committed.palette);
    ws.theme_picker_input = None;
    let restored =
        super::theme_preview_change(&mut ws, &cfg, &kettle_i18n::Translator::default()).unwrap();
    assert_eq!(restored.1.background, baseline.background);
    assert_eq!(restored.1.palette, baseline.palette);
}

#[test]
fn finding_watcher_refresh_keeps_working_registration() {
    let body = finding_body(include_str!("config_reload.rs"), "refresh_config_watcher");
    assert!(
        body.contains("Ok(watcher) =>"),
        "watcher replacement must be installed only after successful registration"
    );
    assert!(
        body.contains("self.config_watcher_path"),
        "watcher retention must be scoped to the same config path"
    );
}

#[cfg(unix)]
#[test]
fn config_watch_paths_keep_dangling_symlink_targets() {
    use std::os::unix::fs::symlink;
    let root = kettle_test_support::private_tempdir("kettle-dangling-config-");
    let links = root.join("links");
    let targets = root.join("targets");
    std::fs::create_dir(&links).unwrap();
    std::fs::create_dir(&targets).unwrap();
    let link = links.join("config");
    symlink("../targets/config", &link).unwrap();
    let target = std::fs::canonicalize(&targets).unwrap().join("config");
    for existed in [false, true] {
        if existed {
            std::fs::write(&target, "font-size = 12\n").unwrap();
            assert!(super::config_watch_paths(&link).unwrap().contains(&target));
            std::fs::remove_file(&target).unwrap();
        }
        let refreshed = super::config_watch_paths(&link).unwrap();
        assert!(
            refreshed.contains(&target),
            "a refreshed watcher must retain the dangling config target"
        );
        std::fs::write(&target, "font-size = 14\n").unwrap();
        let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
            .add_path(target.clone());
        assert!(
            refreshed
                .iter()
                .any(|path| super::watcher_event_matches(&event, path))
        );
        std::fs::remove_file(&target).unwrap();
    }
}

#[test]
fn font_reload_between_recovery_attempts_survives_an_unchanged_reload() {
    let runtime = 18.0;
    let mut pending = super::recovery_font_size_after_reload(None, true, 14.0);
    assert_eq!(
        pending,
        Some(14.0),
        "font reloads while the renderer is absent must survive recovery"
    );
    pending = super::recovery_font_size_after_reload(pending, false, 14.0);
    assert_eq!(
        pending.unwrap_or(runtime),
        14.0,
        "a no-op reload must not resurrect the old snapshot's zoom"
    );
    pending = super::recovery_font_size_after_reload(pending, true, 12.0);
    assert_eq!(pending, Some(12.0));
    assert_eq!(
        super::recovery_font_size_after_reload(None, false, 12.0).unwrap_or(runtime),
        18.0
    );
    let source = production_source();
    let restore = source
        .split("fn restore_gpu_renderer_state(")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    assert!(restore.contains("set_font_family(cfg.font_family.clone())"));
    assert!(restore.contains("set_cell_scale(cfg.cell_width, cfg.cell_height)"));
    let recovery = source
        .split("fn try_recover_gpu(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert_eq!(
        recovery.matches("restore_gpu_renderer_state(").count(),
        2,
        "primary and secondary windows must reconcile recovery fonts"
    );
}

#[cfg(unix)]
#[test]
fn review_config_watcher_survives_a_missing_target_parent() {
    use std::os::unix::fs::symlink;
    #[cfg(target_os = "linux")]
    use std::sync::{Arc, atomic::AtomicBool, mpsc};
    #[cfg(target_os = "linux")]
    use std::time::Duration;

    let root = kettle_test_support::private_tempdir("kettle-missing-target-parent-");
    let links = root.join("links");
    std::fs::create_dir(&links).unwrap();
    let link = links.join("config");
    symlink("../missing/config", &link).unwrap();
    let watched = super::config_watch_paths(&link)
        .expect("the original symlink's directory must remain watchable");
    assert!(watched.contains(&std::path::absolute(&link).unwrap()));
    assert!(watched.iter().all(|path| path.parent().unwrap().is_dir()));
    #[cfg(target_os = "linux")]
    let (tx, rx) = mpsc::channel();
    #[cfg(target_os = "linux")]
    let _watcher = super::config_watcher(
        &link,
        kettle_config::ConfigTrust::ExplicitPath,
        Arc::new(AtomicBool::new(false)),
        move || tx.send(()).is_ok(),
    )
    .expect("the original symlink's directory must remain watchable");
    let valid = root.join("valid");
    std::fs::write(&valid, "font-size = 14\n").unwrap();
    std::fs::remove_file(&link).unwrap();
    symlink("../valid", &link).unwrap();
    let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
        .add_path(std::path::absolute(&link).unwrap());
    assert!(
        watched
            .iter()
            .any(|path| super::watcher_event_matches(&event, path))
    );
    #[cfg(target_os = "linux")]
    rx.recv_timeout(Duration::from_secs(5))
        .expect("retargeting must wake configuration reload");
    let refreshed = super::config_watch_paths(&link).unwrap();
    assert!(refreshed.contains(&std::fs::canonicalize(valid).unwrap()));
}

#[test]
fn cursor_settings_are_applied_to_running_panes_on_reload() {
    let src = production_source();
    let apply = src.split("fn apply_reloaded_config(").nth(1).unwrap();
    assert!(apply.contains("set_cursor_defaults(self.cfg.cursor_style, self.cfg.cursor_blink)"));
}

#[test]
fn cached_profile_cycle_refuses_deleted_target_without_changing_active_state() {
    use super::config_reload::{pick_next_profile, stage_profile_switch};
    use kettle_config::ConfigTrust;

    let root = kettle_test_support::private_tempdir("kettle-profile-switch-deleted-");
    let original = root.join("a.config");
    let removed = root.join("b.config");
    std::fs::write(&original, "font-size = 19.5\n").unwrap();
    std::fs::write(&removed, "font-size = 22\n").unwrap();
    let names = vec!["a".to_string(), "b".to_string()];
    assert_eq!(pick_next_profile(Some("a"), &names, true), "b");
    std::fs::remove_file(&removed).unwrap();
    let mut active = Some(original.clone());
    let mut trust = ConfigTrust::ExplicitPath;
    let mut prepared = None;
    let error = stage_profile_switch(removed, &mut active, &mut trust, &mut prepared).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(active.as_ref(), Some(&original));
    assert!(matches!(trust, ConfigTrust::ExplicitPath));
    assert!(prepared.is_none());
    assert_eq!(std::fs::read_to_string(original).unwrap(), "font-size = 19.5\n");
}

#[test]
fn profile_switch_uses_one_prepared_snapshot_if_target_disappears_after_read() {
    use super::config_reload::{read_config_for_reload_or_prepared, stage_profile_switch};
    use kettle_config::ConfigTrust;

    let root = kettle_test_support::private_tempdir("kettle-profile-switch-snapshot-");
    let path = root.join("work.config");
    std::fs::write(&path, "font-size = 19.5\n").unwrap();
    let mut active = None;
    let mut trust = ConfigTrust::ExplicitPath;
    let mut prepared = None;
    stage_profile_switch(path.clone(), &mut active, &mut trust, &mut prepared).unwrap();
    std::fs::remove_file(&path).unwrap();
    let loaded = read_config_for_reload_or_prepared(&path, trust, &mut prepared)
        .unwrap()
        .expect("a successful switch installs its same prepared snapshot");
    assert_eq!(loaded.config.font_size, 19.5);
    assert_eq!(loaded.text, "font-size = 19.5\n");
    assert!(prepared.is_none());
    assert!(matches!(trust, ConfigTrust::VerifyDirectory));
    assert_eq!(active.as_ref(), Some(&path));
}

#[test]
fn profile_switch_read_failure_preserves_state_and_prepared_reload_is_path_bound() {
    use super::config_reload::{read_config_for_reload_or_prepared, stage_profile_switch};
    use kettle_config::ConfigTrust;

    let root = kettle_test_support::private_tempdir("kettle-profile-switch-path-");
    let first = root.join("a.config");
    let second = root.join("b.config");
    std::fs::write(&first, "font-size = 19.5\n").unwrap();
    std::fs::write(&second, "font-size = 22\n").unwrap();
    let mut active = Some(second.clone());
    let mut trust = ConfigTrust::ExplicitPath;
    let mut prepared = None;
    assert!(stage_profile_switch(root.to_path_buf(), &mut active, &mut trust, &mut prepared).is_err());
    assert_eq!(active.as_ref(), Some(&second));
    assert!(matches!(trust, ConfigTrust::ExplicitPath));
    assert!(prepared.is_none());
    stage_profile_switch(first, &mut active, &mut trust, &mut prepared).unwrap();
    let loaded = read_config_for_reload_or_prepared(&second, trust, &mut prepared).unwrap().unwrap();
    assert_eq!(loaded.config.font_size, 22.0);
    assert!(prepared.is_none(), "a snapshot for another path must not survive");
}

/// A reload installs the saved theme, and the file watcher reloads outside
/// any window event, so the window's next frame would show the saved theme
/// under an open picker's preview. Applying a reload puts the preview back
/// first, after the schedule has had its say.
#[test]
fn a_reload_puts_an_open_pickers_preview_back_before_painting() {
    let src = production_source();
    let apply = src
        .split("fn apply_reloaded_config(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("apply_reloaded_config");
    let schedule = apply
        .find("self.poll_theme_schedule(ws);")
        .expect("the schedule poll");
    let sync = apply
        .find("self.sync_theme_preview(ws);")
        .expect("a reload must re-apply an open picker's preview");
    assert!(sync > schedule, "the preview wins over the schedule's pick");
}

/// The system turns light while the theme picker edits the light side from
/// a dark window. The picker keeps editing the light side and its list; Esc
/// now puts back the new light baseline, and Enter saves `light-theme` and
/// wears the pick, since the window is on that side now.
#[test]
fn a_system_switch_while_editing_the_other_side_keeps_that_side() {
    use super::{ThemeCommitPlan, switch_theme_picker_slot, theme_commit_plan};
    use kettle_render::ThemeSlot;
    let tr = kettle_i18n::Translator::default();
    let cfg = kettle_config::Config::default();
    let live = |name: &str| (name.to_string(), kettle_config::Theme::by_name(name));
    let mut ws = WindowState::new(1, Mux::new());
    super::begin_theme_picker(&mut ws, live(&cfg.dark_theme));
    let picker = ws.theme_picker_input.as_mut().unwrap();
    switch_theme_picker_slot(picker, &cfg, "dark-theme", ThemeSlot::Light, &tr);
    let before = picker.clone();
    assert_eq!(
        theme_commit_plan(picker, &cfg, Some(true)),
        ThemeCommitPlan {
            key: "light-theme",
            restore_active: true
        }
    );
    // What `apply_initial_os_theme_preference` does for each window.
    super::rebase_theme_preview(&mut ws, &live(&cfg.light_theme));
    let picker = ws.theme_picker_input.as_ref().unwrap();
    assert_eq!(*picker, before);
    assert_eq!(
        theme_commit_plan(picker, &cfg, Some(false)),
        ThemeCommitPlan {
            key: "light-theme",
            restore_active: false
        }
    );
    ws.theme_picker_input = None;
    let wearing = kettle_config::Config {
        theme_name: "Dracula".into(),
        ..Default::default()
    };
    let restored = super::theme_preview_change(&mut ws, &wearing, &tr).unwrap();
    assert_eq!(restored.0, cfg.light_theme);
}
