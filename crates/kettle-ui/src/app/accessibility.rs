//! The accessibility tree, its keyboard navigation and the actions assistive technology asks for.

use super::*;

impl App {
    /// The cards on screen that assistive technology can reach, in paint
    /// order: each placement's instance, where it is, and the shelf item it
    /// shows. None while a dialog or a menu covers them.
    pub(super) fn accessible_cards<'a>(
        &self,
        ws: &'a WindowState,
    ) -> Vec<(u64, kettle_render::PaintedCard, &'a crate::media::ShelfItem)> {
        if self.pointer_modal_open(ws) {
            return Vec::new();
        }
        let Some(renderer) = ws.renderer.as_ref() else {
            return Vec::new();
        };
        // The last frame may still show a tab or pane that has since gone
        // from view; only what the layout shows now is reachable.
        let shown: Vec<u64> = ws
            .mux
            .leaves(ws.mux.active, self.area(ws))
            .into_iter()
            .map(|(pane, _)| pane)
            .collect();
        renderer
            .painted_cards()
            .iter()
            .filter(|card| shown.contains(&card.pane))
            .filter_map(|card| {
                let instance = ws.card_sightings.instance(card)?;
                let item = self.media.cards.item(card.pane, card.nonce)?;
                let item = ws
                    .mux
                    .panes
                    .get(&card.pane)?
                    .media_shelf
                    .items()
                    .iter()
                    .find(|it| it.id == item)?;
                Some((instance, *card, item))
            })
            .collect()
    }

    /// The viewer's detail line for a card's item, as assistive technology
    /// hears it: kind, size and sender.
    pub(super) fn card_accessibility_detail(&self, item: &crate::media::ShelfItem) -> String {
        let (kind, sender) = self.media_item_kind_and_sender(item);
        format!(
            "{} · {}",
            media_item_detail(self.ui_text, kind, item),
            sender.text
        )
    }

    pub(super) fn initial_accessibility_tree(tr: &kettle_i18n::Translator) -> TreeUpdate {
        let mut root = Node::new(Role::Window);
        root.set_label(tr.text(kettle_i18n::Text::A11yWindow));
        let mut tree = TreeInfo::new(ACCESSIBILITY_ROOT_ID);
        tree.toolkit_name = Some("Kettle".to_string());
        tree.toolkit_version = Some(env!("CARGO_PKG_VERSION").to_string());
        TreeUpdate {
            nodes: vec![(ACCESSIBILITY_ROOT_ID, root)],
            tree: Some(tree),
            tree_id: TreeId::ROOT,
            focus: ACCESSIBILITY_ROOT_ID,
        }
    }

    pub(super) fn new_accessibility_adapter(
        event_loop: &ActiveEventLoop,
        window: &Window,
        window_seq: u64,
        proxy: EventLoopProxy<UserEvent>,
        tr: &kettle_i18n::Translator,
    ) -> accesskit_winit::Adapter {
        accesskit_winit::Adapter::with_direct_handlers(
            event_loop,
            window,
            AccessibilityActivation {
                initial: Self::initial_accessibility_tree(tr),
                proxy: proxy.clone(),
                window_seq,
            },
            AccessibilityActions { proxy, window_seq },
            AccessibilityDeactivation,
        )
    }

    pub(super) fn accessibility_tree(&self, ws: &WindowState, overlay: &Overlay) -> TreeUpdate {
        if overlay.confirm_dialog.is_some() {
            let (surface, scale) = ws.window.as_ref().map_or(((800.0, 600.0), 1.0), |window| {
                let size = window.inner_size();
                (
                    (size.width as f32, size.height as f32),
                    window.scale_factor() as f32,
                )
            });
            let modal = modal_accessibility_projection(
                overlay,
                surface,
                (
                    self.overlay_text_cell_width(ws),
                    self.overlay_text_line_height(ws),
                ),
                scale,
                &self.ui_text,
            );
            let mut root = Node::new(Role::Window);
            root.set_label(self.ui_text.text(kettle_i18n::Text::A11yWindow));
            root.set_children(modal.roots);
            root.set_bounds(accesskit::Rect::new(
                0.0,
                0.0,
                surface.0 as f64,
                surface.1 as f64,
            ));
            let mut nodes = modal.nodes;
            nodes.push((ACCESSIBILITY_ROOT_ID, root));
            return TreeUpdate {
                nodes,
                tree: None,
                tree_id: TreeId::ROOT,
                focus: modal.focus.unwrap_or(ACCESSIBILITY_ROOT_ID),
            };
        }
        let area = self.area(ws);
        let layout = self.pane_layout(ws, area);
        let focused = ws.mux.active_focus();
        let cards = self.accessible_cards(ws);
        let mut children = Vec::with_capacity(layout.len());
        let mut nodes = Vec::with_capacity(layout.len() * 2 + cards.len() + 1);
        // A pane's terminal node covers its terminal, clear of its titlebar
        // and lane.
        for (pane_id, partition) in layout {
            if ws.preview_overlay.is_some() {
                continue;
            }
            let (x, y, width, height) = partition.terminal;
            let Some(pane) = ws.mux.panes.get(&pane_id) else {
                continue;
            };
            let node_id = accessibility_pane_id(pane_id);
            let text_id = accessibility_text_id(pane_id);
            children.push(node_id);
            let mut node = Node::new(Role::Terminal);
            let label = if pane.title.trim().is_empty() {
                self.ui_text.a11y_pane(pane_id)
            } else {
                pane.title.clone()
            };
            node.set_label(label);
            let mut pane_children = vec![text_id];
            for (instance, card, item) in cards.iter().filter(|(_, card, _)| card.pane == pane_id) {
                let id = accessibility_card_id(*instance);
                pane_children.push(id);
                nodes.push((
                    id,
                    card_accessibility_node(
                        card.rect,
                        &item.title,
                        &self.card_accessibility_detail(item),
                        &self.ui_text,
                    ),
                ));
            }
            node.set_children(pane_children);
            let text = pane
                .term
                .screen_text(0)
                .map(|screen| screen.text.trim_end().to_string())
                .unwrap_or_default();
            let mut text_node = Node::new(Role::TextRun);
            text_node.set_character_lengths(
                text.chars()
                    .map(|character| character.len_utf8() as u8)
                    .collect::<Vec<_>>(),
            );
            text_node.set_value(text);
            text_node.set_bounds(accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + width),
                f64::from(y + height),
            ));
            nodes.push((text_id, text_node));
            if pane.read_only {
                node.set_read_only();
            }
            node.set_bounds(accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + width),
                f64::from(y + height),
            ));
            nodes.push((node_id, node));
        }
        // Each preview lane, beside its pane's terminal, with a button for
        // each control it shows, bounded where the lane paints it.
        let ax_rect = |(x, y, width, height): kettle_render::Rect4| {
            accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + width),
                f64::from(y + height),
            )
        };
        // Under a dialog or menu a lane is out of reach, as its pointer is.
        let lanes_reachable = !self.non_search_modal_open(ws);
        for lane in overlay
            .media_lanes
            .iter()
            .filter(|_| lanes_reachable)
            .filter(|lane| ws.preview_overlay.is_none_or(|open| open.pane == lane.pane))
        {
            let Some(lane_id) = accessibility_lane_id(lane.pane, 0) else {
                continue;
            };
            children.push(lane_id);
            let mut node = Node::new(if lane.pin_label.is_some() {
                Role::Dialog
            } else {
                Role::Group
            });
            if lane.pin_label.is_some() {
                node.set_modal();
            }
            let announced = self.lane_announcement(ws, lane.pane);
            let (label, description) =
                media_lane_accessibility(lane, &self.ui_text, announced.as_deref());
            node.set_label(label);
            node.set_description(description);
            node.set_bounds(ax_rect(lane.rect));
            let mut controls = Vec::new();
            if let Some(geometry) = self.preview_lane_geometry(ws, lane) {
                for (part, rect, hit) in lane_controls(&geometry) {
                    let (Some(rect), Some(id)) = (rect, accessibility_lane_id(lane.pane, part))
                    else {
                        continue;
                    };
                    let text = match hit {
                        kettle_render::MediaLaneHit::Previous => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yPrevious)
                        }
                        kettle_render::MediaLaneHit::Next => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yNext)
                        }
                        kettle_render::MediaLaneHit::PreviousPage => {
                            self.ui_text.text(if lane.document {
                                kettle_i18n::Text::MediaLaneA11yPreviousPage
                            } else {
                                kettle_i18n::Text::MediaLaneA11yPreviousDiagram
                            })
                        }
                        kettle_render::MediaLaneHit::NextPage => {
                            self.ui_text.text(if lane.document {
                                kettle_i18n::Text::MediaLaneA11yNextPage
                            } else {
                                kettle_i18n::Text::MediaLaneA11yNextDiagram
                            })
                        }
                        kettle_render::MediaLaneHit::Pin => {
                            self.ui_text.text(kettle_i18n::Text::MediaViewerA11yPin)
                        }
                        kettle_render::MediaLaneHit::Toggle if lane.collapsed => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yExpand)
                        }
                        kettle_render::MediaLaneHit::Toggle => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yCollapse)
                        }
                        kettle_render::MediaLaneHit::Close => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yClose)
                        }
                        kettle_render::MediaLaneHit::OpenOutside => {
                            match self.preview_opens_outside(ws, lane.pane) {
                                Some(outside) => outside.label(&self.ui_text),
                                None => continue,
                            }
                        }
                        kettle_render::MediaLaneHit::Mode
                            if lane.mode == kettle_render::MediaLaneMode::Source =>
                        {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yRendered)
                        }
                        kettle_render::MediaLaneHit::Mode => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11ySource)
                        }
                        kettle_render::MediaLaneHit::Canvas => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yCanvas)
                        }
                        kettle_render::MediaLaneHit::Copy => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yCopy)
                        }
                        kettle_render::MediaLaneHit::Reload => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yReload)
                        }
                        kettle_render::MediaLaneHit::Play
                            if lane.play == Some(kettle_render::MediaLanePlay::Stop) =>
                        {
                            self.ui_text
                                .text(kettle_i18n::Text::MediaLaneA11yStopPreview)
                        }
                        kettle_render::MediaLaneHit::Play
                            if lane.play == Some(kettle_render::MediaLanePlay::Pause) =>
                        {
                            self.ui_text
                                .text(kettle_i18n::Text::MediaLaneA11yPauseVideo)
                        }
                        kettle_render::MediaLaneHit::Play => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yPlayVideo)
                        }
                        kettle_render::MediaLaneHit::Sound
                            if lane.sound == Some(kettle_render::MediaLaneSound::Muted) =>
                        {
                            self.ui_text
                                .text(kettle_i18n::Text::MediaLaneA11yUnmuteVideo)
                        }
                        kettle_render::MediaLaneHit::Sound => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yMuteVideo)
                        }
                        kettle_render::MediaLaneHit::ZoomOut => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yZoomOut)
                        }
                        kettle_render::MediaLaneHit::ZoomIn => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yZoomIn)
                        }
                        kettle_render::MediaLaneHit::Fit => {
                            self.ui_text.text(kettle_i18n::Text::MediaLaneA11yFit)
                        }
                        kettle_render::MediaLaneHit::Theater => {
                            self.ui_text.text(kettle_i18n::Text::MediaViewerTheater)
                        }
                        kettle_render::MediaLaneHit::Fullscreen => {
                            self.ui_text.text(kettle_i18n::Text::MediaViewerFullscreen)
                        }
                        kettle_render::MediaLaneHit::Resize
                        | kettle_render::MediaLaneHit::Scrub
                        | kettle_render::MediaLaneHit::Inside => continue,
                    };
                    let mut button = Node::new(Role::Button);
                    button.set_label(text);
                    if hit == kettle_render::MediaLaneHit::Theater {
                        button.set_toggled(
                            ws.preview_overlay.is_some_and(|open| open.theater).into(),
                        );
                    } else if hit == kettle_render::MediaLaneHit::Fullscreen {
                        button.set_toggled(
                            ws.window
                                .as_ref()
                                .is_some_and(|window| window.fullscreen().is_some())
                                .into(),
                        );
                    }
                    button.add_action(AccessibilityAction::Click);
                    if lane.pin_label.is_some() {
                        button.add_action(AccessibilityAction::Focus);
                    }
                    button.set_bounds(ax_rect(rect));
                    if !lane_control_enabled(lane, hit) {
                        button.set_disabled();
                    }
                    controls.push(id);
                    nodes.push((id, button));
                }
                // A playing video's bar: a slider in seconds, its value said
                // as a time of the video's length.
                if let (Some(rect), Some(bar), Some(id)) = (
                    geometry.scrubber,
                    lane.scrubber.as_ref(),
                    accessibility_lane_id(lane.pane, LANE_SCRUBBER_PART),
                ) {
                    let mut slider = Node::new(Role::Slider);
                    if lane.pin_label.is_some() {
                        slider.add_action(AccessibilityAction::Focus);
                    }
                    slider.set_label(
                        self.ui_text
                            .text(kettle_i18n::Text::MediaLaneA11yVideoPosition),
                    );
                    slider.set_value(self.ui_text.media_lane_a11y_video_time(
                        &kettle_render::duration_label(bar.position_ms),
                        &kettle_render::duration_label(bar.duration_ms),
                    ));
                    slider.set_numeric_value(bar.position_ms as f64 / 1000.0);
                    slider.set_min_numeric_value(0.0);
                    slider.set_max_numeric_value(bar.duration_ms as f64 / 1000.0);
                    slider.set_numeric_value_step(LANE_SEEK_STEP.as_secs_f64());
                    slider.add_action(AccessibilityAction::SetValue);
                    slider.add_action(AccessibilityAction::Increment);
                    slider.add_action(AccessibilityAction::Decrement);
                    slider.set_bounds(ax_rect(rect));
                    controls.push(id);
                    nodes.push((id, slider));
                }
            }
            node.set_children(controls);
            nodes.push((lane_id, node));
        }
        if ws.search.open
            && let Some(geometry) = self.search_geometry(ws)
        {
            let bounds = |(x, y, width, height): kettle_render::Rect4| {
                accesskit::Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x + width),
                    f64::from(y + height),
                )
            };
            children.push(ACCESSIBILITY_SEARCH_CONTAINER_ID);
            let mut search_children = Vec::with_capacity(8);
            for control in kettle_render::SearchControl::ALL {
                let id = accessibility_search_control_id(control);
                search_children.push(id);
                let mut node = Node::new(match control {
                    kettle_render::SearchControl::Editor => Role::SearchInput,
                    kettle_render::SearchControl::Wrap | kettle_render::SearchControl::Invert => {
                        Role::CheckBox
                    }
                    _ => Role::Button,
                });
                node.set_label(control.accessible_label(&self.ui_text));
                node.set_bounds(bounds(geometry.control_rect(control)));
                node.add_action(AccessibilityAction::Focus);
                node.add_action(AccessibilityAction::Click);
                match control {
                    kettle_render::SearchControl::Editor => {
                        let query = ws.search.query();
                        node.set_value(query);
                        node.set_children([ACCESSIBILITY_SEARCH_TEXT_ID]);
                        node.add_action(AccessibilityAction::SetValue);
                        node.add_action(AccessibilityAction::ReplaceSelectedText);
                        node.add_action(AccessibilityAction::SetTextSelection);

                        let mut text_node = Node::new(Role::TextRun);
                        text_node.set_value(query);
                        text_node.set_character_lengths(
                            query
                                .chars()
                                .map(|character| character.len_utf8() as u8)
                                .collect::<Vec<_>>(),
                        );
                        let byte_character_index =
                            |byte: usize| query[..byte.min(query.len())].chars().count();
                        let (anchor, focus) = ws.search.editor.directed_selection().map_or_else(
                            || {
                                let cursor = byte_character_index(ws.search.editor.cursor());
                                (cursor, cursor)
                            },
                            |(anchor, focus)| {
                                (byte_character_index(anchor), byte_character_index(focus))
                            },
                        );
                        node.set_text_selection(TextSelection {
                            anchor: TextPosition {
                                node: ACCESSIBILITY_SEARCH_TEXT_ID,
                                character_index: anchor,
                            },
                            focus: TextPosition {
                                node: ACCESSIBILITY_SEARCH_TEXT_ID,
                                character_index: focus,
                            },
                        });
                        nodes.push((ACCESSIBILITY_SEARCH_TEXT_ID, text_node));
                    }
                    kettle_render::SearchControl::Wrap => node.set_toggled(ws.search.wrap.into()),
                    kettle_render::SearchControl::Case => {
                        node.set_value(
                            map_search_case_mode(ws.search.case_mode).label(&self.ui_text),
                        );
                    }
                    kettle_render::SearchControl::Invert => {
                        node.set_toggled(ws.search.invert.into());
                    }
                    _ => {}
                }
                nodes.push((id, node));
            }
            search_children.push(ACCESSIBILITY_SEARCH_STATUS_ID);
            let mut status = Node::new(Role::Status);
            status.set_label(effective_search_status(&ws.search).label(&self.ui_text));
            status.set_live(accesskit::Live::Polite);
            status.set_bounds(bounds(geometry.status));
            nodes.push((ACCESSIBILITY_SEARCH_STATUS_ID, status));

            let mut search = Node::new(Role::Search);
            search.set_label(self.ui_text.text(kettle_i18n::Text::SearchA11yRegion));
            search.set_children(search_children);
            search.set_bounds(bounds(geometry.rect));
            nodes.push((ACCESSIBILITY_SEARCH_CONTAINER_ID, search));
        }
        if let Some(completion) = overlay.completion.as_ref()
            && let Some((x, y, width, height)) =
                kettle_render::completion_overlay_rect(completion, self.menu_cell(ws))
        {
            let cell = self.menu_cell(ws);
            let visible_rows = kettle_render::completion_overlay_row_rects(completion, cell);
            let mut row_ids = Vec::with_capacity(visible_rows.len());
            for (index, (x, y, width, height)) in visible_rows {
                let Some(candidate) = completion.candidates.get(index) else {
                    continue;
                };
                let id = accessibility_completion_row_id(candidate.position);
                row_ids.push(id);
                let mut row = Node::new(Role::ListBoxOption);
                row.set_label(if candidate.description.is_empty() {
                    candidate.label.clone()
                } else {
                    format!("{}, {}", candidate.label, candidate.description)
                });
                row.set_position_in_set(candidate.position + 1);
                if completion.selected == Some(index) {
                    row.set_selected(true);
                }
                row.set_bounds(accesskit::Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x + width),
                    f64::from(y + height),
                ));
                nodes.push((id, row));
            }
            let mut list = Node::new(Role::ListBox);
            list.set_label(
                self.ui_text
                    .completion_a11y_list(&completion.kind, &completion.source),
            );
            list.set_children(row_ids);
            list.set_size_of_set(completion.total);
            list.set_live(accesskit::Live::Polite);
            list.set_bounds(accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + width),
                f64::from(y + height),
            ));
            children.push(ACCESSIBILITY_COMPLETION_CONTAINER_ID);
            nodes.push((ACCESSIBILITY_COMPLETION_CONTAINER_ID, list));
        }
        if let Some(receipt) = overlay.media_paste_receipt.as_ref()
            && let Some(geometry) = kettle_render::notification_geometry(
                overlay.update_toast.as_ref().map(|toast| toast.geometry),
                Some(receipt),
                overlay.completion.as_ref(),
                self.menu_cell(ws),
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            )
            .receipt
        {
            let label =
                media_receipt_accessibility_label(&receipt.kind, receipt.remote, &receipt.tr);
            let mut node = Node::new(Role::Button);
            node.set_label(label);
            node.set_description(self.ui_text.text(if receipt.openable {
                kettle_i18n::Text::ReceiptA11yOpenImage
            } else {
                kettle_i18n::Text::ReceiptA11yHideVideo
            }));
            node.set_live(accesskit::Live::Polite);
            node.add_action(AccessibilityAction::Click);
            let (x, y, _width, height) = geometry.rect;
            let open_width = (geometry.dismiss_rect.0 - x).max(0.0);
            node.set_bounds(accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + open_width),
                f64::from(y + height),
            ));
            children.push(ACCESSIBILITY_MEDIA_RECEIPT_ID);
            nodes.push((ACCESSIBILITY_MEDIA_RECEIPT_ID, node));

            let mut dismiss = Node::new(Role::Button);
            dismiss.set_label(self.ui_text.text(kettle_i18n::Text::ReceiptA11yDismiss));
            dismiss.add_action(AccessibilityAction::Click);
            let (x, y, width, height) = geometry.dismiss_rect;
            dismiss.set_bounds(accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + width),
                f64::from(y + height),
            ));
            children.push(ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID);
            nodes.push((ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID, dismiss));
        }
        if let Some(toast) = &overlay.update_toast {
            let projected = update_toast_nodes(
                toast,
                self.ui_text.text(kettle_i18n::Text::UpdateMenuDismiss),
            );
            children.extend(projected.iter().map(|(id, _)| *id));
            nodes.extend(projected);
        }
        let surface = ws
            .window
            .as_ref()
            .map(|window| {
                let size = window.inner_size();
                (size.width as f32, size.height as f32)
            })
            .or_else(|| {
                ws.renderer.as_ref().map(|renderer| {
                    let (width, height) = renderer.surface_size();
                    (width as f32, height as f32)
                })
            })
            .unwrap_or((800.0, 600.0));
        let modal = modal_accessibility_projection(
            overlay,
            surface,
            (
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            ),
            ws.window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32),
            &self.ui_text,
        );
        let modal_focus = modal.focus;
        children.extend(modal.roots);
        nodes.extend(modal.nodes);
        let mut root = Node::new(Role::Window);
        root.set_label(self.ui_text.text(kettle_i18n::Text::A11yWindow));
        root.set_children(children);
        if let Some(window) = &ws.window {
            let size = window.inner_size();
            root.set_bounds(accesskit::Rect::new(
                0.0,
                0.0,
                f64::from(size.width),
                f64::from(size.height),
            ));
        }
        nodes.push((ACCESSIBILITY_ROOT_ID, root));
        let focus = modal_focus
            .or(self
                .update_toast_focus
                .filter(|_| overlay.update_toast.is_some()))
            .unwrap_or_else(|| {
                if ws.search.open {
                    accessibility_search_control_id(ws.search.focused_control)
                } else if let Some(open) = ws.preview_overlay {
                    open.control
                        .and_then(|control| accessibility_lane_id(open.pane, control))
                        .filter(|id| {
                            nodes
                                .iter()
                                .any(|(at, node)| at == id && !node.is_disabled())
                        })
                        .or_else(|| accessibility_lane_id(open.pane, 0))
                        .unwrap_or(ACCESSIBILITY_ROOT_ID)
                } else {
                    ws.card_focus
                        .map(accessibility_card_id)
                        .into_iter()
                        .chain(focused.map(accessibility_pane_id))
                        .find(|id| nodes.iter().any(|(candidate, _)| candidate == id))
                        .unwrap_or(ACCESSIBILITY_ROOT_ID)
                }
            });
        TreeUpdate {
            nodes,
            tree: None,
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    pub(super) fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64 {
        use std::hash::{Hash as _, Hasher as _};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        ws.mux.active_focus().hash(&mut hasher);
        ws.preview_overlay
            .map(|open| (open.pane, open.control))
            .hash(&mut hasher);
        ws.card_focus.hash(&mut hasher);
        for (instance, card, item) in self.accessible_cards(ws) {
            instance.hash(&mut hasher);
            card.rect.map(f32::to_bits).hash(&mut hasher);
            (item.id, item.generation).hash(&mut hasher);
        }
        if let Some(window) = &ws.window {
            let size = window.inner_size();
            size.width.hash(&mut hasher);
            size.height.hash(&mut hasher);
            window.scale_factor().to_bits().hash(&mut hasher);
        }
        ws.search.open.hash(&mut hasher);
        if ws.search.open {
            ws.search.query().hash(&mut hasher);
            ws.search.editor.cursor().hash(&mut hasher);
            ws.search.editor.selection().hash(&mut hasher);
            ws.search.wrap.hash(&mut hasher);
            map_search_case_mode(ws.search.case_mode)
                .label(&self.ui_text)
                .hash(&mut hasher);
            ws.search.invert.hash(&mut hasher);
            effective_search_status(&ws.search)
                .label(&self.ui_text)
                .hash(&mut hasher);
            ws.search.focused_control.hash(&mut hasher);
        }
        overlay.confirm_dialog.is_some().hash(&mut hasher);
        if let Some(dialog) = overlay.confirm_dialog.as_ref() {
            self.overlay_text_cell_width(ws).to_bits().hash(&mut hasher);
            self.overlay_text_line_height(ws)
                .to_bits()
                .hash(&mut hasher);
            dialog.heading.hash(&mut hasher);
            dialog.body.hash(&mut hasher);
            dialog.scroll.hash(&mut hasher);
            dialog.focus_idx.hash(&mut hasher);
            for button in &dialog.buttons {
                button.label.hash(&mut hasher);
                button.destructive.hash(&mut hasher);
            }
        }
        overlay.settings.is_some().hash(&mut hasher);
        if let Some(settings) = overlay.settings.as_ref() {
            settings.categories.hash(&mut hasher);
            settings.label_cols.hash(&mut hasher);
            settings.value_cols.hash(&mut hasher);
            settings.footer_note.hash(&mut hasher);
            settings.active_category.hash(&mut hasher);
            settings.focused_row.hash(&mut hasher);
            for row in &settings.rows {
                row.label.hash(&mut hasher);
                row.value.hash(&mut hasher);
                row.disabled.hash(&mut hasher);
            }
        }
        overlay.context_menu.is_some().hash(&mut hasher);
        if let Some(menu) = overlay.context_menu.as_ref() {
            menu.highlight.hash(&mut hasher);
            menu.scroll_offset.hash(&mut hasher);
            for component in [
                menu.anchor.0,
                menu.anchor.1,
                menu.panel_w_clamped,
                menu.panel_h_clamped,
            ] {
                component.to_bits().hash(&mut hasher);
            }
            for row in &menu.rows {
                row.label.hash(&mut hasher);
                row.hint.hash(&mut hasher);
                row.separator.hash(&mut hasher);
                row.enabled.hash(&mut hasher);
            }
        }
        overlay.palette_query.hash(&mut hasher);
        overlay.layout_picker_query.hash(&mut hasher);
        overlay.theme_picker_query.hash(&mut hasher);
        overlay.ssh_query.hash(&mut hasher);
        // Modal text inputs expose their caret and selection.
        if let Some(field) = overlay.text_field.as_ref() {
            field.text.hash(&mut hasher);
            field.cursor_byte.hash(&mut hasher);
            field.selection.hash(&mut hasher);
        }
        if let Some(edit) = overlay.edit_title.as_ref() {
            edit.label.hash(&mut hasher);
            edit.input.hash(&mut hasher);
            for component in [edit.rect.0, edit.rect.1, edit.rect.2, edit.rect.3] {
                component.to_bits().hash(&mut hasher);
            }
        }
        for hint in &overlay.hint_labels {
            hint.row.hash(&mut hasher);
            hint.col.hash(&mut hasher);
            hint.label.hash(&mut hasher);
            hint.dim.hash(&mut hasher);
        }
        if let Some(toast) = &overlay.update_toast {
            toast.message.hash(&mut hasher);
            for value in [
                toast.geometry.rect.0,
                toast.geometry.rect.1,
                toast.geometry.rect.2,
                toast.geometry.rect.3,
            ] {
                value.to_bits().hash(&mut hasher);
            }
        }
        self.update_toast_focus.hash(&mut hasher);
        let projected_completion = overlay.completion.as_ref();
        projected_completion.is_some().hash(&mut hasher);
        if let Some(completion) = projected_completion {
            completion.source.hash(&mut hasher);
            completion.selected.hash(&mut hasher);
            completion.total.hash(&mut hasher);
            completion.kind.hash(&mut hasher);
            completion.token.hash(&mut hasher);
            completion.command_rows.hash(&mut hasher);
            accessibility_completion_geometry_key(completion, self.menu_cell(ws)).hash(&mut hasher);
            for candidate in &completion.candidates {
                candidate.position.hash(&mut hasher);
                candidate.label.hash(&mut hasher);
                candidate.description.hash(&mut hasher);
            }
        }
        let projected_receipt = overlay.media_paste_receipt.as_ref();
        projected_receipt.is_some().hash(&mut hasher);
        if let Some(receipt) = projected_receipt {
            match &receipt.kind {
                kettle_render::MediaPasteReceiptKind::Image {
                    original_width,
                    original_height,
                } => {
                    0_u8.hash(&mut hasher);
                    original_width.hash(&mut hasher);
                    original_height.hash(&mut hasher);
                }
                kettle_render::MediaPasteReceiptKind::Video {
                    extension,
                    size,
                    count,
                    preview_pending,
                } => {
                    1_u8.hash(&mut hasher);
                    extension.hash(&mut hasher);
                    size.hash(&mut hasher);
                    count.hash(&mut hasher);
                    preview_pending.hash(&mut hasher);
                }
            }
            receipt.remote.hash(&mut hasher);
            receipt.expanded.hash(&mut hasher);
            if let Some(geometry) = kettle_render::notification_geometry(
                overlay.update_toast.as_ref().map(|toast| toast.geometry),
                Some(receipt),
                overlay.completion.as_ref(),
                self.menu_cell(ws),
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            )
            .receipt
            {
                for component in [
                    geometry.rect.0,
                    geometry.rect.1,
                    geometry.rect.2,
                    geometry.rect.3,
                    geometry.dismiss_rect.0,
                    geometry.dismiss_rect.1,
                    geometry.dismiss_rect.2,
                    geometry.dismiss_rect.3,
                ] {
                    component.to_bits().hash(&mut hasher);
                }
            }
        }
        overlay.media_lanes.len().hash(&mut hasher);
        ws.preview_overlay
            .map(|open| (open.theater, open.fullscreen))
            .hash(&mut hasher);
        ws.window
            .as_ref()
            .is_some_and(|window| window.fullscreen().is_some())
            .hash(&mut hasher);
        for lane in &overlay.media_lanes {
            lane.pane.hash(&mut hasher);
            lane.collapsed.hash(&mut hasher);
            lane.open_outside.hash(&mut hasher);
            // The play and sound buttons' names follow what they do.
            lane.play.hash(&mut hasher);
            lane.sound.hash(&mut hasher);
            accessibility_lane_controls_key(lane, self.preview_lane_geometry(ws, lane).as_ref())
                .hash(&mut hasher);
            let announced = self.lane_announcement(ws, lane.pane);
            media_lane_accessibility(lane, &self.ui_text, announced.as_deref()).hash(&mut hasher);
            let (x, y, width, height) = lane.rect;
            for component in [x, y, width, height] {
                component.to_bits().hash(&mut hasher);
            }
        }
        let mut layout = self.pane_layout(ws, self.area(ws));
        layout.sort_by_key(|(pane_id, _)| *pane_id);
        for (pane_id, partition) in layout {
            pane_id.hash(&mut hasher);
            let lane = partition
                .lane
                .and_then(crate::pane_partition::LaneShare::rect);
            for rect in [Some(partition.leaf), Some(partition.terminal), lane] {
                rect.is_some().hash(&mut hasher);
                for component in rect.into_iter().flat_map(|r| [r.0, r.1, r.2, r.3]) {
                    component.to_bits().hash(&mut hasher);
                }
            }
            if let Some(pane) = ws.mux.panes.get(&pane_id) {
                pane.title.hash(&mut hasher);
                pane.read_only.hash(&mut hasher);
                pane.term.output_generation().hash(&mut hasher);
                pane.term
                    .term
                    .lock()
                    .ok()
                    .map(|term| term.grid().display_offset())
                    .hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    pub(super) fn publish_accessibility_if_due(&self, ws: &mut WindowState, overlay: &Overlay) {
        let key = self.accessibility_key(ws, overlay);
        if ws.accessibility_key == Some(key) {
            ws.accessibility_pending = false;
            return;
        }
        let due = ws
            .accessibility_updated_at
            .is_none_or(|updated| updated.elapsed() >= ACCESSIBILITY_UPDATE_INTERVAL);
        if !due {
            ws.accessibility_pending = true;
            return;
        }
        let Some(mut adapter) = ws.accessibility.take() else {
            ws.accessibility_pending = false;
            return;
        };
        let mut published = false;
        adapter.update_if_active(|| {
            published = true;
            self.accessibility_tree(ws, overlay)
        });
        ws.accessibility = Some(adapter);
        ws.accessibility_pending = false;
        if published {
            ws.accessibility_key = Some(key);
            ws.accessibility_updated_at = Some(std::time::Instant::now());
        }
    }

    pub(super) fn handle_accessibility_action(
        &mut self,
        ws: &mut WindowState,
        request: ActionRequest,
        event_loop: &ActiveEventLoop,
    ) {
        if let Some(dialog) = ws.confirm_dialog.as_ref() {
            match confirmation_accessibility_action(
                request.target_node,
                request.action,
                dialog.buttons.len(),
            ) {
                Some(ConfirmAccessibilityAction::Focus(index)) => {
                    if let Some(dialog) = ws.confirm_dialog.as_mut() {
                        dialog.focus_idx = index;
                        dialog.armed_press = None;
                        dialog.space_pressed = None;
                    }
                    self.confirm_scroll(ws, 0, true);
                }
                Some(ConfirmAccessibilityAction::Activate(index)) => {
                    self.resolve_confirm_button(ws, index)
                }
                Some(ConfirmAccessibilityAction::Scroll(delta)) => {
                    self.confirm_scroll(ws, delta, false)
                }
                None => return,
            }
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        if accessibility_confirm_button_index(request.target_node).is_some() {
            return;
        }
        if let Some(index) = accessibility_context_menu_item_index(request.target_node, ws) {
            if !matches!(
                request.action,
                AccessibilityAction::Focus | AccessibilityAction::Click
            ) || self.context_menu_geometry(ws).is_none()
            {
                return;
            }
            self.set_context_menu_highlight(ws, index);
            if request.action == AccessibilityAction::Click {
                self.context_menu_activate(ws, event_loop);
            }
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        if request.target_node == ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID {
            if request.action == AccessibilityAction::Click {
                ws.media_paste_receipt = None;
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            return;
        }
        if matches!(
            request.target_node,
            ACCESSIBILITY_UPDATE_DETAILS_ID | ACCESSIBILITY_UPDATE_DISMISS_ID
        ) {
            if self.update_toast_projection(ws).is_some() {
                if request.action == AccessibilityAction::Focus {
                    self.update_toast_focus = Some(request.target_node);
                    ws.accessibility_pending = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                } else if request.action == AccessibilityAction::Click {
                    self.dismiss_update_toast(ws);
                    if request.target_node == ACCESSIBILITY_UPDATE_DETAILS_ID {
                        self.open_about_panel(ws);
                    }
                }
            }
            return;
        }
        self.update_toast_focus = None;
        if request.target_node == ACCESSIBILITY_MEDIA_RECEIPT_ID {
            if request.action == AccessibilityAction::Click {
                let openable = self
                    .media_paste_receipt_projection(ws)
                    .is_some_and(|(receipt, _)| receipt.openable);
                if openable {
                    self.open_media_paste_receipt(ws);
                } else if ws.media_paste_receipt.take().is_some() {
                    ws.accessibility_pending = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                }
            }
            return;
        }
        if let Some((pane, part)) = accessibility_lane_part(request.target_node) {
            if !self.preview_is_reachable(ws, pane) {
                return;
            }
            if part == LANE_SCRUBBER_PART && request.action != AccessibilityAction::Focus {
                self.seek_lane_by_accessibility(ws, pane, &request);
                return;
            }
            let hit = self
                .shown_preview_controls(ws, pane)
                .into_iter()
                .find(|(at, _, _)| *at == part)
                .map(|(_, _, hit)| hit);
            if let Some(hit) = hit {
                match request.action {
                    AccessibilityAction::Focus => {
                        if let Some(open) =
                            ws.preview_overlay.as_mut().filter(|open| open.pane == pane)
                        {
                            open.control = Some(part);
                        }
                        self.lanes_changed(ws, false);
                    }
                    AccessibilityAction::Click => self.act_on_preview(ws, pane, hit),
                    _ => {}
                }
            }
            return;
        }
        if let Some(instance) = accessibility_card_instance(request.target_node) {
            // Only a card the tree names answers: one on screen, under no
            // dialog or viewer. A stale instance names nothing, so it can
            // never focus or open another card.
            let reachable = self
                .accessible_cards(ws)
                .iter()
                .any(|(card, _, _)| *card == instance);
            if reachable && let Some((pane, nonce)) = ws.card_sightings.by_instance(instance) {
                match request.action {
                    AccessibilityAction::Focus => {
                        // The card's pane takes the keyboard with it, so text
                        // typed next goes where the screen reader is. Search
                        // would keep the keys, so it closes, remembering its
                        // query.
                        self.close_search(ws);
                        let pre = self.focus_key(ws);
                        ws.mux.focus_pane(pane);
                        self.note_focus_change(ws, pre);
                        ws.card_focus = Some(instance);
                    }
                    AccessibilityAction::Click => {
                        self.open_card(ws, pane, nonce);
                    }
                    _ => return,
                }
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            return;
        }
        if !ws.search.open {
            return;
        }
        let control = if request.target_node == ACCESSIBILITY_SEARCH_TEXT_ID {
            Some(kettle_render::SearchControl::Editor)
        } else {
            accessibility_search_control_from_id(request.target_node)
        };
        let Some(control) = control else {
            return;
        };

        match request.action {
            AccessibilityAction::Focus => ws.search.focused_control = control,
            AccessibilityAction::Click => {
                ws.search.focused_control = control;
                self.activate_search_control(ws, control);
            }
            AccessibilityAction::SetValue if control == kettle_render::SearchControl::Editor => {
                if let Some(ActionData::Value(value)) = request.data {
                    let outcome = ws
                        .search
                        .editor
                        .replace_all(&value, kettle_core::MAX_SEARCH_QUERY_BYTES);
                    if outcome.changed {
                        ws.search.note_edit(std::time::Instant::now());
                    }
                    if outcome.truncated {
                        ws.search.status = kettle_render::SearchStatus::TooLong;
                    }
                }
            }
            AccessibilityAction::ReplaceSelectedText
                if control == kettle_render::SearchControl::Editor =>
            {
                if let Some(ActionData::Value(value)) = request.data {
                    let outcome = ws
                        .search
                        .editor
                        .insert(&value, kettle_core::MAX_SEARCH_QUERY_BYTES);
                    if outcome.changed {
                        ws.search.note_edit(std::time::Instant::now());
                    }
                    if outcome.truncated {
                        ws.search.status = kettle_render::SearchStatus::TooLong;
                    }
                }
            }
            AccessibilityAction::SetTextSelection
                if control == kettle_render::SearchControl::Editor =>
            {
                if let Some(ActionData::SetTextSelection(selection)) = request.data
                    && selection.anchor.node == ACCESSIBILITY_SEARCH_TEXT_ID
                    && selection.focus.node == ACCESSIBILITY_SEARCH_TEXT_ID
                {
                    ws.search.editor.set_character_selection(
                        selection.anchor.character_index,
                        selection.focus.character_index,
                    );
                }
            }
            _ => return,
        }
        if ws.search.open {
            self.sync_search_editor_scroll(ws);
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }
}

pub(super) fn update_toast_nodes(
    toast: &kettle_render::UpdateToastOverlay,
    dismiss: &str,
) -> Vec<(NodeId, Node)> {
    let mut nodes = Vec::with_capacity(3);
    let mut status = Node::new(Role::Status);
    status.set_label(toast.message.clone());
    status.set_live(accesskit::Live::Polite);
    nodes.push((ACCESSIBILITY_UPDATE_STATUS_ID, status));
    for (id, label, rect) in [
        (
            ACCESSIBILITY_UPDATE_DETAILS_ID,
            toast.details.clone(),
            toast.geometry.details,
        ),
        (
            ACCESSIBILITY_UPDATE_DISMISS_ID,
            dismiss.to_string(),
            toast.geometry.close,
        ),
    ] {
        let mut node = Node::new(Role::Button);
        node.set_label(label);
        node.add_action(AccessibilityAction::Click);
        node.add_action(AccessibilityAction::Focus);
        node.set_bounds(accesskit::Rect::new(
            rect.0 as f64,
            rect.1 as f64,
            (rect.0 + rect.2) as f64,
            (rect.1 + rect.3) as f64,
        ));
        nodes.push((id, node));
    }
    nodes
}

pub(super) const ACCESSIBILITY_ROOT_ID: NodeId = NodeId(0);
const ACCESSIBILITY_PANE_ID_MASK: u64 = 1 << 63;
const ACCESSIBILITY_TEXT_ID_MASK: u64 = (1 << 63) | (1 << 62);
const ACCESSIBILITY_SEARCH_ID_MASK: u64 = 1 << 61;
pub(super) const ACCESSIBILITY_SEARCH_CONTAINER_ID: NodeId = NodeId(ACCESSIBILITY_SEARCH_ID_MASK);
const ACCESSIBILITY_SEARCH_TEXT_ID: NodeId = NodeId(ACCESSIBILITY_SEARCH_ID_MASK | 16);
const ACCESSIBILITY_SEARCH_STATUS_ID: NodeId = NodeId(ACCESSIBILITY_SEARCH_ID_MASK | 17);
const ACCESSIBILITY_COMPLETION_ID_MASK: u64 = 1 << 60;
pub(super) const ACCESSIBILITY_COMPLETION_CONTAINER_ID: NodeId =
    NodeId(ACCESSIBILITY_COMPLETION_ID_MASK);
pub(super) const ACCESSIBILITY_MEDIA_RECEIPT_ID: NodeId = NodeId(1 << 59);
/// The transient update status.
const ACCESSIBILITY_UPDATE_STATUS_ID: NodeId = NodeId((1 << 59) + 2);
pub(super) const ACCESSIBILITY_UPDATE_DETAILS_ID: NodeId = NodeId((1 << 59) + 3);
pub(super) const ACCESSIBILITY_UPDATE_DISMISS_ID: NodeId = NodeId((1 << 59) + 4);
pub(super) const ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID: NodeId = NodeId((1 << 59) | 1);
pub(super) const ACCESSIBILITY_MODAL_ID_MASK: u64 = 1 << 58;
/// Inline cards on screen, each by its placement's instance number, which
/// stays below this bit; never by its nonce.
pub(super) const ACCESSIBILITY_CARD_ID_MASK: u64 = crate::media::MAX_CARD_INSTANCE;
pub(super) const ACCESSIBILITY_UPDATE_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

pub(super) fn accessibility_completion_geometry_key(
    completion: &kettle_render::CompletionOverlay,
    cell: (f32, f32),
) -> u64 {
    use std::hash::{Hash as _, Hasher as _};

    fn hash_rect(rect: kettle_render::Rect4, hasher: &mut impl std::hash::Hasher) {
        for component in [rect.0, rect.1, rect.2, rect.3] {
            component.to_bits().hash(hasher);
        }
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Some(rect) = kettle_render::completion_overlay_rect(completion, cell) {
        true.hash(&mut hasher);
        hash_rect(rect, &mut hasher);
    } else {
        false.hash(&mut hasher);
    }
    for (index, rect) in kettle_render::completion_overlay_row_rects(completion, cell) {
        index.hash(&mut hasher);
        hash_rect(rect, &mut hasher);
    }
    hasher.finish()
}

pub(super) fn accessibility_pane_id(pane_id: u64) -> NodeId {
    NodeId::from(ACCESSIBILITY_PANE_ID_MASK | pane_id)
}

pub(super) fn accessibility_text_id(pane_id: u64) -> NodeId {
    NodeId::from(ACCESSIBILITY_TEXT_ID_MASK | pane_id)
}

pub(super) fn accessibility_completion_row_id(position: usize) -> NodeId {
    NodeId(ACCESSIBILITY_COMPLETION_ID_MASK | (position as u64 + 1))
}

fn accessibility_search_control_id(control: kettle_render::SearchControl) -> NodeId {
    let offset = match control {
        kettle_render::SearchControl::Editor => 1,
        kettle_render::SearchControl::Previous => 2,
        kettle_render::SearchControl::Next => 3,
        kettle_render::SearchControl::Wrap => 4,
        kettle_render::SearchControl::Case => 5,
        kettle_render::SearchControl::Invert => 6,
        kettle_render::SearchControl::Close => 7,
    };
    NodeId(ACCESSIBILITY_SEARCH_ID_MASK | offset)
}

fn accessibility_search_control_from_id(id: NodeId) -> Option<kettle_render::SearchControl> {
    match id.0 {
        value
            if value == accessibility_search_control_id(kettle_render::SearchControl::Editor).0 =>
        {
            Some(kettle_render::SearchControl::Editor)
        }
        value
            if value
                == accessibility_search_control_id(kettle_render::SearchControl::Previous).0 =>
        {
            Some(kettle_render::SearchControl::Previous)
        }
        value if value == accessibility_search_control_id(kettle_render::SearchControl::Next).0 => {
            Some(kettle_render::SearchControl::Next)
        }
        value if value == accessibility_search_control_id(kettle_render::SearchControl::Wrap).0 => {
            Some(kettle_render::SearchControl::Wrap)
        }
        value if value == accessibility_search_control_id(kettle_render::SearchControl::Case).0 => {
            Some(kettle_render::SearchControl::Case)
        }
        value
            if value == accessibility_search_control_id(kettle_render::SearchControl::Invert).0 =>
        {
            Some(kettle_render::SearchControl::Invert)
        }
        value
            if value == accessibility_search_control_id(kettle_render::SearchControl::Close).0 =>
        {
            Some(kettle_render::SearchControl::Close)
        }
        _ => None,
    }
}

/// What a screen reader hears for a preview lane: its name, with the item's
/// place on the shelf, title and detail line, and a description that says
/// what the lane last did (`announced`), else when the pixels were released
/// and where keys go.
pub(super) fn media_lane_accessibility(
    lane: &kettle_render::MediaLanePanel,
    tr: &kettle_i18n::Translator,
    announced: Option<&str>,
) -> (String, String) {
    let (index, count) = lane.position;
    let detail = match lane.page.filter(|&(_, pages)| pages > 1) {
        Some((page, pages)) => format!(
            "{} · {} · {}",
            lane.detail,
            if lane.document {
                tr.media_lane_a11y_page(page as u64, pages as u64)
            } else {
                tr.media_lane_a11y_diagram(page as u64, pages as u64)
            },
            lane.sender.text
        ),
        None => format!("{} · {}", lane.detail, lane.sender.text),
    };
    let label = if lane.pin_label.is_some() {
        tr.media_viewer_a11y(index as u64, count as u64, &lane.title, &detail)
    } else {
        tr.media_lane_a11y(index as u64, count as u64, &lane.title, &detail)
    };
    let description = match announced {
        Some(announced) => announced.to_owned(),
        None if lane.image.is_some() || lane.frame.is_some() => lane.hint.clone(),
        None => format!("{} {}", lane.status, lane.hint),
    };
    (label, description)
}

/// The node of `pane`'s lane (`part` 0) or one of its controls.
pub(super) fn accessibility_lane_id(pane: u64, part: u64) -> Option<NodeId> {
    (pane < MAX_LANE_ACCESSIBILITY_PANE && part < LANE_ACCESSIBILITY_PARTS)
        .then(|| NodeId(ACCESSIBILITY_LANE_ID_MASK | (pane * LANE_ACCESSIBILITY_PARTS) | part))
}

/// The pane and part a lane node names, if it is a lane's.
pub(super) fn accessibility_lane_part(id: NodeId) -> Option<(u64, u64)> {
    let payload = id.0 & (ACCESSIBILITY_LANE_ID_MASK - 1);
    (id.0 & !(ACCESSIBILITY_LANE_ID_MASK - 1) == ACCESSIBILITY_LANE_ID_MASK).then_some((
        payload / LANE_ACCESSIBILITY_PARTS,
        payload % LANE_ACCESSIBILITY_PARTS,
    ))
}

pub(super) fn accessibility_lane_controls_key(
    lane: &kettle_render::MediaLanePanel,
    geometry: Option<&kettle_render::MediaLaneGeometry>,
) -> u64 {
    use std::hash::{Hash as _, Hasher as _};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (lane.mode == kettle_render::MediaLaneMode::Source).hash(&mut hasher);
    lane.pin_label.is_some().hash(&mut hasher);
    geometry.is_some().hash(&mut hasher);
    let rect_key =
        |(x, y, width, height): kettle_render::Rect4| [x, y, width, height].map(f32::to_bits);
    if let Some(geometry) = geometry {
        for (part, rect, hit) in lane_controls(geometry) {
            (part, rect.map(rect_key), lane_control_enabled(lane, hit)).hash(&mut hasher);
        }
        geometry.scrubber.map(rect_key).hash(&mut hasher);
        if geometry.scrubber.is_some() {
            lane.scrubber
                .as_ref()
                .map(|bar| (bar.position_ms, bar.duration_ms))
                .hash(&mut hasher);
        }
    }
    hasher.finish()
}

pub(super) fn accessibility_card_id(instance: u64) -> NodeId {
    NodeId(ACCESSIBILITY_CARD_ID_MASK | instance)
}

/// The card instance a node names, if it is a card's.
pub(super) fn accessibility_card_instance(id: NodeId) -> Option<u64> {
    let instance = id.0 & (ACCESSIBILITY_CARD_ID_MASK - 1);
    (id.0 & !(ACCESSIBILITY_CARD_ID_MASK - 1) == ACCESSIBILITY_CARD_ID_MASK && instance != 0)
        .then_some(instance)
}

/// `text` without placeholder sequences. A label built from outside text,
/// such as a sender's path, reaches a screen reader or a control client and
/// must never carry a card's marks.
pub(super) fn without_card_marks(text: String) -> String {
    match kettle_core::strip_placeholders(&text) {
        std::borrow::Cow::Borrowed(_) => text,
        std::borrow::Cow::Owned(stripped) => stripped,
    }
}

/// A card's button for assistive technology: named by its item's title and
/// the viewer's detail line, never by its nonce, at the part of the card on
/// screen. Clicking it opens the item in the viewer.
pub(super) fn card_accessibility_node(
    rect: [f32; 4],
    title: &str,
    detail: &str,
    tr: &kettle_i18n::Translator,
) -> Node {
    let mut node = Node::new(Role::Button);
    node.set_label(without_card_marks(tr.card_a11y(title, detail)));
    node.set_description(tr.text(kettle_i18n::Text::CardA11yOpen));
    node.add_action(AccessibilityAction::Focus);
    node.add_action(AccessibilityAction::Click);
    let [x, y, width, height] = rect;
    node.set_bounds(accesskit::Rect::new(
        f64::from(x),
        f64::from(y),
        f64::from(x + width),
        f64::from(y + height),
    ));
    node
}

pub(super) fn accessibility_modal_id(kind: u64, index: usize) -> NodeId {
    NodeId(ACCESSIBILITY_MODAL_ID_MASK | (kind << 16) | index as u64)
}

pub(super) fn accessibility_confirm_button_index(id: NodeId) -> Option<usize> {
    let first = accessibility_modal_id(1, 1).0;
    let raw = id.0.checked_sub(first)?;
    (raw < (1 << 16) - 1).then_some(raw as usize)
}

pub(super) fn accessibility_context_menu_item_index(id: NodeId, ws: &WindowState) -> Option<usize> {
    if ws.confirm_dialog.is_some()
        || ws.palette_input.is_some()
        || ws.theme_picker_input.is_some()
        || ws.layout_picker_input.is_some()
        || ws.ssh_input.is_some()
    {
        return None;
    }
    let raw = id.0.checked_sub(accessibility_modal_id(2, 1).0)?;
    if raw >= (1 << 16) - 1 {
        return None;
    }
    let index = raw as usize;
    ws.context_menu
        .as_ref()?
        .items
        .get(index)
        .is_some_and(item_is_dispatchable)
        .then_some(index)
}

pub(super) struct ModalAccessibilityProjection {
    pub(super) roots: Vec<NodeId>,
    pub(super) nodes: Vec<(NodeId, Node)>,
    pub(super) focus: Option<NodeId>,
}

/// The index of a modal text input's text run, above every row index.
const ACCESSIBILITY_MODAL_TEXT_RUN_INDEX: usize = 0xffff;

/// Give a modal text input its text as a text run, with the caret or the
/// selection when `field` is that input's projection. AT-SPI exposes text
/// only through text ranges, so without the run Orca announces the field as
/// an empty "entry" whatever it holds.
fn push_text_input_run(
    nodes: &mut Vec<(NodeId, Node)>,
    input: &mut Node,
    run_id: NodeId,
    text: &str,
    field: Option<&kettle_render::TextFieldOverlay>,
) {
    let mut run = Node::new(Role::TextRun);
    run.set_value(text);
    run.set_character_lengths(
        text.chars()
            .map(|character| character.len_utf8() as u8)
            .collect::<Vec<_>>(),
    );
    input.set_children([run_id]);
    if let Some(field) = field.filter(|field| field.text == text) {
        let character_index =
            |byte: usize| text.char_indices().take_while(|(at, _)| *at < byte).count();
        let focus = field.cursor_byte;
        let anchor = match field.selection {
            Some((start, end)) if start != end => {
                if focus == start {
                    end
                } else {
                    start
                }
            }
            _ => focus,
        };
        input.set_text_selection(TextSelection {
            anchor: TextPosition {
                node: run_id,
                character_index: character_index(anchor),
            },
            focus: TextPosition {
                node: run_id,
                character_index: character_index(focus),
            },
        });
    }
    nodes.push((run_id, run));
}

fn push_picker_accessibility(
    projection: &mut ModalAccessibilityProjection,
    overlay: &Overlay,
    kind: u64,
    label: &str,
    query: &str,
    full_bounds: accesskit::Rect,
    tr: &kettle_i18n::Translator,
) {
    let menu = overlay.context_menu.as_ref();
    let field = overlay.text_field.as_ref();
    let container_id = accessibility_modal_id(kind, 0);
    let input_id = accessibility_modal_id(kind, 1);
    let list_id = accessibility_modal_id(kind, 2);
    let mut input = Node::new(Role::TextInput);
    input.set_label(label);
    input.set_value(query);
    push_text_input_run(
        &mut projection.nodes,
        &mut input,
        accessibility_modal_id(kind, ACCESSIBILITY_MODAL_TEXT_RUN_INDEX),
        query,
        field,
    );
    projection.nodes.push((input_id, input));

    let mut list_children = Vec::new();
    let mut selected = None;
    if let Some(menu) = menu {
        for (index, row) in menu.rows.iter().enumerate() {
            if row.separator {
                continue;
            }
            let id = accessibility_modal_id(kind, index + 3);
            let mut option = Node::new(Role::ListBoxOption);
            option.set_label(row.label.clone());
            // The painted hint, such as a shortcut or a theme's appearance.
            if !row.hint.is_empty() {
                option.set_description(row.hint.clone());
            }
            if !row.enabled {
                option.set_disabled();
            }
            option.set_selected(index == menu.highlight);
            if index == menu.highlight {
                selected = Some(id);
            }
            list_children.push(id);
            projection.nodes.push((id, option));
        }
    }
    let mut list = Node::new(Role::ListBox);
    list.set_label(tr.picker_a11y_results(label));
    list.set_children(list_children);
    projection.nodes.push((list_id, list));

    let mut dialog = Node::new(Role::Dialog);
    dialog.set_label(label);
    dialog.set_children([input_id, list_id]);
    dialog.set_bounds(full_bounds);
    projection.roots.push(container_id);
    projection.nodes.push((container_id, dialog));
    if projection.focus.is_none() {
        projection.focus = selected.or(Some(input_id));
    }
}

pub(super) fn modal_accessibility_projection(
    overlay: &Overlay,
    surface: (f32, f32),
    metrics: (f32, f32),
    scale_factor: f32,
    tr: &kettle_i18n::Translator,
) -> ModalAccessibilityProjection {
    let full_bounds = accesskit::Rect::new(0.0, 0.0, f64::from(surface.0), f64::from(surface.1));
    let mut projection = ModalAccessibilityProjection {
        roots: Vec::new(),
        nodes: Vec::new(),
        focus: None,
    };

    if let Some(dialog) = overlay.confirm_dialog.as_ref() {
        let layout =
            kettle_render::ConfirmDialogLayout::new(surface, metrics, scale_factor, dialog);
        let bounds = |(x, y, w, h): (f32, f32, f32, f32)| {
            accesskit::Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64)
        };
        let container_id = accessibility_modal_id(1, 0);
        let mut children = Vec::with_capacity(dialog.buttons.len());
        for (index, button) in dialog.buttons.iter().enumerate() {
            let id = accessibility_modal_id(1, index + 1);
            let mut node = Node::new(Role::Button);
            node.set_label(button.label.clone());
            if let Some(rect) = layout.buttons.get(index) {
                node.set_bounds(bounds(*rect));
            }
            node.add_action(AccessibilityAction::Focus);
            node.add_action(AccessibilityAction::Click);
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut node = Node::new(Role::AlertDialog);
        node.set_label(dialog.heading.clone());
        node.set_description(dialog.body.clone());
        node.set_modal();
        if layout.max_scroll > 0 {
            node.add_action(AccessibilityAction::ScrollUp);
            node.add_action(AccessibilityAction::ScrollDown);
        }
        node.set_children(children.clone());
        node.set_bounds(bounds(layout.rect));
        projection.roots.push(container_id);
        projection.nodes.push((container_id, node));
        projection.focus = children
            .get(dialog.focus_idx.min(children.len().saturating_sub(1)))
            .copied()
            .or(Some(container_id));
        return projection;
    }

    let picker_open = overlay.palette_query.is_some()
        || overlay.theme_picker_query.is_some()
        || overlay.layout_picker_query.is_some()
        || overlay.ssh_query.is_some();
    if !picker_open && let Some(menu) = overlay.context_menu.as_ref() {
        let container_id = accessibility_modal_id(2, 0);
        let mut children = Vec::new();
        let mut highlighted = None;
        for (index, row) in menu.rows.iter().enumerate() {
            if row.separator {
                continue;
            }
            let id = accessibility_modal_id(2, index + 1);
            let mut node = Node::new(Role::MenuItem);
            node.set_label(row.label.clone());
            if !row.hint.is_empty() {
                node.set_description(row.hint.clone());
            }
            if !row.enabled {
                node.set_disabled();
            } else {
                node.add_action(AccessibilityAction::Focus);
                node.add_action(AccessibilityAction::Click);
            }
            if index == menu.highlight {
                highlighted = Some(id);
            }
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut node = Node::new(Role::Menu);
        node.set_label(
            menu.a11y_label
                .clone()
                .unwrap_or_else(|| tr.text(kettle_i18n::Text::MenuA11yTerminal).to_string()),
        );
        node.set_children(children);
        node.set_bounds(full_bounds);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, node));
        if projection.focus.is_none() {
            projection.focus = highlighted.or(Some(container_id));
        }
    }

    if !overlay.hint_labels.is_empty() {
        let container_id = accessibility_modal_id(3, 0);
        let mut children = Vec::with_capacity(overlay.hint_labels.len());
        for (index, hint) in overlay.hint_labels.iter().enumerate() {
            let id = accessibility_modal_id(3, index + 1);
            let mut node = Node::new(Role::ListBoxOption);
            node.set_label(hint.label.clone());
            if hint.dim {
                node.set_disabled();
            }
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut node = Node::new(Role::ListBox);
        node.set_label(tr.text(kettle_i18n::Text::HintA11yList));
        node.set_children(children);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, node));
        if projection.focus.is_none() {
            projection.focus = Some(container_id);
        }
    }

    if let Some(query) = overlay.palette_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay,
            4,
            tr.text(kettle_i18n::Text::PickerA11yPalette),
            query,
            full_bounds,
            tr,
        );
    }
    if let Some(query) = overlay.theme_picker_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay,
            10,
            tr.text(kettle_i18n::Text::PickerA11yThemes),
            query,
            full_bounds,
            tr,
        );
    }

    if let Some(settings) = overlay.settings.as_ref() {
        let container_id = accessibility_modal_id(5, 0);
        let list_id = accessibility_modal_id(5, 1);
        let mut children = Vec::with_capacity(settings.rows.len());
        for (index, row) in settings.rows.iter().enumerate() {
            let id = accessibility_modal_id(5, index + 2);
            let mut node = Node::new(Role::ListBoxOption);
            node.set_label(row.label.clone());
            node.set_value(row.value.clone());
            // AT-SPI exposes a string value only through text ranges, so Orca
            // would read the row's name alone; there the value is also the
            // description, which Orca speaks after the name. macOS and
            // Windows read the value itself.
            #[cfg(all(unix, not(target_os = "macos")))]
            node.set_description(row.value.clone());
            if row.disabled {
                node.set_disabled();
            }
            node.set_selected(index == settings.focused_row);
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut list = Node::new(Role::ListBox);
        list.set_label(tr.text(kettle_i18n::Text::SettingsA11yFields));
        list.set_children(children.clone());
        projection.nodes.push((list_id, list));
        let mut dialog = Node::new(Role::Dialog);
        dialog.set_label(match settings.categories.get(settings.active_category) {
            Some(category) => tr.settings_a11y_dialog(category),
            None => tr.text(kettle_i18n::Text::SettingsA11yTitle).to_string(),
        });
        dialog.set_children([list_id]);
        dialog.set_bounds(full_bounds);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, dialog));
        if projection.focus.is_none() {
            projection.focus = children
                .get(settings.focused_row.min(children.len().saturating_sub(1)))
                .copied()
                .or(Some(container_id));
        }
    }

    if let Some(query) = overlay.layout_picker_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay,
            6,
            tr.text(kettle_i18n::Text::PickerA11yLayouts),
            query,
            full_bounds,
            tr,
        );
    }
    if let Some(query) = overlay.ssh_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay,
            7,
            tr.text(kettle_i18n::Text::PickerA11ySsh),
            query,
            full_bounds,
            tr,
        );
    }

    if let Some(edit) = overlay.edit_title.as_ref() {
        let container_id = accessibility_modal_id(8, 0);
        let input_id = accessibility_modal_id(8, 1);
        let mut input = Node::new(Role::TextInput);
        input.set_label(edit.label.clone());
        input.set_value(edit.input.clone());
        push_text_input_run(
            &mut projection.nodes,
            &mut input,
            accessibility_modal_id(8, ACCESSIBILITY_MODAL_TEXT_RUN_INDEX),
            &edit.input,
            overlay.text_field.as_ref(),
        );
        projection.nodes.push((input_id, input));
        let mut dialog = Node::new(Role::Dialog);
        dialog.set_label(edit.label.clone());
        dialog.set_children([input_id]);
        dialog.set_bounds(full_bounds);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, dialog));
        if projection.focus.is_none() {
            projection.focus = Some(input_id);
        }
    }

    projection
}

#[derive(Clone)]
struct AccessibilityActivation {
    initial: TreeUpdate,
    proxy: EventLoopProxy<UserEvent>,
    window_seq: u64,
}

impl ActivationHandler for AccessibilityActivation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let _ = self.proxy.send_event(UserEvent::AccessibilityActivated {
            window_seq: self.window_seq,
        });
        Some(self.initial.clone())
    }
}

struct AccessibilityActions {
    proxy: EventLoopProxy<UserEvent>,
    window_seq: u64,
}

impl ActionHandler for AccessibilityActions {
    fn do_action(&mut self, request: ActionRequest) {
        let _ = self.proxy.send_event(UserEvent::AccessibilityAction {
            window_seq: self.window_seq,
            request,
        });
    }
}

struct AccessibilityDeactivation;

impl DeactivationHandler for AccessibilityDeactivation {
    fn deactivate_accessibility(&mut self) {}
}

pub(super) fn media_receipt_accessibility_label(
    kind: &kettle_render::MediaPasteReceiptKind,
    remote: bool,
    tr: &kettle_i18n::Translator,
) -> String {
    let mut label = match kind {
        kettle_render::MediaPasteReceiptKind::Image {
            original_width,
            original_height,
        } => tr.receipt_a11y_image(u64::from(*original_width), u64::from(*original_height)),
        kettle_render::MediaPasteReceiptKind::Video {
            extension,
            size,
            count,
            ..
        } if *count > 1 => tr.receipt_a11y_videos(
            *count as u64,
            extension,
            &kettle_render::format_media_size(*size),
        ),
        kettle_render::MediaPasteReceiptKind::Video {
            extension, size, ..
        } => tr.receipt_a11y_video(extension, &kettle_render::format_media_size(*size)),
    };
    if remote {
        label.push_str(", ");
        label.push_str(tr.text(kettle_i18n::Text::ReceiptA11yRemote));
    }
    label
}
