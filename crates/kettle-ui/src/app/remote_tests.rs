#[test]
fn remote_poll_stages_checked_out_and_stored_windows_without_stale_roots() {
    let target = |pid| {
        Some(kettle_remote::RemoteProbeTarget {
            pid,
            foreground_pid: Some(pid),
            allow_native_cwd: true,
        })
    };
    let mut scratch = vec![target(999).unwrap()];
    stage_remote_targets(
        &mut scratch,
        [target(10), None, target(11)],
        [target(20), target(21), None],
    );
    assert_eq!(
        scratch.iter().map(|target| target.pid).collect::<Vec<_>>(),
        [10, 11, 20, 21]
    );

    stage_remote_targets(
        &mut scratch,
        [target(30)],
        std::iter::empty::<Option<kettle_remote::RemoteProbeTarget>>(),
    );
    assert_eq!(
        scratch.iter().map(|target| target.pid).collect::<Vec<_>>(),
        [30]
    );
}

#[test]
fn omitted_remote_probe_preserves_shell_until_pane_identity_changes() {
    let probe = kettle_remote::RemoteProbe {
        remote: None,
        native_cwd: None,
        foreground_shell: Some(kettle_remote::ShellLaunch {
            argv: vec!["bash".into(), "-i".into()],
            cwd: Some("/tmp".into()),
        }),
        foreground_process: None,
    };
    let mut applied = std::collections::HashMap::from([
        ((1, 10), probe.clone()),
        ((2, 20), probe.clone()),
        ((3, 30), probe.clone()),
        ((4, 40), probe.clone()),
    ]);
    let mut live = HashSet::from([(99, 99)]);
    super::retain_live_remote_probes(
        &mut applied,
        &mut live,
        [(1, Some(10)), (2, Some(21)), (4, None)],
    );
    assert_eq!(
        applied,
        std::collections::HashMap::from([((1, 10), probe.clone())])
    );
    let cleared = kettle_remote::RemoteProbe {
        foreground_shell: None,
        ..probe
    };
    assert!(stage_applied_remote_probe(&mut applied, 1, 10, &cleared));
    assert!(applied[&(1, 10)].foreground_shell.is_none());
}

#[test]
fn remote_snapshot_compares_against_ui_applied_state_not_worker_history() {
    let old = kettle_remote::RemoteProbe {
        remote: None,
        native_cwd: Some("old".into()),
        foreground_shell: None,
        foreground_process: None,
    };
    let new = kettle_remote::RemoteProbe {
        remote: None,
        native_cwd: Some("new".into()),
        foreground_shell: None,
        foreground_process: None,
    };
    let mut applied = std::collections::HashMap::from([((70, 7), old)]);

    assert!(stage_applied_remote_probe(&mut applied, 70, 7, &new));
    assert!(!stage_applied_remote_probe(&mut applied, 70, 7, &new));

    assert!(
        stage_applied_remote_probe(&mut applied, 71, 7, &new),
        "a second pane object sharing/reusing the pid needs its own first install"
    );
}

#[test]
fn native_cwd_is_suppressed_for_nonlocal_launch_clients() {
    for argv0 in [
        "wsl.exe",
        "C:\\Windows\\System32\\OpenSSH\\ssh.exe",
        "DOCKER.EXE",
        "/usr/bin/podman",
        "kubectl",
        "lxc-attach",
    ] {
        assert!(
            argv_is_nonlocal_client(&[argv0.into()]),
            "nonlocal client {argv0}"
        );
    }
    assert!(!argv_is_nonlocal_client(&["pwsh.exe".into()]));
}

#[test]
fn remote_reconnect_menu_uses_new_pane_action() {
    use super::ContextMenuClick;
    let contexts = [
        kettle_remote::RemoteContext::Ssh {
            host: "box".into(),
            user: Some("alice".into()),
            options: kettle_remote::SshOptions::default(),
        },
        kettle_remote::RemoteContext::Container {
            runtime: kettle_remote::ContainerRuntime::Docker,
            container: "web".into(),
            options: kettle_remote::ContainerOptions::default(),
        },
    ];
    for ctx in contexts {
        let item = super::remote_reconnect_item(&ctx, &EN_TEXT).unwrap();
        assert!(super::item_is_dispatchable(&item));
        let Some(ContextMenuClick::ReconnectRemote(command)) = super::item_to_click(&item, 0)
        else {
            panic!("reconnect must open a new pane");
        };
        assert_eq!(command, kettle_remote::clone_session_command(&ctx));
    }
}

#[test]
fn remote_reconnect_unavailable_environment_reports_failure_without_spawning() {
    use super::{Config, PtyGeometry};
    let mut ctx = kettle_remote::RemoteContext::Container {
        runtime: kettle_remote::ContainerRuntime::Kubectl,
        container: "api".into(),
        options: kettle_remote::ContainerOptions {
            environment_unavailable: true,
            unreproducible: true,
            ..Default::default()
        },
    };
    let item = super::remote_reconnect_item(&ctx, &EN_TEXT).expect("reconnect refusal action");
    let Some(super::ContextMenuClick::ReconnectRemote(command)) = super::item_to_click(&item, 0)
    else {
        panic!("reconnect refusal must use the existing failure-notice path");
    };
    let mut mux = Mux::new();
    let result = super::reconnect_remote_session(
        &mut mux,
        &Config::default(),
        PtyGeometry::from_cell_size(80, 24, 8, 16),
        Arc::new(|| {}),
        command,
    );
    assert!(result.is_err());
    assert!(mux.panes.is_empty());
    if let kettle_remote::RemoteContext::Container { options, .. } = &mut ctx {
        options.environment_unavailable = false;
    }
    assert!(
        super::remote_reconnect_item(&ctx, &EN_TEXT).is_none(),
        "unreproducible arguments still suppress the reconnect action"
    );
}

#[cfg(unix)]
#[test]
fn remote_reconnect_spawns_client_without_typing_into_source() {
    use super::{BroadcastScope, Config, Mux, PtyGeometry};
    let cfg = Config {
        shell: Some("/bin/sh".into()),
        ..Config::default()
    };
    let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
    let waker: kettle_core::Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    mux.new_tab_with_geometry(&cfg, geometry, waker.clone(), &["/bin/cat".into()], None)
        .expect("PTY fixture");
    let source = mux.active_focus().unwrap();
    let generation = mux.panes[&source].input_generation();
    mux.panes.get_mut(&source).unwrap().read_only = true;
    mux.broadcast = BroadcastScope::All;
    super::reconnect_remote_session(
        &mut mux,
        &cfg,
        geometry,
        waker,
        Some(kettle_remote::SessionCommand {
            program: "/bin/cat".into(),
            args: Vec::new(),
            environment: Default::default(),
            argv0: None,
        }),
    )
    .unwrap();
    let target = mux.active_focus().unwrap();
    assert_ne!(target, source);
    assert_eq!(mux.panes.len(), 2);
    assert_eq!(mux.panes[&target].argv, vec!["/bin/cat".to_string()]);
    assert_eq!(mux.panes[&source].input_generation(), generation);
    assert_eq!(mux.panes[&target].input_generation(), 0);
    mux.kill_children();
}

#[test]
fn remote_reconnect_argv_child() {
    let args: Vec<_> = std::env::args().collect();
    if !args.iter().any(|arg| arg == "kettle-reconnect-argv-probe") {
        return;
    }
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--skip", "a'\" b % ^ $ !"])
    );
    println!("RECONNECT_ARGV_PRESERVED");
}

#[test]
fn remote_reconnect_passes_literal_arguments_to_client() {
    use super::{Config, PtyGeometry};
    let cfg = Config {
        shell: Some("kettle-nonexistent-shell".into()),
        login_shell: true,
        shell_integration: true,
        ..Config::default()
    };
    let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
    let mut mux = Mux::new();
    let args = [
        "--exact",
        "app::tests::remote_reconnect_argv_child",
        "--nocapture",
        "--skip",
        "kettle-reconnect-argv-probe",
        "--skip",
        "a'\" b % ^ $ !",
    ]
    .map(String::from)
    .to_vec();
    super::reconnect_remote_session(
        &mut mux,
        &cfg,
        geometry,
        Arc::new(|| {}),
        Some(kettle_remote::SessionCommand {
            program: std::env::current_exe().unwrap().to_str().unwrap().into(),
            args: args.clone(),
            environment: Default::default(),
            argv0: None,
        }),
    )
    .unwrap();
    let target = mux.active_focus().unwrap();
    assert_eq!(&mux.panes[&target].argv[1..], args);
    // The child's own check runs only where this harness can read a pane's
    // screen: no test here observes ConPTY output on Windows, so Windows
    // checks the argv handed to the PTY above and relies on portable-pty's
    // unchanged ArgvQuote for the command line. Starting this test binary in
    // a pseudo-terminal can still take several seconds on a loaded runner.
    #[cfg(unix)]
    if let Err(screen) = pane_screen_shows(
        &mux,
        target,
        "RECONNECT_ARGV_PRESERVED",
        std::time::Duration::from_secs(30),
    ) {
        panic!("the client never confirmed its arguments; screen:\n{screen}");
    }
    mux.kill_children();
}

#[cfg(unix)]
#[test]
fn remote_reconnect_uses_detected_client_outside_kettle_path() {
    use super::{Config, PtyGeometry};
    let cfg = Config {
        shell: Some("kettle-nonexistent-shell".into()),
        login_shell: true,
        shell_integration: true,
        ..Config::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("podman");
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &executable).unwrap();
    assert!(
        !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .any(|path| path == dir.path())
    );
    let argv = [executable.to_str().unwrap(), "exec", "-it", "web", "sh"].map(String::from);
    let ctx = kettle_remote::detect_container(&argv).unwrap();
    let mut command = kettle_remote::clone_session_command(&ctx).unwrap();
    assert_eq!(command.program, executable.to_str().unwrap());
    let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
    let mut mux = Mux::new();
    let args = [
        "--exact",
        "app::tests::remote_reconnect_argv_child",
        "--nocapture",
        "--skip",
        "kettle-reconnect-argv-probe",
        "--skip",
        "a'\" b % ^ $ !",
    ]
    .map(String::from)
    .to_vec();
    command.args = args.clone();
    super::reconnect_remote_session(&mut mux, &cfg, geometry, Arc::new(|| {}), Some(command))
        .unwrap();
    let target = mux.active_focus().unwrap();
    assert_eq!(&mux.panes[&target].argv[1..], args);
    assert!(wait_for_pane_screen(&mux, target, |text, _| text
        .contains("RECONNECT_ARGV_PRESERVED")));
    mux.kill_children();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "subprocess fixture for reconnect environment and argv0"]
fn remote_reconnect_environment_child() {
    if std::env::var_os("KETTLE_RECONNECT_CAPTURE").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    assert_eq!(std::env::args().next().unwrap(), "docker");
    assert_eq!(std::env::var("DOCKER_HOST").unwrap(), "tcp://client:2376");
    assert_eq!(std::env::var("DOCKER_TLS_VERIFY").unwrap(), "1");
    for name in ["DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH"] {
        assert!(
            std::env::var_os(name).is_none(),
            "{name} must remain absent"
        );
    }
    assert_eq!(std::env::var("PRIVATE_TOKEN").unwrap(), "from-config");
    println!("RECONNECT_ENV_AND_ARGV0_PRESERVED");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remote_reconnect_environment_fixture() -> (
    super::Config,
    super::PtyGeometry,
    kettle_remote::SessionCommand,
) {
    use super::{Config, PtyGeometry};
    use kettle_remote::ProcessTree;
    let args = [
        "--exact",
        "app::tests::remote_reconnect_environment_child",
        "--ignored",
        "--nocapture",
    ]
    .map(String::from)
    .to_vec();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(&args)
        .env_clear()
        .env("KETTLE_RECONNECT_CAPTURE", "1")
        .env("DOCKER_HOST", "tcp://client:2376")
        .env("DOCKER_TLS_VERIFY", "1")
        .env("PRIVATE_TOKEN", "from-client")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let sys = kettle_remote::SysinfoSystem::new();
    let names = [
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let environment = loop {
        if let Some(environment) = sys.connection_environment_of(child.id(), &names)
            && environment
                .iter()
                .any(|(name, value)| name == "DOCKER_HOST" && value == Some("tcp://client:2376"))
        {
            break environment;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("client environment never became readable");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let _ = child.kill();
    let _ = child.wait();
    let context = kettle_remote::RemoteContext::Container {
        runtime: kettle_remote::ContainerRuntime::Docker,
        container: "web".into(),
        options: kettle_remote::ContainerOptions {
            executable: Some(std::env::current_exe().unwrap().to_str().unwrap().into()),
            invocation: Some("docker".into()),
            environment,
            ..Default::default()
        },
    };
    let mut command = kettle_remote::clone_session_command(&context).unwrap();
    command.args = args;
    let cfg = Config {
        env: vec![
            ("DOCKER_HOST".into(), "tcp://wrong:2376".into()),
            ("DOCKER_CONTEXT".into(), "wrong-context".into()),
            ("PRIVATE_TOKEN".into(), "from-config".into()),
        ],
        shell: Some("kettle-nonexistent-shell".into()),
        login_shell: true,
        shell_integration: true,
        ..Default::default()
    };
    let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
    (cfg, geometry, command)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn remote_reconnect_preserves_client_environment_and_invocation() {
    let (cfg, geometry, command) = remote_reconnect_environment_fixture();
    for existing_pane in [false, true] {
        let mut mux = Mux::new();
        if existing_pane {
            mux.new_tab_with_geometry(&cfg, geometry, Arc::new(|| {}), &["/bin/cat".into()], None)
                .unwrap();
        }
        super::reconnect_remote_session(
            &mut mux,
            &cfg,
            geometry,
            Arc::new(|| {}),
            Some(command.clone()),
        )
        .unwrap();
        let target = mux.active_focus().unwrap();
        let passed = wait_for_pane_screen(&mux, target, |text, _| {
            text.contains("RECONNECT_ENV_AND_ARGV0_PRESERVED")
        });
        mux.kill_children();
        assert!(passed, "client must receive captured env and invocation");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn remote_reconnect_replays_preserve_environment_and_invocation() {
    let (cfg, geometry, command) = remote_reconnect_environment_fixture();
    for replay in [
        "duplicate-pane",
        "duplicate-tab",
        "split",
        "undo-close",
        "move",
    ] {
        let mut mux = Mux::new();
        super::reconnect_remote_session(
            &mut mux,
            &cfg,
            geometry,
            Arc::new(|| {}),
            Some(command.clone()),
        )
        .unwrap();
        let original = mux.active_focus().unwrap();
        assert!(wait_for_pane_screen(&mux, original, |text, _| {
            text.contains("RECONNECT_ENV_AND_ARGV0_PRESERVED")
        }));
        match replay {
            "duplicate-pane" => mux
                .duplicate_focused_pane_geometry(
                    super::Dir::Horizontal,
                    &cfg,
                    geometry,
                    Arc::new(|| {}),
                )
                .unwrap(),
            "duplicate-tab" => mux
                .duplicate_focused_tab_geometry(&cfg, geometry, Arc::new(|| {}))
                .unwrap(),
            "split" => mux
                .split_geometry(
                    super::Dir::Horizontal,
                    false,
                    &cfg,
                    geometry,
                    Arc::new(|| {}),
                )
                .unwrap(),
            "undo-close" => {
                mux.close_tab_at(0);
                assert!(
                    mux.undo_close_tab_geometry(&cfg, geometry, Arc::new(|| {}))
                        .unwrap()
                );
            }
            "move" => {
                let tab = mux.detach_tab(0).unwrap();
                mux = Mux::new();
                mux.attach_tab(tab, None);
                mux.duplicate_focused_tab_geometry(&cfg, geometry, Arc::new(|| {}))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let target = mux.active_focus().unwrap();
        assert_ne!(target, original);
        let passed = wait_for_pane_screen(&mux, target, |text, _| {
            text.contains("RECONNECT_ENV_AND_ARGV0_PRESERVED")
        });
        let screen = mux.panes[&target].term.screen_text(0).unwrap().text;
        assert!(
            passed,
            "{replay} lost the captured launch settings: {screen}"
        );
        mux.duplicate_focused_tab_geometry(&cfg, geometry, Arc::new(|| {}))
            .unwrap();
        let target = mux.active_focus().unwrap();
        let passed = wait_for_pane_screen(&mux, target, |text, _| {
            text.contains("RECONNECT_ENV_AND_ARGV0_PRESERVED")
        });
        mux.kill_children();
        assert!(passed, "{replay} must retain settings for the next replay");
    }
}

#[cfg(unix)]
#[test]
fn remote_reconnect_session_restore_falls_back_to_configured_shell() {
    let cfg = super::Config {
        shell: Some("/bin/sh".into()),
        login_shell: false,
        shell_integration: false,
        ..Default::default()
    };
    let geometry = super::PtyGeometry::from_cell_size(80, 24, 8, 16);
    let mut mux = Mux::new();
    super::reconnect_remote_session(
        &mut mux,
        &cfg,
        geometry,
        Arc::new(|| {}),
        Some(kettle_remote::SessionCommand {
            program: "/bin/cat".into(),
            args: Vec::new(),
            environment: Default::default(),
            argv0: Some("private-client-mode".into()),
        }),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&mux.snapshot()).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("private-client-mode"));
    let saved = serde_json::from_slice(&bytes).unwrap();
    mux.kill_children();
    let mut restored = Mux::new();
    assert!(restored.restore(&saved, &cfg, 8, 16, &|| Arc::new(|| {})));
    let target = restored.active_focus().unwrap();
    let argv = restored.panes[&target].argv.clone();
    restored.kill_children();
    assert_eq!(
        argv,
        vec!["/bin/sh".to_owned()],
        "private launch cannot be replayed from a session"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn remote_reconnect_environment_session_restore_omits_private_values() {
    let (mut cfg, geometry, mut command) = remote_reconnect_environment_fixture();
    cfg.shell = Some("/bin/sh".into());
    command.argv0 = None;
    let mut mux = Mux::new();
    super::reconnect_remote_session(&mut mux, &cfg, geometry, Arc::new(|| {}), Some(command))
        .unwrap();
    let bytes = serde_json::to_vec(&mux.snapshot()).unwrap();
    let json = String::from_utf8_lossy(&bytes);
    for private in [
        "tcp://client:2376",
        "from-client",
        "from-config",
        "DOCKER_HOST",
    ] {
        assert!(
            !json.contains(private),
            "session must omit private settings"
        );
    }
    let saved = serde_json::from_slice(&bytes).unwrap();
    mux.kill_children();
    let mut restored = Mux::new();
    assert!(restored.restore(&saved, &cfg, 8, 16, &|| Arc::new(|| {})));
    let target = restored.active_focus().unwrap();
    let argv = restored.panes[&target].argv.clone();
    restored.kill_children();
    assert_eq!(
        argv,
        vec!["/bin/sh".to_owned()],
        "environment alone makes a session launch unreproducible"
    );
}



