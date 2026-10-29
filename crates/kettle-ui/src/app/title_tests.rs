#[test]
fn oversized_initial_titles_do_not_open_an_empty_editor() {
    let (mut mux, panes) = title_test_tab_mux(1);
    let limit = crate::modal_input::MAX_MODAL_INPUT_BYTES;
    for title in ["x".repeat(limit + 1), "\u{65e5}".repeat(limit / 3 + 1)] {
        mux.tabs[0].title_override = Some(title.clone());
        mux.panes.get_mut(&panes[0]).unwrap().title = title.clone();
        mux.panes.get_mut(&panes[0]).unwrap().group_name = Some(title.clone());
        for scope in [
            TitleEditScope::Tab,
            TitleEditScope::Window,
            TitleEditScope::Pane,
            TitleEditScope::Group,
        ] {
            assert!(
                begin_title_edit(&mux, scope, GroupBulkScope::Single, &title).is_none(),
                "{scope:?}"
            );
        }
        assert_eq!(mux.tabs[0].title_override.as_deref(), Some(title.as_str()));
        assert_eq!(mux.panes[&panes[0]].title, title);
        assert_eq!(
            mux.panes[&panes[0]].group_name.as_deref(),
            Some(title.as_str())
        );
    }
    for title in [
        String::new(),
        "x".repeat(limit),
        "\u{65e5}".repeat(limit / 3),
    ] {
        mux.tabs[0].title_override = Some(title.clone());
        let edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "").unwrap();
        assert_eq!(edit.input.text(), title);
        assert!(apply_mux_title_edit(&mut mux, &edit));
        assert_eq!(
            mux.tabs[0].title_override.as_deref().unwrap_or_default(),
            title
        );
    }
}

#[test]
fn pane_title_edit_never_follows_focus_to_a_sibling() {
    let (mut mux, target, sibling) = title_test_split_mux();
    mux.panes.get_mut(&target).unwrap().title = "pane-a-before".into();
    mux.panes.get_mut(&sibling).unwrap().title = "pane-b-before".into();
    let mut edit = begin_title_edit(&mux, TitleEditScope::Pane, GroupBulkScope::Single, "window")
        .expect("pane edit opens");
    edit.input = "pane-a-after".into();

    assert!(mux.focus_pane(sibling), "focus drifted while edit was open");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.panes[&target].title, "pane-a-after");
    assert_eq!(mux.panes[&sibling].title, "pane-b-before");

    let (mut mux, target, sibling) = title_test_split_mux();
    mux.panes.get_mut(&sibling).unwrap().title = "survivor".into();
    let mut edit = begin_title_edit(&mux, TitleEditScope::Pane, GroupBulkScope::Single, "window")
        .expect("pane edit opens");
    edit.input = "dead-target-title".into();
    mux.panes.get_mut(&target).unwrap().closed = true;
    assert!(!mux.reap().mux_empty, "the sibling keeps the mux alive");
    assert_eq!(mux.active_focus(), Some(sibling));
    let mut open_edit = Some(edit.clone());
    assert!(clear_stale_title_edit(&mux, &mut open_edit));
    assert!(open_edit.is_none(), "a dead pane cannot retain its modal");
    assert!(!apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.panes[&sibling].title, "survivor");

    // The behavioral helper above is wired immediately after every
    // production reap, before either path can return on an empty mux.
    let source = production_source();
    // Needle the call, not the binding it lands in. Built at runtime so
    // this guard cannot match its own source.
    let reap_call = ["ws.mux.", "reap()"].concat();
    let reap_sites = source.matches(&reap_call).count();
    assert_eq!(reap_sites, 2, "update this guard when a reap site is added");
    assert_eq!(
        source
            .split(&reap_call)
            .skip(1)
            .filter(|tail| tail[..tail.len().min(600)]
                .contains("clear_stale_title_edit(&ws.mux, &mut ws.editing_title)"))
            .count(),
        reap_sites,
        "every reap path must dismiss a title edit whose pinned target died"
    );
}

#[test]
fn tab_title_edit_uses_its_anchor_after_focus_and_index_drift() {
    let (mut mux, panes) = title_test_tab_mux(3);
    let [before, target, sibling] = panes.as_slice() else {
        panic!("three test tabs")
    };
    assert!(mux.focus_pane(*target));
    let mut edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "window")
        .expect("tab edit opens");
    edit.input = "target-tab".into();

    let before_idx = mux.tab_index_of_any_pane(&[*before]).unwrap();
    assert!(!mux.close_tab_at(before_idx), "other tabs remain");
    assert!(mux.focus_pane(*sibling), "focus drifted to another tab");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    let target_idx = mux.tab_index_of_any_pane(&[*target]).unwrap();
    let sibling_idx = mux.tab_index_of_any_pane(&[*sibling]).unwrap();
    assert_eq!(
        mux.tabs[target_idx].title_override.as_deref(),
        Some("target-tab")
    );
    assert_eq!(mux.tabs[sibling_idx].title_override, None);

    // A tab anchor contains every opening pane, so one pane can be reaped
    // without killing an edit over the tab that its sibling still owns.
    let (mut mux, exiting, survivor) = title_test_split_mux();
    let mut edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "window")
        .expect("split-tab edit opens");
    edit.input = "split-tab-survives".into();
    mux.panes.get_mut(&exiting).unwrap().closed = true;
    assert!(
        !mux.reap().mux_empty,
        "the split sibling keeps the tab alive"
    );
    assert_eq!(mux.active_focus(), Some(survivor));
    let mut open_edit = Some(edit.clone());
    assert!(!clear_stale_title_edit(&mux, &mut open_edit));
    assert!(open_edit.is_some(), "the target tab still exists");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    let survivor_tab = mux.tab_index_of_any_pane(&[survivor]).unwrap();
    assert_eq!(
        mux.tabs[survivor_tab].title_override.as_deref(),
        Some("split-tab-survives")
    );

    let (mut mux, panes) = title_test_tab_mux(2);
    let target = panes[0];
    let sibling = panes[1];
    assert!(mux.focus_pane(target));
    let mut edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "window")
        .expect("tab edit opens");
    edit.input = "dead-tab-title".into();
    let target_idx = mux.tab_index_of_any_pane(&[target]).unwrap();
    assert!(!mux.close_tab_at(target_idx), "the sibling tab remains");
    assert_eq!(mux.active_focus(), Some(sibling));
    let mut open_edit = Some(edit.clone());
    assert!(clear_stale_title_edit(&mux, &mut open_edit));
    assert!(open_edit.is_none(), "a dead tab cannot retain its modal");
    assert!(!apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.tabs[0].title_override, None);
}

/// An OSC-set title is neutralized before it reaches the OS titlebar, tab,
/// or status bar. Control chars and Unicode bidi-override format chars
/// become spaces, and the length is capped.
#[test]
fn sanitize_title_neutralizes_control_bidi_and_caps_length() {
    // C0/C1 control chars -> spaces.
    assert_eq!(sanitize_title("a\x07b\x1b\nc"), "a b  c");
    // U+202E (RLO) and friends -> spaces (Alt-Tab / titlebar spoofing).
    assert_eq!(sanitize_title("safe\u{202e}evil"), "safe evil");
    assert_eq!(sanitize_title("\u{2066}\u{2069}x"), "  x");
    // Plain text is untouched.
    assert_eq!(
        sanitize_title("~/projects/kettle — main"),
        "~/projects/kettle — main"
    );
    // Length is capped at 512 chars.
    let long = "x".repeat(5000);
    assert_eq!(sanitize_title(&long).chars().count(), 512);
}

/// `poll_remote_contexts` must sanitize
/// `kettle_remote::format_remote_title`'s output exactly like the OSC-2
/// `Title` path does. The host, user, and container strings it
/// interpolates come verbatim from a scanned process's argv, as untrusted
/// as an OSC-set title. Without this, a maliciously named SSH host or
/// container could smuggle bidi overrides or control characters into the
/// OS titlebar, Alt-Tab, the tab label, and the accessibility tree.
#[test]
fn remote_title_is_sanitized_like_osc_titles() {
    let evil_host = kettle_remote::RemoteContext::Ssh {
        host: "safe\u{202e}evil".to_string(),
        user: None,
        options: kettle_remote::SshOptions::default(),
    };
    let raw = kettle_remote::format_remote_title(&evil_host);
    assert!(
        raw.contains('\u{202e}'),
        "sanity: format_remote_title itself does no filtering"
    );
    let sanitized = sanitize_title(&raw);
    assert!(
        !sanitized.contains('\u{202e}') && !sanitized.chars().any(|c| c.is_control()),
        "sanitize_title(format_remote_title(..)) must neutralize bidi/control chars: {sanitized:?}"
    );

    let evil_container = kettle_remote::RemoteContext::Container {
        runtime: kettle_remote::ContainerRuntime::Docker,
        container: "evil\x1b]0;pwned\x07".to_string(),
        options: kettle_remote::ContainerOptions::default(),
    };
    let sanitized = sanitize_title(&kettle_remote::format_remote_title(&evil_container));
    assert!(
        !sanitized.chars().any(|c| c.is_control()),
        "container-name control chars must be neutralized: {sanitized:?}"
    );
}

#[test]
fn remote_generated_title_restores_its_prior_origin_on_exit() {
    let first = kettle_remote::RemoteContext::Ssh {
        host: "one.example".to_string(),
        user: Some("dev".to_string()),
        options: kettle_remote::SshOptions::default(),
    };
    let second = kettle_remote::RemoteContext::Ssh {
        host: "two.example".to_string(),
        user: None,
        options: kettle_remote::SshOptions::default(),
    };
    let mut title = "kettle".to_string();
    let mut placeholder = true;
    let mut origin = PaneTitleOrigin::Placeholder;
    let mut before_remote = None;

    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some(&first),
    );
    assert_eq!(origin, PaneTitleOrigin::Remote);
    assert!(!placeholder);
    assert_eq!(
        before_remote,
        Some(("kettle".to_string(), PaneTitleOrigin::Placeholder))
    );

    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some(&second),
    );
    assert_eq!(
        before_remote,
        Some(("kettle".to_string(), PaneTitleOrigin::Placeholder)),
        "remote-to-remote changes must not overwrite the local fallback"
    );

    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        None,
    );
    assert_eq!(title, "kettle");
    assert!(placeholder);
    assert_eq!(origin, PaneTitleOrigin::Placeholder);
    assert!(before_remote.is_none());

    title = "nvim project".to_string();
    placeholder = false;
    origin = PaneTitleOrigin::Osc;
    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        None,
    );
    assert_eq!(title, "nvim project", "non-remote OSC titles are retained");
    assert_eq!(origin, PaneTitleOrigin::Osc);
}

/// Native OS titlebars do not reliably use kettle's terminal font fallback,
/// so Private Use / Nerd Font glyphs are stripped there only.
#[test]
fn native_window_title_strips_private_use_glyphs_only() {
    assert_eq!(
        sanitize_native_window_title("\u{f015} kevim — kettle"),
        "kevim — kettle"
    );
    assert_eq!(
        sanitize_native_window_title("\u{f120} SPI-1 — kettle"),
        "SPI-1 — kettle"
    );
    assert_eq!(
        sanitize_native_window_title("\u{e795} projects \u{e0b0} kettle"),
        "projects  kettle"
    );
    assert_eq!(
        sanitize_native_window_title("\u{f0000} build \u{100000} — kettle"),
        "build  — kettle"
    );
    assert_eq!(
        sanitize_native_window_title("世界 😀 — kettle"),
        "世界 😀 — kettle"
    );
    assert_eq!(sanitize_native_window_title("\u{f015}\u{e0b0}"), "kettle");
}

#[test]
fn the_shelf_badge_counts_unseen_items_only() {
    assert_eq!(super::media_shelf_badge(0), "");
    assert_eq!(super::media_shelf_badge(3), "▣3 ");
}

/// The OS titlebar is a spoofing sink: the Cf bidi overrides are not
/// `char::is_control`, so they pass every control-character filter and can
/// reverse the rendered text in the titlebar and the Alt-Tab switcher.
///
/// Every route to that sink must run `sanitize_title`, including the
/// user-set window title. Its editor accepts paste, which puts a clipboard
/// payload one keystroke away, and terminal programs can write the
/// clipboard through OSC 52. Pin sanitization on the shared title path;
/// the edit buffer itself deliberately keeps what was typed.
#[test]
fn the_user_set_window_title_is_sanitized_before_it_reaches_the_window() {
    let src = production_source();
    let arm = src
        .split("TitleEditScope::Window => {")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("window title-edit arm present");
    assert!(arm.contains("self.sync_window_title(ws);"));
    assert!(!arm.contains("w.set_title("));
    let sync = src
        .split("fn sync_window_title(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap();
    assert!(sync.contains("let want = self.desired_window_title(ws);"));
    assert!(!sync.contains("window_title_override"));
    let desired = src
        .split("fn desired_window_title(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap();
    assert!(desired.contains("Some(forced) => sanitize_title(forced),"));
    let base = desired.find("ws.window_title_override").unwrap();
    let recording = desired.find("recording_window_title(").unwrap();
    let sanitize = desired.find("sanitize_native_window_title(").unwrap();
    assert!(base < recording && recording < sanitize);
    // And the sanitizer must still cover the bidi class, not just controls.
    assert_eq!(sanitize_title("safe\u{202e}evil"), "safe evil");
    assert_eq!(sanitize_title("ok\u{2069}"), "ok ");
    // A ZWJ emoji is not a bidi override and must survive a title.
    assert_eq!(
        sanitize_title("a\u{1f469}\u{200d}\u{1f680}"),
        "a\u{1f469}\u{200d}\u{1f680}"
    );
}

#[test]
fn cap_title_for_status_bar_truncates_at_char_budget_with_ellipsis() {
    // Drift guard. The status-bar strip is 1 cell tall;
    // a long title would wrap past the visible region without
    // this cap. Pins the contract:
    //   - under-budget: returned as-is, no `…`.
    //   - over-budget: truncated to `max` chars + `…`.
    //   - UTF-8 multibyte: char-count not byte-count (so a
    //     "🦀🦀🦀…" title isn't cut mid-character).
    use super::cap_title_for_status_bar;
    assert_eq!(cap_title_for_status_bar("short", 60), "short");
    assert_eq!(cap_title_for_status_bar("", 60), "");
    let exactly_60 = "x".repeat(60);
    assert_eq!(cap_title_for_status_bar(&exactly_60, 60), exactly_60);
    let long = "x".repeat(80);
    let capped = cap_title_for_status_bar(&long, 60);
    assert_eq!(capped.chars().count(), 61); // 60 + the `…`
    assert!(capped.ends_with('…'));
    // UTF-8 multibyte at the boundary — must split on a char
    // boundary, not a byte boundary.
    let crab_run = "🦀".repeat(80);
    let capped_crab = cap_title_for_status_bar(&crab_run, 60);
    assert_eq!(capped_crab.chars().count(), 61);
    // Every char before the `…` should still be a full crab.
    assert!(capped_crab.chars().take(60).all(|c| c == '🦀'));
}

/// The conhost startup-title detector that lets a stock Windows shell fall
/// back to a cwd label instead of showing its own exe path.
#[test]
fn conhost_startup_title_detected_real_titles_pass() {
    use super::is_conhost_startup_title;
    let pwsh = ["pwsh.exe".to_string()];
    // The injected full exe path for a WindowsApps-packaged pwsh.
    assert!(is_conhost_startup_title(
        "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe\\pwsh.exe",
        &pwsh
    ));
    assert!(is_conhost_startup_title(
        "C:\\Windows\\System32\\cmd.exe",
        &["cmd.exe".to_string()]
    ));
    // Bare-name argv0 equality (no path in the title).
    assert!(is_conhost_startup_title("pwsh", &["pwsh".to_string()]));
    // Default shell (empty argv, argv0 unknown): any absolute .exe qualifies.
    assert!(is_conhost_startup_title(
        "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
        &[]
    ));
    // A REAL program-set title is NEVER suppressed.
    assert!(!is_conhost_startup_title("PS C:\\repos\\kettle>", &pwsh));
    assert!(!is_conhost_startup_title("vim README.md", &pwsh));
    // A different exe path the program legitimately set (leaf != argv0 leaf).
    assert!(!is_conhost_startup_title("C:\\tools\\fzf.exe", &pwsh));
    // Empty title, and a default-shell non-path title.
    assert!(!is_conhost_startup_title("", &pwsh));
    assert!(!is_conhost_startup_title("my session", &[]));
}

/// The per-pane
/// titlebar badges, composed by `compose_pane_title`.
#[test]
fn pane_title_badges_compose_from_state() {
    use super::{compose_pane_title, pane_title_parts};
    // No badges: title unchanged regardless of the configured badge text.
    assert_eq!(compose_pane_title("[agent] ", false, false, "bash"), "bash");
    // Agent attached: badge prefixed.
    assert_eq!(
        compose_pane_title("[agent] ", true, false, "bash"),
        "[agent] bash"
    );
    // Empty badge disables the agent prefix even when attached.
    assert_eq!(compose_pane_title("", true, false, "bash"), "bash");
    // Custom badge glyph.
    assert_eq!(compose_pane_title("🤖 ", true, false, "vim"), "🤖 vim");
    // Read-only: `[RO] ` prefixed.
    assert_eq!(
        compose_pane_title("[agent] ", false, true, "bash"),
        "[RO] bash"
    );
    // Both: read-only leads, then the agent badge.
    assert_eq!(
        compose_pane_title("[agent] ", true, true, "bash"),
        "[RO] [agent] bash"
    );
    // Read-only with an empty agent badge still shows `[RO]`.
    assert_eq!(compose_pane_title("", true, true, "bash"), "[RO] bash");

    let parts = pane_title_parts(
        "[agent] ",
        true,
        true,
        "..ine-server-go",
        false,
        Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
        Some("/home/u"),
    );
    assert_eq!(parts.prefix, "[RO] [agent] ");
    assert_eq!(parts.title, "flight-event-line-server-go");
    assert_eq!(
        parts.path.as_deref(),
        Some("~/Repos/SPI-1/flight-event-line-server-go")
    );
}

#[test]
fn pane_titlebar_hit_geometry() {
    use super::pane_titlebar_hit;
    // 800x600 surface with 2 panes side-by-side, 24px titlebar.
    // Left pane:  (0, 0, 400, 600)
    // Right pane: (400, 0, 400, 600)
    let rects = vec![
        (1u64, (0.0_f32, 0.0_f32, 400.0_f32, 600.0_f32)),
        (2u64, (400.0_f32, 0.0_f32, 400.0_f32, 600.0_f32)),
    ];
    // Top titlebar: y-band [1, 25).
    // Click inside left pane's titlebar:
    assert_eq!(pane_titlebar_hit(50.0, 12.0, &rects, false, 24.0), Some(1));
    // Click inside right pane's titlebar:
    assert_eq!(pane_titlebar_hit(500.0, 12.0, &rects, false, 24.0), Some(2));
    // Click below the titlebar (in cell content) → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 100.0, &rects, false, 24.0), None);
    // Click ABOVE the bar (y < 1) → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 0.5, &rects, false, 24.0), None);
    // Bottom titlebar: y-band [576, 600).
    assert_eq!(pane_titlebar_hit(50.0, 580.0, &rects, true, 24.0), Some(1));
    assert_eq!(pane_titlebar_hit(500.0, 580.0, &rects, true, 24.0), Some(2));
    // Click at top-of-pane with title_at_bottom=true → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 12.0, &rects, true, 24.0), None);
    // Click in cell content with title_at_bottom=true → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 300.0, &rects, true, 24.0), None);
}

/// A titlebar the layout carves off the top moves row zero down by its
/// height for the pointer and the grid alike; one at the bottom leaves row
/// zero at the padded pane origin. Both follow from the pane's partition,
/// whose terminal the pointer maps into and the PTY is sized from.
#[test]
fn titlebar_inset_realigns_hit_test_and_grid() {
    use super::{grid_dims_px, px_to_cell};
    use crate::pane_partition::{Metrics, partition_leaf};
    let rect = (10.0, 20.0, 800.0, 600.0);
    let (cw, ch, pad) = (8.0_f32, 16.0_f32, 4.0_f32);
    let tb = ch + 6.0; // the titlebar height the window's style uses
    let terminal = |titlebar: f32, at_bottom: bool| {
        let metrics = Metrics::new((cw, ch), (pad, pad), titlebar, at_bottom).unwrap();
        partition_leaf(rect, metrics, None).terminal
    };
    let bare = terminal(0.0, false);
    let top = terminal(tb, false);
    let bottom = terminal(tb, true);

    // No titlebar (single-pane tab): content origin = ry + pad = 24.
    let (_, line0, _) = px_to_cell(100.0, 24.0, bare, (cw, ch), (pad, pad));
    assert_eq!(line0, 0, "py at content origin → row 0 (no titlebar)");

    // With a titlebar: content origin shifts down by `tb` to 24 + 22 = 46.
    let (_, line_origin, _) = px_to_cell(100.0, 24.0 + tb, top, (cw, ch), (pad, pad));
    assert_eq!(
        line_origin, 0,
        "py at the titlebar'd content origin → row 0"
    );
    // One cell below the titlebar'd origin → row 1. Ignoring the titlebar
    // inset would give row 2, an off-by-one.
    let (_, line1, _) = px_to_cell(100.0, 24.0 + tb + ch, top, (cw, ch), (pad, pad));
    assert_eq!(line1, 1);
    // A click up in the titlebar band clamps to row 0 (never negative).
    let (_, clamped, _) = px_to_cell(100.0, 24.0, top, (cw, ch), (pad, pad));
    assert_eq!(clamped, 0);

    // Moving the title to the bottom moves row zero back to the ordinary
    // padded pane origin. Pointer projection must follow the rendered grid,
    // not retain the old top-title inset.
    let (_, bottom_line0, _) = px_to_cell(100.0, 24.0, bottom, (cw, ch), (pad, pad));
    assert_eq!(bottom_line0, 0);
    let (_, bottom_line1, _) = px_to_cell(100.0, 24.0 + ch, bottom, (cw, ch), (pad, pad));
    assert_eq!(bottom_line1, 1);

    // The titlebar steals height, so fewer rows fit in its terminal.
    let (cols_no, rows_no) = grid_dims_px((bare.2, bare.3), (cw, ch), (pad, pad));
    let (cols_tb, rows_tb) = grid_dims_px((top.2, top.3), (cw, ch), (pad, pad));
    assert_eq!(cols_no, cols_tb, "titlebar doesn't change column count");
    assert!(
        rows_tb < rows_no,
        "titlebar must reduce reported rows ({rows_tb} < {rows_no})"
    );
}

/// A hand-set pane name must survive the shell's next prompt. bash and zsh
/// emit OSC 0/2 on EVERY prompt, so without the Manual origin, naming a pane
/// `db-prod` would last under a second.
#[test]
fn a_hand_set_pane_title_outranks_the_shells_osc_title() {
    use super::osc_title_may_replace;
    use crate::mux::PaneTitleOrigin;

    assert!(
        !osc_title_may_replace(PaneTitleOrigin::Manual),
        "a title the user typed must not be replaced by the shell"
    );
    // Every other origin is shell- or launch-derived and must keep
    // tracking OSC, or titles would freeze at the first thing set.
    for origin in [
        PaneTitleOrigin::Placeholder,
        PaneTitleOrigin::ExplicitLaunch,
        PaneTitleOrigin::Osc,
        PaneTitleOrigin::Remote,
    ] {
        assert!(
            osc_title_may_replace(origin),
            "{origin:?} must still follow the shell's title"
        );
    }
}

/// The gate has to cover BOTH doors. Shells also emit a title RESET at
/// prompts, so gating only the OSC set would still lose the hand-set name,
/// just via the other event.
#[test]
fn a_hand_set_pane_title_survives_a_title_reset_too() {
    use super::apply_shell_title;
    use crate::mux::PaneTitleOrigin;

    let mut title = "db-prod".to_string();
    let mut placeholder = false;
    let mut origin = PaneTitleOrigin::Manual;
    let mut before_remote = None;

    for door in [Some("bash"), None] {
        apply_shell_title(
            &mut title,
            &mut placeholder,
            &mut origin,
            &mut before_remote,
            door,
        );
        assert_eq!(title, "db-prod", "the shell must not take a hand-set name");
        assert_eq!(origin, PaneTitleOrigin::Manual);
        assert!(!placeholder);
    }
}

/// Exiting ssh must put the pane's own name back. While connected, the
/// remote shell's title shows, since it is more useful than kettle's
/// synthetic label. Applying it must keep the saved pre-remote title, or
/// the disconnect restore has nothing to restore. It must also keep the
/// `Remote` origin, or the restore does not run and a pane named `db-prod`
/// comes back from ssh still calling itself the remote host.
#[test]
fn a_remote_shells_title_shows_without_losing_the_name_to_come_back_to() {
    use super::{apply_remote_title_transition, apply_shell_title};
    use crate::mux::PaneTitleOrigin;

    let mut title = "db-prod".to_string();
    let mut placeholder = false;
    let mut origin = PaneTitleOrigin::Manual;
    let mut before_remote = None;

    // Enter a remote context: the manual name is stashed.
    let ctx = kettle_remote::RemoteContext::Ssh {
        host: "buildbox".into(),
        user: Some("kevim".into()),
        options: kettle_remote::SshOptions::default(),
    };
    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some(&ctx),
    );
    assert_eq!(origin, PaneTitleOrigin::Remote);
    assert_eq!(
        before_remote,
        Some(("db-prod".to_string(), PaneTitleOrigin::Manual))
    );

    // The remote shell sets its own title. It shows...
    apply_shell_title(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some("kevim@buildbox:~/work"),
    );
    assert_eq!(title, "kevim@buildbox:~/work", "the remote title shows");
    // ...without disturbing either thing the restore depends on.
    assert_eq!(
        origin,
        PaneTitleOrigin::Remote,
        "the pane is still in a remote context"
    );
    assert_eq!(
        before_remote,
        Some(("db-prod".to_string(), PaneTitleOrigin::Manual)),
        "the name to come back to must survive the remote shell's title"
    );

    // Disconnect: the hand-set name comes back.
    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        None,
    );
    assert_eq!(title, "db-prod");
    assert_eq!(origin, PaneTitleOrigin::Manual);
    assert_eq!(before_remote, None);
}

#[test]
fn tab_title_rect_excludes_visible_close_button() {
    use super::tab_title_rect;

    let segment = (100.0, 0.0, 240.0, 24.0);
    let close = (316.0, 0.0, 24.0, 24.0);
    assert_eq!(
        tab_title_rect(segment, close, true),
        (108.0, 0.0, 208.0, 24.0)
    );
    assert_eq!(
        tab_title_rect(segment, close, false),
        (108.0, 0.0, 224.0, 24.0)
    );
}

#[test]
fn window_title_formats_and_falls_back() {
    use super::{window_title, window_title_with_home};
    let dflt = "{title} — kettle";
    assert_eq!(
        window_title(dflt, "vim README.md", "", 1),
        "vim README.md — kettle"
    );
    assert_eq!(window_title(dflt, "  spaced  ", "", 1), "spaced — kettle");
    // Empty / placeholder titles with no cwd → just the app name (the
    // template never produces a stub like " — kettle").
    assert_eq!(window_title(dflt, "", "", 1), "kettle");
    assert_eq!(window_title(dflt, "   ", "", 1), "kettle");
    assert_eq!(window_title(dflt, "kettle", "", 1), "kettle");
    // Empty / placeholder title *with* cwd → cwd basename fills in
    // (the cwd-basename tab-title fallback, same shape for the window title).
    assert_eq!(
        window_title(dflt, "", "/home/k/Repos/kettle", 1),
        "kettle — kettle",
        "cwd basename `kettle` fills the {{title}} slot pre-OSC 2"
    );
    assert_eq!(
        window_title(dflt, "kettle", "/home/k/Documents", 1),
        "Documents — kettle"
    );
    assert_eq!(
        window_title_with_home(
            dflt,
            "..PI-1/platform",
            "/home/k/Repos/SPI-1/platform",
            Some("/home/k"),
            1
        ),
        "~/Repos/SPI-1/platform — kettle",
        "truncated cwd-rendered shell titles recover the full cwd for the OS title"
    );
    // Custom templates can use {tab} and {cwd}.
    let t = "[{tab}] {title} ({cwd})";
    assert_eq!(
        window_title(t, "vim", "/home/k/Repos/kettle", 2),
        "[2] vim (/home/k/Repos/kettle)"
    );
    assert_eq!(
        window_title_with_home(
            t,
            "...PI-1/platform",
            "/home/k/Repos/SPI-1/platform",
            Some("/home/k"),
            2
        ),
        "[2] ~/Repos/SPI-1/platform (~/Repos/SPI-1/platform)"
    );
    for title in ["", "kettle"] {
        assert_eq!(
            window_title_with_home(t, title, "/", None, 2),
            "[2] kettle (/)"
        );
        assert_eq!(
            window_title_with_home(t, title, "", None, 2),
            "[2] kettle ()"
        );
        assert_eq!(window_title_with_home("{tab}", title, "/", None, 2), "2");
        assert_eq!(window_title_with_home("", title, "/", None, 2), "");
        assert_eq!(window_title_with_home(dflt, title, "/", None, 2), "kettle");
    }
}

#[test]
fn get_state_reports_desired_window_title() {
    let src = production_source();
    let body = src
        .split("fn ctl_get_state")
        .nth(1)
        .and_then(|s| s.split("fn ctl_list_tabs").next())
        .expect("ctl_get_state body present");
    assert!(
        body.contains("\"window_title\": self.desired_window_title(ws),"),
        "get_state must expose the same native title string that sync_window_title sets"
    );
}

/// The title-edit overlay must be wide enough to show what you are typing.
///
/// Under a vertical tab bar, the first tab SEGMENT's rect is only
/// `tab-bar-width` wide (default 180px). The renderer fits
/// `"  ✎ {label} {input}_   (Enter apply · Esc cancel)"` into
/// `overlay_label_cols(width, cw)`; at 180px and an 8px cell that is 21
/// columns, and the trailing hint alone is 30. An overlay placed in that
/// rect shows zero input columns, no caret, and no horizontal scroll, yet
/// Enter still commits the invisible buffer. For `EditPaneGroup` that
/// buffer is the broadcast-group name, which decides which panes receive
/// later keystrokes.
#[test]
fn title_edit_overlay_is_wide_enough_to_show_the_input() {
    use super::{ContentBands, content_rect_for_with_strip, title_edit_rect_for};
    use kettle_config::{StatusBarMode, TabBarPos};

    let sw = 1512.0f32;
    let sh = 945.0f32;
    let row_h = 24.0f32;
    let strip_w = 180.0f32;
    let content = content_rect_for_with_strip(
        (sw as u32, sh as u32),
        ContentBands {
            tab_bar_h: row_h,
            vertical_title_edit_h: row_h,
            ..ContentBands::default()
        },
        TabBarPos::Left,
        StatusBarMode::Off,
        strip_w,
    );
    // A vertical strip: tall bar, tab-bar-width wide. The overlay occupies
    // the horizontal band immediately above terminal content.
    let vertical = title_edit_rect_for(true, 0.0, sh, content.1, sw, sh, row_h);
    assert_eq!(
        vertical.2, sw,
        "vertical tab bar: overlay must span the window, not the 180px strip"
    );
    assert_eq!(
        vertical.3, row_h,
        "vertical tab bar: overlay is one row, not the full strip height"
    );

    let intersects = vertical.0 < content.0 + content.2
        && content.0 < vertical.0 + vertical.2
        && vertical.1 < content.1 + content.3
        && content.1 < vertical.1 + vertical.3;
    assert!(
        !intersects,
        "the full-width title input must occupy reserved chrome, not cover terminal content: \
             overlay={vertical:?}, content={content:?}"
    );

    // A top status bar keeps its own band. The title input starts after it,
    // and terminal content starts after both pieces of chrome.
    let status_h = 22.0;
    let content_below_status = content_rect_for_with_strip(
        (sw as u32, sh as u32),
        ContentBands {
            tab_bar_h: row_h,
            status_bar_h: status_h,
            vertical_title_edit_h: row_h,
        },
        TabBarPos::Right,
        StatusBarMode::Top,
        strip_w,
    );
    let below_status = title_edit_rect_for(true, 0.0, sh, content_below_status.1, sw, sh, row_h);
    assert_eq!(below_status.1, status_h);
    assert_eq!(content_below_status.1, status_h + row_h);
    assert!(below_status.1 + below_status.3 <= content_below_status.1);

    // Horizontal is unchanged: the overlay sits on the bar itself.
    let horizontal = title_edit_rect_for(false, 12.0, 28.0, 0.0, sw, sh, row_h);
    assert_eq!(horizontal, (0.0, 12.0, sw, 28.0));

    // No tab bar at all: a single row at the top.
    let none = title_edit_rect_for(false, 0.0, 0.0, 0.0, sw, sh, row_h);
    assert_eq!(none, (0.0, 0.0, sw, row_h));
}

/// #251: on macOS the titlebar theme must not be seeded at window
/// creation. winit turns a creation-time hint into an `NSWindow`
/// appearance override before AppKit has built the titlebar's container
/// view, and the native material view kettle adds to the frame view right
/// afterwards then locks in the container-less layout — AppKit centres the
/// title over the traffic lights, and nothing recovers it for the life of
/// the window. The appearance goes on once the window is the key window
/// instead. Layout itself is only checkable live, on a real desktop; this
/// pins the policy and the wiring that carry it.
#[test]
fn macos_applies_the_titlebar_theme_after_the_window_is_up() {
    use super::{WindowTheme, creation_theme_hint, native_theme_sync_is_due};
    for hint in [None, Some(WindowTheme::Light), Some(WindowTheme::Dark)] {
        assert_eq!(
            creation_theme_hint(hint, false),
            hint,
            "every other platform still seeds the caption at creation"
        );
        assert_eq!(
            creation_theme_hint(hint, true),
            None,
            "macOS must reach the window with no appearance override"
        );
    }
    // The deferral is macOS-only.
    for already_synced in [false, true] {
        assert!(native_theme_sync_is_due(false, already_synced, false));
        assert!(native_theme_sync_is_due(true, already_synced, false));
    }
    // On macOS only the first appearance waits for the key window; once a
    // window has one, later changes must not be held back or a background
    // window strands on a stale titlebar until someone clicks it.
    assert!(!native_theme_sync_is_due(false, false, true));
    assert!(native_theme_sync_is_due(true, false, true));
    assert!(native_theme_sync_is_due(false, true, true));
    assert!(native_theme_sync_is_due(true, true, true));

    // Policy alone does not fix anything if the call sites drop it.
    let src = production_source();
    assert!(
        src.contains(".with_theme(creation_theme_hint("),
        "window creation must route its theme hint through creation_theme_hint"
    );
    assert!(
        src.contains("state.window_shown && w.has_focus()"),
        "the first apply must wait for a shown, key window"
    );
    assert!(
        src.contains("state.native_theme_synced.is_some(),"),
        "later changes must not be gated on focus, or a background window \
             keeps a stale titlebar"
    );
    assert!(
        src.contains("state.native_theme_synced = Some(hint)"),
        "the applied hint must be tracked per window, not once per process"
    );
}

/// Drift guard: the winit dependency must enable Wayland CSD via
/// `wayland-csd-adwaita-notitle`, the sctk-adwaita variant with NO text
/// renderer, and never the `ab_glyph`/`crossfont` variants, which would bring
/// back the unmaintained `ttf-parser` (RUSTSEC-2026-0192). `deny.toml` bans it
/// from the graph; this pins the manifest side so `cargo test` catches a
/// feature edit before CI.
/// Dropping the feature entirely would regress GNOME Wayland to
/// smithay-client-toolkit's FallbackFrame (flat gray bar, filled-square
/// close button, no Adwaita styling).
#[test]
fn winit_wayland_csd_stays_notitle() {
    let manifest = include_str!("../../Cargo.toml");
    let winit_features = manifest
        .split("winit = {")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("kettle-ui Cargo.toml declares winit with a feature list");
    assert!(
        winit_features.contains("\"wayland-csd-adwaita-notitle\""),
        "winit must keep the wayland-csd-adwaita-notitle feature — \
             without it GNOME Wayland falls back to the ugly FallbackFrame"
    );
    assert!(
        !winit_features.contains("\"wayland-csd-adwaita\"\n")
            && !winit_features.contains("\"wayland-csd-adwaita\",")
            && !winit_features.contains("crossfont"),
        "the ab_glyph/crossfont CSD variants reintroduce ttf-parser or a \
             FreeType build dep — keep the notitle variant (RUSTSEC-2026-0192)"
    );
}

#[test]
fn unix_first_shell_title_can_equal_argv0() {
    assert!(
        !super::suppress_startup_title("sh", &["sh".into()], false),
        "a Unix shell's OSC title equal to argv0 is legitimate"
    );
    assert!(super::suppress_startup_title(
        "pwsh",
        &["pwsh".into()],
        true
    ));
}

