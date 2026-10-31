//! Context menus, the new-tab menu and the About panel.

use super::*;

impl ContextMenuState {
    pub(super) fn preview_target(&self) -> Option<PreviewMenuTarget> {
        self.items.iter().find_map(|item| match item {
            ContextMenuItem::Preview { target, .. } => Some(*target),
            _ => None,
        })
    }

    fn enter_submenu(&mut self, index: usize) -> bool {
        let nested = match self.items.get(index) {
            Some(ContextMenuItem::Submenu { items, .. }) if !items.is_empty() => items.clone(),
            _ => return false,
        };
        self.drill_stack.push(ContextMenuLevel {
            items: std::mem::replace(&mut self.items, nested),
            highlight: index,
            scroll_offset: self.scroll_offset,
        });
        self.highlight = self
            .items
            .iter()
            .position(item_is_dispatchable)
            .unwrap_or(0);
        self.scroll_offset = 0;
        self.typeahead_buf.clear();
        self.typeahead_until = None;
        true
    }

    fn leave_submenu(&mut self) -> bool {
        let Some(parent) = self.drill_stack.pop() else {
            return false;
        };
        self.items = parent.items;
        self.highlight = parent.highlight;
        self.scroll_offset = parent.scroll_offset;
        self.typeahead_buf.clear();
        self.typeahead_until = None;
        true
    }
}

pub(super) fn preview_menu_items(
    target: PreviewMenuTarget,
    lane: &kettle_render::MediaLanePanel,
    content_visible: bool,
    outside: Option<crate::media::Outside>,
    tr: kettle_i18n::Translator,
) -> Vec<ContextMenuItem> {
    use kettle_i18n::Text as T;
    use kettle_render::{
        MediaLaneHit as H, MediaLaneMode as M, MediaLanePlay as P, MediaLaneSound as S,
    };

    let row = |label, action| ContextMenuItem::Preview {
        label,
        target,
        action,
    };
    // Keep supported actions available when toolbar buttons overflow.
    let video = lane.play.is_some() && lane.mode == M::Rendered;
    let pinning = lane.pin_label.is_some();
    let zooms = content_visible
        && lane.mode == M::Rendered
        && lane.image.is_some()
        && lane.frame.is_none()
        && !video;
    let mut items = Vec::with_capacity(16);
    if lane.copy {
        let label = if lane.mode == M::Source {
            T::MediaPreviewMenuCopySource
        } else if video {
            T::MediaPreviewMenuCopyFrame
        } else if lane.document {
            T::MediaPreviewMenuCopyPage
        } else {
            T::MediaPreviewMenuCopyImage
        };
        items.push(row(tr.text(label), PreviewMenuAction::Copy(lane.mode)));
    }
    if lane.mode == M::Rendered && lane.source.is_some() {
        items.push(row(
            tr.text(T::MediaPreviewMenuCopySource),
            PreviewMenuAction::Copy(M::Source),
        ));
    }
    if lane.open_outside
        && let Some(outside) = outside
    {
        items.push(row(
            outside.label(&tr),
            PreviewMenuAction::OpenOutside(outside),
        ));
    }
    items.push(ContextMenuItem::Separator);
    let controls = [
        (
            lane.source.is_some(),
            if lane.mode == M::Source {
                T::MediaLaneA11yRendered
            } else {
                T::MediaLaneA11ySource
            },
            H::Mode,
        ),
        (
            content_visible && lane.page.is_some_and(|(index, _)| index > 1),
            if lane.document {
                T::MediaLaneA11yPreviousPage
            } else {
                T::MediaLaneA11yPreviousDiagram
            },
            H::PreviousPage,
        ),
        (
            content_visible && lane.page.is_some_and(|(index, count)| index < count),
            if lane.document {
                T::MediaLaneA11yNextPage
            } else {
                T::MediaLaneA11yNextDiagram
            },
            H::NextPage,
        ),
        (
            lane.play.is_some(),
            match lane.play {
                Some(P::Pause) => T::MediaLaneA11yPauseVideo,
                Some(P::Stop) => T::MediaLaneA11yStopPreview,
                _ => T::MediaLaneA11yPlayVideo,
            },
            H::Play,
        ),
        (
            lane.sound.is_some(),
            if lane.sound == Some(S::Heard) {
                T::MediaLaneA11yMuteVideo
            } else {
                T::MediaLaneA11yUnmuteVideo
            },
            H::Sound,
        ),
        (video && pinning, T::MediaPreviewMenuTheater, H::Theater),
        (video && pinning, T::PaletteToggleFullscreen, H::Fullscreen),
        (zooms, T::MediaLaneA11yZoomIn, H::ZoomIn),
        (zooms, T::MediaLaneA11yZoomOut, H::ZoomOut),
        (zooms, T::MediaPreviewMenuFit, H::Fit),
    ];
    for (shown, label, hit) in controls {
        if shown {
            let action = match hit {
                H::Play => {
                    let Some(play) = lane.play else { continue };
                    PreviewMenuAction::Play(play)
                }
                H::Sound => {
                    let Some(sound) = lane.sound else { continue };
                    PreviewMenuAction::Sound(sound)
                }
                _ => PreviewMenuAction::Control(hit),
            };
            items.push(row(tr.text(label), action));
        }
    }
    items.push(ContextMenuItem::Separator);
    let (label, action) = if pinning {
        (T::MediaViewerA11yPin, PreviewMenuAction::Control(H::Pin))
    } else if lane.collapsed {
        (T::MediaLaneA11yExpand, PreviewMenuAction::Expanded(true))
    } else {
        (T::MediaLaneA11yCollapse, PreviewMenuAction::Expanded(false))
    };
    items.push(row(tr.text(label), action));
    items.push(row(
        tr.text(T::MediaLaneA11yClose),
        PreviewMenuAction::Control(H::Close),
    ));
    items
}

impl App {
    pub(super) fn preview_menu_target_is_current(
        &self,
        ws: &WindowState,
        target: PreviewMenuTarget,
    ) -> bool {
        preview_item(ws, target.pane)
            .zip(ws.preview_panels.get(&target.pane))
            .is_some_and(|(item, panel)| target.matches(item, panel))
            && self.preview_panel_of(ws, target.pane).is_some()
    }

    pub(super) fn open_preview_menu(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        at: Option<(f32, f32)>,
    ) -> bool {
        let Some((item, panel)) = preview_item(ws, pane).zip(ws.preview_panels.get(&pane)) else {
            return false;
        };
        let target = PreviewMenuTarget {
            pane,
            item: item.id,
            generation: item.generation,
            mode: lane_mode(item, panel),
        };
        let outside = self.preview_opens_outside(ws, pane);
        let Some(lane) = self.preview_panel_of(ws, pane) else {
            return false;
        };
        let Some(geometry) = self.preview_lane_geometry(ws, &lane) else {
            return false;
        };
        let (x, y) = at.unwrap_or_else(|| {
            let (x, y, w, h) = geometry.content;
            (x + w / 2.0, y + h / 2.0)
        });
        let items = preview_menu_items(target, &lane, geometry.full, outside, self.ui_text);
        self.show_context_menu(ws, items, x, y);
        ws.context_menu.is_some()
    }

    pub(super) fn act_on_preview_menu(
        &mut self,
        ws: &mut WindowState,
        target: PreviewMenuTarget,
        action: PreviewMenuAction,
    ) {
        if !self.preview_menu_target_is_current(ws, target) {
            return;
        }
        match action {
            PreviewMenuAction::Copy(mode) => self.copy_preview_in_mode(ws, target.pane, mode),
            PreviewMenuAction::Control(hit) => self.act_on_preview(ws, target.pane, hit),
            PreviewMenuAction::Expanded(expanded) => {
                self.set_preview_expanded(ws, target.pane, expanded);
            }
            PreviewMenuAction::Play(expected) => {
                if self
                    .preview_panel_of(ws, target.pane)
                    .and_then(|lane| lane.play)
                    == Some(expected)
                {
                    self.act_on_preview(ws, target.pane, kettle_render::MediaLaneHit::Play);
                }
            }
            PreviewMenuAction::Sound(expected) => {
                if self
                    .preview_panel_of(ws, target.pane)
                    .and_then(|lane| lane.sound)
                    == Some(expected)
                {
                    self.act_on_preview(ws, target.pane, kettle_render::MediaLaneHit::Sound);
                }
            }
            PreviewMenuAction::OpenOutside(outside) => {
                if let Some(item) = preview_item(ws, target.pane) {
                    self.open_item_outside(item, outside);
                }
            }
        }
    }

    /// Build the right-click context-menu item list. Each `Item`'s
    /// `enabled` flag is computed from current state: Copy needs a
    /// selection, the split rows need an unzoomed tab, and Ungroup needs
    /// any pane in the active tab to be in a group. `show_context_menu` runs the list
    /// through `filter_disabled`, which hides disabled rows and the
    /// separators they would orphan (Terminator-style) instead of greying
    /// them out. That cuts visual clutter and leaves every visible row
    /// actionable.
    pub(super) fn context_menu_items(&mut self, ws: &mut WindowState) -> Vec<ContextMenuItem> {
        let has_selection = ws
            .mux
            .focused()
            .and_then(|p| p.term.term.lock().ok().map(|t| t.selection.is_some()))
            .unwrap_or(false);
        let has_group = ws
            .mux
            .tab_pane_ids(ws.mux.active)
            .iter()
            .filter_map(|id| ws.mux.panes.get(id))
            .any(|pane| {
                pane.group_name
                    .as_ref()
                    .is_some_and(|group| !group.is_empty())
            });
        // Terminator parity, terminal_popup_menu.py "Read only":
        // checked while the focused pane drops user input.
        let read_only = ws.mux.focused().map(|p| p.read_only).unwrap_or(false);
        // Terminator parity, terminal_popup_menu.py: the split rows are offered
        // only when NOT zoomed — a zoomed pane is showing alone, so splitting it
        // has nothing to put the new pane beside. The zoom row itself swaps
        // label with the state, the way Terminator's Zoom/Restore pair does.
        let zoomed = ws
            .mux
            .tabs
            .get(ws.mux.active)
            .map(|t| t.zoomed)
            .unwrap_or(false);
        use kettle_i18n::Text as T;
        let tr = self.ui_text;
        vec![
            ContextMenuItem::Item {
                label: tr.text(T::MenuCopy),
                action: Action::Copy,
                enabled: has_selection,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuPaste),
                action: Action::Paste,
                enabled: true,
            },
            ContextMenuItem::Separator,
            // `Split Auto` splits along the pane's longer axis — Terminator
            // offers it first because it is the one that needs no decision.
            ContextMenuItem::Item {
                label: tr.text(T::MenuSplitAuto),
                action: Action::SplitAuto,
                enabled: !zoomed,
            },
            // Ghostty's four sides, in its order: Right, Left, Down, Up.
            ContextMenuItem::Item {
                label: tr.text(T::MenuSplitRight),
                action: Action::SplitRight,
                enabled: !zoomed,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuSplitLeft),
                action: Action::SplitLeft,
                enabled: !zoomed,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuSplitDown),
                action: Action::SplitDown,
                enabled: !zoomed,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuSplitUp),
                action: Action::SplitUp,
                enabled: !zoomed,
            },
            ContextMenuItem::DynamicItem {
                label: tr
                    .text(if zoomed { T::MenuRestore } else { T::MenuZoom })
                    .to_string(),
                action: Action::ToggleZoom,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuClosePane),
                action: Action::ClosePane,
                enabled: true,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: tr.text(T::MenuNewTab),
                action: Action::NewTab,
                enabled: true,
            },
            // Ghostty's Tab and Window submenu rows, flattened.
            ContextMenuItem::Item {
                label: tr.text(T::MenuCloseTab),
                action: Action::CloseTab,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuNewWindow),
                action: Action::NewWindow,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuCloseWindow),
                action: Action::CloseWindow,
                enabled: true,
            },
            // Terminator parity, terminal_popup_menu.py "Set Window Title".
            ContextMenuItem::Item {
                label: tr.text(T::MenuSetWindowTitle),
                action: Action::EditWindowTitle,
                enabled: true,
            },
            // The same editors as a double-click on a tab or a pane titlebar.
            ContextMenuItem::Item {
                label: tr.text(T::MenuSetTabTitle),
                action: Action::EditTabTitle,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuSetPaneTitle),
                action: Action::EditPaneTitle,
                enabled: true,
            },
            ContextMenuItem::Separator,
            // Ghostty's "Reset Terminal": RIS on this pane's terminal.
            ContextMenuItem::Item {
                label: tr.text(T::MenuResetTerminal),
                action: Action::Reset,
                enabled: true,
            },
            // Terminator parity: per-pane read-only toggle. The
            // check marker mirrors the Preferences-submenu convention
            // ("✓ on / off"); dispatch goes through the same
            // `Action::TogglePaneReadOnly` the keybind uses.
            ContextMenuItem::DynamicItem {
                label: format!(
                    "{}{}",
                    if read_only { "✓ " } else { "  " },
                    tr.text(T::MenuReadOnly)
                ),
                action: Action::TogglePaneReadOnly,
                enabled: true,
            },
            // Right-click entries for the broadcast-group surface. Layered
            // below the close-family + new-tab so they don't
            // hijack muscle memory; users who never use groups
            // see them at the bottom and can ignore.
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: tr.text(T::MenuSetGroup),
                action: Action::CreateGroup,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuGroupTab),
                action: Action::GroupTab,
                enabled: true,
            },
            ContextMenuItem::Item {
                label: tr.text(T::MenuUngroupTab),
                action: Action::UngroupTab,
                enabled: has_group,
            },
        ]
    }

    /// Terminator parity, `custom_commands.py`: append every
    /// `menu-item = LABEL = CMD` config entry to the context-menu item list,
    /// preceded by a separator. `open_context_menu` calls it after the
    /// built-in items and before the Lua-registered items, matching the
    /// layered priority: kettle's own, then the user's config file, then
    /// their Lua plugins.
    pub(super) fn append_config_menu_items(&self, items: &mut Vec<ContextMenuItem>) {
        if self.cfg.menu_items.is_empty() {
            return;
        }
        items.push(ContextMenuItem::Separator);
        for mi in &self.cfg.menu_items {
            items.push(ContextMenuItem::ConfigItem {
                label: mi.label.clone(),
                command: mi.command.clone(),
            });
        }
    }

    /// Append runtime settings to the Preferences submenu. Each action updates
    /// the live config and persists through `persist_config_toggle`; radio and
    /// check marks are labels rather than separate state.
    pub(super) fn append_preferences_submenu_items(&self, items: &mut Vec<ContextMenuItem>) {
        use kettle_i18n::Text as T;
        let tr = &self.ui_text;
        items.push(ContextMenuItem::Separator);
        let mut inner: Vec<ContextMenuItem> = Vec::new();
        let r = |sel: bool| if sel { "● " } else { "○ " };
        let c = |sel: bool| if sel { "✓ " } else { "  " };
        let dyn_item = |label: String, action: Action| ContextMenuItem::DynamicItem {
            label,
            action,
            enabled: true,
        };
        // Scrollbar radio.
        let sb = self.cfg.scrollbar;
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(sb == kettle_config::ScrollbarMode::Always),
                tr.text(T::MenuPrefScrollbarAlways)
            ),
            Action::SetScrollbarAlways,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(sb == kettle_config::ScrollbarMode::Auto),
                tr.text(T::MenuPrefScrollbarAuto)
            ),
            Action::SetScrollbarAuto,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(sb == kettle_config::ScrollbarMode::Never),
                tr.text(T::MenuPrefScrollbarHidden)
            ),
            Action::SetScrollbarNever,
        ));
        inner.push(ContextMenuItem::Separator);
        // Boolean toggles.
        inner.push(dyn_item(
            format!(
                "{}{}",
                c(self.cfg.cursor_blink),
                tr.text(T::MenuPrefCursorBlink)
            ),
            Action::ToggleCursorBlink,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                c(self.cfg.copy_on_select),
                tr.text(T::MenuPrefCopyOnSelect)
            ),
            Action::ToggleCopyOnSelect,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                c(self.cfg.mouse_hide_while_typing),
                tr.text(T::MenuPrefMouseHide)
            ),
            Action::ToggleMouseHide,
        ));
        inner.push(ContextMenuItem::Separator);
        // Close-confirmation radio. `always` is worded to say that it also
        // covers the titlebar close, because that gesture ignores the other
        // two policies by design.
        let ask_before_closing = self.cfg.ask_before_closing;
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(ask_before_closing == kettle_config::AskBeforeClosing::Always),
                tr.text(T::MenuPrefConfirmAlways)
            ),
            Action::SetAskBeforeClosingAlways,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(ask_before_closing == kettle_config::AskBeforeClosing::MultipleTerminals),
                tr.text(T::MenuPrefConfirmMultiple)
            ),
            Action::SetAskBeforeClosingMultiple,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(ask_before_closing == kettle_config::AskBeforeClosing::Never),
                tr.text(T::MenuPrefConfirmNever)
            ),
            Action::SetAskBeforeClosingNever,
        ));
        inner.push(ContextMenuItem::Separator);
        // Bell radio.
        let bell = self.cfg.bell;
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(bell == kettle_config::BellMode::Off),
                tr.text(T::MenuPrefBellOff)
            ),
            Action::SetBellOff,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(bell == kettle_config::BellMode::Visual),
                tr.text(T::MenuPrefBellVisual)
            ),
            Action::SetBellVisual,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(bell == kettle_config::BellMode::Attention),
                tr.text(T::MenuPrefBellAttention)
            ),
            Action::SetBellAttention,
        ));
        inner.push(dyn_item(
            format!(
                "{}{}",
                r(bell == kettle_config::BellMode::Both),
                tr.text(T::MenuPrefBellBoth)
            ),
            Action::SetBellBoth,
        ));
        inner.push(ContextMenuItem::Separator);
        // Font size +/- (reuse existing actions).
        inner.push(ContextMenuItem::Item {
            label: tr.text(T::MenuPrefFontBigger),
            action: kettle_config::Action::IncreaseFontSize,
            enabled: true,
        });
        inner.push(ContextMenuItem::Item {
            label: tr.text(T::MenuPrefFontSmaller),
            action: kettle_config::Action::DecreaseFontSize,
            enabled: true,
        });
        inner.push(ContextMenuItem::Separator);
        // The Advanced… escape hatch for everything not exposed as a toggle.
        inner.push(ContextMenuItem::Item {
            label: tr.text(T::MenuPrefAdvanced),
            action: kettle_config::Action::EditConfig,
            enabled: true,
        });
        items.push(ContextMenuItem::Submenu {
            label: tr.text(T::MenuPreferences).to_string(),
            items: inner,
        });
    }

    pub(super) fn append_profile_submenu_items(&self, items: &mut Vec<ContextMenuItem>) {
        let profile_names = kettle_config::Config::list_profiles();
        if profile_names.is_empty() {
            return;
        }
        items.push(ContextMenuItem::Separator);
        let inner: Vec<ContextMenuItem> = profile_names
            .into_iter()
            .map(|name| ContextMenuItem::ProfileChoice {
                label: name.clone(),
                profile: name,
            })
            .collect();
        items.push(ContextMenuItem::Submenu {
            label: self
                .ui_text
                .text(kettle_i18n::Text::MenuProfile)
                .to_string(),
            items: inner,
        });
    }

    /// Append a "Theme…" entry that opens the searchable theme picker, which
    /// replaced a submenu of every bundled theme.
    pub(super) fn append_theme_menu_item(&self, items: &mut Vec<ContextMenuItem>) {
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::Item {
            label: self.ui_text.text(kettle_i18n::Text::MenuThemePicker),
            action: kettle_config::Action::OpenThemePicker,
            enabled: true,
        });
    }

    pub(super) fn append_remote_menu_items(
        &mut self,
        ws: &mut WindowState,
        items: &mut Vec<ContextMenuItem>,
    ) {
        let Some(pane) = ws.mux.focused() else {
            return;
        };
        let Some(ctx) = &pane.remote_context else {
            return;
        };
        // Unreproducible argv suppresses the action. A failed environment
        // capture keeps a refusal action that uses the existing failure notice.
        if let Some(item) = remote_reconnect_item(ctx, &self.ui_text) {
            items.push(ContextMenuItem::Separator);
            items.push(item);
        }
    }

    /// Terminator plugin parity: append every Lua-registered menu item to
    /// the context-menu item list. `open_context_menu` calls it after the
    /// built-in and config-file rows. Each entry's label is shown; clicking
    /// dispatches the registered Lua callback (via
    /// `LuaEngine::invoke_menu_item`).
    pub(super) fn append_lua_menu_items(&self, items: &mut Vec<ContextMenuItem>) {
        if let Some(eng) = &self.lua_engine
            && let Ok(labels) = eng.list_menu_item_labels()
            && !labels.is_empty()
        {
            items.push(ContextMenuItem::Separator);
            for (idx, label) in labels.into_iter().enumerate() {
                items.push(ContextMenuItem::LuaItem {
                    label,
                    lua_idx: idx,
                });
            }
        }
    }

    /// Open the terminal menu, preserving search. Shift+right-click also
    /// offers the file or diagram named by the clipboard (`offer_copied`).
    pub(super) fn open_context_menu(
        &mut self,
        ws: &mut WindowState,
        px: f32,
        py: f32,
        offer_copied: bool,
    ) {
        self.close_modals_for_context_menu(ws);
        // Terminator parity, terminal_popup_menu.py "Open link" /
        // "Copy address": when the right-click landed on a detected
        // hyperlink, lead with the URL rows. The URL is captured NOW — fresh
        // output scrolling the grid between open and click must not retarget
        // the action. `update_links` is keyed so this is a no-op
        // when the viewport hasn't changed since the last scan.
        self.update_links(ws);
        use kettle_i18n::Text as T;
        let tr = self.ui_text;
        // Only offer the rows when the click is INSIDE the
        // focused pane's rect. `cursor_cell` clamps out-of-rect coordinates to
        // the nearest cell (xterm parity — right for mouse reports), which
        // here could surface "Open Link" for a link the user never pointed at
        // (right-click on chrome / another pane mapping into the focused grid).
        let in_focused_pane = self
            .focused_terminal(ws, self.area(ws))
            .is_some_and(|(rx, ry, rw, rh)| px >= rx && px < rx + rw && py >= ry && py < ry + rh);
        let mut items = Vec::new();
        if in_focused_pane && let Some(url) = self.link_at_cursor(ws).map(|l| l.uri.clone()) {
            items.extend(link_menu_rows(&url, links_pane(ws), tr));
        }
        // A selection holding a diagram an agent printed offers it, even
        // one whose wraps will turn out not to join: picking it says why.
        let offers_diagram = |result: &Result<Vec<String>, kettle_core::DiagramCopyError>| {
            !matches!(
                result,
                Err(kettle_core::DiagramCopyError::Empty | kettle_core::DiagramCopyError::NoDiagram)
            )
        };
        // Only the user's own click reads a selection or the clipboard: a
        // control client driving the menu gets neither probed.
        let reads = !self.ctl_driving;
        let selection_pane = ws.selecting_pane.or_else(|| ws.mux.active_focus());
        if reads
            && in_focused_pane
            && let Some(pane) = selection_pane
            && offers_diagram(&self.selected_diagram(ws, pane))
        {
            items.push(ContextMenuItem::Item {
                label: tr.text(T::MenuPreviewSelectedDiagram),
                action: kettle_config::Action::RenderSelectionAsDiagram,
                enabled: true,
            });
            items.push(ContextMenuItem::Separator);
        }
        // Shift+right-click also offers the file the clipboard names, and
        // the diagram it holds. Only then is the clipboard read, so a slow
        // clipboard owner never holds up a plain right-click or a menu
        // opened from the keyboard.
        if reads
            && in_focused_pane
            && offer_copied
            && let Some(pane) = ws.mux.active_focus()
        {
            let file = self.copied_preview(ws, pane).is_some();
            let diagram = offers_diagram(&self.copied_diagram());
            if file {
                items.push(ContextMenuItem::Item {
                    label: tr.text(T::MenuPreviewCopiedFile),
                    action: kettle_config::Action::PreviewClipboardPath,
                    enabled: true,
                });
            }
            if diagram {
                items.push(ContextMenuItem::Item {
                    label: tr.text(T::MenuPreviewCopiedDiagram),
                    action: kettle_config::Action::RenderClipboardAsDiagram,
                    enabled: true,
                });
            }
            if file || diagram {
                items.push(ContextMenuItem::Separator);
            }
        }
        items.extend(self.context_menu_items(ws));
        self.append_config_menu_items(&mut items);
        self.append_lua_menu_items(&mut items);
        // Append the remote-session reconnect entry when the focused pane
        // has a detected SSH/Docker/Podman/kubectl context.
        self.append_remote_menu_items(ws, &mut items);
        self.append_theme_menu_item(&mut items);
        // Same machinery for Profile, appended only when
        // ~/.config/kettle/profiles/ has any *.config files.
        self.append_profile_submenu_items(&mut items);
        // Top-level "Settings…" entry opens the full in-app
        // settings overlay (the richer, keyboard-navigable panel). The
        // Preferences ▸ submenu below stays as the quick-toggle surface.
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::Item {
            label: tr.text(T::MenuSettings),
            action: kettle_config::Action::OpenSettings,
            enabled: true,
        });
        // Preferences submenu: runtime-mutable settings + the Advanced…
        // escape hatch.
        self.append_preferences_submenu_items(&mut items);
        self.show_context_menu(ws, items, px, py);
    }

    /// Preserve search under a terminal menu; close other modals.
    pub(super) fn close_modals_for_context_menu(&mut self, ws: &mut WindowState) {
        if ws.search.open {
            ws.context_menu = None;
        } else {
            self.close_all_modals(ws);
        }
    }

    /// Filter rows, clamp the shared popup and end active pointer gestures.
    pub(super) fn show_context_menu(
        &mut self,
        ws: &mut WindowState,
        items: Vec<ContextMenuItem>,
        px: f32,
        py: f32,
    ) {
        // A menu takes exclusive ownership of pointer motion. Preserve the
        // completed selection itself, but end any gesture that could otherwise
        // keep mutating selection, scrollback, split geometry, or tab layout
        // behind the menu. CursorMoved deliberately preserves the hover-reuse
        // hint, so leaving one of these armed would make the cached snapshot
        // stale without changing output_generation.
        self.finish_selection_gesture(ws);
        ws.context_menu_a11y_label = None;
        ws.scrollbar_drag_offset = None;
        ws.dragging_split = None;
        self.release_held_buttons(ws, |_| true);
        ws.card_press = None;
        ws.last_mouse_cell = None;
        ws.tab_drag_active = false;
        ws.tab_drag_press = None;
        ws.tab_pressed_idx = None;
        ws.detach_drag = crate::detach::DragState::default();
        ws.drag_press = None;
        ws.pane_drag = None;
        ws.lane_drag = None;
        self.end_lane_resize(ws);
        ws.reuse_pane_snapshots_once = false;
        if self.torn_drag.as_ref().is_some_and(|drag| {
            drag.seq == ws.seq
                || drag.carrier == ws.seq
                || drag.dock.is_some_and(|(seq, _)| seq == ws.seq)
        }) {
            self.abandon_torn_drag(Some(ws));
        }

        // Terminator menu UX: every visible row is actionable.
        let items = filter_disabled(items);
        let highlight = items.iter().position(item_is_dispatchable).unwrap_or(0);
        let (cw, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let panel_h: f32 = items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Separator => sep_h,
                ContextMenuItem::Item { .. }
                | ContextMenuItem::DynamicItem { .. }
                | ContextMenuItem::LuaItem { .. }
                | ContextMenuItem::ConfigItem { .. }
                | ContextMenuItem::ReconnectItem { .. }
                | ContextMenuItem::Submenu { .. }
                | ContextMenuItem::WindowChoice { .. }
                | ContextMenuItem::ProfileChoice { .. }
                | ContextMenuItem::NewTabShell { .. }
                | ContextMenuItem::UrlItem { .. }
                | ContextMenuItem::Media { .. }
                | ContextMenuItem::Preview { .. }
                | ContextMenuItem::Info { .. } => row_h,
            })
            .sum();
        // Dropdown parity: the width budget includes each row's
        // right-aligned shortcut hint (+2 spacer columns) — the same formula
        // the renderer's `menu_row_chars` uses, kept in lockstep.
        let hints = self.menu_item_hints(&items);
        let max_columns = items
            .iter()
            .zip(hints.iter())
            .map(|(item, hint)| context_menu_item_columns(item, hint))
            .max()
            .unwrap_or(0) as f32;
        let panel_w = kettle_render::menu::panel_width(max_columns, cw);
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        if !context_menu_surface_can_fit_row((sw, sh), (cw, ch)) {
            ws.context_menu = None;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        let panel = clamp_context_menu_panel((panel_w, panel_h), (sw, sh));
        let anchor = clamp_context_menu_anchor((px, py), panel, (sw, sh));
        let anchor_scale = ws
            .window
            .as_ref()
            .map(|window| window.scale_factor())
            .unwrap_or(1.0);
        ws.context_menu = Some(ContextMenuState {
            anchor,
            anchor_scale,
            items,
            highlight,
            drill_stack: Vec::new(),
            scroll_offset: 0,
            typeahead_buf: String::new(),
            typeahead_until: None,
        });
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// The renderer's FRACTIONAL cell metrics, for menu geometry. `cell_px`
    /// rounds to integers for the PTY grid, but the renderer positions menu
    /// rows with the f32 metrics. The rounded value under-measures the panel
    /// by ~0.5px per row, enough to clip the last row of a 9-row dropdown and
    /// draw a phantom "more rows" scroll marker.
    pub(super) fn menu_cell(&self, ws: &WindowState) -> (f32, f32) {
        ws.renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((8.0, 16.0))
    }

    /// Per-row shortcut hints for a menu, from the
    /// LIVE keybind map (a user rebind shows their actual chord). `Item` /
    /// `DynamicItem` rows look up their own Action; the Nth `NewTabShell`
    /// row maps to `Action::NewTabShell(N)` (the Ctrl+Shift+N family).
    /// Everything else gets no hint. Empty string = none.
    pub(super) fn menu_item_hints(&self, items: &[ContextMenuItem]) -> Vec<String> {
        let mut shell_ordinal: u8 = 0;
        items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Item { action, .. }
                | ContextMenuItem::DynamicItem { action, .. } => {
                    kettle_config::keybinds::hint_label(&self.cfg.keybinds, action)
                        .unwrap_or_default()
                }
                ContextMenuItem::NewTabShell { .. } => {
                    let h = if shell_ordinal < 9 {
                        kettle_config::keybinds::hint_label(
                            &self.cfg.keybinds,
                            &Action::NewTabShell(shell_ordinal),
                        )
                        .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    shell_ordinal = shell_ordinal.saturating_add(1);
                    h
                }
                _ => String::new(),
            })
            .collect()
    }

    /// The new-tab `▾` dropdown's rows — the detected
    /// shells, then Windows Terminal's bottom section (Settings / Command
    /// palette / About) behind a separator. Pure over the shell list so the
    /// menu shape is unit-testable.
    pub(super) fn new_tab_menu_items(
        shells: &[kettle_core::term::ShellChoice],
        tr: &kettle_i18n::Translator,
    ) -> Vec<ContextMenuItem> {
        use kettle_i18n::Text as T;
        let mut items: Vec<ContextMenuItem> = shells
            .iter()
            .cloned()
            .map(|(label, argv)| ContextMenuItem::NewTabShell { label, argv })
            .collect();
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::Item {
            label: tr.text(T::MenuSettings),
            action: Action::OpenSettings,
            enabled: true,
        });
        items.push(ContextMenuItem::Item {
            label: tr.text(T::MenuCommandPalette),
            action: Action::CommandPalette,
            enabled: true,
        });
        items.push(ContextMenuItem::Item {
            label: tr.text(T::MenuAbout),
            action: Action::About,
            enabled: true,
        });
        items
    }

    /// The About panel — version + git hash, update
    /// status, and link rows, reusing the context-menu machinery (Info rows
    /// render dimmed and are not clickable; UrlItem rows copy/open).
    pub(super) fn open_about_panel(&mut self, ws: &mut WindowState) {
        self.close_all_modals(ws);
        use kettle_i18n::Text as T;
        let tr = self.ui_text;
        let v = &self.version_line;
        let mut items = vec![ContextMenuItem::Info {
            label: format!("kettle {v}"),
        }];
        let facts = crate::update_notice::fact_rows(&self.update_facts, &tr);
        if facts.is_empty() {
            items.push(ContextMenuItem::Info {
                label: tr.text(T::AboutUpdateUnknown).to_string(),
            });
        }
        items.extend(
            facts
                .into_iter()
                .map(|label| ContextMenuItem::Info { label }),
        );
        items.extend([
            ContextMenuItem::Separator,
            ContextMenuItem::UrlItem {
                label: tr.text(T::AboutCopyVersion),
                url: format!("kettle {v}"),
                how: UrlHow::Copy,
                pane: None,
            },
            ContextMenuItem::UrlItem {
                label: tr.text(T::AboutOpenGithub),
                url: "https://github.com/Reddimus/kettle".to_string(),
                how: UrlHow::Open,
                pane: None,
            },
        ]);
        if let Some(url) = self
            .update_facts
            .release
            .as_ref()
            .and_then(|release| crate::update_notice::release_url(&release.tag))
        {
            items.push(ContextMenuItem::UrlItem {
                label: tr.text(T::AboutOpenRelease),
                url,
                how: UrlHow::Open,
                pane: None,
            });
        }
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        self.show_context_menu(ws, items, sw * 0.5 - 140.0, sh * 0.4);
        // Opening a menu clears its name, so name the panel afterwards.
        ws.context_menu_a11y_label = Some(tr.text(T::MenuA11yAbout).to_string());
    }

    pub(super) fn shell_menu_items(
        shells: Option<&[kettle_core::term::ShellChoice]>,
        tr: &kettle_i18n::Translator,
    ) -> Vec<ContextMenuItem> {
        match shells {
            Some(shells) => Self::new_tab_menu_items(shells, tr),
            None => vec![ContextMenuItem::Info {
                label: tr.text(kettle_i18n::Text::MenuDetectingShells).to_string(),
            }],
        }
    }

    pub(super) fn shell_menu_is_loading(
        items: &[ContextMenuItem],
        tr: &kettle_i18n::Translator,
    ) -> bool {
        matches!(items, [ContextMenuItem::Info { label }]
            if label == tr.text(kettle_i18n::Text::MenuDetectingShells))
    }

    /// Open the new-tab `▾` dropdown at
    /// `(px, py)` — detected shells (process-cached in kettle-core, prewarmed
    /// at startup) plus the Settings / Command palette / About bottom rows.
    pub(super) fn open_new_tab_menu(&mut self, ws: &mut WindowState, px: f32, py: f32) {
        self.close_all_modals(ws);
        let shells = kettle_core::term::cached_shells();
        let items = Self::shell_menu_items(shells.as_deref(), &self.ui_text);
        self.show_context_menu(ws, items, px, py);
    }

    /// Move the context-menu highlight by `delta` (±1), skipping
    /// `Separator` rows and disabled `Item` rows. Wraps at the ends so
    /// `↑` on the first row jumps to the last enabled row and vice
    /// versa (Chrome / Firefox menu convention). The wrap+skip math lives
    /// in the pure `next_context_menu_highlight`, so it is unit-testable
    /// without App or cursor state.
    pub(super) fn step_context_menu_highlight(&mut self, ws: &mut WindowState, delta: isize) {
        let next = ws
            .context_menu
            .as_ref()
            .map(|m| next_context_menu_highlight(&m.items, m.highlight, delta));
        if let Some(next) = next {
            self.set_context_menu_highlight(ws, next);
        }
    }

    /// Move the context-menu highlight to `next` and sync `scroll_offset` so
    /// it stays visible. `step_context_menu_highlight` and the vim-menu-nav
    /// jumps (`g`/`G`, `Ctrl+d`/`Ctrl+u`) share this scroll-window math.
    pub(super) fn set_context_menu_highlight(&mut self, ws: &mut WindowState, next: usize) {
        let Some(((_, _), (_, panel_h))) = self.context_menu_geometry(ws) else {
            return;
        };
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let Some(menu) = ws.context_menu.as_mut() else {
            return;
        };
        let (_, panel_h) = context_menu_content_geometry(&menu.items, panel_h, row_h, sep_h);
        menu.highlight = next;
        menu.scroll_offset = context_menu_scroll_for_highlight(
            &menu.items,
            menu.scroll_offset,
            next,
            panel_h,
            row_h,
            sep_h,
        );
        ws.reuse_pane_snapshots_once = true;
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Scroll the context-menu by `delta` rows (positive
    /// = down). Clamped so we can't scroll past the last row that
    /// would still fill the visible window.
    pub(super) fn scroll_context_menu(&mut self, ws: &mut WindowState, delta: isize) {
        let Some(((_, _), (_, panel_h))) = self.context_menu_geometry(ws) else {
            return;
        };
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        let Some(menu) = ws.context_menu.as_mut() else {
            return;
        };
        let (_, panel_h) = context_menu_content_geometry(&menu.items, panel_h, row_h, sep_h);
        let new_off = menu.scroll_offset.saturating_add_signed(delta);
        // Clamp: never scroll past the point where the last visible
        // row is the final item.
        let max_off = context_menu_max_scroll_offset(&menu.items, panel_h, row_h, sep_h);
        let clamped = new_off.min(max_off);
        if clamped != menu.scroll_offset {
            menu.scroll_offset = clamped;
            ws.reuse_pane_snapshots_once = true;
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }

    /// Return the row index under the cursor when the context menu
    /// is open, `None` if the cursor is outside the panel OR landed
    /// on a separator. Thin wrapper around the pure `find_menu_row_y`
    /// helper so the row-walk is unit-testable without standing up an
    /// App. Used by `update_menu_highlight_from_cursor` on every
    /// `CursorMoved` so the highlight tracks the pointer the way every
    /// desktop menu does (GTK / macOS NSMenu / Windows).
    pub(super) fn menu_row_at_cursor(&self, ws: &WindowState) -> Option<usize> {
        let menu = ws.context_menu.as_ref()?;
        let ((ax, ay), (panel_w, panel_h)) = self.context_menu_geometry(ws)?;
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if px < ax || px >= ax + panel_w || py < ay || py >= ay + panel_h {
            return None;
        }
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        // Row-walk starts at scroll_offset; only the
        // visible slice is hit-tested. Off-by-one is handled by
        // find_menu_row_y's half-open interval [y, y+h).
        let start = menu.scroll_offset.min(menu.items.len());
        let kinds = menu.items[start..]
            .iter()
            .map(|it| matches!(it, ContextMenuItem::Separator));
        let (inset, content_h) = context_menu_content_geometry(&menu.items, panel_h, row_h, sep_h);
        find_menu_row_y(py, ay + inset, content_h, row_h, sep_h, kinds).map(|i| i + start)
    }

    /// Set `menu.highlight` to whichever row the cursor is
    /// over right now; no-op when the cursor is outside the panel or
    /// on a separator. Called from `CursorMoved`. Requests a redraw
    /// only when the highlight actually changed so we don't churn the
    /// GPU on every sub-pixel motion event.
    pub(super) fn update_menu_highlight_from_cursor(&mut self, ws: &mut WindowState) {
        if let Some(delta) = self.context_menu_scroll_edge(ws) {
            self.scroll_context_menu(ws, delta);
            return;
        }
        let Some(idx) = self.menu_row_at_cursor(ws) else {
            return;
        };
        let Some(menu) = ws.context_menu.as_mut() else {
            return;
        };
        if menu.highlight == idx {
            return;
        }
        menu.highlight = idx;
        ws.reuse_pane_snapshots_once = true;
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Dispatch one resolved context-menu selection. The single sink for
    /// mouse clicks, keyboard Enter / Space, and mnemonic / typeahead
    /// activation so all three behave identically.
    ///
    /// Every *leaf* click closes the menu first, then acts. Only
    /// `DrillIntoSubmenu` keeps the menu open — it replaces the visible
    /// level with the submenu's items (parent pushed onto `drill_stack`).
    ///
    /// Callers must not clear `ws.context_menu` before dispatching. The
    /// `DrillIntoSubmenu` arm needs `ws.context_menu.as_mut()`, so an early
    /// clear would dismiss the whole menu instead of drilling in.
    pub(super) fn dispatch_context_menu_click(
        &mut self,
        ws: &mut WindowState,
        click: ContextMenuClick,
        event_loop: &ActiveEventLoop,
    ) {
        self.sync_pane_focus_reports(ws);
        match click {
            // Keep the menu open: swap the visible level for the submenu.
            ContextMenuClick::DrillIntoSubmenu(idx) => {
                if let Some(menu) = ws.context_menu.as_mut() {
                    menu.enter_submenu(idx);
                }
                let (width, height) = ws
                    .renderer
                    .as_ref()
                    .map(|renderer| renderer.surface_size())
                    .unwrap_or((800, 600));
                self.reclamp_context_menu(ws, (width as f32, height as f32));
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ContextMenuClick::Action(action) => {
                ws.context_menu = None;
                self.handle_action(ws, action, event_loop);
            }
            ContextMenuClick::Media { target, action } => {
                ws.context_menu = None;
                self.act_on_media(ws, target, action);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ContextMenuClick::Preview { target, action } => {
                ws.context_menu = None;
                self.act_on_preview_menu(ws, target, action);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ContextMenuClick::LuaMenuItem(idx) => {
                ws.context_menu = None;
                // Invoke the Lua callback + drain any
                // LuaCommands it queued through the canonical helper.
                if let Some(eng) = &self.lua_engine {
                    eng.invoke_menu_item(idx);
                }
                self.drain_lua_hook_commands(ws, "lua menu-item");
            }
            ContextMenuClick::ConfigCommand(command) => {
                ws.context_menu = None;
                // Terminator parity: write `CMD\n` to the PTY.
                // Acts as the user — a read-only pane drops it.
                let mut bytes = command.into_bytes();
                bytes.push(b'\n');
                self.write_focused_terminal_input(ws, &bytes);
            }
            ContextMenuClick::ReconnectRemote(command) => {
                ws.context_menu = None;
                let pre_focus = self.focus_key(ws);
                let geometry = self.prospective_split_geometry(ws, Dir::Horizontal, false);
                let waker = self.waker();
                match reconnect_remote_session(&mut ws.mux, &self.cfg, geometry, waker, command) {
                    Ok(()) => {
                        self.hoover_groups(ws);
                        self.note_focus_change(ws, pre_focus);
                        if ws.window.is_some() {
                            ws.pending_resize = true;
                        } else {
                            self.resize_all(ws);
                        }
                        self.save_session(ws);
                    }
                    Err(error) => report_split_failure("right", &error, &self.ui_text),
                }
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            ContextMenuClick::MoveTabToWindow { window, panes } => {
                ws.context_menu = None;
                self.move_tab_to_window(ws, window, &panes);
            }
            ContextMenuClick::SetProfile(name) => {
                ws.context_menu = None;
                if let Some(p) = kettle_config::Config::path_for_profile(&name) {
                    self.switch_profile(ws, p);
                }
            }
            ContextMenuClick::NewTabWithArgv(argv) => {
                ws.context_menu = None;
                self.open_tab_with_argv(ws, &argv);
            }
            // Terminator parity: the URL-aware leading rows.
            // Open routes through the `open_url` chain (Lua URL
            // handlers → custom_url_handler → system open, with the
            // `is_safe_url` guard); Copy puts the address on the clipboard.
            ContextMenuClick::Url { url, how, pane } => {
                ws.context_menu = None;
                match how {
                    UrlHow::Copy => {
                        if let Some(cb) = &mut self.clipboard
                            && let Err(e) = cb.set_text(url)
                        {
                            log::warn!("clipboard set_text failed (link address copy): {e}");
                        }
                    }
                    UrlHow::Open => self.open_pane_link(ws, pane, &url),
                    UrlHow::Preview => {
                        if let Some(pane) = pane {
                            self.preview_pane_link(ws, pane, &url);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn context_menu_scroll_edge(&self, ws: &WindowState) -> Option<isize> {
        let menu = ws.context_menu.as_ref()?;
        let ((x, y), (w, h)) = self.context_menu_geometry(ws)?;
        let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if px < x || px >= x + w || py < y || py >= y + h {
            return None;
        }
        let (_, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let (inset, content_h) =
            context_menu_content_geometry(&menu.items, h, row_h, kettle_render::menu::SEP_H);
        if inset <= 0.0 {
            return None;
        }
        if py < y + inset {
            Some(if menu.scroll_offset > 0 { -1 } else { 0 })
        } else if py >= y + inset + content_h {
            let max = context_menu_max_scroll_offset(
                &menu.items,
                content_h,
                row_h,
                kettle_render::menu::SEP_H,
            );
            Some(if menu.scroll_offset < max { 1 } else { 0 })
        } else {
            None
        }
    }

    pub(super) fn context_menu_click_action(
        &self,
        ws: &WindowState,
        bcode: u8,
    ) -> Option<ContextMenuClick> {
        if bcode != 0 {
            return None;
        }
        // Hover and click must resolve through the same clipped-row geometry.
        // A second row walk here would let the blank partial strip below the
        // final fully drawn row dispatch an invisible item.
        let idx = self.menu_row_at_cursor(ws)?;
        let menu = ws.context_menu.as_ref()?;
        // Shared mapper — the same row-to-click table the keyboard Enter /
        // Space and mnemonic paths use, so mouse and keyboard dispatch cannot
        // diverge.
        menu.items
            .get(idx)
            .and_then(|item| item_to_click(item, idx))
    }

    /// `(items, anchor, panel_w, panel_h)` snapshot for the click /
    /// hover hit-tests — returned in pixels so callers don't have to
    /// re-derive the layout. `None` when the menu isn't open.
    pub(super) fn context_menu_geometry(
        &self,
        ws: &WindowState,
    ) -> Option<((f32, f32), (f32, f32))> {
        let menu = ws.context_menu.as_ref()?;
        let (cw, ch) = self.menu_cell(ws);
        let row_h = ch + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        // Natural height: sum every row + separator.
        let natural_h: f32 = menu
            .items
            .iter()
            .map(|it| match it {
                ContextMenuItem::Separator => sep_h,
                _ => row_h,
            })
            .sum();
        // Terminator menu UX: clamp the panel height to the surface so a
        // ~512-entry Theme submenu can't grow off-screen. Reserve 80px of
        // vertical breathing room (40px top + 40px bottom) so the menu
        // doesn't bump into the window edge.
        let (surface_w, surface_h) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        if !context_menu_surface_can_fit_row((surface_w, surface_h), (cw, ch)) {
            return None;
        }
        let max_h = (surface_h - kettle_render::menu::PANEL_BREATHING)
            .max(row_h)
            .min((surface_h - 8.0).max(1.0));
        let panel_h = natural_h.min(max_h);
        // Dropdown parity: include the right-aligned hint budget —
        // kept in lockstep with `show_context_menu` and the renderer's
        // `menu_row_chars`.
        let hints = self.menu_item_hints(&menu.items);
        let max_columns = menu
            .items
            .iter()
            .zip(hints.iter())
            .map(|(item, hint)| context_menu_item_columns(item, hint))
            .max()
            .unwrap_or(0) as f32;
        let natural_w = kettle_render::menu::panel_width(max_columns, cw);
        let (panel_w, panel_h) =
            clamp_context_menu_panel((natural_w, panel_h), (surface_w, surface_h));
        Some((menu.anchor, (panel_w, panel_h)))
    }

    /// Keep an already-open menu reachable when a resize or DPI transition
    /// makes its old bottom/right anchor invalid. The menu's natural geometry
    /// is recomputed from the live fractional font metrics before clamping, so
    /// drawing and hit-testing continue to share one anchor.
    pub(super) fn reclamp_context_menu(&self, ws: &mut WindowState, surface: (f32, f32)) {
        let current_scale = ws
            .window
            .as_ref()
            .map(|window| window.scale_factor())
            .unwrap_or(1.0);
        let previous_scale = ws
            .context_menu
            .as_ref()
            .map(|menu| menu.anchor_scale)
            .unwrap_or(current_scale);
        let Some((anchor, panel)) = self.context_menu_geometry(ws) else {
            ws.context_menu = None;
            return;
        };
        let anchor = scale_context_menu_anchor(anchor, previous_scale, current_scale);
        let anchor = clamp_context_menu_anchor(anchor, panel, surface);
        let (_, cell_h) = self.menu_cell(ws);
        let row_h = cell_h + kettle_render::menu::ROW_PAD;
        let sep_h = kettle_render::menu::SEP_H;
        if let Some(menu) = ws.context_menu.as_mut() {
            let (_, content_h) = context_menu_content_geometry(&menu.items, panel.1, row_h, sep_h);
            menu.anchor = anchor;
            menu.anchor_scale = current_scale;
            menu.scroll_offset = context_menu_scroll_for_highlight(
                &menu.items,
                menu.scroll_offset,
                menu.highlight,
                content_h,
                row_h,
                sep_h,
            );
        }
    }

    /// Build the renderer-side `ContextMenu` slice from the App-side
    /// state. Splits the labels (owned `String`) from the dispatch
    /// actions so the renderer stays Action-agnostic.
    pub(super) fn context_menu_overlay(&self, ws: &WindowState) -> Option<ContextMenu> {
        let menu = ws.context_menu.as_ref()?;
        // Dropdown parity: per-row shortcut hints from the LIVE keybind
        // map (computed per frame; the map only changes on reload).
        let hints = self.menu_item_hints(&menu.items);
        let mut rows: Vec<ContextMenuRow> =
            menu.items
                .iter()
                .zip(hints)
                .map(|(it, hint)| match it {
                    ContextMenuItem::Item { label, enabled, .. } => ContextMenuRow {
                        label: (*label).to_string(),
                        separator: false,
                        enabled: *enabled,
                        hint,
                    },
                    ContextMenuItem::DynamicItem { label, enabled, .. } => ContextMenuRow {
                        label: label.clone(),
                        separator: false,
                        enabled: *enabled,
                        hint,
                    },
                    ContextMenuItem::Separator => ContextMenuRow {
                        label: String::new(),
                        separator: true,
                        enabled: false,
                        hint: String::new(),
                    },
                    ContextMenuItem::LuaItem { label, .. } => ContextMenuRow {
                        label: label.clone(),
                        separator: false,
                        enabled: true,
                        hint: String::new(),
                    },
                    ContextMenuItem::ConfigItem { label, .. }
                    | ContextMenuItem::ReconnectItem { label, .. } => ContextMenuRow {
                        label: label.clone(),
                        separator: false,
                        enabled: true,
                        hint: String::new(),
                    },
                    ContextMenuItem::Submenu { label, .. } => ContextMenuRow {
                        // Append "▸" to signal "this row opens a submenu".
                        label: format!("{label} ▸"),
                        separator: false,
                        enabled: true,
                        hint: String::new(),
                    },
                    // ProfileChoice rows only live inside a Submenu, so they
                    // reach menu.items (and this arm) after a drill-in swaps the
                    // submenu's items in.
                    ContextMenuItem::ProfileChoice { label, .. }
                    | ContextMenuItem::WindowChoice { label, .. } => ContextMenuRow {
                        label: label.clone(),
                        separator: false,
                        enabled: true,
                        hint: String::new(),
                    },
                    // New-tab ▾ shell choice — a normal clickable row.
                    ContextMenuItem::NewTabShell { label, .. } => ContextMenuRow {
                        label: label.clone(),
                        separator: false,
                        enabled: true,
                        hint,
                    },
                    // URL-aware leading rows ("Open Link" /
                    // "Copy Link Address") — normal clickable rows.
                    ContextMenuItem::UrlItem { label, .. } => ContextMenuRow {
                        label: (*label).to_string(),
                        separator: false,
                        enabled: true,
                        hint: String::new(),
                    },
                    // A card's media rows — normal clickable rows.
                    ContextMenuItem::Media { label, .. }
                    | ContextMenuItem::Preview { label, .. } => ContextMenuRow {
                        label: (*label).to_string(),
                        separator: false,
                        enabled: true,
                        hint: String::new(),
                    },
                    // Dropdown parity: Info renders as a dimmed
                    // (disabled-style) static line.
                    ContextMenuItem::Info { label } => ContextMenuRow {
                        label: label.clone(),
                        separator: false,
                        enabled: false,
                        hint: String::new(),
                    },
                })
                .collect();
        // Terminator menu UX: pass through the scroll state + clamped panel
        // geometry the renderer needs to draw only the visible slice.
        let (panel_w_clamped, panel_h_clamped) = self
            .context_menu_geometry(ws)
            .map(|(_, panel)| panel)
            .unwrap_or((0.0, 0.0));
        let (cell_w, _) = self.menu_cell(ws);
        let max_row_columns = kettle_render::menu::panel_columns(panel_w_clamped, cell_w);
        for row in &mut rows {
            fit_context_menu_row(row, max_row_columns);
        }
        Some(ContextMenu {
            anchor: menu.anchor,
            rows,
            highlight: menu.highlight,
            scroll_offset: menu.scroll_offset,
            panel_w_clamped,
            panel_h_clamped,
            a11y_label: ws.context_menu_a11y_label.clone(),
        })
    }

    /// Esc / `h` semantics: pop a drilled-in submenu back to its parent, or
    /// close the menu when at the top level.
    pub(super) fn context_menu_back(&mut self, ws: &mut WindowState) {
        if ws
            .context_menu
            .as_mut()
            .is_some_and(ContextMenuState::leave_submenu)
        {
            let (width, height) = ws
                .renderer
                .as_ref()
                .map(|renderer| renderer.surface_size())
                .unwrap_or((800, 600));
            self.reclamp_context_menu(ws, (width as f32, height as f32));
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
            return;
        }
        ws.context_menu = None;
        self.reset_blink_phase(ws);
    }

    /// Enter / Space / `l` semantics: dispatch the highlighted row through the
    /// shared mapper.
    pub(super) fn context_menu_activate(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
    ) {
        // Resolve the highlighted row through the shared mapper so Enter /
        // Space dispatches every row type (submenu drill-in, Lua item, config
        // command, theme / profile choice, new-tab ▾ shell). The mapper +
        // dispatcher also own the close-or-keep decision, so a submenu Enter
        // keeps the menu open.
        let chosen = ws.context_menu.as_ref().and_then(|m| {
            m.items
                .get(m.highlight)
                .and_then(|it| item_to_click(it, m.highlight))
        });
        self.reset_blink_phase(ws);
        match chosen {
            Some(click) => self.dispatch_context_menu_click(ws, click, event_loop),
            // Highlight on a non-dispatchable row (shouldn't happen —
            // nav skips them — but close rather than trap the user).
            None => ws.context_menu = None,
        }
    }

    /// The context menu's `vim-menu-nav` layer. Returns `true` when the key
    /// was consumed. Called BEFORE the mnemonic/typeahead catch-all (which
    /// eats every bare a–z) so nav letters always navigate;
    /// `assign_mnemonics` reserves the same letters (`VIM_NAV_RESERVED`) so
    /// no row ever points at a key this layer intercepts.
    pub(super) fn context_menu_vim_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        let Key::Character(s) = key else {
            return false;
        };
        if ws.mods.alt_key() || ws.mods.super_key() {
            return false;
        }
        let ctrl = ws.mods.control_key();
        // Case-fold so CapsLock can't kill the nav layer (letters arrive
        // uppercased); g-vs-G derives from the PHYSICAL Shift state, not
        // character case, so CapsLock+g still means "first" (the user didn't
        // press Shift).
        let folded = s.to_ascii_lowercase();
        match (folded.as_str(), ctrl) {
            ("j", false) => {
                self.step_context_menu_highlight(ws, 1);
                true
            }
            ("k", false) => {
                self.step_context_menu_highlight(ws, -1);
                true
            }
            ("g", false) => {
                let last = ws.mods.shift_key();
                let next = ws.context_menu.as_ref().and_then(|m| {
                    if last {
                        m.items.iter().rposition(item_is_dispatchable)
                    } else {
                        m.items.iter().position(item_is_dispatchable)
                    }
                });
                if let Some(next) = next {
                    self.set_context_menu_highlight(ws, next);
                }
                true
            }
            ("h", false) => {
                self.context_menu_back(ws);
                true
            }
            ("l", false) => {
                self.context_menu_activate(ws, event_loop);
                true
            }
            ("d", true) | ("u", true) => {
                // Derive direction from the case-FOLDED key, not the raw char.
                // Ctrl+Shift+D arrives as `s == "D"`, so a raw `s == "d"` test
                // would turn a half-page-DOWN into a scroll UP.
                let dir: isize = if folded == "d" { 1 } else { -1 };
                let Some(((_, _), (_, panel_h))) = self.context_menu_geometry(ws) else {
                    return true;
                };
                let (_, ch) = self.menu_cell(ws);
                let row_h = ch + kettle_render::menu::ROW_PAD;
                let sep_h = kettle_render::menu::SEP_H;
                let next = ws.context_menu.as_ref().map(|m| {
                    let (_, panel_h) =
                        context_menu_content_geometry(&m.items, panel_h, row_h, sep_h);
                    let visible =
                        count_rows_fitting(&m.items, m.scroll_offset, panel_h, row_h, sep_h);
                    half_page_menu_target(&m.items, m.highlight, (visible / 2).max(1), dir)
                });
                if let Some(next) = next {
                    self.set_context_menu_highlight(ws, next);
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn context_menu_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        text: Option<&str>,
        event_loop: &ActiveEventLoop,
    ) {
        // Vim navigation intercepts BEFORE the mnemonic/typeahead catch-all
        // below, which would otherwise eat bare `j`/`k`/… as mnemonic or
        // typeahead input. With `vim-menu-nav = false` it is skipped.
        // Keyboard routing while the right-click context menu is open:
        // `Esc` closes (or pops a submenu), `↑/↓` step the highlight
        // (skipping separators + disabled rows), `Enter`/`Space` fire the
        // highlighted action, and bare letters dispatch mnemonics /
        // typeahead. Any other key is swallowed so a stray keypress doesn't
        // leak into the focused pane while the menu is expecting nav input.
        if self.cfg.vim_menu_nav && self.context_menu_vim_key(ws, key, event_loop) {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.context_menu_back(ws);
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                self.step_context_menu_highlight(ws, 1);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.step_context_menu_highlight(ws, -1);
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.context_menu_activate(ws, event_loop);
            }
            _ => {
                // Terminator menu UX: mnemonics + typeahead. A single A-Z
                // keystroke dispatches a row whose mnemonic char matches;
                // multi-char accumulates into `typeahead_buf` for
                // prefix-match. Buffer clears after 750ms of inactivity.
                let Some(t) = crate::modal_input::accept_text(text, ws.mods) else {
                    return;
                };
                let mut chars = t.chars();
                let Some(c) = chars.next() else { return };
                if chars.next().is_some() || !c.is_ascii_alphabetic() {
                    // Multi-char text events or non-alpha (digits,
                    // punctuation, IME composition) are ignored.
                    return;
                }
                let lower = c.to_ascii_lowercase();
                let now = std::time::Instant::now();
                // Clear stale typeahead buffer.
                if let Some(menu) = ws.context_menu.as_mut()
                    && menu
                        .typeahead_until
                        .map(|deadline| now > deadline)
                        .unwrap_or(false)
                {
                    menu.typeahead_buf.clear();
                    menu.typeahead_until = None;
                }
                // First key after a long pause: try mnemonic dispatch
                // (single-char). On a hit, dispatch the row + close
                // the menu (matches Win32 / GTK convention). On a
                // miss, accumulate into typeahead.
                // With vim-menu-nav on, the nav letters are reserved. They were
                // intercepted above and must never match a row.
                let reserved: &[char] = if self.cfg.vim_menu_nav {
                    VIM_NAV_RESERVED
                } else {
                    &[]
                };
                let mnemonic_hit = ws.context_menu.as_ref().and_then(|menu| {
                    if !menu.typeahead_buf.is_empty() {
                        return None;
                    }
                    let mn = assign_mnemonics(&menu.items, reserved);
                    mn.iter().enumerate().find_map(|(idx, slot)| {
                        slot.and_then(|(_, ch)| (ch == lower).then_some(idx))
                    })
                });
                if let Some(idx) = mnemonic_hit {
                    // Dispatch the matched row through the same
                    // shared mapper + sink as mouse clicks and Enter / Space,
                    // so a mnemonic reaches every row type (Lua / config /
                    // ▾ shell included) and the close-or-drill decision stays
                    // in one place.
                    let click = ws
                        .context_menu
                        .as_ref()
                        .and_then(|m| m.items.get(idx).and_then(|it| item_to_click(it, idx)));
                    if let Some(click) = click {
                        self.dispatch_context_menu_click(ws, click, event_loop);
                        return;
                    }
                }
                // Typeahead path: accumulate into the buffer, find
                // the first row whose label has this prefix, advance
                // highlight without dispatching.
                let next = ws.context_menu.as_mut().and_then(|menu| {
                    menu.typeahead_buf.push(lower);
                    menu.typeahead_until = Some(now + std::time::Duration::from_millis(750));
                    typeahead_match(&menu.items, &menu.typeahead_buf)
                });
                if let Some(idx) = next {
                    self.set_context_menu_highlight(ws, idx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize) -> Vec<ContextMenuItem> {
        (0..count)
            .map(|i| ContextMenuItem::DynamicItem {
                label: format!("Row {i}"),
                action: Action::Paste,
                enabled: true,
            })
            .collect()
    }

    fn menu(items: Vec<ContextMenuItem>) -> ContextMenuState {
        ContextMenuState {
            anchor: (0.0, 0.0),
            anchor_scale: 1.0,
            items,
            highlight: 0,
            drill_stack: Vec::new(),
            scroll_offset: 0,
            typeahead_buf: String::new(),
            typeahead_until: None,
        }
    }

    #[test]
    fn submenu_back_restores_each_scrolled_parent() {
        let mut child = rows(20);
        child[17] = ContextMenuItem::Submenu {
            label: "Nested".into(),
            items: vec![
                ContextMenuItem::Separator,
                ContextMenuItem::Item {
                    label: "Disabled",
                    action: Action::Paste,
                    enabled: false,
                },
                ContextMenuItem::Item {
                    label: "Enabled",
                    action: Action::Paste,
                    enabled: true,
                },
            ],
        };
        let mut root = rows(30);
        root[28] = ContextMenuItem::Submenu {
            label: "Preferences".into(),
            items: child,
        };
        let mut menu = menu(root);
        menu.scroll_offset = 20;
        assert!(menu.enter_submenu(28));
        assert_eq!((menu.highlight, menu.scroll_offset), (0, 0));
        menu.scroll_offset = 12;
        assert!(menu.enter_submenu(17));
        assert_eq!((menu.highlight, menu.scroll_offset), (2, 0));
        menu.typeahead_buf = "child".into();
        menu.typeahead_until = Some(std::time::Instant::now());

        assert!(menu.leave_submenu());
        assert_eq!((menu.highlight, menu.scroll_offset), (17, 12));
        assert!(menu.typeahead_buf.is_empty() && menu.typeahead_until.is_none());
        assert_eq!(
            context_menu_scroll_for_highlight(&menu.items, 12, 17, 60.0, 10.0, 2.0),
            12,
        );
        assert!(menu.leave_submenu());
        assert_eq!((menu.highlight, menu.scroll_offset), (28, 20));
        assert_eq!(
            context_menu_scroll_for_highlight(&menu.items, 20, 28, 90.0, 10.0, 2.0),
            20,
        );
        assert!(!menu.leave_submenu());
    }

    #[test]
    fn invalid_submenus_leave_the_current_level_unchanged() {
        let mut items = rows(2);
        items[1] = ContextMenuItem::Submenu {
            label: "Empty".into(),
            items: Vec::new(),
        };
        let mut menu = menu(items);
        menu.highlight = 1;
        menu.scroll_offset = 1;
        menu.typeahead_buf = "parent".into();
        for index in [0, 1, usize::MAX] {
            assert!(!menu.enter_submenu(index));
            assert_eq!(menu.items.len(), 2);
            assert_eq!((menu.highlight, menu.scroll_offset), (1, 1));
            assert_eq!(menu.typeahead_buf, "parent");
            assert!(menu.drill_stack.is_empty());
        }
    }
    #[test]
    fn scroll_strip_viewport_keeps_keyboard_selection_visible() {
        let items = rows(30);
        let (inset, content_h) = context_menu_content_geometry(&items, 200.0, 28.0, 8.0);
        assert_eq!((inset, content_h), (18.0, 164.0));
        for selected in 0..items.len() {
            let offset =
                context_menu_scroll_for_highlight(&items, 0, selected, content_h, 28.0, 8.0);
            let visible = count_rows_fitting(&items, offset, content_h, 28.0, 8.0);
            assert!((offset..offset + visible).contains(&selected));
        }
    }
}

/// What a context-menu click dispatches, such as a kettle Action (built-in
/// items) or a Lua callback index (kettle.add_menu_item entries).
/// The rows a right-click on the link `url` from pane `pane`'s output leads
/// the menu with: open it, preview it in Kettle when it is a local file a lane
/// can show from a pane still known, copy its address, then a separator.
pub(super) fn link_menu_rows(
    url: &str,
    pane: Option<u64>,
    tr: kettle_i18n::Translator,
) -> Vec<ContextMenuItem> {
    use kettle_i18n::Text as T;
    let row = |label: T, how: UrlHow| ContextMenuItem::UrlItem {
        label: tr.text(label),
        url: url.to_string(),
        how,
        pane,
    };
    let previewable = pane.is_some()
        && kettle_core::links::local_file_path(url)
            .is_some_and(|path| previewable_name(&path.to_string_lossy()));
    let mut rows = vec![row(T::MenuOpenLink, UrlHow::Open)];
    if previewable {
        rows.push(row(T::MenuPreviewInKettle, UrlHow::Preview));
    }
    rows.push(row(T::MenuCopyLink, UrlHow::Copy));
    rows.push(ContextMenuItem::Separator);
    rows
}

/// What a link row of the context menu does with its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UrlHow {
    Open,
    Copy,
    /// Preview the local file it names in the pane's lane.
    Preview,
}

#[derive(Clone)]
pub(super) enum ContextMenuClick {
    Action(Action),
    Preview {
        target: PreviewMenuTarget,
        action: PreviewMenuAction,
    },
    /// A media row: what it does, and to which shelf item.
    Media {
        target: MediaTarget,
        action: MediaMenuAction,
    },
    LuaMenuItem(usize),
    /// Terminator parity (`custom_commands.py`): a
    /// `menu-item = LABEL = CMD` config entry. Dispatch writes
    /// `CMD\n` to the focused pane's PTY.
    ConfigCommand(String),
    /// Reopen a detected session in a fresh local pane.
    ReconnectRemote(Option<kettle_remote::SessionCommand>),
    /// Profile picked from the right-click "Profile ▸" submenu.
    /// Dispatch sets `App::config_path` via the profile-path
    /// resolution helper and calls `reload_config`.
    SetProfile(String),
    MoveTabToWindow {
        window: u64,
        panes: Arc<[u64]>,
    },
    /// Drill into a `Submenu` row by index. The click handler pushes
    /// the current items onto `drill_stack` + replaces them
    /// with the submenu's items.
    DrillIntoSubmenu(usize),
    /// Open a new tab running an explicit argv (a shell picked from
    /// the new-tab `▾` dropdown). Dispatch calls `Mux::new_tab_with` with the
    /// focused tab's current working directory.
    NewTabWithArgv(Vec<String>),
    /// Terminator parity (terminal_popup_menu.py "Open link" /
    /// "Copy address"): a click on one of the URL-aware leading rows.
    /// `how` says what the row does with the address: open it through the
    /// `open_url` chain (Lua handler → custom_url_handler → system open,
    /// `is_safe_url`-guarded), copy it, or preview the local file it names
    /// in the pane's lane.
    Url {
        url: String,
        how: UrlHow,
        /// The pane whose output held the link; `None` for Kettle's own
        /// links (About).
        pane: Option<u64>,
    },
}

/// One entry in the right-click context menu. `Separator` rows render
/// as a thin divider in the menu and are skipped during keyboard nav
/// and click dispatch; `Item` rows carry the action to fire.
#[derive(Clone)]
pub(super) enum ContextMenuItem {
    Item {
        label: &'static str,
        action: Action,
        enabled: bool,
    },
    /// Like `Item` but with an owned `String` label, so dynamic state
    /// markers (radio `● / ○`, check `✓ /  `) can be baked into the
    /// label at build time without leaking memory via `Box::leak`.
    /// Same dispatch surface as `Item` (typed `Action`).
    DynamicItem {
        label: String,
        action: Action,
        enabled: bool,
    },
    Separator,
    /// Terminator plugin parity:
    /// menu item supplied by a Lua plugin via `kettle.add_menu_item(
    /// label, callback)`. Dispatch invokes the registered Lua
    /// callback (looked up by `lua_idx` in the kettle_menu_items
    /// table) instead of an Action.
    LuaItem {
        label: String,
        lua_idx: usize,
    },
    /// Terminator parity (`custom_commands.py`): user-
    /// defined menu entry from a config-file `menu-item = LABEL =
    /// CMD` line. Dispatch writes `command + "\n"` to the focused
    /// PTY. Simpler than `LuaItem` — no callback, just literal
    /// text. Use this for "Run `clear`" / "Open `~/.bashrc` in
    /// `$EDITOR`" / etc.
    ConfigItem {
        label: String,
        command: String,
    },
    ReconnectItem {
        label: String,
        // None keeps an unreadable-environment action available to explain
        // the refusal through the existing split-failure notice.
        command: Option<kettle_remote::SessionCommand>,
    },
    /// A submenu row carries a nested item list; activation replaces the visible
    /// menu level.
    Submenu {
        label: String,
        // Consumed by the drill-in dispatch
        // (`ContextMenuClick::DrillIntoSubmenu`), which pushes the
        // parent items onto `drill_stack` and replaces them with
        // these.
        items: Vec<ContextMenuItem>,
    },
    WindowChoice {
        label: String,
        window: u64,
        panes: Arc<[u64]>,
    },
    /// A profile-choice leaf row used inside a
    /// `Submenu { label: "Profile", … }`. Clicking dispatches
    /// `ContextMenuClick::SetProfile(profile)` which switches the
    /// active profile (`App::config_path` + reload_config).
    ProfileChoice {
        label: String,
        profile: String,
    },
    /// A shell-choice leaf in the new-tab `▾` dropdown. Clicking
    /// dispatches `ContextMenuClick::NewTabWithArgv(argv)` to open a tab
    /// running that shell.
    NewTabShell {
        label: String,
        argv: Vec<String>,
    },
    /// Terminator parity (terminal_popup_menu.py "Open link" /
    /// "Copy address"): URL-aware leading rows, present only when the
    /// right-click landed on a detected hyperlink. The URL is captured at
    /// menu-open time so a subsequent output scroll can't retarget the click.
    /// Clicking dispatches `ContextMenuClick::Url { url, how }`.
    UrlItem {
        label: &'static str,
        url: String,
        how: UrlHow,
        /// The pane whose output held the link, captured with it.
        pane: Option<u64>,
    },
    /// A row of an inline card's menu: open its item in Kettle, or a copy of
    /// it in the permitted image viewer. The card and its item, with the
    /// item's generation, are captured when the menu opens.
    Media {
        label: &'static str,
        target: MediaTarget,
        action: MediaMenuAction,
    },
    /// Actions bound to the preview that opened the menu.
    Preview {
        label: &'static str,
        target: PreviewMenuTarget,
        action: PreviewMenuAction,
    },
    /// A static, non-dispatchable information line
    /// (the About panel's version/update rows). Rendered like a disabled row
    /// (dimmed), survives `filter_disabled`, never highlighted, claims no
    /// mnemonic, and maps to no click.
    Info {
        label: String,
    },
}

/// The card a media menu row acts on, and the shelf item it showed when the
/// menu opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MediaTarget {
    pub(super) pane: u64,
    pub(super) nonce: kettle_core::InlineNonce,
    pub(super) item: u64,
    pub(super) generation: u64,
}

/// What a media menu row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MediaMenuAction {
    /// Open the item in Kettle's viewer.
    View,
    /// Open a copy of it in the permitted image viewer, or of a video in
    /// the permitted video player.
    OpenOutside(crate::media::Outside),
}

/// Replacements and mode changes invalidate this menu's actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PreviewMenuTarget {
    pub(super) pane: u64,
    pub(super) item: u64,
    pub(super) generation: u64,
    pub(super) mode: kettle_render::MediaLaneMode,
}

impl PreviewMenuTarget {
    pub(super) fn matches(
        self,
        item: &crate::media::ShelfItem,
        panel: &crate::window_state::PreviewPanel,
    ) -> bool {
        panel.item == self.item
            && item.id == self.item
            && item.generation == self.generation
            && lane_mode(item, panel) == self.mode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreviewMenuAction {
    Copy(kettle_render::MediaLaneMode),
    Control(kettle_render::MediaLaneHit),
    Expanded(bool),
    Play(kettle_render::MediaLanePlay),
    Sound(kettle_render::MediaLaneSound),
    OpenOutside(crate::media::Outside),
}

/// UI-side context-menu state (Terminator / GNOME / iTerm2 parity).
/// Anchor is the post-clamp panel top-left; rows mirror the renderer's
/// `ContextMenu` slice but carry the live `Action` for dispatch.
pub(crate) struct ContextMenuState {
    pub(super) anchor: (f32, f32),
    /// DPI scale at which `anchor` was captured. Anchors are physical pixels;
    /// rescale them across monitor transitions before clamping so the menu
    /// keeps the same logical relationship to the pointer.
    pub(super) anchor_scale: f64,
    pub(super) items: Vec<ContextMenuItem>,
    /// Selected dispatchable row; never a separator or disabled row.
    pub(super) highlight: usize,
    /// Parent levels, including the selected submenu and scroll position.
    pub(super) drill_stack: Vec<ContextMenuLevel>,
    /// First visible item in a height-clamped menu.
    pub(super) scroll_offset: usize,
    /// Pending keyboard prefix, cleared after 750ms of inactivity.
    pub(super) typeahead_buf: String,
    /// Deadline after which `typeahead_buf` is cleared
    /// on the next key. `None` when the buffer is empty.
    pub(super) typeahead_until: Option<std::time::Instant>,
}

pub(super) struct ContextMenuLevel {
    items: Vec<ContextMenuItem>,
    highlight: usize,
    scroll_offset: usize,
}

/// Letters the menu's vim navigation layer (`vim-menu-nav`) consumes
/// (`g`/`G` first/last, `h` back, `j`/`k` move, `l` activate). While the
/// setting is on, `assign_mnemonics` must not hand any of these to a row.
/// The nav layer intercepts them before mnemonic dispatch, so a row keyed on
/// one would silently lose its hotkey.
pub(super) const VIM_NAV_RESERVED: &[char] = &['g', 'h', 'j', 'k', 'l'];

/// Display-column budget for one context-menu item, kept in lockstep with the
/// row constructed by `context_menu_overlay` and the renderer's
/// `menu_row_chars`. Centralizing this prevents the initial anchor clamp,
/// resize/DPI reclamp, hit testing, ellipsis, and draw width from disagreeing.
pub(super) fn context_menu_item_columns(item: &ContextMenuItem, hint: &str) -> usize {
    let (label, suffix, shows_hint): (&str, &str, bool) = match item {
        ContextMenuItem::Item { label, .. } => (label, "", true),
        ContextMenuItem::DynamicItem { label, .. } => (label, "", true),
        ContextMenuItem::LuaItem { label, .. } => (label, "", false),
        ContextMenuItem::ConfigItem { label, .. }
        | ContextMenuItem::ReconnectItem { label, .. } => (label, "", false),
        ContextMenuItem::Submenu { label, .. } => (label, " ▸", false),
        ContextMenuItem::ProfileChoice { label, .. }
        | ContextMenuItem::WindowChoice { label, .. } => (label, "", false),
        ContextMenuItem::NewTabShell { label, .. } => (label, "", true),
        ContextMenuItem::UrlItem { label, .. } => (label, "", false),
        ContextMenuItem::Media { label, .. } | ContextMenuItem::Preview { label, .. } => {
            (label, "", false)
        }
        ContextMenuItem::Info { label } => (label, "", false),
        ContextMenuItem::Separator => return 0,
    };
    label.width()
        + suffix.width()
        + if shows_hint && !hint.is_empty() {
            hint.width() + 2
        } else {
            0
        }
}

/// Compute mnemonics for the context-menu rows. For each row, returns
/// `Some((byte_index, char))` where `char` is the first lowercase A-Z
/// letter in the label that hasn't already been claimed by an earlier row,
/// or `None` for rows without any A-Z (separators, choice rows with no
/// label letters, etc.).
///
/// First-letter is the canonical priority (matches GTK / Win32);
/// fall through to subsequent letters only if the first letter is
/// already taken by an earlier row. Pure so the collision rules
/// are unit-tested without spinning up App.
pub(super) fn assign_mnemonics(
    items: &[ContextMenuItem],
    reserved: &[char],
) -> Vec<Option<(usize, char)>> {
    let labels: Vec<&str> = items
        .iter()
        .map(|it| match it {
            ContextMenuItem::Item { label, .. } => *label,
            ContextMenuItem::DynamicItem { label, .. } => label.as_str(),
            ContextMenuItem::LuaItem { label, .. } => label.as_str(),
            ContextMenuItem::ConfigItem { label, .. }
            | ContextMenuItem::ReconnectItem { label, .. } => label.as_str(),
            ContextMenuItem::Submenu { label, .. } => label.as_str(),
            ContextMenuItem::ProfileChoice { label, .. }
            | ContextMenuItem::WindowChoice { label, .. } => label.as_str(),
            ContextMenuItem::NewTabShell { label, .. } => label.as_str(),
            ContextMenuItem::UrlItem { label, .. } => *label,
            ContextMenuItem::Media { label, .. } | ContextMenuItem::Preview { label, .. } => *label,
            // Info rows are non-dispatchable — no mnemonic to claim.
            ContextMenuItem::Info { .. } => "",
            ContextMenuItem::Separator => "",
        })
        .collect();
    let mut claimed: std::collections::HashSet<char> = std::collections::HashSet::new();
    let mut out: Vec<Option<(usize, char)>> = vec![None; labels.len()];
    // Two rounds. The stable core rows claim their letters first, then the
    // context-dependent UrlItem rows (present only when the right-click landed
    // on a link) claim from what's left. Otherwise "Open Link" / "Copy Link
    // Address", which lead the menu, would take 'o'/'c' and silently remap
    // muscle-memory mnemonics ('p' would fire Copy instead of Paste).
    let round = |items: &[ContextMenuItem], idx: usize| -> usize {
        usize::from(matches!(
            items[idx],
            ContextMenuItem::UrlItem { .. }
                | ContextMenuItem::Media { .. }
                | ContextMenuItem::Preview { .. }
        ))
    };
    for pass in 0..2 {
        for (i, label) in labels.iter().enumerate() {
            if round(items, i) != pass {
                continue;
            }
            let mut chosen: Option<(usize, char)> = None;
            for (bi, c) in label.char_indices() {
                if !c.is_ascii_alphabetic() {
                    continue;
                }
                let low = c.to_ascii_lowercase();
                // Letters owned by vim-menu-nav are never assignable.
                if reserved.contains(&low) {
                    continue;
                }
                if !claimed.contains(&low) {
                    claimed.insert(low);
                    chosen = Some((bi, low));
                    break;
                }
            }
            out[i] = chosen;
        }
    }
    out
}

/// Match the user's typeahead buffer to a row by
/// case-insensitive prefix on the label. Returns the first
/// dispatchable row whose label (lowercased) starts with `buf`
/// (also lowercased). Separators/empty labels are skipped.
pub(super) fn typeahead_match(items: &[ContextMenuItem], buf: &str) -> Option<usize> {
    if buf.is_empty() {
        return None;
    }
    let needle = buf.to_ascii_lowercase();
    items.iter().position(|it| match it {
        ContextMenuItem::Item {
            label,
            enabled: true,
            ..
        } => label.to_ascii_lowercase().starts_with(&needle),
        ContextMenuItem::DynamicItem {
            label,
            enabled: true,
            ..
        } => label.to_ascii_lowercase().starts_with(&needle),
        ContextMenuItem::LuaItem { label, .. }
        | ContextMenuItem::ConfigItem { label, .. }
        | ContextMenuItem::ReconnectItem { label, .. }
        | ContextMenuItem::Submenu { label, .. }
        | ContextMenuItem::WindowChoice { label, .. }
        | ContextMenuItem::ProfileChoice { label, .. }
        | ContextMenuItem::NewTabShell { label, .. } => {
            label.to_ascii_lowercase().starts_with(&needle)
        }
        ContextMenuItem::UrlItem { label, .. }
        | ContextMenuItem::Media { label, .. }
        | ContextMenuItem::Preview { label, .. } => label.to_ascii_lowercase().starts_with(&needle),
        // Info rows aren't dispatchable, so typeahead skips them.
        _ => false,
    })
}

pub(super) fn context_menu_content_geometry(
    items: &[ContextMenuItem],
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> (f32, f32) {
    let natural_h = items
        .iter()
        .map(|item| context_menu_item_height(item, row_h, sep_h))
        .sum();
    kettle_render::menu::scroll_content(natural_h, panel_h, row_h)
}

/// How many context-menu rows starting at
/// `start` fit within `panel_h` pixels. Separators take `sep_h`,
/// every other row takes `row_h`. Used by `step_context_menu_highlight`
/// and `scroll_context_menu` to keep `scroll_offset` honest when the
/// panel is height-clamped by `context_menu_geometry`. Pure so the
/// arithmetic is drift-guarded without standing up the App.
pub(super) fn count_rows_fitting(
    items: &[ContextMenuItem],
    start: usize,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    let mut used = 0.0_f32;
    let mut count = 0;
    for it in items.iter().skip(start) {
        let h = context_menu_item_height(it, row_h, sep_h);
        if used + h > panel_h {
            break;
        }
        used += h;
        count += 1;
    }
    count
}

fn context_menu_item_height(item: &ContextMenuItem, row_h: f32, sep_h: f32) -> f32 {
    if matches!(item, ContextMenuItem::Separator) {
        sep_h
    } else {
        row_h
    }
}

/// Earliest suffix that fits through the final row. Scrolling past it would
/// leave avoidable blank space below the menu. This is O(items) even for the
/// 512-entry theme submenu.
pub(super) fn context_menu_max_scroll_offset(
    items: &[ContextMenuItem],
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let mut suffix_height = 0.0;
    let mut max_offset = items.len() - 1;
    for candidate in (0..items.len()).rev() {
        let next = suffix_height + context_menu_item_height(&items[candidate], row_h, sep_h);
        if next > panel_h {
            break;
        }
        suffix_height = next;
        max_offset = candidate;
    }
    max_offset
}

pub(super) fn context_menu_scroll_for_highlight(
    items: &[ContextMenuItem],
    current_offset: usize,
    highlight: usize,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let highlight = highlight.min(items.len() - 1);
    let max_offset = context_menu_max_scroll_offset(items, panel_h, row_h, sep_h);
    let mut offset = current_offset.min(max_offset);
    let visible = count_rows_fitting(items, offset, panel_h, row_h, sep_h);
    if highlight < offset {
        offset = highlight;
    } else if highlight >= offset.saturating_add(visible) {
        // Pull back until the highlight is the last fully visible row. This
        // keeps Enter bound to a visible choice after a window/DPI shrink.
        let mut used = 0.0;
        offset = highlight;
        for candidate in (0..=highlight).rev() {
            let next = used + context_menu_item_height(&items[candidate], row_h, sep_h);
            if next > panel_h {
                break;
            }
            used = next;
            offset = candidate;
        }
    }
    offset.min(max_offset)
}

pub(super) fn ellipsize_menu_label(label: &str, max_columns: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;

    if label.width() <= max_columns {
        return label.to_string();
    }
    if max_columns == 0 {
        return String::new();
    }
    if max_columns == 1 {
        return "…".to_string();
    }
    let content_columns = max_columns - 1;
    let mut used: usize = 0;
    let mut output = String::new();
    for grapheme in label.graphemes(true) {
        let width = grapheme.width();
        if used.saturating_add(width) > content_columns {
            break;
        }
        output.push_str(grapheme);
        used += width;
    }
    output.push('…');
    output
}

/// Drop disabled rows from the context menu and collapse the separators
/// they would orphan. Hiding them instead of greying them out matches
/// Terminator / GNOME Terminal, which show only what you can click.
///
/// Three passes:
///   1. drop any disabled `Item` or `DynamicItem`. Other variants (LuaItem,
///      ConfigItem, Submenu, Theme/ProfileChoice, Separator) stay.
///   2. collapse runs of `Separator` to a single one.
///   3. trim leading + trailing separators (orphaned by step 1).
///
/// Pure so the contract is unit-tested without spinning up App.
pub(super) fn filter_disabled(items: Vec<ContextMenuItem>) -> Vec<ContextMenuItem> {
    // Step 1: drop disabled Items.
    let kept: Vec<ContextMenuItem> = items
        .into_iter()
        .filter(|it| {
            !matches!(
                it,
                ContextMenuItem::Item { enabled: false, .. }
                    | ContextMenuItem::DynamicItem { enabled: false, .. }
            )
        })
        .collect();
    // Step 2: collapse separator runs. Walk linearly; only push a
    // Separator when the previous pushed item wasn't already one.
    let mut collapsed: Vec<ContextMenuItem> = Vec::with_capacity(kept.len());
    let mut last_was_sep = true; // pretend the "before-start" was a sep so leading sep gets dropped
    for it in kept {
        let is_sep = matches!(it, ContextMenuItem::Separator);
        if is_sep && last_was_sep {
            continue;
        }
        last_was_sep = is_sep;
        collapsed.push(it);
    }
    // Step 3: trim a trailing separator (orphan).
    if let Some(ContextMenuItem::Separator) = collapsed.last() {
        collapsed.pop();
    }
    collapsed
}

/// Terminator menu UX, hover-to-highlight. Walk the
/// vertical pixel layout of a context-menu's rows and return the
/// row index containing `cursor_y`, or `None` if the cursor landed
/// on a separator (visual gap) or beyond the last row. Pure so the
/// arithmetic + the separator-skip contract is unit-tested without
/// standing up an App + a winit window. A `true` item flags a separator (uses
/// `sep_h` instead of `row_h`). Accepting an iterator lets the live pointer path
/// project item kinds without allocating a temporary vector.
pub(crate) fn find_menu_row_y(
    cursor_y: f32,
    anchor_y: f32,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
    kinds: impl IntoIterator<Item = bool>,
) -> Option<usize> {
    let mut row_y = anchor_y;
    let panel_bottom = anchor_y + panel_h.max(0.0);
    for (idx, is_sep) in kinds.into_iter().enumerate() {
        let h = if is_sep { sep_h } else { row_h };
        // The renderer draws only complete rows. A clamped panel can leave a
        // short blank strip at the bottom; never dispatch the next invisible
        // row through that strip.
        if row_y + h > panel_bottom {
            break;
        }
        if cursor_y >= row_y && cursor_y < row_y + h {
            return if is_sep { None } else { Some(idx) };
        }
        row_y += h;
    }
    None
}

/// The `vim-menu-nav` `Ctrl+d`/`Ctrl+u` half-page target. Moves
/// `current` by `rows` items in `dir` (no wrap — vim half-page semantics
/// clamp at the ends), then snaps to the nearest dispatchable row in the
/// direction of travel (falling back to the other direction so a
/// trailing separator can't strand the highlight). Pure so the
/// clamp+snap math is unit-testable without App state.
pub(super) fn half_page_menu_target(
    items: &[ContextMenuItem],
    current: usize,
    rows: usize,
    dir: isize,
) -> usize {
    if items.is_empty() {
        return current;
    }
    let n = items.len() as isize;
    let step = rows.max(1) as isize;
    let raw = (current as isize + dir * step).clamp(0, n - 1) as usize;
    let scan_down = |from: usize| (from..items.len()).find(|&i| item_is_dispatchable(&items[i]));
    let scan_up = |from: usize| (0..=from).rev().find(|&i| item_is_dispatchable(&items[i]));
    let snapped = if dir > 0 {
        scan_down(raw).or_else(|| scan_up(raw))
    } else {
        scan_up(raw).or_else(|| scan_down(raw))
    };
    snapped.unwrap_or(current)
}

/// Whether keyboard nav can land on `item` and Enter / click can dispatch it.
pub(super) fn item_is_dispatchable(item: &ContextMenuItem) -> bool {
    matches!(
        item,
        ContextMenuItem::Item { enabled: true, .. }
            | ContextMenuItem::DynamicItem { enabled: true, .. }
            | ContextMenuItem::LuaItem { .. }
            | ContextMenuItem::ConfigItem { .. }
            | ContextMenuItem::ReconnectItem { .. }
            // Submenu rows are dispatchable for keyboard
            // nav (↑↓ lands on them); click / Enter drills in.
            | ContextMenuItem::Submenu { .. }
            // Profile choice leaves are the *contents* of a drilled-in
            // Profile ▸ submenu. Without them, keyboard nav inside that
            // submenu could not land on any row or pick a profile.
            | ContextMenuItem::WindowChoice { .. }
            | ContextMenuItem::ProfileChoice { .. }
            // New-tab ▾ shell choices are always clickable + keyboard-
            // navigable.
            | ContextMenuItem::NewTabShell { .. }
            // The URL-aware "Open Link" / "Copy Link Address" rows.
            | ContextMenuItem::UrlItem { .. }
            // A card's or the viewer's media rows.
            | ContextMenuItem::Media { .. }
            | ContextMenuItem::Preview { .. } // ContextMenuItem::Info is deliberately absent: a static info line
                                              // (About panel) is not highlightable or clickable.
    )
}

/// Map a context-menu row at position `idx` (within the current level)
/// to the click it dispatches. Shared by the mouse hit-test
/// ([`App::context_menu_click_action`]) and the keyboard Enter / Space and
/// mnemonic paths so every dispatchable row type (submenu, Lua,
/// config-command, theme/profile choice, new-tab shell) is reachable
/// identically from mouse and keyboard. One mapper keeps the three input
/// paths from drifting apart.
pub(super) fn item_to_click(item: &ContextMenuItem, idx: usize) -> Option<ContextMenuClick> {
    match item {
        ContextMenuItem::Item {
            action,
            enabled: true,
            ..
        }
        | ContextMenuItem::DynamicItem {
            action,
            enabled: true,
            ..
        } => Some(ContextMenuClick::Action(action.clone())),
        ContextMenuItem::LuaItem { lua_idx, .. } => Some(ContextMenuClick::LuaMenuItem(*lua_idx)),
        ContextMenuItem::ConfigItem { command, .. } => {
            Some(ContextMenuClick::ConfigCommand(command.clone()))
        }
        ContextMenuItem::ReconnectItem { command, .. } => {
            Some(ContextMenuClick::ReconnectRemote(command.clone()))
        }
        ContextMenuItem::Submenu { .. } => Some(ContextMenuClick::DrillIntoSubmenu(idx)),
        ContextMenuItem::WindowChoice { window, panes, .. } => {
            Some(ContextMenuClick::MoveTabToWindow {
                window: *window,
                panes: Arc::clone(panes),
            })
        }
        ContextMenuItem::ProfileChoice { profile, .. } => {
            Some(ContextMenuClick::SetProfile(profile.clone()))
        }
        ContextMenuItem::NewTabShell { argv, .. } => {
            Some(ContextMenuClick::NewTabWithArgv(argv.clone()))
        }
        ContextMenuItem::UrlItem { url, how, pane, .. } => Some(ContextMenuClick::Url {
            url: url.clone(),
            how: *how,
            pane: *pane,
        }),
        ContextMenuItem::Media { target, action, .. } => Some(ContextMenuClick::Media {
            target: *target,
            action: *action,
        }),
        ContextMenuItem::Preview { target, action, .. } => Some(ContextMenuClick::Preview {
            target: *target,
            action: *action,
        }),
        ContextMenuItem::Item { enabled: false, .. }
        | ContextMenuItem::DynamicItem { enabled: false, .. }
        | ContextMenuItem::Info { .. }
        | ContextMenuItem::Separator => None,
    }
}

/// Pure: walk the menu item list to find the next enabled, non-
/// separator row index, given a `delta` (±1) and a wrap-around at the
/// list ends. Used by both `↑` and `↓` keyboard nav. Returns `current`
/// unchanged if no enabled rows exist at all (defensive; the menu
/// shouldn't have been opened with zero actionable rows).
pub(super) fn next_context_menu_highlight(
    items: &[ContextMenuItem],
    current: usize,
    delta: isize,
) -> usize {
    if items.is_empty() {
        return current;
    }
    let len = items.len() as isize;
    let step = if delta >= 0 { 1 } else { -1 };
    let mut i = current as isize;
    for _ in 0..len {
        i = (i + step).rem_euclid(len);
        if item_is_dispatchable(&items[i as usize]) {
            return i as usize;
        }
    }
    current
}

/// Pure: clamp the requested panel anchor so the panel of size
/// `(panel_w, panel_h)` stays fully inside a surface of size
/// `(surface_w, surface_h)`. A 4-px screen-edge margin so the panel
/// reads as floating rather than glued to the edge. Right-click near
/// the bottom-right corner therefore flips the menu up-and-left
/// instead of rendering off-screen.
pub(super) fn clamp_context_menu_anchor(
    (req_x, req_y): (f32, f32),
    (panel_w, panel_h): (f32, f32),
    (surface_w, surface_h): (f32, f32),
) -> (f32, f32) {
    let margin = 4.0_f32;
    let max_x = (surface_w - panel_w - margin).max(margin);
    let max_y = (surface_h - panel_h - margin).max(margin);
    let x = req_x.clamp(margin, max_x);
    let y = req_y.clamp(margin, max_y);
    (x, y)
}

pub(super) fn clamp_context_menu_panel(
    (panel_w, panel_h): (f32, f32),
    (surface_w, surface_h): (f32, f32),
) -> (f32, f32) {
    let available_w = (surface_w - 8.0).max(1.0);
    let available_h = (surface_h - 8.0).max(1.0);
    (panel_w.min(available_w), panel_h.min(available_h))
}

pub(super) fn context_menu_surface_can_fit_row(
    (surface_w, surface_h): (f32, f32),
    (cell_w, cell_h): (f32, f32),
) -> bool {
    surface_w.is_finite()
        && surface_h.is_finite()
        && cell_w.is_finite()
        && cell_h.is_finite()
        && surface_w >= 8.0 + kettle_render::menu::H_PAD + cell_w.max(1.0)
        && surface_h >= 8.0 + cell_h.max(1.0) + kettle_render::menu::ROW_PAD
}

pub(super) fn fit_context_menu_row(row: &mut ContextMenuRow, max_columns: usize) {
    if row.separator {
        return;
    }
    let hint_columns = if row.hint.is_empty() {
        0
    } else {
        row.hint.width().saturating_add(2)
    };
    let hint_columns = if hint_columns >= max_columns {
        row.hint.clear();
        0
    } else {
        hint_columns
    };
    row.label = ellipsize_menu_label(&row.label, max_columns.saturating_sub(hint_columns));
}

pub(super) fn scale_context_menu_anchor(
    (x, y): (f32, f32),
    previous_scale: f64,
    current_scale: f64,
) -> (f32, f32) {
    if !previous_scale.is_finite()
        || previous_scale <= 0.0
        || !current_scale.is_finite()
        || current_scale <= 0.0
    {
        return (x, y);
    }
    let ratio = (current_scale / previous_scale) as f32;
    (x * ratio, y * ratio)
}

#[derive(Default)]
pub(super) struct PendingShellAction(Option<(u64, usize)>);

impl PendingShellAction {
    pub(super) fn request(
        &mut self,
        window: u64,
        index: usize,
        shells: Option<&[kettle_core::term::ShellChoice]>,
    ) -> Option<Vec<String>> {
        match shells {
            Some(shells) => {
                self.0 = None;
                shells.get(index).map(|(_, argv)| argv.clone())
            }
            None => {
                self.0 = Some((window, index));
                None
            }
        }
    }

    pub(super) fn complete(
        &mut self,
        shells: &[kettle_core::term::ShellChoice],
    ) -> Option<(u64, Vec<String>)> {
        let (window, index) = self.0.take()?;
        shells.get(index).map(|(_, argv)| (window, argv.clone()))
    }
}
