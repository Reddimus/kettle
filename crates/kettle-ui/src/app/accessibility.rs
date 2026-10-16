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
            },
            AccessibilityActions { proxy, window_seq },
            AccessibilityDeactivation,
        )
    }

    pub(super) fn accessibility_tree(&self, ws: &WindowState, overlay: &Overlay) -> TreeUpdate {
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
                        kettle_render::MediaLaneHit::PreviousPage => self
                            .ui_text
                            .text(kettle_i18n::Text::MediaLaneA11yPreviousDiagram),
                        kettle_render::MediaLaneHit::NextPage => self
                            .ui_text
                            .text(kettle_i18n::Text::MediaLaneA11yNextDiagram),
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
                        kettle_render::MediaLaneHit::Resize
                        | kettle_render::MediaLaneHit::Scrub
                        | kettle_render::MediaLaneHit::Inside => continue,
                    };
                    let mut button = Node::new(Role::Button);
                    button.set_label(text);
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
            && let Some(geometry) = kettle_render::media_paste_receipt_geometry(
                receipt,
                overlay.completion.as_ref(),
                self.menu_cell(ws),
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            )
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
        // The update chip: a button under the window root, never the focus,
        // so a screen reader stays on the terminal. Its name is stable for a
        // given notice, so the polite announcement comes once when it appears.
        if let Some(notice) = self.update_notice()
            && let Some(chip) = self.tab_bar(ws).update_chip
        {
            let mut node = Node::new(Role::Button);
            node.set_label(crate::update_notice::a11y_label(&notice, &self.ui_text));
            node.set_live(accesskit::Live::Polite);
            node.add_action(AccessibilityAction::Click);
            let (x, y, width, height) = chip.rect;
            node.set_bounds(accesskit::Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + width),
                f64::from(y + height),
            ));
            children.push(ACCESSIBILITY_UPDATE_CHIP_ID);
            nodes.push((ACCESSIBILITY_UPDATE_CHIP_ID, node));
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
        let modal = modal_accessibility_projection(overlay, surface, &self.ui_text);
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
        let focus = modal_focus.unwrap_or_else(|| {
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
            dialog.prompt.hash(&mut hasher);
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
        self.update_notice().hash(&mut hasher);
        if let Some(chip) = self.tab_bar(ws).update_chip {
            for component in [chip.rect.0, chip.rect.1, chip.rect.2, chip.rect.3] {
                component.to_bits().hash(&mut hasher);
            }
        }
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
            if let Some(geometry) = kettle_render::media_paste_receipt_geometry(
                receipt,
                overlay.completion.as_ref(),
                self.menu_cell(ws),
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            ) {
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
        for lane in &overlay.media_lanes {
            lane.pane.hash(&mut hasher);
            lane.collapsed.hash(&mut hasher);
            lane.open_outside.hash(&mut hasher);
            // The play and sound buttons' names follow what they do.
            lane.play.hash(&mut hasher);
            lane.sound.hash(&mut hasher);
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
        if let Some(index) = accessibility_confirm_button_index(request.target_node) {
            let Some(dialog) = ws.confirm_dialog.as_mut() else {
                return;
            };
            if index >= dialog.buttons.len() {
                return;
            }
            match request.action {
                AccessibilityAction::Focus => dialog.focus_idx = index,
                AccessibilityAction::Click => {
                    let confirm = matches!(dialog.buttons[index], ConfirmButton::Confirm { .. });
                    if confirm {
                        let action = dialog.on_confirm.clone();
                        ws.confirm_dialog = None;
                        self.dispatch_confirm_action(ws, action, event_loop);
                    } else {
                        self.cancel_confirm_dialog(ws);
                    }
                }
                _ => return,
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
        if request.target_node == ACCESSIBILITY_UPDATE_CHIP_ID {
            if request.action == AccessibilityAction::Click
                && let Some(chip) = self.tab_bar(ws).update_chip
            {
                let anchor = update_chip_menu_anchor(chip.rect, self.cfg.tab_bar_pos);
                self.open_update_menu(ws, anchor);
            }
            return;
        }
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
