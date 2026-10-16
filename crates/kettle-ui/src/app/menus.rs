//! Context menus, the new-tab menu and the About panel.

use super::*;

impl App {
    /// Build the right-click context-menu item list. Each `Item`'s
    /// `enabled` flag is computed from current state: Copy needs a
    /// selection, the split rows need an unzoomed tab, and Ungroup needs
    /// the focused pane to be in a group. `show_context_menu` runs the list
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
        // Only enable Ungroup when the focused pane has a group_name set.
        // Otherwise the row is filtered out until it's actionable, since a
        // greyed-out row confuses new users.
        let has_group = ws
            .mux
            .focused()
            .map(|p| p.group_name.as_ref().is_some_and(|g| !g.is_empty()))
            .unwrap_or(false);
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
        // clone_session_command returns None when a host/user/container token
        // is unsafe (control chars / shell metachars that can't be
        // neutralized). Drop the menu item and its separator rather than type
        // an unsafe line into the PTY.
        if let Some(command) = kettle_remote::clone_session_command(ctx) {
            items.push(ContextMenuItem::Separator);
            items.push(ContextMenuItem::ConfigItem {
                label: remote_reconnect_label(ctx, &self.ui_text),
                command,
            });
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

    /// Open the right-click context menu at `(px, py)`. Closes every other
    /// modal except an open search bar, then `show_context_menu` sizes the
    /// panel from the cell metrics and clamps the anchor so the menu fits
    /// the surface (right-click near the bottom-right corner flips up-
    /// and-left rather than rendering off-screen).
    /// Open the right-click menu at (`px`, `py`). `offer_copied`, for a
    /// Shift+right-click only, also offers the file the clipboard names.
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

    /// Shared tail of context-menu opening — drop disabled rows +
    /// collapse orphaned separators, compute panel geometry, clamp the anchor
    /// on-screen, and install the `ContextMenuState`. Used by both the
    /// right-click menu and the new-tab `▾` dropdown so they render
    /// pixel-identically.
    /// The popup menu over the grid leaves an open search bar in place, as
    /// Terminator leaves its bar under the menu: the user right-clicks to copy
    /// what they just selected under the query, not to abandon the query.
    /// Search is the only modal that can be open together with the menu:
    /// every other opener runs `close_all_modals`, which closes search, and
    /// search's own opener closes everything else. Keys go to the menu while
    /// it is up (its arm precedes search in the key handler) and return to the
    /// editor when it closes. `show_context_menu` ends the pointer gestures a
    /// modal must not leave armed.
    pub(super) fn close_modals_for_context_menu(&mut self, ws: &mut WindowState) {
        if ws.search.open {
            ws.context_menu = None;
        } else {
            self.close_all_modals(ws);
        }
    }

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
                | ContextMenuItem::Submenu { .. }
                | ContextMenuItem::WindowChoice { .. }
                | ContextMenuItem::ProfileChoice { .. }
                | ContextMenuItem::NewTabShell { .. }
                | ContextMenuItem::UrlItem { .. }
                | ContextMenuItem::Media { .. }
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
            scroll_stack: Vec::new(),
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
    }

    /// Open the update chip's menu at `(px, py)`: a disabled row for each
    /// fact, then the release page and Dismiss. There is no one-click
    /// dismiss.
    pub(super) fn open_update_menu(&mut self, ws: &mut WindowState, (px, py): (f32, f32)) {
        let Some(notice) = self.update_notice() else {
            return;
        };
        self.close_all_modals(ws);
        let tr = self.ui_text;
        let mut items: Vec<ContextMenuItem> =
            crate::update_notice::fact_rows(&self.update_facts, &tr)
                .into_iter()
                .map(|label| ContextMenuItem::Info { label })
                .collect();
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::DynamicItem {
            label: tr.update_menu_whats_new(&notice.tag),
            action: Action::OpenUpdate,
            enabled: crate::update_notice::release_url(&notice.tag).is_some(),
        });
        items.push(ContextMenuItem::Item {
            label: tr.text(kettle_i18n::Text::UpdateMenuDismiss),
            action: Action::DismissUpdate,
            enabled: true,
        });
        self.show_context_menu(ws, items, px, py);
        ws.context_menu_a11y_label = Some(tr.text(kettle_i18n::Text::MenuA11yUpdate).to_string());
    }

    /// Open the new-tab `▾` dropdown at
    /// `(px, py)` — detected shells (process-cached in kettle-core, prewarmed
    /// at startup) plus the Settings / Command palette / About bottom rows.
    pub(super) fn open_new_tab_menu(&mut self, ws: &mut WindowState, px: f32, py: f32) {
        self.close_all_modals(ws);
        let shells = kettle_core::term::detect_shells();
        let items = Self::new_tab_menu_items(&shells, &self.ui_text);
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
        let new_off = (menu.scroll_offset as isize + delta).max(0) as usize;
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
        find_menu_row_y(py, ay, panel_h, row_h, sep_h, kinds).map(|i| i + start)
    }

    /// Set `menu.highlight` to whichever row the cursor is
    /// over right now; no-op when the cursor is outside the panel or
    /// on a separator. Called from `CursorMoved`. Requests a redraw
    /// only when the highlight actually changed so we don't churn the
    /// GPU on every sub-pixel motion event.
    pub(super) fn update_menu_highlight_from_cursor(&mut self, ws: &mut WindowState) {
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
                    let nested_items = match menu.items.get(idx) {
                        Some(ContextMenuItem::Submenu { items, .. }) => items.clone(),
                        _ => Vec::new(),
                    };
                    if !nested_items.is_empty() {
                        let parent = std::mem::replace(&mut menu.items, nested_items);
                        menu.drill_stack.push(parent);
                        // Each level keeps its own scroll view.
                        menu.scroll_stack.push(menu.scroll_offset);
                        menu.scroll_offset = 0;
                        // Drilling resets any in-progress typeahead so a
                        // half-typed parent prefix doesn't bleed into the
                        // child level.
                        menu.typeahead_buf.clear();
                        menu.typeahead_until = None;
                        menu.highlight = menu
                            .items
                            .iter()
                            .position(item_is_dispatchable)
                            .unwrap_or(0);
                    }
                }
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
                if let Some(pane_id) = ws.mux.active_focus()
                    && let Some(p) = ws.mux.panes.get(&pane_id)
                {
                    let mut bytes = command.into_bytes();
                    bytes.push(b'\n');
                    let result = p.feed_input(&bytes);
                    self.report_input_result(result);
                    Self::dismiss_media_paste_receipt_after_input(ws, pane_id, result);
                }
            }
            ContextMenuClick::MoveTabToWindow { window, panes } => {
                ws.context_menu = None;
                self.move_tab_to_window(ws, window, &panes);
            }
            ContextMenuClick::SetProfile(name) => {
                ws.context_menu = None;
                if let Some(p) = kettle_config::Config::path_for_profile(&name) {
                    self.config_path = Some(p);
                    self.config_trust = ConfigTrust::VerifyDirectory;
                    self.reload_config(ws);
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
            .max(1.0)
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
            menu.anchor = anchor;
            menu.anchor_scale = current_scale;
            menu.scroll_offset = context_menu_scroll_for_highlight(
                &menu.items,
                menu.scroll_offset,
                menu.highlight,
                panel.1,
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
        let mut rows: Vec<ContextMenuRow> = menu
            .items
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
                ContextMenuItem::ConfigItem { label, .. } => ContextMenuRow {
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
                ContextMenuItem::Media { label, .. } => ContextMenuRow {
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
        if let Some(menu) = ws.context_menu.as_mut()
            && let Some(parent) = menu.drill_stack.pop()
        {
            menu.items = parent;
            // Restore the parent level's
            // scroll_offset so popping out of a deep theme
            // list doesn't snap the parent's scroll position
            // to 0.
            menu.scroll_offset = menu.scroll_stack.pop().unwrap_or(0);
            menu.highlight = menu
                .items
                .iter()
                .position(item_is_dispatchable)
                .unwrap_or(0);
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
                if let Some(menu) = ws.context_menu.as_mut() {
                    menu.typeahead_buf.push(lower);
                    menu.typeahead_until = Some(now + std::time::Duration::from_millis(750));
                    if let Some(idx) = typeahead_match(&menu.items, &menu.typeahead_buf) {
                        menu.highlight = idx;
                    }
                }
            }
        }
    }
}
