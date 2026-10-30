#[test]
fn accessibility_initial_tree_is_complete_and_pane_ids_are_stable() {
    let update = App::initial_accessibility_tree(&kettle_i18n::Translator::default());
    assert_eq!(update.focus, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(update.nodes.len(), 1);
    let (root_id, root) = &update.nodes[0];
    assert_eq!(*root_id, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(root.role(), accesskit::Role::Window);
    assert_eq!(root.label(), Some("Kettle terminal"));
    let es =
        App::initial_accessibility_tree(&kettle_i18n::Translator::new(kettle_i18n::Language::Es));
    assert_eq!(es.nodes[0].1.label(), Some("Terminal Kettle"));

    let tree = update.tree.expect("initial update must establish a tree");
    assert_eq!(tree.root, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(tree.toolkit_name.as_deref(), Some("Kettle"));
    assert_eq!(
        tree.toolkit_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    let first = super::accessibility_pane_id(1);
    let text = super::accessibility_text_id(1);
    assert_ne!(first, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(first, super::accessibility_pane_id(1));
    assert_ne!(first, super::accessibility_pane_id(2));
    assert_ne!(first, text);
    assert_eq!(text, super::accessibility_text_id(1));
}

#[test]
fn confirm_dialog_accessibility_focuses_the_selected_button() {
    let overlay = kettle_render::Overlay {
        confirm_dialog: Some(kettle_render::ConfirmDialogOverlay {
            body: "Commands may be interrupted.".into(),
            hover_idx: None,
            pressed_idx: None,
            scroll: 0,
            heading: "Close 3 running panes?".to_string(),
            buttons: vec![
                kettle_render::ConfirmDialogButton {
                    label: "Cancel".to_string(),
                    destructive: false,
                },
                kettle_render::ConfirmDialogButton {
                    label: "Close".to_string(),
                    destructive: true,
                },
            ],
            focus_idx: 1,
        }),
        ..kettle_render::Overlay::default()
    };

    let projection = super::modal_accessibility_projection(
        &overlay,
        (800.0, 600.0),
        (8.0, 16.0),
        1.0,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    let dialog = projection
        .nodes
        .iter()
        .find(|(_, node)| node.role() == accesskit::Role::AlertDialog)
        .expect("confirm dialog node");
    assert_eq!(dialog.1.label(), Some("Close 3 running panes?"));
    let focused = projection
        .nodes
        .iter()
        .find(|(id, _)| *id == projection.focus.expect("modal focus"))
        .expect("focused modal node");
    assert_eq!(focused.1.role(), accesskit::Role::Button);
    assert_eq!(focused.1.label(), Some("Close"));
}

#[test]
fn confirmation_accessibility_bounds_follow_chrome_metrics() {
    let overlay = kettle_render::Overlay {
        confirm_dialog: Some(kettle_render::ConfirmDialogOverlay {
            body: "Commands may be interrupted.".into(),
            hover_idx: None,
            pressed_idx: None,
            scroll: 0,
            heading: "Close window with 2 panes?".into(),
            buttons: vec![
                kettle_render::ConfirmDialogButton {
                    label: "Cancel".into(),
                    destructive: false,
                },
                kettle_render::ConfirmDialogButton {
                    label: "Close".into(),
                    destructive: true,
                },
            ],
            focus_idx: 0,
        }),
        ..Default::default()
    };
    let first = super::modal_accessibility_projection(
        &overlay,
        (1600.0, 1200.0),
        (8.0, 16.0),
        1.0,
        &EN_TEXT,
    );
    let second = super::modal_accessibility_projection(
        &overlay,
        (1600.0, 1200.0),
        (16.0, 32.0),
        1.0,
        &EN_TEXT,
    );
    let dialog_bounds = |projection: &super::ModalAccessibilityProjection| {
        projection
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::AlertDialog)
            .unwrap()
            .1
            .bounds()
            .unwrap()
    };
    assert_ne!(dialog_bounds(&first), dialog_bounds(&second));
    for projection in [&first, &second] {
        let bounds = dialog_bounds(projection);
        assert!(bounds.x0 > 0.0 && bounds.y0 > 0.0 && bounds.x1 < 1600.0 && bounds.y1 < 1200.0);
        for (_, node) in &projection.nodes {
            if node.role() == accesskit::Role::Button {
                let button = node.bounds().unwrap();
                assert!(
                    button.x0 >= bounds.x0
                        && button.y0 >= bounds.y0
                        && button.x1 <= bounds.x1
                        && button.y1 <= bounds.y1
                );
            }
        }
    }
    let source = super::production_source();
    let hash = source
        .split_once("fn accessibility_key(")
        .unwrap()
        .1
        .split("fn publish_accessibility_if_due(")
        .next()
        .unwrap();
    let hash: String = hash.split_whitespace().collect();
    assert!(hash.contains("self.overlay_text_cell_width(ws).to_bits().hash(&muthasher);"));
    assert!(hash.contains("self.overlay_text_line_height(ws).to_bits().hash(&muthasher);"));
}

#[test]
fn context_menu_accessibility_exposes_actions_and_validates_targets() {
    use super::{ContextMenuItem, ContextMenuState, accessibility_context_menu_item_index};
    let mut ws = WindowState::new(1, Mux::new());
    ws.context_menu = Some(ContextMenuState {
        anchor: (0.0, 0.0),
        anchor_scale: 1.0,
        items: vec![
            ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: true,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "Paste",
                action: Action::Paste,
                enabled: false,
            },
            ContextMenuItem::Submenu {
                label: "Theme".into(),
                items: Vec::new(),
            },
        ],
        highlight: 0,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        typeahead_buf: String::new(),
        typeahead_until: None,
    });
    let overlay = kettle_render::Overlay {
        context_menu: Some(kettle_render::ContextMenu {
            anchor: (0.0, 0.0),
            a11y_label: None,
            rows: [
                ("Copy", false, true),
                ("", true, false),
                ("Paste", false, false),
                ("Theme", false, true),
            ]
            .into_iter()
            .map(
                |(label, separator, enabled)| kettle_render::ContextMenuRow {
                    label: label.into(),
                    separator,
                    enabled,
                    hint: String::new(),
                },
            )
            .collect(),
            highlight: 0,
            scroll_offset: 0,
            panel_w_clamped: 200.0,
            panel_h_clamped: 120.0,
        }),
        ..kettle_render::Overlay::default()
    };
    let projection =
        super::modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &EN_TEXT);
    for index in [0, 2, 3] {
        let id = super::accessibility_modal_id(2, index + 1);
        let (_, node) = projection.nodes.iter().find(|(at, _)| *at == id).unwrap();
        let enabled = index != 2;
        assert_eq!(node.supports_action(accesskit::Action::Focus), enabled);
        assert_eq!(node.supports_action(accesskit::Action::Click), enabled);
        assert_eq!(
            accessibility_context_menu_item_index(id, &ws),
            enabled.then_some(index)
        );
    }
    for id in [
        super::accessibility_modal_id(2, 0),
        super::accessibility_modal_id(2, 2),
        super::accessibility_modal_id(2, 5),
        super::accessibility_modal_id(1, 1),
        super::accessibility_modal_id(3, 1),
    ] {
        assert_eq!(accessibility_context_menu_item_index(id, &ws), None);
    }
    let copy = super::accessibility_modal_id(2, 1);
    ws.palette_input = Some((Default::default(), 0));
    assert_eq!(accessibility_context_menu_item_index(copy, &ws), None);
    ws.palette_input = None;
    ws.context_menu = None;
    assert_eq!(accessibility_context_menu_item_index(copy, &ws), None);

    let source = super::production_source();
    let handler = source
        .split_once("fn handle_accessibility_action(")
        .unwrap()
        .1;
    let menu = handler
        .split_once("accessibility_context_menu_item_index(")
        .unwrap()
        .1
        .split("ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID")
        .next()
        .unwrap();
    assert!(menu.contains("self.set_context_menu_highlight(ws, index)"));
    assert!(menu.contains("self.context_menu_activate(ws, event_loop)"));
}

#[test]
fn confirm_dialog_accessibility_button_ids_round_trip() {
    for index in 0..3 {
        let id = super::accessibility_modal_id(1, index + 1);
        assert_eq!(super::accessibility_confirm_button_index(id), Some(index));
    }
    assert_eq!(
        super::accessibility_confirm_button_index(super::accessibility_modal_id(1, 0)),
        None
    );
    assert_eq!(
        super::accessibility_confirm_button_index(super::accessibility_modal_id(5, 2)),
        None
    );
}

#[test]
fn ui_geometry_exposes_modal_accessibility_and_settings_scroll_without_values() {
    let src = production_source();
    let body = src
        .split("fn ctl_ui_geometry(")
        .nth(1)
        .and_then(|rest| rest.split("fn ctl_perform_action(").next())
        .expect("ui_geometry body");
    for needle in [
        "kettle_render::settings_panel_geometry",
        "\"first_line\":",
        "\"visible_lines\":",
        "\"focused_rect\":",
        "\"accessibility\": accessibility",
        "\"render_uploads\": target.renderer.as_ref().map(",
        "\"frames_presented\": u.frames_presented",
        "\"buffer_writes\": u.buffer_writes",
        "\"text_prepares\": u.text_prepares",
        "\"cursor_blink\": self.cursor_blink_diagnostics(target),",
        "\"renderer\": if layer.is_active() { \"layer\" } else { \"gpu\" },",
        "\"layer_rect\":",
        "\"phase_on\": phase_on,",
        "\"next_edge_ms\":",
        "\"handoffs\": layer.handoffs,",
        "\"exits\": layer.exits,",
        "\"mapped_uploads\": u.mapped_uploads",
        "\"mapped_writes\": u.mapped_writes",
        "\"chrome_prepares\": u.chrome_prepares",
    ] {
        assert!(body.contains(needle), "ui_geometry lost {needle:?}");
    }
    assert!(
        !body.contains("\"value\":"),
        "settings diagnostics must not expose config values"
    );
}

#[test]
fn preview_accessibility_key_tracks_controls_and_video_position() {
    let source = super::production_source();
    let key_body = source
        .split_once("fn accessibility_key(")
        .unwrap()
        .1
        .split("fn publish_accessibility_if_due(")
        .next()
        .unwrap();
    assert!(key_body.contains("accessibility_lane_controls_key("));
    let geometry = |lane: &kettle_render::MediaLanePanel| {
        kettle_render::media_lane_geometry(lane, (8.4, 16.4), (8.4, 16.4)).unwrap()
    };
    let key = |lane: &kettle_render::MediaLanePanel| {
        super::accessibility_lane_controls_key(lane, Some(&geometry(lane)))
    };
    let mut lane = test_lane_panel();
    lane.source = Some(kettle_render::MediaLaneSource::default());
    let rendered = key(&lane);
    lane.mode = kettle_render::MediaLaneMode::Source;
    assert_ne!(rendered, key(&lane));
    lane.mode = kettle_render::MediaLaneMode::Rendered;
    lane.position = (1, 3);
    assert_ne!(rendered, key(&lane));
    lane.position = (2, 3);
    lane.pin_label = Some("Pin".into());
    assert_ne!(rendered, key(&lane));
    lane.pin_label = None;
    let mut moved = geometry(&lane);
    moved.close.0 += 1.0;
    assert_ne!(
        rendered,
        super::accessibility_lane_controls_key(&lane, Some(&moved))
    );
    let mut fewer = geometry(&lane);
    assert!(fewer.copy.take().is_some());
    assert_ne!(
        rendered,
        super::accessibility_lane_controls_key(&lane, Some(&fewer))
    );
    assert_ne!(
        rendered,
        super::accessibility_lane_controls_key(&lane, None)
    );

    lane.scrubber = Some(kettle_render::MediaLaneScrubber {
        seekable: true,
        position_ms: 1_000,
        duration_ms: 10_000,
        label: String::new(),
    });
    assert!(geometry(&lane).scrubber.is_some());
    let playing = key(&lane);
    assert_ne!(rendered, playing);
    lane.scrubber.as_mut().unwrap().position_ms = 2_000;
    assert_ne!(playing, key(&lane));
    let later = key(&lane);
    lane.scrubber.as_mut().unwrap().duration_ms = 20_000;
    assert_ne!(later, key(&lane));
    let changed_duration = key(&lane);
    lane.scrubber = None;
    assert_ne!(changed_duration, key(&lane));
    assert_eq!(rendered, key(&lane));
}

#[test]
fn accessibility_key_tracks_each_visible_panes_scroll_offset() {
    let src = production_source();
    let key = src
        .split_once("fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64")
        .unwrap()
        .1
        .split("fn publish_accessibility_if_due")
        .next()
        .unwrap();
    let panes = key
        .split_once("for (pane_id, partition) in layout {")
        .unwrap()
        .1;
    let flat: String = panes.split_whitespace().collect();
    assert!(flat.contains(
        "pane.term.term.lock().ok().map(|term|term.grid().display_offset()).hash(&muthasher);"
    ));
}

#[test]
fn accessibility_hashes_the_visible_completion_projection() {
    let src = production_source();
    let tree = src
        .split("fn accessibility_tree(&self, ws: &WindowState, overlay: &Overlay)")
        .nth(1)
        .and_then(|rest| rest.split("fn accessibility_key").next())
        .expect("accessibility_tree body");
    assert!(
        tree.contains("row.set_position_in_set(candidate.position + 1)")
            && tree.contains("list.set_size_of_set(completion.total)")
            && tree.contains("completion_overlay_rect(completion, self.menu_cell(ws))"),
        "assistive technology must hear each visible row's absolute position and the full set size"
    );
    let receipt = tree
        .split("if let Some(receipt) = overlay.media_paste_receipt.as_ref()")
        .nth(1)
        .and_then(|rest| rest.split("let mut root = Node::new(Role::Window)").next())
        .expect("media receipt accessibility nodes");
    assert!(
        receipt.contains("let mut node = Node::new(Role::Button)")
            && receipt.contains("node.add_action(AccessibilityAction::Click)")
            && receipt.contains(
                "media_receipt_accessibility_label(&receipt.kind, receipt.remote, &receipt.tr)"
            )
            && !receipt.contains("Role::Group"),
        "receipt bodies must advertise their action and visible video details"
    );
    let body = src
        .split("fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64")
        .nth(1)
        .and_then(|rest| rest.split("fn publish_accessibility_if_due").next())
        .expect("accessibility_key body");
    assert!(
        body.contains("overlay.completion.as_ref()"),
        "accessibility invalidation must use the frame's focus/modal/viewport-gated projection"
    );
    assert!(
        body.contains("accessibility_completion_geometry_key(completion, self.menu_cell(ws))"),
        "accessibility invalidation must include the exact visible completion geometry"
    );
    assert!(
        !body.contains("self.overlay(") && !body.contains("pane.term.completion()"),
        "accessibility must neither rebuild the overlay nor read hidden raw completion state"
    );

    let draw = src
        .split("let overlay = self.overlay(ws, reuse_pane_snapshots);")
        .nth(1)
        .and_then(|rest| rest.split("let area = self.area(ws);").next())
        .expect("overlay publication block");
    assert!(
        draw.contains("self.publish_accessibility_if_due(ws, &overlay);")
            && !draw.contains("if !reuse_pane_snapshots"),
        "snapshot-only hover redraws must not strand a pending accessibility update"
    );
}

#[test]
fn video_receipt_accessibility_names_the_visible_details() {
    let one = kettle_render::MediaPasteReceiptKind::Video {
        extension: "MP4".into(),
        size: 1_500,
        count: 1,
        preview_pending: false,
    };
    let en = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    assert_eq!(
        super::media_receipt_accessibility_label(&one, false, &en),
        "Video path pasted, MP4, 1.5 KB"
    );
    assert_eq!(
        super::media_receipt_accessibility_label(&one, false, &es),
        "Ruta de video pegada, MP4, 1.5 KB"
    );

    let batch = kettle_render::MediaPasteReceiptKind::Video {
        extension: "WEBM".into(),
        size: 999,
        count: 3,
        preview_pending: true,
    };
    assert_eq!(
        super::media_receipt_accessibility_label(&batch, true, &en),
        "3 video paths pasted, first is WEBM, 999 B, remote pane, local path only"
    );
    assert_eq!(
        super::media_receipt_accessibility_label(&batch, true, &es),
        "3 rutas de video pegadas; el primer video es WEBM y ocupa 999 B, panel remoto, solo ruta local"
    );
    let image = kettle_render::MediaPasteReceiptKind::Image {
        original_width: 1920,
        original_height: 1080,
    };
    assert_eq!(
        super::media_receipt_accessibility_label(&image, false, &en),
        "Image path pasted, 1920 by 1080"
    );
    assert_eq!(
        super::media_receipt_accessibility_label(&image, false, &es),
        "Ruta de imagen pegada, 1920 por 1080"
    );
}

#[test]
fn media_receipt_accessibility_id_cannot_collide_with_completion_rows() {
    for receipt in [
        ACCESSIBILITY_MEDIA_RECEIPT_ID,
        ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID,
    ] {
        assert_ne!(
            receipt,
            accessibility_completion_row_id(0),
            "duplicate child ids make AccessKit panic when both overlays project"
        );
        assert_ne!(receipt, ACCESSIBILITY_COMPLETION_CONTAINER_ID);
    }
    assert_ne!(
        ACCESSIBILITY_MEDIA_RECEIPT_ID,
        ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID
    );
}

#[test]
fn completion_accessibility_geometry_key_tracks_layout_and_cell_metrics() {
    let completion = kettle_render::CompletionOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (20.0, 30.0, 800.0, 500.0),
        grid_rect: (28.0, 46.0, 784.0, 468.0),
        command_rows: (20, 21),
        anchor_col: None,
        kind: "Completions".to_string(),
        source: "fish".to_string(),
        selected: Some(0),
        total: 2,
        token: Some("al".to_string()),
        candidates: vec![
            kettle_render::CompletionOverlayRow {
                label: "alpha".to_string(),
                description: "first".to_string(),
                position: 0,
            },
            kettle_render::CompletionOverlayRow {
                label: "beta".to_string(),
                description: "second".to_string(),
                position: 1,
            },
        ],
    };
    let baseline = super::accessibility_completion_geometry_key(&completion, (9.0, 18.0));
    let rect = kettle_render::completion_overlay_rect(&completion, (9.0, 18.0))
        .expect("completion card geometry");
    assert!(super::completion_overlay_hit_test(
        Some(&completion),
        (9.0, 18.0),
        (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0),
    ));
    assert!(!super::completion_overlay_hit_test(
        Some(&completion),
        (9.0, 18.0),
        (rect.0 - 1.0, rect.1),
    ));
    assert!(!super::completion_overlay_hit_test(
        None,
        (9.0, 18.0),
        (rect.0, rect.1),
    ));
    assert_ne!(
        baseline,
        super::accessibility_completion_geometry_key(&completion, (10.0, 20.0)),
        "font and cell-size changes must invalidate accessible row bounds"
    );

    let mut moved = completion.clone();
    moved.grid_rect.0 += 13.0;
    moved.grid_rect.1 += 7.0;
    assert_ne!(
        baseline,
        super::accessibility_completion_geometry_key(&moved, (9.0, 18.0)),
        "pane and grid movement must invalidate accessible row bounds"
    );
}

/// The card grew a header. It belongs to the list container, so no exposed
/// row may start at the top of the card or overlap the header band, and the
/// header press must still dismiss rather than reach terminal content.
#[test]
fn completion_rows_start_below_the_header_band() {
    let completion = kettle_render::CompletionOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (20.0, 30.0, 800.0, 500.0),
        grid_rect: (28.0, 46.0, 784.0, 468.0),
        command_rows: (20, 21),
        anchor_col: None,
        kind: "Completions".to_string(),
        source: "fish".to_string(),
        selected: Some(1),
        total: 2,
        token: None,
        candidates: vec![
            kettle_render::CompletionOverlayRow {
                label: "alpha".to_string(),
                description: "first".to_string(),
                position: 0,
            },
            kettle_render::CompletionOverlayRow {
                label: "beta".to_string(),
                description: "second".to_string(),
                position: 1,
            },
        ],
    };
    let cell = (9.0, 18.0);
    let (x, y, width, height) =
        kettle_render::completion_overlay_rect(&completion, cell).expect("card geometry");
    let rows = kettle_render::completion_overlay_row_rects(&completion, cell);
    assert_eq!(rows.len(), 2);
    let first_row_top = rows[0].1.1;
    assert!(
        first_row_top > y,
        "the header band must sit above every exposed candidate row"
    );
    for (_, row) in &rows {
        assert!(row.1 >= first_row_top && row.1 + row.3 <= y + height);
        assert_eq!(row.0, x, "rows span the card so hit targets match paint");
        assert_eq!(row.2, width);
    }
    assert!(
        super::completion_overlay_hit_test(Some(&completion), cell, (x + width / 2.0, y + 1.0),),
        "a press on the header dismisses instead of reaching hidden content"
    );
}

#[test]
fn confirmation_accessibility_excludes_the_retained_settings_subtree() {
    let overlay = kettle_render::Overlay {
        confirm_dialog: Some(confirmation_test_overlay()),
        settings: Some(kettle_render::SettingsOverlay {
            categories: vec!["Keybinds".into()],
            label_cols: 20,
            value_cols: 20,
            active_category: 0,
            background_page: false,
            rows: Vec::new(),
            focused_row: 0,
            title: "Settings".into(),
            hints: String::new(),
            footer_note: None,
        }),
        ..Default::default()
    };
    let projection =
        super::modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &EN_TEXT);
    assert_eq!(projection.roots.len(), 1);
    let (_, dialog) = projection
        .nodes
        .iter()
        .find(|(_, node)| node.role() == accesskit::Role::AlertDialog)
        .unwrap();
    assert!(dialog.is_modal());
    assert_eq!(dialog.label(), Some("Close this window?"));
    assert!(dialog.description().unwrap().contains("unsaved work"));
    assert_eq!(projection.nodes.len(), 3);
}

#[test]
fn confirmation_accessibility_rejects_background_and_stale_targets() {
    use super::{ConfirmAccessibilityAction as C, confirmation_accessibility_action as route};
    use accesskit::Action;
    for target in [
        super::ACCESSIBILITY_ROOT_ID,
        super::accessibility_pane_id(1),
        super::accessibility_modal_id(5, 2),
        super::ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID,
    ] {
        for action in [
            Action::Focus,
            Action::Click,
            Action::ScrollUp,
            Action::SetValue,
        ] {
            assert_eq!(route(target, action, 2), None);
        }
    }
    assert_eq!(
        route(super::accessibility_modal_id(1, 1), Action::Focus, 2),
        Some(C::Focus(0))
    );
    assert_eq!(
        route(super::accessibility_modal_id(1, 2), Action::Click, 2),
        Some(C::Activate(1))
    );
    assert_eq!(
        route(super::accessibility_modal_id(1, 3), Action::Click, 2),
        None
    );
    assert_eq!(
        route(super::accessibility_modal_id(1, 0), Action::ScrollDown, 2),
        Some(C::Scroll(1))
    );
}

#[test]
fn accessibility_reactivation_invalidates_published_tree() {
    let source = super::app_source();
    let activation = source
        .split("impl ActivationHandler for AccessibilityActivation")
        .nth(1)
        .unwrap()
        .split("struct AccessibilityActions")
        .next()
        .unwrap();
    assert!(
        activation.contains("UserEvent::AccessibilityActivated"),
        "activation must notify the event loop to invalidate the cached tree"
    );
    let dispatch = source
        .split("UserEvent::AccessibilityActivated { window_seq } =>")
        .nth(1)
        .expect("activation dispatch");
    assert!(dispatch.contains("ws.accessibility_key = None;"));
    assert!(dispatch.contains("ws.accessibility_updated_at = None;"));
    assert!(dispatch.contains("ws.accessibility_pending = true;"));
}


#[test]
fn modal_text_inputs_expose_their_text_and_selection() {
    let field = |text: &str, cursor_byte, selection| kettle_render::TextFieldOverlay {
        text: text.into(),
        cursor_byte,
        selection,
        horizontal_scroll: 0,
        rect: (0.0, 0.0, 400.0, 26.0),
        prefix: String::new(),
        hint: String::new(),
        accent_palette: 0,
    };
    let text_input = |overlay: &kettle_render::Overlay| {
        let projection = super::modal_accessibility_projection(
            overlay,
            (800.0, 600.0),
            (8.0, 16.0),
            1.0,
            &EN_TEXT,
        );
        let input = projection
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TextInput)
            .map(|(_, node)| node.clone())
            .expect("a text input");
        let run = projection
            .nodes
            .iter()
            .find(|(id, _)| Some(id) == input.children().first())
            .map(|(_, node)| node.clone())
            .expect("the input's text run");
        (input, run)
    };
    // The title editor, with "año" selected from the end back to the caret.
    let overlay = kettle_render::Overlay {
        edit_title: Some(kettle_render::TitleEditOverlay {
            label: "Edit tab title:".into(),
            input: "mi año".into(),
            hint: String::new(),
            rect: (0.0, 0.0, 400.0, 26.0),
        }),
        text_field: Some(field("mi año", 3, Some((3, 7)))),
        ..Default::default()
    };
    let (input, run) = text_input(&overlay);
    assert_eq!(run.role(), accesskit::Role::TextRun);
    assert_eq!(run.value(), Some("mi año"));
    assert_eq!(run.character_lengths(), &[1, 1, 1, 1, 2, 1]);
    let selection = input.text_selection().expect("a selection");
    assert_eq!(
        (selection.anchor.character_index, selection.focus.character_index),
        (6, 3)
    );
    // A picker's query gets one too, with a collapsed caret.
    let overlay = kettle_render::Overlay {
        palette_query: Some("spl".into()),
        text_field: Some(field("spl", 3, None)),
        ..Default::default()
    };
    let (input, run) = text_input(&overlay);
    assert_eq!(run.value(), Some("spl"));
    let selection = input.text_selection().expect("a caret");
    assert_eq!(
        (selection.anchor.character_index, selection.focus.character_index),
        (3, 3)
    );
}

#[test]
fn settings_rows_carry_their_values_for_every_screen_reader() {
    let overlay = kettle_render::Overlay {
        settings: Some(kettle_render::SettingsOverlay {
            categories: vec!["Appearance".into()],
            label_cols: 20,
            value_cols: 20,
            active_category: 0,
            background_page: false,
            rows: vec![kettle_render::SettingsRow {
                label: "Theme".into(),
                value: "Builtin Dark".into(),
                disabled: false,
            }],
            focused_row: 0,
            title: "Settings".into(),
            hints: String::new(),
            footer_note: None,
        }),
        ..Default::default()
    };
    let projection =
        super::modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &EN_TEXT);
    let (_, row) = projection
        .nodes
        .iter()
        .find(|(_, node)| node.role() == accesskit::Role::ListBoxOption)
        .expect("a settings row");
    assert_eq!(row.label(), Some("Theme"));
    assert_eq!(row.value(), Some("Builtin Dark"));
    // AT-SPI reads a string value only through text ranges, so there the
    // value is also the description, which Orca speaks after the name.
    let spoken = cfg!(all(unix, not(target_os = "macos"))).then_some("Builtin Dark");
    assert_eq!(row.description(), spoken);
}

#[test]
fn accessibility_key_tracks_the_modal_text_caret_and_selection() {
    let src = production_source();
    let body = src
        .split("fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64")
        .nth(1)
        .and_then(|rest| rest.split("fn publish_accessibility_if_due").next())
        .expect("accessibility_key body");
    for line in [
        "if let Some(field) = overlay.text_field.as_ref() {",
        "field.text.hash(&mut hasher);",
        "field.cursor_byte.hash(&mut hasher);",
        "field.selection.hash(&mut hasher);",
    ] {
        assert!(body.contains(line), "a moved caret must republish: {line}");
    }
}
