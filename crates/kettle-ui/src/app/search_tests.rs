/// The search bar's word-wise editing is bound to Option on macOS and is
/// gated only on Backspace, Delete and the horizontal arrows, each a key
/// `option_is_meta_for` exempts. Reading the masked `ws.mods` there would
/// make `delete_word_backward` and `move_left(by_word)` unreachable on the
/// shipped default.
#[test]
fn the_search_bar_word_modifier_reads_the_physical_option_key() {
    let src = production_source();
    let body = src
        .split("fn search_key(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("search_key body");
    let word_modifier = body
        .split("let word_modifier =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("search_key computes a word modifier");
    assert!(
        word_modifier.contains("option_physically_held(ws)"),
        "macOS word editing must read the physical Option key, not the \
             masked modifiers: {word_modifier}"
    );
    assert!(
        !word_modifier.contains("ws.mods.alt_key()"),
        "the masked value is exactly what made these motions dead"
    );
}
#[test]
fn config_reload_synchronizes_open_search_preferences() {
    let now = std::time::Instant::now();
    let mut search = crate::search_input::SearchState {
        open: true,
        editor: crate::search_input::SearchEditor::from_text("(".into(), 4096),
        compiled_revision: Some(0),
        status: kettle_render::SearchStatus::Invalid,
        ..crate::search_input::SearchState::default()
    };

    assert!(super::sync_reloaded_search_preferences(
        &mut search,
        false,
        kettle_config::SearchCaseSensitivity::Smart,
        true,
        now,
    ));
    assert!(!search.wrap);
    assert!(search.invert);
    assert_eq!(search.status, kettle_render::SearchStatus::Invalid);
    assert!(search.unlimited_retry_at.is_none());

    assert!(!super::sync_reloaded_search_preferences(
        &mut search,
        false,
        kettle_config::SearchCaseSensitivity::Always,
        true,
        now,
    ));
    assert_eq!(
        search.case_mode,
        kettle_config::SearchCaseSensitivity::Always
    );
    assert_eq!(search.status, kettle_render::SearchStatus::Searching);
    assert_eq!(search.revision, 1);
}

#[test]
fn visible_projection_limit_is_derived_and_clears_with_the_viewport() {
    let mut search = crate::search_input::SearchState {
        status: kettle_render::SearchStatus::Match,
        visible_truncated: true,
        ..crate::search_input::SearchState::default()
    };
    assert_eq!(
        super::effective_search_status(&search),
        kettle_render::SearchStatus::Limited
    );
    search.visible_truncated = false;
    assert_eq!(
        super::effective_search_status(&search),
        kettle_render::SearchStatus::Match
    );
    search.status = kettle_render::SearchStatus::Limited;
    assert_eq!(
        super::effective_search_status(&search),
        kettle_render::SearchStatus::Limited
    );
}

#[test]
fn output_continuation_keeps_progress_and_defers_a_definitive_miss() {
    let token = kettle_core::SearchScanToken {
        query_revision: 3,
        output_generation: 9,
        layout: kettle_core::SearchLayout {
            columns: 80,
            screen_lines: 24,
            history_size: 10_000,
        },
    };
    let mut job = crate::search_input::BackgroundSearch {
        token: kettle_core::SearchScanToken {
            output_generation: 8,
            ..token
        },
        direction: kettle_core::SearchDirection::Forward,
        cursor: kettle_core::SearchPoint::new(-4_000, 0),
        edge: kettle_core::SearchPoint::new(23, 79),
        wrap_anchor: kettle_core::SearchPoint::new(0, 0),
        wrapped: false,
        nearby: false,
        navigation: false,
        had_focus: false,
        output_drifted: false,
    };
    super::continue_background_after_output(&mut job, token, -10_000, 23, 79);
    assert_eq!(job.cursor, kettle_core::SearchPoint::new(-4_000, 0));
    assert_eq!(job.edge, kettle_core::SearchPoint::new(23, 79));
    assert_eq!(job.token, token);
    assert!(job.output_drifted);

    let mut nearby_job = job;
    nearby_job.nearby = true;
    let mut nearby_search = crate::search_input::SearchState {
        revision: 3,
        background: Some(nearby_job),
        ..crate::search_input::SearchState::default()
    };
    assert!(!super::should_start_nearby_search(&nearby_search));
    nearby_search.background.as_mut().unwrap().nearby = false;
    assert!(!super::should_start_nearby_search(&nearby_search));
    nearby_search.background = None;
    assert!(super::should_start_nearby_search(&nearby_search));
    nearby_search.typing_scanned_revision = Some(3);
    assert!(!super::should_start_nearby_search(&nearby_search));

    let now = std::time::Instant::now();
    nearby_job.output_drifted = false;
    nearby_search.typing_scanned_revision = None;
    nearby_search.unlimited_retry_at = Some(now);
    assert!(!super::prepare_initial_search_retry_after_output(
        &mut nearby_search,
        &nearby_job,
        nearby_job.wrap_anchor,
    ));
    assert_eq!(nearby_search.typing_scanned_revision, Some(3));
    assert!(nearby_search.unlimited_retry_at.is_none());
    assert!(!super::should_start_nearby_search(&nearby_search));

    nearby_job.output_drifted = true;
    let retry_at = now + std::time::Duration::from_millis(500);
    nearby_search.unlimited_retry_at = Some(retry_at);
    assert!(super::prepare_initial_search_retry_after_output(
        &mut nearby_search,
        &nearby_job,
        nearby_job.wrap_anchor,
    ));
    assert!(nearby_search.quiet_retry_pending);
    assert_eq!(nearby_search.unlimited_retry_at, Some(retry_at));
    assert!(!super::should_start_nearby_search(&nearby_search));

    let (status, retry_at) = super::finished_background_search(&job, std::time::Instant::now());
    assert_eq!(status, kettle_render::SearchStatus::Searching);
    assert!(retry_at.is_some());

    job.navigation = true;
    job.direction = kettle_core::SearchDirection::Reverse;
    job.had_focus = true;
    assert!(!super::should_retry_output_drift(&job));
    let original_anchor = kettle_core::SearchPoint::new(-99, 7);
    let mut search = crate::search_input::SearchState {
        anchor: Some(original_anchor),
        typing_scanned_revision: Some(3),
        ..crate::search_input::SearchState::default()
    };
    assert!(!super::prepare_initial_search_retry_after_output(
        &mut search,
        &job,
        kettle_core::SearchPoint::new(0, 0),
    ));
    assert_eq!(search.anchor, Some(original_anchor));
    assert_eq!(search.typing_scanned_revision, Some(3));
    let (status, retry_at) = super::finished_background_search(&job, std::time::Instant::now());
    assert_eq!(status, kettle_render::SearchStatus::Limited);
    assert!(retry_at.is_none());

    job.navigation = false;
    let retry_anchor = kettle_core::SearchPoint::new(-20, 0);
    assert!(super::prepare_initial_search_retry_after_output(
        &mut search,
        &job,
        retry_anchor,
    ));
    assert_eq!(search.anchor, Some(retry_anchor));
    assert_eq!(search.typing_scanned_revision, Some(3));
    assert!(search.quiet_retry_pending);
    search.unlimited_retry_at = Some(std::time::Instant::now());
    assert!(super::activate_quiet_search_retry(&mut search));
    assert!(!search.quiet_retry_pending);
    assert!(search.typing_scanned_revision.is_none());
    // The nearby scan goes first, and the full scan is already due behind
    // it, so a nearby miss cannot leave the search stuck at `Searching`.
    assert!(super::should_start_nearby_search(&search));
    assert!(
        search
            .unlimited_retry_at
            .is_some_and(|deadline| deadline <= std::time::Instant::now())
    );

    search.quiet_retry_pending = true;
    search.unlimited_retry_at = Some(std::time::Instant::now());
    super::cancel_deferred_search_retry(&mut search);
    assert!(!search.quiet_retry_pending);
    assert!(search.unlimited_retry_at.is_none());

    job.navigation = true;
    job.output_drifted = false;
    let (status, retry_at) = super::finished_background_search(&job, std::time::Instant::now());
    assert_eq!(status, kettle_render::SearchStatus::Start);
    assert!(retry_at.is_none());
}

/// The search bar reserves a lane below the grid; the grid stays
/// mouse-interactive while it is open (Terminator parity). A press is
/// routed by where it lands, motion and release by whether an editor drag
/// is live, and a closed bar routes nothing.
#[test]
fn search_bar_blocks_new_reports_but_preserves_captured_drags() {
    use super::search_bar_blocks_mouse_report as blocks;
    for pressed in [false, true] {
        for motion in [false, true] {
            for held in [false, true] {
                assert!(!blocks(false, pressed, motion, held));
            }
        }
    }
    assert!(blocks(true, true, true, false));
    assert!(blocks(true, true, false, false));
    assert!(blocks(true, true, false, true));
    assert!(!blocks(true, true, true, true));
    assert!(!blocks(true, false, false, true));
    assert!(!blocks(true, false, false, false));
}

/// The bar shows the last result while a new search is only waiting for
/// the typing pause or has run briefly; a scan that is really slow says so.
#[test]
fn searching_is_shown_only_once_a_scan_is_actually_slow() {
    use super::displayed_search_status;
    use kettle_render::SearchStatus;
    let t0 = std::time::Instant::now();
    let ms = std::time::Duration::from_millis;
    let mut search = crate::search_input::SearchState {
        editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
        status: SearchStatus::NoMatch,
        ..Default::default()
    };
    search.note_edit(t0);
    assert_eq!(
        displayed_search_status(&search, t0 + ms(450)),
        SearchStatus::NoMatch
    );
    // The full scan starts when the pause ends and clears its deadline,
    // as `update_search` does. The grace runs 200 ms from there.
    search.unlimited_retry_at = None;
    assert_eq!(
        displayed_search_status(&search, t0 + ms(650)),
        SearchStatus::NoMatch
    );
    assert_eq!(
        displayed_search_status(&search, t0 + ms(750)),
        SearchStatus::Searching
    );

    // Finished searches are shown as automation sees them.
    search.status = SearchStatus::Match;
    search.visible_truncated = true;
    assert_eq!(
        displayed_search_status(&search, t0 + ms(10)),
        SearchStatus::Limited
    );
}

/// Turning Wrap off while a step has already wrapped stops it at the edge
/// instead of landing on a match past it.
#[test]
fn turning_wrap_off_stops_a_step_that_already_wrapped() {
    use kettle_core::{SearchDirection, SearchPoint};
    let job = |direction, wrapped| crate::search_input::BackgroundSearch {
        token: kettle_core::SearchScanToken {
            query_revision: 1,
            output_generation: 1,
            layout: kettle_core::SearchLayout {
                columns: 80,
                screen_lines: 24,
                history_size: 10_000,
            },
        },
        direction,
        cursor: SearchPoint::new(0, 0),
        edge: SearchPoint::new(10, 79),
        wrap_anchor: SearchPoint::new(5, 0),
        wrapped,
        nearby: false,
        navigation: true,
        had_focus: true,
        output_drifted: false,
    };
    let span = kettle_core::SearchSpan {
        start: SearchPoint::new(1, 0),
        end: SearchPoint::new(1, 1),
    };
    for (direction, status) in [
        (SearchDirection::Forward, kettle_render::SearchStatus::End),
        (SearchDirection::Reverse, kettle_render::SearchStatus::Start),
    ] {
        let mut search = crate::search_input::SearchState {
            editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
            wrap: false,
            focused: Some(span),
            status: kettle_render::SearchStatus::Searching,
            background: Some(job(direction, true)),
            ..Default::default()
        };
        super::search_wrap_toggled(&mut search, std::time::Instant::now());
        assert!(search.background.is_none());
        assert_eq!(search.status, status);
    }
    // A step that never had a match reports no match, and one that output
    // drifted under stays limited, as they would at the edge.
    for (had_focus, output_drifted, status) in [
        (false, false, kettle_render::SearchStatus::NoMatch),
        (true, true, kettle_render::SearchStatus::Limited),
    ] {
        let mut search = crate::search_input::SearchState {
            editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
            wrap: false,
            status: kettle_render::SearchStatus::Searching,
            background: Some(crate::search_input::BackgroundSearch {
                had_focus,
                output_drifted,
                ..job(SearchDirection::Forward, true)
            }),
            ..Default::default()
        };
        super::search_wrap_toggled(&mut search, std::time::Instant::now());
        assert_eq!(
            search.status, status,
            "had_focus={had_focus} drifted={output_drifted}"
        );
    }
    // A step that has not wrapped yet keeps going.
    let mut search = crate::search_input::SearchState {
        editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
        wrap: false,
        focused: Some(span),
        status: kettle_render::SearchStatus::Searching,
        background: Some(job(SearchDirection::Forward, false)),
        ..Default::default()
    };
    super::search_wrap_toggled(&mut search, std::time::Instant::now());
    assert!(search.background.is_some());
}

/// Shortcuts that would type into the terminal do nothing while the bar
/// has the keyboard; Copy and Paste act on the query.
#[test]
fn shortcuts_that_type_into_the_terminal_do_nothing_while_searching() {
    use super::{SearchBarShortcut as K, search_bar_shortcut_kind as kind};
    use kettle_config::keybinds::Action;
    assert_eq!(kind(&Action::SendText("\x15".into())), K::Typing);
    assert_eq!(kind(&Action::SendNewline), K::Typing);
    assert_eq!(kind(&Action::InsertPaneNumber), K::Typing);
    assert_eq!(kind(&Action::PastePrimary), K::Typing);
    assert_eq!(kind(&Action::Copy), K::Copy);
    assert_eq!(kind(&Action::Paste), K::Paste);
    for action in [
        Action::NewTab,
        Action::CloseTab,
        Action::IncreaseFontSize,
        Action::ScrollPageUp,
        Action::FocusLeft,
        Action::ToggleZoom,
        Action::CommandPalette,
        Action::GotoTab(0),
    ] {
        assert_eq!(kind(&action), K::Run, "{action:?}");
    }
}

/// Both keyboard paths hand a key the bar did not use to the shortcuts.
#[test]
fn keys_the_search_bar_does_not_use_reach_the_shortcuts() {
    let src = super::production_source();
    let native = src
        .split("if ws.search.open {\n                    if event.repeat")
        .nth(1)
        .and_then(|rest| {
            rest.split("remember_closing_key(ws, key_modal, event.physical_key);")
                .next()
        })
        .expect("native search key branch");
    assert!(
        native.contains("if !self.search_key(")
            && native.contains("Some(&event.key_without_modifiers()),")
            && native.contains("self.search_bar_shortcut("),
        "a native key the bar does not use must reach the shortcuts"
    );
    let ctl = src
        .split("TextModal::Search => {")
        .nth(1)
        .and_then(|rest| rest.split("}\n            }").next())
        .expect("control-plane search key branch");
    assert!(
        ctl.contains("if !self.search_key(ws, key, None, text, None) {")
            && ctl.contains("self.search_bar_shortcut(ws, key, None, false, event_loop, None);"),
        "a dispatched key the bar does not use must reach the shortcuts"
    );
}

/// A focused toggle or Close acts once per press; Previous, Next and
/// the query keep repeating.
#[test]
fn held_space_or_enter_presses_a_search_button_once() {
    use kettle_render::SearchControl as C;
    use winit::keyboard::{Key, NamedKey};
    let space = Key::Named(NamedKey::Space);
    let enter = Key::Named(NamedKey::Enter);
    let typed_space = Key::Character(" ".into());
    for control in [C::Wrap, C::Case, C::Invert, C::Close] {
        for key in [&space, &enter, &typed_space] {
            assert!(
                super::search_button_ignores_repeat(control, key),
                "{control:?} {key:?}"
            );
        }
    }
    for control in [C::Editor, C::Previous, C::Next] {
        assert!(
            !super::search_button_ignores_repeat(control, &enter),
            "{control:?}"
        );
    }
    assert!(!super::search_button_ignores_repeat(
        C::Wrap,
        &Key::Named(NamedKey::Backspace)
    ));
}

/// Holding the key that closed the bar sends nothing to the terminal
/// until it is released; the next press is the terminal's.
#[test]
fn the_key_that_closed_search_does_not_repeat_into_the_terminal() {
    use winit::event::ElementState;
    use winit::keyboard::PhysicalKey;
    let space = PhysicalKey::Code(winit::keyboard::KeyCode::Space);
    let other = PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
    let mut closing = std::collections::HashSet::from([space]);
    let pressed = ElementState::Pressed;
    assert!(super::drop_closing_key_repeat(
        &mut closing,
        space,
        pressed,
        true
    ));
    assert!(!super::drop_closing_key_repeat(
        &mut closing,
        other,
        pressed,
        true
    ));
    assert!(closing.contains(&space));
    assert!(!super::drop_closing_key_repeat(
        &mut closing,
        space,
        ElementState::Released,
        false
    ));
    assert!(closing.is_empty());
    assert!(!super::drop_closing_key_repeat(
        &mut closing,
        space,
        pressed,
        true
    ));
}

/// Toggling Wrap with a match on screen keeps the match and clears a step
/// status that described the old setting.
#[test]
fn wrap_toggle_keeps_the_match_and_clears_the_old_step_status() {
    use kettle_render::SearchStatus as S;
    let span = kettle_core::SearchSpan {
        start: kettle_core::SearchPoint::new(3, 0),
        end: kettle_core::SearchPoint::new(3, 4),
    };
    for (before, after) in [
        (S::End, S::Match),
        (S::Start, S::Match),
        (S::Wrapped, S::Match),
        (S::Limited, S::Limited),
    ] {
        let mut search = crate::search_input::SearchState {
            editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
            focused: Some(span),
            status: before,
            ..Default::default()
        };
        super::search_wrap_toggled(&mut search, std::time::Instant::now());
        assert_eq!(search.focused, Some(span));
        assert_eq!(search.status, after, "{before:?}");
    }
    let mut search = crate::search_input::SearchState {
        editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
        status: S::NoMatch,
        ..Default::default()
    };
    super::search_wrap_toggled(&mut search, std::time::Instant::now());
    assert_eq!(search.status, S::Searching, "no match: search again");
}

/// A toggle restarts the search from the match on screen, or from the
/// viewport when that match has been scrolled away.
#[test]
fn a_toggle_restarts_search_from_what_is_on_screen() {
    use kettle_core::SearchPoint as P;
    let span = kettle_core::SearchSpan {
        start: P::new(-30, 2),
        end: P::new(-30, 6),
    };
    // 24 lines scrolled back 40: lines -40 ..= -17 are on screen.
    let viewport = Some((40, 24, 79));
    assert_eq!(
        super::search_anchor_for(Some(span), false, viewport),
        Some(P::new(-30, 2))
    );
    assert_eq!(
        super::search_anchor_for(Some(span), true, viewport),
        Some(P::new(-30, 6))
    );
    // Back at the bottom, the match is off screen.
    let bottom = Some((0, 24, 79));
    assert_eq!(
        super::search_anchor_for(Some(span), false, bottom),
        Some(P::new(0, 0))
    );
    assert_eq!(
        super::search_anchor_for(Some(span), true, bottom),
        Some(P::new(23, 79))
    );
    // With no pane to read, the match is still the best guess.
    assert_eq!(
        super::search_anchor_for(Some(span), false, None),
        Some(P::new(-30, 2))
    );
    assert_eq!(super::search_anchor_for(None, false, None), None);
}

#[test]
fn search_pointer_route_keeps_the_grid_live_under_the_bar() {
    use super::{SearchPointerRoute, search_pointer_route};

    for press in [None, Some(true), Some(false)] {
        for dragging in [false, true] {
            assert_eq!(
                search_pointer_route(false, press, dragging),
                SearchPointerRoute::Closed,
                "a closed bar routes nothing (press={press:?}, dragging={dragging})"
            );
        }
    }
    assert_eq!(
        search_pointer_route(true, Some(true), false),
        SearchPointerRoute::Bar,
        "a press inside the lane belongs to the bar's controls"
    );
    assert_eq!(
        search_pointer_route(true, Some(false), false),
        SearchPointerRoute::Grid,
        "a press above the lane is ordinary grid input"
    );
    assert_eq!(
        search_pointer_route(true, Some(false), true),
        SearchPointerRoute::Grid,
        "a fresh press decides by position even if a stale editor drag flag survived"
    );
    assert_eq!(
        search_pointer_route(true, None, true),
        SearchPointerRoute::Bar,
        "motion and release follow a live editor drag"
    );
    assert_eq!(
        search_pointer_route(true, None, false),
        SearchPointerRoute::Grid,
        "motion and release without an editor drag belong to the grid gesture"
    );
}

/// Source pins for the routing above: every pointer arm, native and
/// control-plane, must consult the same helper, the pointer gate must not
/// treat the bar as a pointer-owning modal, the cursor icon must follow
/// the pointer gate (I-beam over the grid while searching), and the
/// right-click menu must leave the bar in place.
#[test]
fn search_bar_pointer_routing_is_wired_on_both_input_paths() {
    let src = production_source();
    let native_press = src
        .split("WindowEvent::MouseInput {\n                state: ElementState::Pressed,")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::MouseInput {").next())
        .expect("native Pressed arm");
    let native_release = src
        .split("WindowEvent::MouseInput {\n                state: ElementState::Released,")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native Released arm");
    let native_move = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native CursorMoved arm");
    let ctl_press = src
        .split("fn ctl_mouse_press(")
        .nth(1)
        .and_then(|body| body.split("fn ctl_mouse_release").next())
        .expect("ctl_mouse_press body");
    let ctl_release = src
        .split("fn ctl_mouse_release(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("ctl_mouse_release body");
    let ctl_move = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("ctl_mouse_move body");

    for (label, body) in [("native press", native_press), ("ctl press", ctl_press)] {
        assert!(
            body.contains("Some(self.cursor_in_search_bar(ws)),")
                && body.contains("== SearchPointerRoute::Bar"),
            "{label} must route a search-time press by the bar's rectangle"
        );
        assert!(
            !body.contains("if ws.search.open {"),
            "{label} must not swallow every press while the bar is open"
        );
    }
    for (label, body) in [
        ("native move", native_move),
        ("ctl move", ctl_move),
        ("native release", native_release),
        ("ctl release", ctl_release),
    ] {
        assert!(
            body.contains(
                "search_pointer_route(ws.search.open, None, ws.search.pointer_captured())"
            ),
            "{label} must route search-time motion/release by the live bar gesture"
        );
        assert!(
            !body.contains("if ws.search.open {"),
            "{label} must not swallow every event while the bar is open"
        );
    }
    assert!(
        native_press.contains(
            "if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some())"
        ),
        "the native press gate must ignore the search bar"
    );
    let normalized = src.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains(
            "let chrome = chrome_cursor_icon( self.cursor_in_chrome_band(ws), self.pointer_modal_open(ws), );"
        ) || normalized.contains(
            "let chrome = chrome_cursor_icon(self.cursor_in_chrome_band(ws), self.pointer_modal_open(ws));"
        ),
        "the cursor icon over the grid must not turn into the arrow while searching"
    );
    let open_menu = src
        .split("fn open_context_menu(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("open_context_menu body");
    assert!(
        open_menu.contains("self.close_modals_for_context_menu(ws);")
            && !open_menu.contains("self.close_all_modals(ws);"),
        "the right-click menu must leave an open search bar in place"
    );
    let note_focus = src
        .split("fn note_focus_change(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("note_focus_change body");
    assert!(
        note_focus.contains("self.retarget_search_to_focus(ws);"),
        "an open search bar must follow pane focus"
    );
    let begin_selection = src
        .split("fn begin_or_extend_mouse_selection(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("begin_or_extend_mouse_selection body");
    assert!(
        begin_selection.contains("ws.search.editor.clear_selection();"),
        "a grid press must drop the editor selection so Copy reaches the grid"
    );
    // Keyboard, file drops, and focus-follows-mouse keep the full gate:
    // search does own the keyboard.
    assert!(
        src.contains(
            "if self.any_modal_open(ws) {\n                    return;\n                }"
        ),
        "DroppedFile must still return while any modal, including search, is open"
    );
    assert!(
        native_move.contains("&& !self.any_modal_open(ws)"),
        "focus-follows-mouse must stay off while the bar is open, or the bar would retarget as the pointer drifts"
    );
}

/// An open bar follows focus: the query and toggles come along, the pane
/// id changes, and a non-empty query starts searching the new pane at once.
#[test]
fn search_follows_focus_with_its_query_and_toggles() {
    use super::{SearchCarryOver, fresh_search_state, search_retarget};

    assert_eq!(
        search_retarget(Some(1), Some(1)),
        None,
        "already on the focused pane"
    );
    assert_eq!(search_retarget(Some(1), None), None, "nothing focused");
    assert_eq!(search_retarget(None, Some(2)), Some(2));
    assert_eq!(search_retarget(Some(1), Some(2)), Some(2));

    let now = std::time::Instant::now();
    let carried = SearchCarryOver {
        query: "needle".into(),
        wrap: false,
        case_mode: kettle_config::SearchCaseSensitivity::Always,
        invert: true,
        focused_control: kettle_render::SearchControl::Next,
    };
    let moved = fresh_search_state(2, carried.clone(), 7, now);
    assert!(moved.open);
    assert_eq!(moved.target_pane, Some(2));
    assert_eq!(moved.query(), "needle");
    assert!(!moved.wrap);
    assert_eq!(
        moved.case_mode,
        kettle_config::SearchCaseSensitivity::Always
    );
    assert!(moved.invert);
    assert_eq!(moved.focused_control, kettle_render::SearchControl::Next);
    assert_eq!(moved.pre_open_display_offset, Some(7));
    assert_eq!(
        moved.status,
        kettle_render::SearchStatus::Searching,
        "a carried query scans the new pane immediately"
    );
    assert!(
        moved.anchor.is_none(),
        "the anchor is derived after the lane settles"
    );
    assert!(moved.focused.is_none() && moved.visible.is_empty());

    let fresh = fresh_search_state(
        3,
        SearchCarryOver {
            query: String::new(),
            ..carried
        },
        0,
        now,
    );
    assert_eq!(fresh.status, kettle_render::SearchStatus::Typing);
    assert_eq!(fresh.target_pane, Some(3));
}

#[test]
fn non_text_modals_block_control_plane_typing_into_search() {
    use super::{ContextMenuState, TextModal, ViState, open_text_modal};

    let mut ws = WindowState::new(1, Mux::new());
    ws.search.open = true;
    assert_eq!(open_text_modal(&ws), Some(TextModal::Search));
    ws.vi_mode = Some(ViState {
        pane_id: 1,
        visual: false,
    });
    assert_eq!(
        open_text_modal(&ws),
        None,
        "vi owns the keyboard above search"
    );
    ws.context_menu = Some(ContextMenuState {
        anchor: (0.0, 0.0),
        anchor_scale: 1.0,
        items: Vec::new(),
        highlight: 0,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        typeahead_buf: String::new(),
        typeahead_until: None,
    });
    assert_eq!(
        open_text_modal(&ws),
        None,
        "the menu owns the keyboard above vi"
    );
    ws.vi_mode = None;
    assert_eq!(
        open_text_modal(&ws),
        None,
        "the menu also blocks search alone"
    );
    ws.context_menu = None;
    assert_eq!(open_text_modal(&ws), Some(TextModal::Search));
}

#[test]
fn grid_selection_gestures_clear_the_search_editor_selection() {
    use super::{Mux, WindowState, arm_selection_gesture};
    use crate::search_input::SearchEditor;

    let mut ws = WindowState::new(1, Mux::new());
    ws.search.open = true;
    ws.search.editor = SearchEditor::from_text("query".into(), 100);
    for button in [0, 2] {
        ws.search.editor.select_all();
        assert_eq!(ws.search.editor.selected_text(), Some("query"));
        arm_selection_gesture(&mut ws, 42, button);
        assert_eq!(ws.search.editor.selected_text(), None);
        assert_eq!(ws.search.query(), "query");
    }
}

/// A bottom bar sits above the search bar and a bottom status bar; the
/// docking band must be where the bar is painted, not at the window edge.
#[test]
fn bottom_dock_band_follows_search_and_bottom_status_chrome() {
    use super::{TabBarPos, dist_to_rect, tab_band_rect, tear_lift_ratio};
    for search_height in [0.0, 26.0, 78.0] {
        for bottom_status_height in [0.0, 22.0] {
            let band = tab_band_rect(
                (800, 600),
                TabBarPos::Bottom,
                24.0,
                180.0,
                search_height,
                bottom_status_height,
            );
            let visible_center = 600.0 - search_height - bottom_status_height - 12.0;
            assert_eq!(dist_to_rect(400.0, visible_center, band), 0.0);
            assert_eq!(tear_lift_ratio(400.0, visible_center, band, 36.0), 0.0);
            if search_height + bottom_status_height >= 24.0 {
                assert!(dist_to_rect(400.0, 588.0, band) > 0.0);
            }
        }
    }
}

/// Side strips stop above the search bar; the top band is unaffected.
#[test]
fn side_dock_bands_exclude_search_without_moving_the_top_band() {
    use super::{TabBarPos, dist_to_rect, tab_band_rect};
    for pos in [TabBarPos::Left, TabBarPos::Right] {
        let band = tab_band_rect((800, 600), pos, 24.0, 180.0, 78.0, 22.0);
        let x = if matches!(pos, TabBarPos::Left) {
            90.0
        } else {
            710.0
        };
        assert_eq!(dist_to_rect(x, 510.0, band), 0.0);
        assert!(dist_to_rect(x, 560.0, band) > 0.0);
        assert_eq!(band.3, 522.0);
    }
    assert_eq!(
        tab_band_rect((800, 600), TabBarPos::Top, 24.0, 180.0, 78.0, 22.0),
        (0.0, 0.0, 800.0, 24.0),
    );
}

#[test]
fn pending_run_orphan_check_searches_all_windows() {
    let src = production_source();
    let check = src
        .split("fn check_pending_run_deadlines(&mut self, ws: &mut WindowState) {")
        .nth(1)
        .and_then(|s| s.split("fn ctl_capture_output_since").next())
        .expect("check_pending_run_deadlines present");
    assert!(
        check.contains("Self::ctl_pane_ref(ws, &self.windows, *pane).is_none()"),
        "pending run orphan detection must search the checked-out window \
             and the other windows, not only the current ws.mux"
    );
    assert!(
        !check.contains("!ws.mux.panes.contains_key(pane)"),
        "current-window-only orphan checks kill pending runs in other windows"
    );
}

#[test]
fn status_bar_cursor_tracks_tab_and_search_offsets() {
    use kettle_config::StatusBarMode::{Bottom, Top};
    assert!(
        super::cursor_in_status_bar_band(30.0, 22.0, 600.0, Top, 24.0, 0.0),
        "the top status strip begins below the tab bar"
    );
    assert!(!super::cursor_in_status_bar_band(
        10.0, 22.0, 600.0, Top, 24.0, 0.0
    ));
    assert!(super::cursor_in_status_bar_band(
        550.0, 22.0, 600.0, Bottom, 24.0, 30.0
    ));
    assert!(!super::cursor_in_status_bar_band(
        590.0, 22.0, 600.0, Bottom, 24.0, 30.0
    ));
    assert_eq!(super::status_bar_y(600.0, 22.0, Top, 24.0, 30.0), 24.0);
    assert_eq!(super::status_bar_y(600.0, 22.0, Bottom, 24.0, 30.0), 548.0);
}

#[test]
fn finding_remote_new_tab_reconciles_search() {
    let source = include_str!("automation.rs");
    let branch = source
        .split("PendingRemoteCommand::NewTab { .. } => {")
        .nth(1)
        .unwrap()
        .split("PendingRemoteCommand::SendText")
        .next()
        .unwrap();
    assert!(
        branch.contains("let pre = self.focus_key(ws);"),
        "remote new-tab must capture the previous focus"
    );
    assert!(
        branch.contains("self.note_focus_change(ws, pre);"),
        "remote new-tab must retarget open search"
    );
}

#[test]
fn finding_query_edit_refreshes_viewport_anchor() {
    let mut search = crate::search_input::SearchState {
        anchor: Some(kettle_core::SearchPoint { line: 0, column: 0 }),
        ..Default::default()
    };
    search.note_edit(std::time::Instant::now());
    assert!(
        search.anchor.is_none(),
        "query edits must discard the old viewport anchor"
    );
}

#[test]
fn finding_tear_off_retargets_source_search() {
    let body = finding_body(include_str!("windowing.rs"), "maybe_tear_off");
    assert!(
        body.contains("self.note_focus_change(ws, pre_focus);"),
        "successful tear-off must retarget source search"
    );
}

#[test]
fn review_search_projection_uses_natural_text_width() {
    let src = production_source();
    let overlay = finding_body(&src, "overlay");
    let scroll = overlay
        .split("let search_horizontal_scroll =")
        .nth(1)
        .unwrap();
    assert!(scroll.contains("renderer.overlay_text_cell_width()"));
    assert!(!scroll.contains("geometry.editor.2, renderer.cell_w"));
    let ime = finding_body(&src, "update_ime_cursor_area");
    let search = ime
        .split("let search_cursor =")
        .nth(1)
        .unwrap()
        .split("let (x, y) =")
        .next()
        .unwrap();
    assert!(search.contains("self.overlay_text_cell_width(ws)"));

    let natural_width = 10.0;
    let terminal_width = natural_width * 0.5;
    let geometry = kettle_render::search_bar_geometry(800.0, 600.0, natural_width, 20.0, &EN_TEXT);
    let columns =
        kettle_render::search_bar_columns(geometry.editor.2, natural_width).saturating_sub(2);
    let mut editor = crate::search_input::SearchEditor::default();
    editor.insert(&"x".repeat(columns + 6), 1024);
    editor.ensure_cursor_visible(columns);
    let scroll = crate::search_input::SearchEditor::visible_scroll_for(
        editor.text(),
        editor.cursor(),
        editor.horizontal_scroll(),
        columns,
    );
    assert!(scroll > 0);
    assert_eq!(scroll, editor.horizontal_scroll());
    let wrong_columns =
        kettle_render::search_bar_columns(geometry.editor.2, terminal_width).saturating_sub(2);
    assert_eq!(
        crate::search_input::SearchEditor::visible_scroll_for(
            editor.text(),
            editor.cursor(),
            scroll,
            wrong_columns,
        ),
        0,
        "terminal width reproduces the tail being clipped"
    );
}

