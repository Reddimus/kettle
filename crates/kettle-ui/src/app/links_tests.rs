#[test]
fn refused_terminal_url_warning_never_contains_the_payload() {
    let hostile = "javascript:alert('private-token=sekret')\nforged-log-line";
    let mut messages = Vec::new();

    super::warn_refused_terminal_url(hostile, |message| messages.push(message.to_string()));

    assert_eq!(messages.len(), 1);
    assert!(!messages[0].contains(hostile));
    assert!(!messages[0].contains("sekret"));
    assert_eq!(
        messages[0],
        "refused to open an unsafe URL from terminal output"
    );
}

/// Zooming a pane, splitting next to it or resizing the window reflows
/// the focused pane's text with no output. The detected links must follow
/// the reflowed text, or their underlines are painted over the wrong
/// cells (grep output whose path underlines went out of sync after a
/// zoom).
#[cfg(unix)]
#[test]
fn detected_links_follow_a_resize_that_reflows_the_text() {
    use kettle_config::Config;
    use kettle_core::{PtyGeometry, Waker};
    use std::sync::Arc;

    // Absolute paths, so detection needs no reported cwd.
    let line = "/srv/virtual-rack-api-tests/behave/features/contract/flight-data.contract.feature:1395:  \
                    When I send GET /flight-data/v1/time-of-departure request";
    let cfg = Config {
        shell: Some("/bin/sh".into()),
        ..Config::default()
    };
    let waker: Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf '%s\\n' '{line}'; sleep 30"),
    ];
    if let Err(e) = mux.new_tab_with_geometry(
        &cfg,
        PtyGeometry::from_cell_size(160, 10, 8, 16),
        waker,
        &argv,
        None,
    ) {
        eprintln!("skipping: no PTY ({e})");
        return;
    }
    let mut ws = WindowState::new(1, mux);
    let pane = ws.mux.active_focus().expect("a focused pane");
    let spans = |links: &[kettle_core::Link]| -> Vec<(usize, usize, usize, String)> {
        links
            .iter()
            .map(|link| (link.row, link.start_col, link.end_col, link.uri.clone()))
            .collect()
    };
    let detected_now = |ws: &WindowState| {
        let term = ws.mux.panes[&pane].term.term.lock().expect("term lock");
        kettle_core::links::links_with_cwd(
            &term,
            ws.mux.panes[&pane].term.current_dir_or_native().as_deref(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ws.mux.panes[&pane]
        .term
        .screen_text(0)
        .is_some_and(|screen| screen.text.lines().next() == Some(line) && screen.cursor == (1, 0))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the shell never printed the complete fixture"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Text is visible before its chunk's output generation is published.
    // Wait for parser completion before priming the cache, so a late
    // generation cannot hide a missing geometry key.
    while ws.mux.panes[&pane].term.pty_read_progress().pending_chunks != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the parser never finished the fixture"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    super::scan_links(&mut ws);
    let wide = spans(&ws.links);
    assert_eq!(wide, spans(&detected_now(&ws)));

    // Narrower: the line wraps and every span after the first row moves.
    // No output follows.
    let output = ws.mux.panes[&pane].term.output_generation();
    ws.mux
        .panes
        .get_mut(&pane)
        .expect("pane")
        .term
        .resize(60, 10, 8, 16)
        .expect("resize");
    assert_eq!(ws.mux.panes[&pane].term.output_generation(), output);
    super::scan_links(&mut ws);
    let narrow = spans(&detected_now(&ws));
    assert_ne!(narrow, wide, "the reflow moved the links");
    assert_eq!(spans(&ws.links), narrow, "the scan followed the reflow");
}

#[test]
fn link_resize_fixture_waits_for_parser_completion_before_scanning() {
    // The fixture is a sibling test in this file.
    let code = kettle_test_support::code_only(include_str!("links_tests.rs"));
    let fixture = code
        .split_once("fn detected_links_follow_a_resize_that_reflows_the_text() {")
        .expect("resize fixture")
        .1
        .split_once("\nfn ")
        .expect("end of resize fixture")
        .0;
    let setup = fixture
        .split_once("super::scan_links(&mut ws);")
        .expect("cache priming")
        .0;
    let wait = setup
        .rsplit_once("while ")
        .expect("completion wait")
        .1
        .split_once('{')
        .expect("wait body")
        .0;
    assert!(wait.contains(".pty_read_progress()"));
    assert!(wait.contains(".pending_chunks != 0"));
}

#[test]
fn cwd_is_local_rejects_unc_and_traversal() {
    use super::cwd_is_local;
    // Drift guard. Ordinary absolute cwds (the OSC 7 path form,
    // which uses forward slashes even on Windows: /C:/Users/x) are fine.
    assert!(cwd_is_local("/home/user"));
    assert!(cwd_is_local("/C:/Users/me/Repos"));
    // UNC / authority forms and traversal are refused (the SMB/NTLM vector).
    assert!(!cwd_is_local("//evil.host/share"));
    assert!(!cwd_is_local("\\\\evil\\share"));
    assert!(!cwd_is_local("/x/../../etc/passwd"));
    assert!(!cwd_is_local(""));
}

/// Link/path overlay underlines must follow focused output immediately.
/// `less -R` redraws the viewport with output bytes while
/// `display_offset` stays fixed, so an output-only debounce leaves stale
/// link rects visibly attached to the previous frame.
#[test]
fn focused_link_scans_are_not_output_debounced() {
    let app_src = production_source();
    let window_src = kettle_test_support::production_source(include_str!("../window_state.rs"));
    let removed_const = format!("LINKS_SCAN_{}", "DEBOUNCE");
    let removed_field = format!("last_{}_scan", "links");
    assert!(
        !app_src.contains(&removed_const),
        "focused output link scans must not be debounce-delayed"
    );
    assert!(
        !window_src.contains(&removed_field),
        "WindowState must not keep a timestamp for output-only link-scan debounce"
    );
    assert!(
        app_src.contains("output_generation"),
        "link scan key must still include focused output_generation"
    );
}

#[test]
fn custom_url_handler_failure_falls_back_to_system_open() {
    let src = production_source();
    let open_url = src
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .and_then(|s| s.split("\n    }\n").next())
        .expect("open_url helper present");
    assert!(open_url.contains("system_opener::open_custom(&cmd, uri);\n            return;"));
    let checked = src
        .split("fn open_checked_file(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    assert!(checked.contains("spawn_custom_url_handler(cmd, &url)"));
    let helper = src
        .split("fn spawn_custom_url_handler(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    let worker = helper.find(".spawn(move ||").unwrap();
    let child = helper.find("Command::new(&cmd)").unwrap();
    let wait = helper.find("child.wait()").unwrap();
    assert!(worker < child && child < wait);
    let opener = include_str!("system_opener.rs");
    let custom = opener
        .split("fn open_custom(")
        .nth(1)
        .unwrap()
        .split("\nfn ")
        .next()
        .unwrap();
    assert!(custom.contains("spawn_with("));
    assert!(
        custom.contains("Ok(()) => Ok(()),"),
        "a successful handler owns the URL"
    );
    let fallback = custom.split("Err(error) => {").nth(1).unwrap();
    assert!(
        fallback.contains("falling through to system")
            && fallback.contains("open::that_detached(target)"),
        "a failed custom handler must fall back on its worker"
    );
    assert!(
        !fallback
            .split("open::that_detached(target)")
            .next()
            .unwrap()
            .contains("return;")
    );
}

#[test]
fn custom_url_launch_uses_the_bounded_opener_worker() {
    let src = production_source();
    let open_url = src
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(
        open_url.contains("system_opener::open_custom(&cmd, uri)"),
        "custom URL process creation must run on the bounded opener worker"
    );
}

