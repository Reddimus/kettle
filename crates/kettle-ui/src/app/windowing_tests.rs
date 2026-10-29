#[test]
fn bulk_group_edits_keep_pinned_tab_and_live_window_semantics() {
    let (mut mux, first, second) = title_test_split_mux();
    let cfg = title_test_config();
    let argv = title_test_argv();
    let waker: kettle_core::Waker = Arc::new(|| {});
    mux.new_tab_with(&cfg, 80, 24, 8, 16, waker, &argv, None)
        .expect("spawn other group tab");
    let other = mux.active_focus().expect("other tab pane");

    assert!(mux.focus_pane(first));
    let mut tab_edit = begin_title_edit(&mux, TitleEditScope::Group, GroupBulkScope::Tab, "window")
        .expect("tab group edit opens");
    tab_edit.input = "tab-fleet".into();
    assert!(mux.focus_pane(other), "focus drifted to another tab");
    assert!(apply_mux_title_edit(&mut mux, &tab_edit));
    assert_eq!(mux.panes[&first].group_name.as_deref(), Some("tab-fleet"));
    assert_eq!(mux.panes[&second].group_name.as_deref(), Some("tab-fleet"));
    assert_eq!(mux.panes[&other].group_name, None);

    let mut window_edit = begin_title_edit(
        &mux,
        TitleEditScope::Group,
        GroupBulkScope::Window,
        "window",
    )
    .expect("window group edit opens");
    window_edit.input = "window-fleet".into();
    assert!(apply_mux_title_edit(&mut mux, &window_edit));
    assert!(
        mux.panes
            .values()
            .all(|pane| pane.group_name.as_deref() == Some("window-fleet"))
    );

    window_edit
        .input
        .replace_all("", crate::modal_input::MAX_MODAL_INPUT_BYTES);
    assert!(apply_mux_title_edit(&mut mux, &window_edit));
    assert!(
        mux.panes.values().all(|pane| pane.group_name.is_none()),
        "an empty GroupWindow edit still clears every live pane"
    );
}

/// The early reveal exists to buy back renderer-init time, but only where
/// the pre-paint frame is indistinguishable from what will be painted.
/// A light theme must keep the old hide-until-painted behaviour: revealing
/// it early trades a late window for a BLACK flash, which is the same bad
/// deal the policy was written to avoid.
#[test]
fn only_a_background_indistinguishable_from_black_reveals_early() {
    use kettle_config::Rgb;
    let dark = [
        (Rgb { r: 0, g: 0, b: 0 }, "pure black"),
        (
            Rgb {
                r: 16,
                g: 16,
                b: 16,
            },
            "#101010, the benchmark profile",
        ),
        (
            Rgb {
                r: 24,
                g: 24,
                b: 24,
            },
            "the threshold itself",
        ),
    ];
    for (bg, what) in dark {
        assert_eq!(
            background_is_dark_enough_to_reveal_early(bg),
            cfg!(target_os = "windows"),
            "{what} is imperceptible against black, so it reveals early on                  Windows -- and nowhere else, because that is the only platform                  where the pre-paint frame was measured to be black"
        );
    }
    let keep_hiding = [
        (
            Rgb {
                r: 240,
                g: 232,
                b: 216,
            },
            "a light/solarized background",
        ),
        (
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
            "pure white",
        ),
        (
            Rgb {
                r: 25,
                g: 24,
                b: 24,
            },
            "one level past the threshold",
        ),
        (
            Rgb {
                r: 16,
                g: 16,
                b: 120,
            },
            "dark on two channels, blue on one",
        ),
        (
            Rgb {
                r: 40,
                g: 44,
                b: 52,
            },
            "a typical dark-but-visible theme",
        ),
    ];
    for (bg, what) in keep_hiding {
        assert!(
            !background_is_dark_enough_to_reveal_early(bg),
            "{what} would flash black; it must wait for the first paint"
        );
    }
}

/// Window changes that move or invalidate the patch hide the layer at the
/// event, without waiting for a frame that may retry: a new size (the
/// Metal layer stretches its last frame but not the patch), a new scale,
/// focus loss and occlusion. A Resized that keeps the surface size, or
/// a degenerate one, leaves the blink alone.
#[test]
fn window_changes_hide_the_layer_without_a_frame() {
    let src = super::production_source();
    let arm = |head: &str, end: &str| -> String {
        src.split_once(head)
            .unwrap_or_else(|| panic!("{head}"))
            .1
            .split_once(end)
            .unwrap_or_else(|| panic!("{end}"))
            .0
            .to_string()
    };
    let resized = arm(
        "WindowEvent::Resized(size) => {",
        "WindowEvent::ScaleFactorChanged",
    );
    let degenerate = resized
        .find("ws.dpi_resize_surface_suspended = true;")
        .expect("degenerate return");
    let changed = resized
        .find(".is_none_or(|r| r.surface_size() != (size.width, size.height))")
        .expect("size comparison");
    let hide = resized
        .find("Self::hide_cursor_layer(ws);")
        .expect("resize hides the layer");
    let layout = resized
        .find("ws.dpi_resize.on_event(")
        .expect("resize coalescer");
    assert!(degenerate < changed && changed < hide && hide < layout);
    let scale = arm(
        "WindowEvent::ScaleFactorChanged { scale_factor, .. } => {",
        "WindowEvent::ThemeChanged",
    );
    assert!(
        scale
            .find("Self::hide_cursor_layer(ws);")
            .expect("scale hides")
            < scale
                .find("ws.dpi_resize.on_event(")
                .expect("scale coalescer")
    );
    let focus = arm("WindowEvent::Focused(f) => {", "rec.record_marker(");
    assert!(
        focus
            .split_once("if !f {")
            .expect("focus loss")
            .1
            .contains("Self::hide_cursor_layer(ws);")
    );
    let occluded = arm("WindowEvent::Occluded(occluded) => {", "if !occluded {");
    assert!(occluded.contains("if occluded {\n                    Self::hide_cursor_layer(ws);"));
}

/// The App announces those panes wherever they go: after both reaps,
/// before every `tab_close`, and for all panes when a window closes.
#[test]
fn every_pane_teardown_fires_pane_close() {
    let src = production_source();
    for after_reap in src.split("let reaped = ws.mux.reap();").skip(1) {
        let flat: String = after_reap.split_whitespace().collect();
        assert!(
            flat.starts_with(
                "ifreaped.layout_changed{ws.dragging_split=None;}self.fire_closed_pane_events(ws);"
            ),
            "a reap must announce the panes it removed"
        );
    }
    let tab_close = include_str!("automation.rs")
        .split("fn fire_tab_close_event(")
        .nth(1)
        .unwrap()
        .split("\n    }")
        .next()
        .unwrap();
    assert!(tab_close.contains("self.fire_closed_pane_events(ws);"));
    let window_close = include_str!("windowing.rs")
        .split("fn close_window_now(")
        .nth(1)
        .unwrap()
        .split("ws.mux.close_window();")
        .next()
        .unwrap();
    assert!(window_close.contains("self.fire_pane_close_event(ws, pane_id);"));
}

/// A torn-off window keeps the source window's font zoom: the drag anchor
/// is placed with the source's tab- and status-bar metrics, so a window
/// opened at the configured size put the pointer off its tab strip.
#[test]
fn torn_off_windows_keep_the_source_font_size() {
    let app = concat!(include_str!("window_events.rs"), include_str!("actions.rs"));
    let windowing = include_str!("windowing.rs");
    let carries = "font_size: ws.renderer.as_ref().map(|r| r.font_size()),";
    assert_eq!(
        app.matches(carries).count() + windowing.matches(carries).count(),
        3,
        "every non-Wayland tear-off passes its font size"
    );
    assert!(windowing.contains("let font_size = ws.renderer.as_ref().map(|r| r.font_size());"));
    let open = windowing
        .split("pub(super) fn open_window(")
        .nth(1)
        .unwrap()
        .split("let area = self.area(&ws);")
        .next()
        .unwrap();
    assert!(
        open.contains("renderer.set_font_size(*font_size);"),
        "the font size applies before the grid is sized"
    );
}

/// Census: a tab that leaves its window takes the reported pane with it,
/// so every function that detaches a tab releases the report through the
/// detached tab before handing it on.
#[test]
fn every_tab_transfer_releases_the_focus_report() {
    let source = production_source();
    let mut transfers = 0;
    for chunk in source.split("\n    fn ").skip(1) {
        let name = chunk.split('(').next().unwrap_or_default().to_string();
        let body = chunk.split("\n    }\n").next().unwrap_or(chunk);
        if !body.contains(".mux.detach_tab(") {
            continue;
        }
        transfers += 1;
        let release = body.find("self.release_focus_report_to(");
        let handoff = ["WindowOpen::AdoptTab {", "self.dock_tab_into("]
            .iter()
            .filter_map(|needle| body.find(needle))
            .min()
            .expect("a detached tab is handed on");
        assert!(
            release.is_some_and(|release| release < handoff),
            "{name} hands a tab on without releasing its focus report"
        );
    }
    assert!(transfers >= 4, "census found only {transfers} transfers");
    // A target window docked into outside the dispatch funnel is
    // reconciled before it goes back into the window map.
    let docks = source
        .matches("self.dock_tab_into(&mut target, dt, paste, idx, td.seq);")
        .count();
    let reconciled = source
        .matches(
            "self.dock_tab_into(&mut target, dt, paste, idx, td.seq);\n            // The docked tab becomes the target's active tab; report it now,\n            // since an already focused target gets no new focus event.\n            self.sync_pane_focus_reports(&mut target);",
        )
        .count()
        + source
            .matches(
                "self.dock_tab_into(&mut target, dt, paste, idx, td.seq);\n                self.sync_pane_focus_reports(&mut target);",
            )
            .count();
    assert_eq!(docks, 2, "both cross-window docks are counted");
    assert_eq!(docks, reconciled, "every docked target is reconciled");
}

#[test]
fn docking_retargets_pane_owned_focus_state() {
    let src = production_source();
    let dock = src
        .split_once("fn dock_tab_into(")
        .unwrap()
        .1
        .split_once("\n    }")
        .unwrap()
        .0;
    let before = dock
        .find("let pre_focus = self.focus_key(target);")
        .unwrap();
    let attach = dock.find("target.mux.attach_tab(").unwrap();
    let notify = dock
        .find("self.note_focus_change(target, pre_focus);")
        .unwrap();
    assert!(before < attach && attach < notify);
    assert!(notify < dock.find("self.resize_all(target);").unwrap());
}

#[test]
fn a_pending_wayland_drop_cannot_follow_changed_focus_or_broadcast() {
    let mut ws = WindowState::new(1, Mux::new());
    let target = super::WaylandDropTarget::capture(&ws);
    assert!(target == super::WaylandDropTarget::capture(&ws));
    ws.ime_focus_generation += 1;
    assert!(
        target != super::WaylandDropTarget::capture(&ws),
        "modal opened and closed"
    );
    ws.ime_focus_generation -= 1;
    ws.mux.broadcast = crate::mux::BroadcastScope::All;
    assert!(target != super::WaylandDropTarget::capture(&ws));
    ws.mux.broadcast = crate::mux::BroadcastScope::Off;
    ws.preview_focus = Some(crate::media::PreviewFocus { pane: 7, item: 8 });
    assert!(target != super::WaylandDropTarget::capture(&ws));
}

/// Peacock drift guard: the live-dedupe pool walk. Same project →
/// same starting slot; a collision with a live window advances to the
/// next free hue; a fully-claimed pool accepts the seed slot.
#[test]
fn accent_slot_walk_dedupes_live_windows() {
    let pool: Vec<kettle_config::Rgb> =
        (0u8..4).map(|i| kettle_config::Rgb::new(i, i, i)).collect();
    // Empty in-use → the seed's own slot.
    assert_eq!(super::pick_accent_slot(&pool, 1, &[]), 1);
    assert_eq!(
        super::pick_accent_slot(&pool, 5, &[]),
        1,
        "seed wraps modulo"
    );
    // Seed slot taken → next free.
    assert_eq!(super::pick_accent_slot(&pool, 1, &[pool[1]]), 2);
    // Walk wraps past the end.
    assert_eq!(super::pick_accent_slot(&pool, 3, &[pool[3], pool[0]]), 1);
    // Fully claimed → fall back to the seed slot (accept the collision).
    assert_eq!(
        super::pick_accent_slot(&pool, 2, &[pool[0], pool[1], pool[2], pool[3]]),
        2
    );
    // Degenerate empty pool.
    assert_eq!(super::pick_accent_slot(&[], 7, &[]), 0);
    // Presence wire format round-trips through Rgb::parse.
    let c = kettle_config::Rgb::new(0xcb, 0xa6, 0xf7);
    assert_eq!(
        kettle_config::Rgb::parse(&crate::window_accent::rgb_hex(c)),
        Some(c)
    );
}

/// Multi-window drift guard. A window close must NOT exit the event loop
/// directly. It requests close for its exact window id, and the single
/// funnel (`finish_window_dispatch`) drops the window, exiting only when
/// the windows map is empty. The only legitimate direct exits are the
/// funnel itself (close + quit arms) and `resumed_inner`'s window-1
/// startup failures (window create / renderer init / `-e` spawn / shell
/// spawn), where no other window can exist yet. A new `event_loop.exit()`
/// anywhere else makes closing one window kill them all.
#[test]
fn event_loop_exit_sites_are_allowlisted() {
    let src = production_source();
    // concat! so this test's own literals don't self-match the scan.
    let exit_needle = concat!("event_loop", ".exit();");
    let n_exits = src.matches(exit_needle).count();
    assert_eq!(
        n_exits, 7,
        "expected exactly 7 event_loop.exit() sites (2 in \
             finish_window_dispatch + 5 resumed_inner startup failures, one of \
             them the first pane failing to start before the window); a new \
             one must route through request_window_close instead"
    );
    // The close paths all flag instead of exiting: keybind CloseTab /
    // ClosePane / CloseWindow, the three confirm-dialog arms, the OS
    // close button, the tab-bar ✕ on the last tab, and the reap path.
    let flag_needle = concat!("self.request_window_close", "(ws.seq);");
    let n_flags = src.matches(flag_needle).count();
    assert!(
        n_flags >= 9,
        "expected the 9 window-close paths to request their own window close \
             (found {n_flags})"
    );
    assert!(
        src.contains("pending_window_closes: std::collections::BTreeSet<u64>")
            && src.contains("if take_window_close_request(&mut self.pending_window_closes, seq)"),
        "close requests must be keyed and consumed by the dispatched window id"
    );
}

#[test]
fn one_windows_close_request_cannot_be_consumed_by_a_sibling() {
    use super::take_window_close_request;

    let mut pending = std::collections::BTreeSet::from([20]);
    assert!(!take_window_close_request(&mut pending, 10));
    assert!(
        pending.contains(&20),
        "window 10 must leave window 20 armed"
    );
    assert!(take_window_close_request(&mut pending, 20));
    assert!(pending.is_empty());
}

#[test]
fn tab_drag_target_index_uses_rendered_segment_rects() {
    use super::tab_drag_target_index;
    let seg = |idx, x, w| {
        let rect = (x, 0.0, w, 24.0);
        let close = (x + w - 24.0, 0.0, 24.0, 24.0);
        kettle_render::TabSeg {
            idx,
            rect,
            title_rect: super::tab_title_rect(rect, close, true),
            close,
            title: format!("tab-{idx}"),
            path: None,
            active: idx == 0,
            activity: kettle_render::TabActivity::Normal,
        }
    };
    // 3 equal tabs, 300-px strip → 100 px per segment. Cursor at
    // 50 → tab 0; 150 → tab 1; 250 → tab 2.
    let equal = [
        seg(0, 0.0, 100.0),
        seg(1, 100.0, 100.0),
        seg(2, 200.0, 100.0),
    ];
    assert_eq!(tab_drag_target_index(50.0, 12.0, &equal), 0);
    assert_eq!(tab_drag_target_index(150.0, 12.0, &equal), 1);
    assert_eq!(tab_drag_target_index(250.0, 12.0, &equal), 2);
    // Right at the boundary: 100 → tab 1 (floor); 200 → tab 2.
    assert_eq!(tab_drag_target_index(100.0, 12.0, &equal), 1);
    assert_eq!(tab_drag_target_index(200.0, 12.0, &equal), 2);
    // Natural-width tabs: targeting follows the rendered rects,
    // not a hidden strip/n assumption.
    let natural = [seg(0, 0.0, 72.0), seg(1, 72.0, 144.0), seg(2, 216.0, 88.0)];
    assert_eq!(tab_drag_target_index(71.0, 12.0, &natural), 0);
    assert_eq!(tab_drag_target_index(72.0, 12.0, &natural), 1);
    assert_eq!(tab_drag_target_index(215.0, 12.0, &natural), 1);
    assert_eq!(tab_drag_target_index(216.0, 12.0, &natural), 2);
    // Negative cursor (past the left edge) → clamps to 0.
    assert_eq!(tab_drag_target_index(-50.0, 12.0, &natural), 0);
    // Past the right edge → clamps to last segment, not n.
    assert_eq!(tab_drag_target_index(900.0, 12.0, &natural), 2);
    assert_eq!(tab_drag_target_index(f32::MAX, 12.0, &natural), 2);
    // Empty bar → 0 (defensive no-op).
    assert_eq!(tab_drag_target_index(50.0, 12.0, &[]), 0);
}

/// Side tab bars reorder by drag like top and bottom ones. Both pointer paths
/// once skipped the reorder for `tab-position = left` / `right`, a leftover
/// from before `tab_drag_target_index` read the strip's axis off its segments.
#[test]
fn every_tab_bar_position_reorders_by_drag() {
    let src = production_source();
    let native_move = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native CursorMoved arm");
    let ctl_move = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("ctl_mouse_move body");
    for (label, body) in [("native move", native_move), ("ctl move", ctl_move)] {
        let reorder = body
            .find("self.reorder_active_tab_for_cursor(ws)")
            .unwrap_or_else(|| panic!("{label} reorders through the shared helper"));
        let gate = &body[body[..reorder].rfind("if ").expect("a reorder gate")..reorder];
        assert!(
            gate.contains("ws.tab_drag_active") && gate.contains("ws.tab_drag_press.is_none()"),
            "{label} reorders only a promoted tab drag"
        );
        assert!(
            !gate.contains("is_vertical"),
            "{label} reorders on every tab bar position"
        );
    }
}

#[test]
fn tab_reorder_drag_threshold_filters_click_jitter() {
    use super::tab_reorder_drag_threshold_px;
    // Default-height tab bars get a 12px promotion distance. That leaves
    // normal mouse-down jitter visually in the pressed-tab state instead of
    // flashing the drag ghost beside the selected tab.
    assert!((tab_reorder_drag_threshold_px(24.0) - 12.0).abs() < f32::EPSILON);
    // Tiny and large bars are bounded so the gesture remains usable.
    assert_eq!(tab_reorder_drag_threshold_px(8.0), 8.0);
    assert_eq!(tab_reorder_drag_threshold_px(60.0), 16.0);
}

/// The tear decision is uniform hysteresis in every direction away from the
/// tab band, for every `tab-bar-pos`.
#[test]
fn tear_threshold_crossed_per_orientation() {
    use super::{dist_to_rect, tear_threshold_crossed};
    // Top band (0,0,800,24), threshold 36 (= 1.5 × 24).
    let top = (0.0, 0.0, 800.0, 24.0);
    // Inside the band, anywhere along the strip: reorder, never tear.
    assert!(!tear_threshold_crossed(5.0, 12.0, top, 36.0));
    assert!(!tear_threshold_crossed(790.0, 12.0, top, 36.0));
    // Below the band but within the slop: still no tear.
    assert!(!tear_threshold_crossed(400.0, 59.0, top, 36.0));
    // Past the slop downward (into the terminal content): tear.
    assert!(tear_threshold_crossed(400.0, 60.0, top, 36.0));
    // Above the window (band edge == window edge): same hysteresis.
    assert!(!tear_threshold_crossed(400.0, -35.0, top, 36.0));
    assert!(tear_threshold_crossed(400.0, -36.0, top, 36.0));
    // Past the strip's right end horizontally: distance is measured
    // from the band edge, so the slop applies there too.
    assert!(!tear_threshold_crossed(820.0, 12.0, top, 36.0));
    assert!(tear_threshold_crossed(836.0, 12.0, top, 36.0));
    // Diagonal: Euclidean, not Manhattan — 30² + 30² > 36².
    assert!(tear_threshold_crossed(830.0, 54.0, top, 36.0));
    // Bottom band on a 600-tall surface.
    let bottom = (0.0, 576.0, 800.0, 24.0);
    assert!(!tear_threshold_crossed(400.0, 580.0, bottom, 36.0));
    assert!(tear_threshold_crossed(400.0, 540.0, bottom, 36.0));
    // Left vertical band (width = tab_bar_width 200): tearing happens
    // rightward past the slop.
    let left = (0.0, 0.0, 200.0, 600.0);
    assert!(!tear_threshold_crossed(100.0, 300.0, left, 36.0));
    assert!(!tear_threshold_crossed(235.0, 300.0, left, 36.0));
    assert!(tear_threshold_crossed(236.0, 300.0, left, 36.0));
    // Right vertical band: leftward.
    let right = (600.0, 0.0, 200.0, 600.0);
    assert!(tear_threshold_crossed(560.0, 300.0, right, 36.0));
    // A zero threshold can never tear (defensive: hidden bar).
    assert!(!tear_threshold_crossed(400.0, 500.0, top, 0.0));
    // dist_to_rect itself: inside = 0, outside = Euclidean.
    assert_eq!(dist_to_rect(10.0, 10.0, top), 0.0);
    assert_eq!(dist_to_rect(400.0, 124.0, top), 100.0);
    assert_eq!(dist_to_rect(803.0, 28.0, top), 5.0); // 3-4-5 corner
}

/// The ghost's tear-lift is a clamped linear ramp over the SAME
/// distance/threshold pair the tear decision uses, so 1.0 lands exactly
/// where `tear_threshold_crossed` fires.
#[test]
fn tear_lift_ratio_scales_with_distance() {
    use super::{tear_lift_ratio, tear_threshold_crossed};
    let top = (0.0, 0.0, 800.0, 24.0);
    // Inside the band: no lift, anywhere along the strip.
    assert_eq!(tear_lift_ratio(5.0, 12.0, top, 36.0), 0.0);
    assert_eq!(tear_lift_ratio(790.0, 12.0, top, 36.0), 0.0);
    // Halfway to the threshold below the band: half lift.
    assert_eq!(tear_lift_ratio(400.0, 42.0, top, 36.0), 0.5);
    // Exactly at the threshold: full lift — and the tear fires here.
    assert_eq!(tear_lift_ratio(400.0, 60.0, top, 36.0), 1.0);
    assert!(tear_threshold_crossed(400.0, 60.0, top, 36.0));
    // Far past the threshold (Wayland's at-release hover): clamped.
    assert_eq!(tear_lift_ratio(400.0, 500.0, top, 36.0), 1.0);
    // Above the window: same uniform hysteresis as the tear decision.
    assert_eq!(tear_lift_ratio(400.0, -18.0, top, 36.0), 0.5);
    // A zero threshold can never tear, so it never lifts either.
    assert_eq!(tear_lift_ratio(400.0, 500.0, top, 0.0), 0.0);
}

#[test]
fn window_choice_keeps_the_original_tab_and_is_keyboard_reachable() {
    let panes: Arc<[u64]> = vec![41, 42].into();
    let row = super::ContextMenuItem::WindowChoice {
        label: "2: project".into(),
        window: 2,
        panes: Arc::clone(&panes),
    };
    assert!(super::item_is_dispatchable(&row));
    let Some(super::ContextMenuClick::MoveTabToWindow {
        window,
        panes: captured,
    }) = super::item_to_click(&row, 0)
    else {
        panic!("window choices need the same mouse and keyboard dispatch");
    };
    assert_eq!(window, 2);
    assert_eq!(&*captured, &[41, 42]);
    assert!(Arc::ptr_eq(&panes, &captured));
}

/// Painting and docking share one band function.
#[test]
fn painted_and_docking_tab_bands_share_one_function() {
    let src = production_source();
    assert_eq!(src.matches("self.tab_band(ws, height)").count(), 2);
}

#[test]
fn manual_redocking_releases_the_closed_windows_toast() {
    let src = production_source();
    let close = src.split_once("drop(torn);").expect("manual donor close").0;
    let close = close
        .rsplit_once("if torn.mux.tabs.is_empty() {")
        .unwrap()
        .1;
    assert!(close.contains("self.close_update_toast_window(torn.seq);"));
}

#[test]
fn rescue_tick_finishes_released_tab_drags_before_following() {
    let src = production_source();
    let tick = src
        .split_once("fn torn_drag_pointer_tick(")
        .unwrap()
        .1
        .split_once("let cursor = cursor_screen_pos()?;")
        .unwrap()
        .1;
    let before_follow = tick.split_once("if td.native {").unwrap().0;
    assert!(before_follow.contains("if !primary_button_physically_held() {"));
    assert!(before_follow.contains("self.revalidate_dock_latch(&ws)"));
    assert!(before_follow.contains("self.finalize_torn_drag(&mut ws, commit);"));
}

/// Docking inserts BETWEEN segments, so n tabs have n+1 slots, decided by
/// segment midpoints.
#[test]
fn dock_insertion_index_slots_between_segments() {
    use super::dock_insertion_index;
    // Three segments at mids 50 / 150 / 250.
    let mids = [50.0, 150.0, 250.0];
    assert_eq!(dock_insertion_index(&mids, 0.0), 0); // before first
    assert_eq!(dock_insertion_index(&mids, 49.0), 0);
    assert_eq!(dock_insertion_index(&mids, 51.0), 1); // past mid 0
    assert_eq!(dock_insertion_index(&mids, 149.0), 1);
    assert_eq!(dock_insertion_index(&mids, 200.0), 2);
    assert_eq!(dock_insertion_index(&mids, 251.0), 3); // append
    assert_eq!(dock_insertion_index(&mids, f32::MAX), 3);
    // No segments (hidden-bar fallback handles this elsewhere): 0.
    assert_eq!(dock_insertion_index(&[], 100.0), 0);
}

#[test]
fn dock_insertion_index_preserves_visible_tab_indices() {
    use crate::app::dock_insertion_index;
    let mut mids = vec![-100_000.0; 20];
    mids[..5].copy_from_slice(&[50.0, 150.0, 250.0, 350.0, 450.0]);
    assert_eq!(dock_insertion_index(&mids, 475.0), 5);
    assert_eq!(dock_insertion_index(&mids, 425.0), 4);
    mids.fill(-100_000.0);
    mids[8..11].copy_from_slice(&[50.0, 150.0, 250.0]);
    assert_eq!(dock_insertion_index(&mids, 0.0), 8);
    assert_eq!(dock_insertion_index(&mids, 251.0), 11);
}

/// Drift guard: the tear-at-threshold call sites and the native drag
/// handoff stay wired the way the platform analysis verified them. The
/// needles are source-level because the flow spans winit's event dispatch
/// and can't run headless.
#[test]
fn tear_off_flow_stays_wired() {
    let src = production_source();
    // 1. The CursorMoved FSM block tears at the threshold (Chromium
    //    model), not at release.
    assert!(
        src.contains("if self.maybe_tear_off(ws, event_loop) {"),
        "CursorMoved must drive the threshold tear"
    );
    // 2. The torn window inherits the source size and is positioned by
    //    the grab offset (open_window's size override).
    assert!(
        src.contains(
            "WindowOpen::AdoptTab {\n                    tab: dt,\n                    source_color: self.tear_off_source_color(ws),\n                    font_size: ws.renderer.as_ref().map(|r| r.font_size()),\n                    scaled_zoom_prev_font_size: ws.scaled_zoom_prev_font_size,\n                    paste: Box::new(paste),\n                },\n                pos,\n                Some(size),"
        ),
        "the tear must open the torn window at the grab position and source size"
    );
    // 2b. A lone-tab drag follows the pointer on the same motion that
    //     started it, so one fast gesture can latch a dock target.
    assert!(
        src.contains(
            "                caption: false,\n                saw_move: false,\n                last_signal: std::time::Instant::now(),\n                hwnd: window_hwnd(&src),\n                signal_cursor: None,\n            });\n            self.follow_torn_drag(ws);\n            return true;"
        ),
        "the lone-tab drag must follow the pointer immediately"
    );
    // 2c. A new window applies a due scheduled theme before claiming its
    //     accent, so the source exclusion uses the pool it will paint.
    assert!(
        src.contains(
            "        self.poll_theme_schedule(&mut ws);\n        if self.cfg.accent_auto && self.cfg.accent_color.is_none() {\n            self.assign_window_accent(&mut ws, source_color);"
        ),
        "the theme schedule must run before the accent claim"
    );
    // 3. The native handoff happens right after the insert.
    assert!(
        src.contains(".map(|w| match w.drag_window() {"),
        "the torn window must be handed to the OS move loop"
    );
    // 4. The at-release tear is Wayland-only now.
    assert!(
        src.contains(
            "if self.cfg.detachable_tabs\n                        && dropped_outside\n                        && wayland\n                        && ws.mux.tabs.len() > 1\n                    {"
        ),
        "the Released-arm tear must be gated to Wayland and detachable-tabs"
    );
    // 5. The drop commits the latched dock from the left-release, GATED on
    //    the primary button being physically up. Windows' synthesized
    //    release also fires for an Esc-CANCELLED modal loop, with the
    //    button still held, and committing there would perform the exact
    //    merge the user is cancelling.
    assert!(
        src.contains("let commit = !primary_button_physically_held();"),
        "the release-drop must distinguish Esc-cancel via physical button state"
    );
    // 6. The X11/macOS pointer-event commits revalidate the latch
    //    against the torn window's FINAL position (a WM-cancelled move
    //    snapped it back to its origin, off the band).
    assert!(
        src.contains("let commit = self.revalidate_dock_latch(ws);"),
        "heuristic commits must revalidate the latch"
    );
    // 7. Manual-follow listens only to the capture holder (stale
    //    tracking must not hijack every window's cursor stream).
    assert!(
        src.contains("if td.native || td.carrier != ws.seq {"),
        "manual-follow must be carrier-gated"
    );
    // 8. A window that dies mid-drag takes its tracking with it.
    assert!(
        src.contains(".is_some_and(|t| t.seq == seq || t.carrier == seq)"),
        "finish_window_dispatch must abandon tracking for a dying torn/carrier window"
    );
    // 9. The dock hit-test runs from the LIVE cursor when a query source
    //    exists (GetCursorPos / X11 QueryPointer). The frame+grab
    //    approximation alone misses the band by the WM's anchor drift
    //    (measured 55-86px under Mutter).
    assert!(
        src.contains("let cursor = real.unwrap_or(approx);"),
        "the Moved-driven hit-test must prefer the live cursor"
    );
    // 10. The frozen-drag rescue tick is wired into about_to_wait. Without
    //     it, a silent native handoff (or a pointer past the carrier's
    //     bounds) leaves the torn window frozen mid-air with no path to
    //     carry or dock it.
    assert!(
        src.contains("let torn_tick_wait = self.torn_drag_pointer_tick(event_loop);"),
        "about_to_wait must run the frozen-drag rescue tick"
    );
    // 11. The heuristic pointer-event commits distinguish an Esc-cancel
    //     from a real drop by PHYSICAL button state (the X11 QueryPointer
    //     mask / Windows GetAsyncKeyState). Position heuristics cannot,
    //     because Esc moves the frame, never the pointer, and the WM's
    //     restore `Moved` re-syncs any frame-anchor estimate before the
    //     commit event arrives.
    assert!(
        src.contains("if primary_button_physically_held() {\n            return false;\n        }"),
        "revalidate_dock_latch must key Esc-cancel detection off physical button state"
    );
}

/// Drift guard: the Terminator-parity detachable-tabs setting is
/// a real runtime switch, not just a parsed compatibility key.
#[test]
fn detachable_tabs_config_gates_all_detach_paths() {
    let src = production_source();
    assert!(
        src.contains("if !self.cfg.detachable_tabs {\n                    log::info!(\"move_tab_to_new_window ignored because detachable-tabs = false\");"),
        "keyboard/palette move_tab_to_new_window must honor detachable-tabs = false"
    );
    let tear_off = src
        .split_once("fn maybe_tear_off(")
        .and_then(|(_, rest)| rest.split_once(") -> bool {"))
        .expect("maybe_tear_off")
        .1;
    assert!(
        tear_off
            .trim_start()
            .starts_with("if !self.cfg.detachable_tabs {"),
        "mouse threshold tear-off must honor detachable-tabs = false"
    );
    assert!(
        src.contains(
            "if self.cfg.detachable_tabs {\n                                    ws.detach_drag ="
        ),
        "tab press must not arm the cross-window detach FSM when disabled"
    );
    assert!(
        src.contains(
            "if self.cfg.detachable_tabs\n                        && dropped_outside\n                        && wayland\n                        && ws.mux.tabs.len() > 1\n                    {"
        ),
        "Wayland release-only tear-off must honor detachable-tabs = false"
    );
}

/// The tab-drag cursor tracks the detach FSM (Grab while armed, Grabbing
/// while dragging) and outranks every other icon rule so it can't flicker
/// mid-drag.
#[test]
fn tab_drag_cursor_icon_by_drag_state() {
    use super::tab_drag_cursor_icon;
    use crate::detach::DragState;
    use winit::window::CursorIcon;
    assert_eq!(tab_drag_cursor_icon(&DragState::Idle), None);
    assert_eq!(
        tab_drag_cursor_icon(&DragState::ArmedInside { tab_idx: 0 }),
        Some(CursorIcon::Grab)
    );
    assert_eq!(
        tab_drag_cursor_icon(&DragState::DraggingInside { tab_idx: 1 }),
        Some(CursorIcon::Grabbing)
    );
    assert_eq!(
        tab_drag_cursor_icon(&DragState::DraggingOutside { tab_idx: 2 }),
        Some(CursorIcon::Grabbing)
    );
    // Compose: a live drag outranks the close-✕ Pointer (the pointer
    // can transiently cross another tab's close zone mid-drag).
    assert_eq!(
        tab_drag_cursor_icon(&DragState::DraggingInside { tab_idx: 0 })
            .or(super::tab_close_hover_icon(true)),
        Some(CursorIcon::Grabbing)
    );
    // Idle defers — the close-hover rule gets its turn.
    assert_eq!(
        tab_drag_cursor_icon(&DragState::Idle).or(super::tab_close_hover_icon(true)),
        Some(CursorIcon::Pointer)
    );
}

/// A drag past the last tab on screen stops there, and a drop lands right
/// after it, on either bar axis. Both once reached the hidden last tab: a
/// drag onto `⌄` or `›` moved the tab to the very end, and a drop appended
/// after every hidden tab.
#[test]
fn dragging_and_docking_stop_at_the_tabs_on_screen() {
    use super::{dock_insertion_tab, tab_drag_target_index};
    for vertical in [true, false] {
        let segs = parked_strip(vertical);
        let at = |main: f32| if vertical { (80.0, main) } else { (main, 12.0) };
        let (x, y) = at(24.0 + 3.0 * 30.0 + 10.0);
        assert_eq!(tab_drag_target_index(x, y, &segs), 4, "past the last shown");
        let (x, y) = at(5.0);
        assert_eq!(
            tab_drag_target_index(x, y, &segs),
            2,
            "before the first shown"
        );
        let (x, y) = at(24.0 + 30.0 + 5.0);
        assert_eq!(tab_drag_target_index(x, y, &segs), 3);

        assert_eq!(
            dock_insertion_tab(&segs, vertical, 24.0 + 2.0 * 30.0 + 20.0),
            Some(5)
        );
        assert_eq!(dock_insertion_tab(&segs, vertical, 24.0 + 5.0), Some(2));
        assert_eq!(
            dock_insertion_tab(&segs, vertical, 24.0 + 30.0 + 20.0),
            Some(4)
        );
    }
    assert_eq!(dock_insertion_tab(&[], true, 10.0), None);
}

/// `native_theme_hint` is the pure policy behind the native titlebar theme
/// (window creation + `maybe_sync_native_theme`). Pin the decision table:
/// plain Auto defers to the OS (`None`, keeping winit's `ThemeChanged`
/// flowing for the palette auto-switcher); every other mode, and Auto with
/// a schedule, forces the titlebar to the active theme's darkness.
#[test]
fn native_theme_hint_decision_table() {
    use super::{WindowTheme, native_theme_hint};
    use kettle_config::ThemeMode;
    // Plain Auto: follow the OS regardless of the active palette.
    assert_eq!(native_theme_hint(ThemeMode::Auto, false, true), None);
    assert_eq!(native_theme_hint(ThemeMode::Auto, false, false), None);
    // Auto + schedule: the schedule drives the palette, so the titlebar
    // matches the palette, not the OS.
    assert_eq!(
        native_theme_hint(ThemeMode::Auto, true, true),
        Some(WindowTheme::Dark)
    );
    // Explicit / Light / Dark: titlebar matches the active theme.
    for mode in [ThemeMode::Explicit, ThemeMode::Light, ThemeMode::Dark] {
        assert_eq!(
            native_theme_hint(mode, false, true),
            Some(WindowTheme::Dark)
        );
        assert_eq!(
            native_theme_hint(mode, false, false),
            Some(WindowTheme::Light)
        );
    }
}

#[test]
fn live_window_icon_uses_the_active_palette_variant() {
    let src = kettle_test_support::production_source(&super::app_source());
    assert!(
        src.contains(".with_window_icon(load_window_icon(self.cfg.theme.is_dark()))"),
        "window creation must start with the active theme's icon"
    );
    assert!(
        src.contains("w.set_window_icon(icon.clone());"),
        "theme changes must update Windows and X11 live icons"
    );
    for asset in ["kettle-256.png", "kettle-light-256.png"] {
        assert!(
            src.contains(&format!("packaging/linux/{asset}")),
            "the runtime icon loader must embed {asset}"
        );
    }
}

/// winit's macOS backend drops a window icon, so macOS never decodes one
/// on the way to the first window.
#[cfg(target_os = "macos")]
#[test]
fn macos_skips_the_window_icon_decode() {
    use super::load_window_icon;
    assert!(load_window_icon(true).is_none(), "dark icon decoded");
    assert!(load_window_icon(false).is_none(), "light icon decoded");
}

#[test]
fn finding_hidden_dock_uses_materialized_segments() {
    let body = finding_body(include_str!("windowing.rs"), "dock_index_in_client");
    assert!(
        body.contains("self.tab_bar_at_height(w, height)"),
        "hidden docking must use the visible bar's segment layout"
    );
    assert!(!body.contains("bw * 0.5"));
}

#[test]
fn finding_tear_off_anchor_includes_status_bar() {
    let source = include_str!("windowing.rs");
    assert!(
        source.contains("self.tear_off_tab_band(ws, (size.width, size.height)"),
        "native tear-off anchors must use the destination tab band"
    );
    assert!(
        source.contains("self.tear_off_tab_band(ws, (size.width, size.height), height)"),
        "Wayland tear-off anchors must use the destination tab band"
    );
}

#[test]
fn review_torn_tab_keeps_scaled_zoom_baseline() {
    let src = production_source();
    let mut handoffs = 0;
    for body in src.split("WindowOpen::AdoptTab {").skip(1) {
        let fields = body.split('}').next().unwrap();
        if fields.contains("paste: Box::new(paste)") {
            handoffs += 1;
            assert!(
                fields.contains("scaled_zoom_prev_font_size: ws.scaled_zoom_prev_font_size"),
                "every live tab handoff must carry the unzoom font size: {fields}"
            );
        }
    }
    assert_eq!(handoffs, 4);
    let open = finding_body(include_str!("windowing.rs"), "open_window");
    assert!(open.contains("ws.scaled_zoom_prev_font_size = scaled_zoom_prev_font_size;"));
}

