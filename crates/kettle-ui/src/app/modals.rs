//! Keyboard and pointer handling for confirmations, Settings, the palette, pickers, hints, vi mode and title editing.

use super::*;

pub(super) fn begin_settings_text_edit(
    key: &'static str,
    cur: &str,
) -> Option<crate::settings::SettingsTextEdit> {
    if cur.len() > crate::modal_input::MAX_MODAL_INPUT_BYTES {
        log::warn!("setting exceeds the modal editor byte limit; leaving the value unchanged");
        return None;
    }
    Some(crate::settings::SettingsTextEdit {
        key,
        buf: cur.into(),
    })
}

pub(super) fn cancel_modal_pointer_composition(
    ws: &mut WindowState,
    mut set_ime_allowed: impl FnMut(bool),
    discard_marked_text: impl FnOnce(),
) {
    if ws.ime_preedit.is_some() {
        end_modal_composition(ws);
        // Moving the caret must also discard the platform's unfinished phrase;
        // otherwise another Preedit would restart it at the new insertion point.
        set_ime_allowed(false);
        discard_marked_text();
        set_ime_allowed(true);
    }
}

#[cfg(target_os = "macos")]
fn discard_native_marked_text(window: &winit::window::Window) {
    use objc2_app_kit::NSView;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Some(_main_thread) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    // SAFETY: winit lends a live NSView for this window, and the marker above
    // ensures AppKit is called on the main thread.
    let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    if let Some(context) = view.inputContext() {
        // winit 0.30.13's IME toggle only clears its own marked-text buffer.
        context.discardMarkedText();
    }
}

pub(super) fn edit_modal_text(
    ws: &mut WindowState,
    key: &Key,
    read: impl FnMut() -> Option<String>,
    write: impl FnMut(&str) -> bool,
) -> bool {
    let mut mods = ws.mods;
    // Option-as-Alt governs composition, not the editor's word commands.
    if matches!(
        key,
        Key::Named(
            NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Backspace | NamedKey::Delete
        )
    ) && option_physically_held(ws)
    {
        mods.insert(ModifiersState::ALT);
    }
    let Some(editor) = modal_editor_mut(ws) else {
        return false;
    };
    crate::modal_input::edit_key(editor, key, mods, read, write)
}

impl App {
    /// Shared native/control press dispatch. Returning true owns the entire click.
    pub(super) fn dismiss_modal_outside(&mut self, ws: &mut WindowState, button: u8) -> bool {
        let Some((kind, inside)) = self.modal_press_target(ws) else {
            return false;
        };
        let consumed = dispatch_modal_press(ws, kind, inside, button);
        if kind == ModalKind::Theme && !inside {
            self.sync_theme_preview(ws);
        }
        consumed
    }

    fn modal_press_target(&self, ws: &WindowState) -> Option<(ModalKind, bool)> {
        let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if ws.confirm_dialog.is_some() {
            return Some((ModalKind::Confirmation, false));
        }
        if ws.context_menu.is_some() {
            let inside = self.context_menu_overlay(ws).is_some_and(|menu| {
                rect_contains(
                    (
                        menu.anchor.0,
                        menu.anchor.1,
                        menu.panel_w_clamped,
                        menu.panel_h_clamped,
                    ),
                    x,
                    y,
                )
            });
            return Some((ModalKind::ContextMenu, inside));
        }
        let kind = if ws.palette_input.is_some() {
            ModalKind::Palette
        } else if ws.theme_picker_input.is_some() {
            ModalKind::Theme
        } else if ws.settings_text_edit.is_some()
            || ws.settings_nav.as_ref().is_some_and(|nav| nav.capturing)
        {
            ModalKind::SettingsCapture
        } else if ws.settings_nav.is_some() {
            ModalKind::Settings
        } else if ws.layout_picker_input.is_some() {
            ModalKind::Layout
        } else if ws.ssh_input.is_some() {
            ModalKind::Ssh
        } else if ws.editing_title.is_some() {
            ModalKind::Title
        } else if ws.hint_state.is_some() {
            ModalKind::Hint
        } else {
            return None;
        };
        let field_hit = self
            .modal_text_projection(ws)
            .is_some_and(|field| rect_contains(field.rect, x, y));
        let (cw, ch) = self.menu_cell(ws);
        let surface = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let inside = if matches!(kind, ModalKind::Settings | ModalKind::SettingsCapture) {
            self.settings_overlay_projection(ws).is_some_and(|set| {
                let geometry =
                    kettle_render::settings_panel_geometry(&set, cw, ch, surface.0, surface.1);
                rect_contains(
                    if kind == ModalKind::Settings {
                        geometry.rect
                    } else {
                        geometry.focused_rect
                    },
                    x,
                    y,
                )
            })
        } else {
            field_hit
                || open_picker_list(ws, &self.cfg, &self.ui_text, self.update_notice().as_ref())
                    .and_then(|list| picker_context_menu(list, surface, (cw, ch)))
                    .is_some_and(|menu| {
                        rect_contains(
                            (
                                menu.anchor.0,
                                menu.anchor.1,
                                menu.panel_w_clamped,
                                menu.panel_h_clamped,
                            ),
                            x,
                            y,
                        )
                    })
        };
        Some((kind, inside))
    }

    pub(super) fn modal_text_projection(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::TextFieldOverlay> {
        let editor = modal_editor(ws)?;
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let (cw, ch) = self.menu_cell(ws);
        let mut rect = (0.0, (sh - ch - 10.0).max(0.0), sw, ch + 10.0);
        let mut hint = String::new();
        let prefix = if ws.palette_input.is_some() {
            "  \u{2318} ".to_string()
        } else if ws.theme_picker_input.is_some() {
            kettle_render::theme_picker_prefix(&self.ui_text)
        } else if ws.settings_text_edit.is_some() {
            let set = self.settings_overlay_projection(ws)?;
            rect = kettle_render::settings_value_rect(
                &set,
                cw,
                ch,
                sw,
                sh,
                self.overlay_text_cell_width(ws),
            );
            String::new()
        } else if ws.layout_picker_input.is_some() {
            format!(
                "  \u{25a4} {} ",
                self.ui_text.text(kettle_i18n::Text::PickerLayoutPrompt)
            )
        } else if ws.ssh_input.is_some() {
            "  ssh \u{276f} ".to_string()
        } else {
            let state = ws.editing_title.as_ref()?;
            rect = self.title_edit_rect(ws);
            kettle_render::title_edit_prefix(title_edit_label(state.scope, &self.ui_text))
        };
        if ws.settings_text_edit.is_none() {
            use kettle_i18n::Text as T;
            let hints: &[(&str, T)] = if ws.palette_input.is_some() {
                &[
                    ("Enter", T::PickerHintRun),
                    ("Tab/↑↓", T::PickerHintSelect),
                    ("Esc", T::PickerHintCancel),
                ]
            } else if ws.theme_picker_input.is_some() {
                &[
                    ("Enter", T::PickerHintKeep),
                    ("↑↓", T::PickerHintSelect),
                    ("Esc", T::PickerHintRestore),
                ]
            } else if ws.layout_picker_input.is_some() {
                &[
                    ("Enter", T::PickerHintSpawn),
                    ("Tab/↑↓", T::PickerHintSelect),
                    ("Esc", T::PickerHintCancel),
                ]
            } else if ws.ssh_input.is_some() {
                &[
                    ("Enter", T::PickerHintConnect),
                    ("Tab", T::PickerHintComplete),
                    ("↑↓", T::PickerHintSelect),
                    ("Esc", T::PickerHintCancel),
                ]
            } else {
                &[("Enter", T::EditHintApply), ("Esc", T::PickerHintCancel)]
            };
            hint = picker_hint(&self.ui_text, hints);
        }
        let (text, cursor_byte, selection) = project_modal_editor(editor, ws.ime_preedit.as_ref());
        let mut field = kettle_render::TextFieldOverlay {
            text,
            cursor_byte,
            selection,
            rect,
            prefix,
            hint,
            horizontal_scroll: 0,
            accent_palette: if ws.ssh_input.is_some() {
                4
            } else if ws.layout_picker_input.is_some() {
                6
            } else if ws.editing_title.is_some() {
                3
            } else {
                5
            },
        };
        let width = kettle_render::text_field_rects(&field, self.overlay_text_cell_width(ws))[1].2;
        field.horizontal_scroll = crate::text_editor::TextEditor::visible_scroll_for(
            &field.text,
            field.cursor_byte,
            editor.horizontal_scroll(),
            (width / self.overlay_text_cell_width(ws)).floor().max(1.0) as usize,
        );
        Some(field)
    }

    pub(super) fn sync_modal_editor_scroll(&self, ws: &mut WindowState) {
        let Some(field) = self.modal_text_projection(ws) else {
            return;
        };
        let cw = self.overlay_text_cell_width(ws);
        let width = kettle_render::text_field_rects(&field, self.overlay_text_cell_width(ws))[1].2;
        if let Some(editor) = modal_editor_mut(ws) {
            editor.ensure_cursor_visible((width / cw).floor().max(1.0) as usize);
        }
    }

    pub(super) fn modal_text_pointer(&self, ws: &mut WindowState, press: bool) -> bool {
        if !press && !ws.dragging_modal_editor {
            return false;
        }
        let Some(mut field) = self.modal_text_projection(ws) else {
            ws.dragging_modal_editor = false;
            return false;
        };
        let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let slot = kettle_render::text_field_rects(&field, self.overlay_text_cell_width(ws))[1];
        if press && !rect_contains(slot, x, y) {
            return false;
        }
        if ws.ime_preedit.is_some() {
            let window = ws.window.clone();
            cancel_modal_pointer_composition(
                ws,
                |allowed| {
                    if let Some(window) = &window {
                        window.set_ime_allowed(allowed);
                    }
                },
                || {
                    #[cfg(target_os = "macos")]
                    if let Some(window) = &window {
                        discard_native_marked_text(window);
                    }
                },
            );
            let Some(committed) = self.modal_text_projection(ws) else {
                return false;
            };
            field = committed;
        }
        let Some(renderer) = ws.renderer.as_mut() else {
            return false;
        };
        let layout = renderer.measure_text_field(&field);
        let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let (ex, ey, ew, eh) = layout.editor;
        if press && !(x >= ex && x < ex + ew && y >= ey && y < ey + eh) {
            return false;
        }
        let byte = crate::text_editor::nearest_stop_byte(&layout.stops, x).unwrap_or(0);
        let selecting = !press || ws.mods.shift_key();
        if let Some(editor) = modal_editor_mut(ws) {
            editor.set_cursor_at(crate::text_editor::QueryPoint::Byte(byte), selecting);
        }
        ws.dragging_modal_editor = true;
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    fn modal_editor_key(&mut self, ws: &mut WindowState, key: &Key) -> bool {
        let Some(editor) = modal_editor(ws) else {
            return false;
        };
        let before = editor.text().to_string();
        let clipboard = std::cell::RefCell::new(self.clipboard.as_mut());
        let handled = edit_modal_text(
            ws,
            key,
            || {
                clipboard
                    .borrow_mut()
                    .as_mut()
                    .and_then(|cb| cb.get_text().ok())
            },
            |text| {
                clipboard
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(|cb| cb.set_text(text.to_string()).is_ok())
            },
        );
        if handled {
            let changed = modal_editor(ws).is_some_and(|editor| editor.text() != before);
            if changed {
                reset_modal_results(ws);
            }
            ws.accessibility_pending = true;
            self.update_ime_cursor_area(ws);
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        handled
    }

    pub(super) fn confirmation_layout(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::ConfirmDialogLayout> {
        let dialog = ws.confirm_dialog.as_ref()?;
        let overlay = confirm_dialog_overlay(dialog, &self.ui_text);
        let window = ws.window.as_ref()?;
        let size = window.inner_size();
        Some(kettle_render::ConfirmDialogLayout::new(
            (size.width as f32, size.height as f32),
            (
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            ),
            window.scale_factor() as f32,
            &overlay,
        ))
    }

    pub(super) fn confirm_button_hovered(&self, ws: &WindowState) -> bool {
        self.confirmation_layout(ws).is_some_and(|layout| {
            confirm_dialog_button_hit(&layout, ws.cursor.x as f32, ws.cursor.y as f32).is_some()
        })
    }

    pub(super) fn confirm_mouse_press(
        &mut self,
        ws: &mut WindowState,
        _event_loop: &ActiveEventLoop,
        bcode: u8,
    ) -> bool {
        if ws.confirm_dialog.is_none() {
            if bcode == 0 {
                ws.confirm_pointer_release = false;
            }
            return false;
        }
        if bcode == 0 {
            ws.confirm_pointer_release = true;
            let armed = self.confirmation_layout(ws).and_then(|layout| {
                let index =
                    confirm_dialog_button_hit(&layout, ws.cursor.x as f32, ws.cursor.y as f32)?;
                Some((index, layout.buttons[index]))
            });
            if let Some(dialog) = ws.confirm_dialog.as_mut() {
                dialog.armed_press = armed;
                dialog.space_pressed = None;
                if let Some((index, _)) = armed {
                    dialog.focus_idx = index;
                }
            }
        }
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    pub(super) fn confirm_mouse_release(&mut self, ws: &mut WindowState, bcode: u8) -> bool {
        let captured = bcode == 0 && std::mem::take(&mut ws.confirm_pointer_release);
        if !captured && ws.confirm_dialog.is_none() {
            return false;
        }
        if bcode == 0 {
            let armed = ws
                .confirm_dialog
                .as_mut()
                .and_then(|dialog| dialog.armed_press.take());
            let target = self.confirmation_layout(ws).and_then(|layout| {
                confirm_release_target(armed, &layout, ws.cursor.x as f32, ws.cursor.y as f32)
            });
            if let Some(index) = target {
                self.resolve_confirm_button(ws, index);
            }
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    pub(super) fn resolve_confirm_button(&mut self, ws: &mut WindowState, index: usize) {
        let Some(dialog) = ws.confirm_dialog.as_ref() else {
            return;
        };
        let Some(button) = dialog.buttons.get(index) else {
            return;
        };
        match button {
            ConfirmButton::Cancel => self.cancel_confirm_dialog(ws),
            ConfirmButton::Confirm { .. } => {
                let action = dialog.on_confirm.clone();
                ws.confirm_dialog = None;
                self.dispatch_confirm_action(ws, action);
            }
        }
        ws.wheel.reset();
        ws.lane_wheel.reset();
        ws.accessibility_pending = true;
    }

    pub(super) fn confirm_cancel_press(&self, ws: &mut WindowState) {
        if let Some(dialog) = ws.confirm_dialog.as_mut() {
            dialog.armed_press = None;
            dialog.space_pressed = None;
        }
    }

    pub(super) fn confirm_scroll(&self, ws: &mut WindowState, delta: isize, focus: bool) {
        let Some(layout) = self.confirmation_layout(ws) else {
            return;
        };
        if let Some(dialog) = ws.confirm_dialog.as_mut() {
            dialog.scroll = if focus {
                layout.scroll_to_button(dialog.focus_idx)
            } else {
                layout
                    .scroll_offset
                    .saturating_add_signed(delta)
                    .min(layout.max_scroll)
            };
            dialog.armed_press = None;
        }
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn confirm_space_release(&mut self, ws: &mut WindowState, ui_owned: bool) {
        let target = ws.confirm_dialog.as_mut().and_then(|dialog| {
            confirmation_space_release_target(&mut dialog.space_pressed, dialog.focus_idx, ui_owned)
        });
        if let Some(index) = target {
            self.resolve_confirm_button(ws, index);
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn install_confirm_dialog(
        &mut self,
        ws: &mut WindowState,
        dialog: ConfirmDialogState,
    ) {
        // A confirmation takes exclusive pointer ownership just like every
        // other modal. Commit a completed selection before ending its gesture;
        // otherwise the visible highlight survives while copy-on-select is
        // silently lost.
        self.finish_selection_gesture(ws);
        ws.card_press = None;
        ws.dragging_split = None;
        // The press that opens a question also owns its later release.
        ws.confirm_pointer_release |= ws
            .held_buttons
            .dragging()
            .is_some_and(|held| held.button == 0);
        // A modal ends the terminal's mouse gesture now. Its physical release
        // is consumed later, so send each program its owed release first.
        self.release_held_buttons(ws, |_| true);
        // And the keyboard, from a preview lane holding it.
        self.exit_preview_focus(ws);
        ws.wheel.reset();
        ws.lane_wheel.reset();
        ws.confirm_dialog = Some(dialog);
        ws.accessibility_pending = true;
    }

    /// Scan the focused pane's visible grid for quick-select targets and
    /// assign each a short label.
    pub(super) fn collect_hints(&mut self, ws: &mut WindowState, preview: bool) -> Vec<HintTarget> {
        use kettle_core::hints;
        use kettle_core::{Column, Dimensions, Line, Point};
        let Some(pane) = ws.mux.active_focus() else {
            return Vec::new();
        };
        let Some(p) = ws.mux.focused() else {
            return Vec::new();
        };
        let Ok(t) = p.term.term.lock() else {
            return Vec::new();
        };
        let g = t.grid();
        let (rows, cols) = (g.screen_lines(), g.columns());
        // Each card the grid shows is a target too, read from the same grid
        // as the text under the same lock, so a scroll or new output since
        // the last frame moves both alike. It is labelled at its first
        // visible cell; text inside it is its marks, not something to pick.
        let mut snap = kettle_render::PaneSnapshot::default();
        snap.capture_with_card_marks(&t, true);
        let offset = i32::try_from(g.display_offset()).unwrap_or(i32::MAX);
        let last_row = i32::try_from(rows).unwrap_or(i32::MAX).saturating_sub(1);
        let cards: Vec<CardCells> = p
            .inline_cards
            .placements(&snap)
            .into_iter()
            .filter(|card| self.media.cards.item(pane, card.nonce).is_some())
            .filter_map(|card| {
                let top = card.line.checked_add(offset)?;
                let bottom = top.checked_add(i32::from(card.rows).max(1) - 1)?;
                if bottom < 0 || top > last_row || cols == 0 {
                    return None;
                }
                let right = (card.column + usize::from(card.columns).max(1) - 1).min(cols - 1);
                Some((
                    (usize::try_from(top.max(0)).ok()?, card.column.min(cols - 1)),
                    (usize::try_from(bottom.min(last_row)).ok()?, right),
                    card.nonce,
                ))
            })
            .collect();
        // Convert each viewport row to its grid-absolute line so hint detection
        // scans the visible rows (including history when scrolled back), not the
        // active screen. Otherwise a label drawn over a visible URL would open
        // the active-screen URL at the same index. `HintTarget.row` stays
        // viewport-relative for label placement.
        let off = g.display_offset();
        let lines: Vec<i32> = (0..rows)
            .map(|r| {
                let base = viewport_point_to_grid(Point::new(Line(r as i32), Column(0)), off);
                base.line.0
            })
            .collect();
        let mut found = hint_targets_with_cards(hints::detect_rows(g, &lines, cols), &cards);
        // Quick select for previews labels only what a lane can show.
        if preview {
            found.retain(|(_, _, what)| hint_previews(what));
        }
        let labels = hints::labels(found.len(), hints::ALPHABET);
        found
            .into_iter()
            .zip(labels)
            .map(|((row, col, what), label)| HintTarget {
                row,
                col,
                label,
                pane,
                what,
            })
            .collect()
    }

    /// Whether the focused Settings row is a number, which Left/Right step.
    pub(super) fn settings_row_steps(&self, ws: &WindowState) -> bool {
        let Some(nav) = ws.settings_nav.as_ref() else {
            return false;
        };
        crate::settings::categories(&self.gpu_choices)
            .get(nav.category)
            .and_then(|category| category.fields.get(nav.field))
            .is_some_and(|field| matches!(field.kind, crate::settings::FieldKind::Number { .. }))
    }

    /// Build the renderer-side settings projection from the live `settings_nav`
    /// and config. Used by the draw path AND the mouse hit-test, so the
    /// painted panel and the clickable regions are computed from one source.
    /// The theme picker hides the panel that opened it, so the preview shows on
    /// the whole window and a click cannot reach a row it covers.
    pub(super) fn settings_overlay_projection(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::SettingsOverlay> {
        if ws.theme_picker_input.is_some() {
            return None;
        }
        let nav = ws.settings_nav.as_ref()?;
        let cats = crate::settings::categories(&self.gpu_choices);
        let cat = nav.category.min(cats.len().saturating_sub(1));
        let active = &cats[cat];
        let tr = &self.ui_text;
        let (label_cols, value_cols) = crate::settings::column_widths(&self.cfg, &cats, tr);
        let fld = nav.field.min(active.fields.len().saturating_sub(1));
        Some(kettle_render::SettingsOverlay {
            categories: cats.iter().map(|c| tr.text(c.name).to_string()).collect(),
            label_cols,
            value_cols,
            active_category: cat,
            background_page: active.name == kettle_i18n::Text::SettingsCategoryBackground,
            rows: active
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    // An open inline text edit for this field shows its
                    // editable buffer + a caret instead of the stored value.
                    let editing = ws
                        .settings_text_edit
                        .as_ref()
                        .is_some_and(|e| e.key == f.key);
                    let value = if editing {
                        String::new()
                    } else if i == fld && nav.capturing {
                        // Capture prompt on the focused keybind row.
                        tr.text(kettle_i18n::Text::SettingsPressChord).to_string()
                    } else {
                        crate::settings::read(&self.cfg, f, tr)
                    };
                    kettle_render::SettingsRow {
                        label: tr.text(f.label).to_string(),
                        value,
                        disabled: crate::settings::field_disabled(&self.cfg, f.key),
                    }
                })
                .collect(),
            focused_row: fld,
            title: tr.settings_heading(tr.text(active.name)),
            hints: settings_hints(tr, self.cfg.vim_menu_nav),
            footer_note: {
                let graphics = active.name == kettle_i18n::Text::SettingsCategoryGraphics;
                let active_gpu = graphics.then(|| {
                    self.gpu
                        .as_ref()
                        .map(|g| {
                            let i = g.adapter_info();
                            let backend = match i.backend {
                                "DX12" => "DirectX 12",
                                "GL" => "OpenGL",
                                backend => backend,
                            };
                            tr.settings_active_gpu(&i.name, gpu_kind_text(tr, i.kind), backend)
                        })
                        .unwrap_or_else(|| {
                            tr.text(kettle_i18n::Text::SettingsActiveGpuInitializing)
                                .to_string()
                        })
                });
                settings_footer_note(
                    tr,
                    active_gpu.as_deref(),
                    active.fields.get(fld).map(|f| f.key),
                    ws.settings_restart_pending,
                    self.language_change_pending(),
                    match active.fields.get(fld).map(|f| f.key) {
                        Some("agent-display-claude-code") => Some(claude_code_note(
                            self.agent_previews_live(),
                            crate::agent_plugin::refusal(),
                        )),
                        Some("agent-display-codex") => Some(codex_note(
                            self.agent_previews_live(),
                            crate::codex_shell::refusal(),
                        )),
                        _ => Some(self.agent_previews_note()),
                    },
                )
            },
        })
    }

    /// Handle a mouse interaction with the settings overlay. `dir` is
    /// the adjust direction for a field (left-click `+1`, right-click `-1`,
    /// wheel `±1`); a category-tab hit switches category. `is_click` (vs a wheel)
    /// makes a hit OUTSIDE the panel close settings and makes inert hits consume
    /// the event; a wheel outside/inert is left for the modal swallow so a stray
    /// scroll can't dismiss the panel. Returns `true` if the event was consumed.
    pub(super) fn settings_mouse(
        &mut self,
        ws: &mut WindowState,
        dir: i32,
        is_click: bool,
    ) -> bool {
        if ws.settings_nav.is_none() {
            return false;
        }
        if ws.settings_text_edit.is_some()
            || ws.settings_nav.as_ref().is_some_and(|nav| nav.capturing)
        {
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
            return true;
        }
        let Some(set) = self.settings_overlay_projection(ws) else {
            return false;
        };
        let (cw, ch) = self.menu_cell(ws);
        let (sw, sh) = ws
            .renderer
            .as_ref()
            .map(|r| {
                let (w, h) = r.surface_size();
                (w as f32, h as f32)
            })
            .unwrap_or((800.0, 600.0));
        let (mx, my) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let hit = kettle_render::settings_hit_test(&set, cw, ch, sw, sh, mx, my);
        match hit {
            kettle_render::SettingsHit::Outside => {
                if !is_click {
                    return false; // let a stray wheel fall through to the swallow
                }
                ws.settings_nav = None;
            }
            kettle_render::SettingsHit::Inert => {
                if !is_click {
                    return false;
                }
            }
            kettle_render::SettingsHit::Category(i) => {
                if let Some(nav) = ws.settings_nav.as_mut() {
                    let cats = crate::settings::categories(&self.gpu_choices);
                    nav.category = i.min(cats.len().saturating_sub(1));
                    // Clamp the focused field into the new category + cancel any
                    // in-progress keybind capture.
                    let fcount = cats[nav.category].fields.len().max(1);
                    nav.field = nav.field.min(fcount - 1);
                    nav.capturing = false;
                }
            }
            kettle_render::SettingsHit::Field(f) => {
                let cats = crate::settings::categories(&self.gpu_choices);
                let cat = ws
                    .settings_nav
                    .as_ref()
                    .map(|n| n.category.min(cats.len().saturating_sub(1)))
                    .unwrap_or(0);
                let fcount = cats[cat].fields.len();
                if f < fcount {
                    let key = cats[cat].fields[f].key;
                    // A dimmed/inapplicable row is a no-op (but consumes the click).
                    if !crate::settings::field_disabled(&self.cfg, key) {
                        if let Some(nav) = ws.settings_nav.as_mut() {
                            nav.field = f;
                            nav.capturing = false;
                        }
                        // Keybind + text rows ACTIVATE on click (dir 0 — capture /
                        // open prompt); a wheel just focuses them. A click on the
                        // Theme row opens the theme picker, and a wheel steps it.
                        // Other value rows cycle.
                        let field = &cats[cat].fields[f];
                        let activate =
                            crate::settings::is_keybind(field) || crate::settings::is_text(field);
                        if activate {
                            if is_click {
                                self.settings_adjust(ws, &cats, cat, f, 0);
                            }
                        } else if is_click && crate::settings::opens_theme_picker(field) {
                            self.settings_adjust(ws, &cats, cat, f, 0);
                        } else {
                            self.settings_adjust(ws, &cats, cat, f, dir);
                        }
                    }
                }
            }
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        true
    }

    /// Raise the `ask-before-closing` prompt if the policy calls for one.
    ///
    /// Returns `true` when the prompt is up and the caller must abandon its
    /// close, `false` when the close should go ahead untouched. Every close
    /// ACTION routes through here so none of them can drift out of step: the
    /// tab-bar ✕, middle-click, and the three close actions all ask the same
    /// question the same way.
    ///
    /// The titlebar ✕ and Alt+F4 are the exception. They are OS window-destroy
    /// requests rather than Kettle commands, so `WindowEvent::CloseRequested`
    /// reaches this only under `ask-before-closing = always`.
    pub(super) fn confirm_close(
        &mut self,
        ws: &mut WindowState,
        scope: usize,
        busy: usize,
        on_confirm: ConfirmAction,
    ) -> bool {
        // The two questions are deliberately separate. `busy == 0` is the
        // all-idle skip — nothing is running, so there is nothing to lose and
        // no reason to interrupt. The single-vs-multiple decision then uses
        // the TOTAL scope, not the busy count, so a 3-pane tab with one busy
        // pane still prompts under `multiple-terminals` instead of silently
        // taking all three.
        if busy == 0 || !self.cfg.ask_before_closing.should_prompt(scope) {
            return false;
        }
        use kettle_i18n::Text;
        let (title, affirmative) = match &on_confirm {
            ConfirmAction::CloseWindow { .. } => (
                Text::ConfirmTitleCloseWindow,
                Text::ConfirmButtonCloseWindow,
            ),
            ConfirmAction::CloseTabHolding(_) => {
                (Text::ConfirmTitleCloseTab, Text::ConfirmButtonCloseTab)
            }
            ConfirmAction::ClosePane(_) => {
                (Text::ConfirmTitleClosePane, Text::ConfirmButtonClosePane)
            }
            ConfirmAction::RebindKeybind { .. } => return false,
        };
        let consequence = self.ui_text.text(Text::ConfirmCloseConsequences);
        let body = if matches!(&on_confirm, ConfirmAction::ClosePane(_)) {
            consequence.to_string()
        } else {
            let scope_label = if scope == 2 {
                self.ui_text.text(Text::ConfirmCloseBothPanes).to_string()
            } else {
                self.ui_text.confirm_close_scope(scope as u64)
            };
            format!("{scope_label} {consequence}")
        };
        self.close_all_modals(ws);
        self.install_confirm_dialog(
            ws,
            ConfirmDialogState {
                heading: self.ui_text.text(title).to_string(),
                body,
                buttons: vec![
                    ConfirmButton::Cancel,
                    ConfirmButton::Confirm {
                        label: self.ui_text.text(affirmative).to_string(),
                        destructive: true,
                    },
                ],
                focus_idx: 0, // Cancel — safe default.
                on_confirm,
                scroll: 0,
                armed_press: None,
                space_pressed: None,
            },
        );
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        true
    }

    /// Dispatch the accepted confirmation action without asking again.
    pub(super) fn dispatch_confirm_action(&mut self, ws: &mut WindowState, action: ConfirmAction) {
        self.dispatch_confirm_action_arms(ws, action);
        self.hoover_groups(ws);
        // Resize, as `handle_action`'s tail does for every keybind and menu
        // action. A confirmed pane or tab close collapses the layout, and
        // without this the surviving PTYs keep their old rows and columns, so
        // the shell draws into part of its pane with dead space around it and
        // typing does not heal it.
        //
        // It lives here rather than in the arms so a new arm cannot forget it.
        // A window on its way out has nothing left to resize.
        if !self.window_close_pending(ws.seq) {
            self.resize_all(ws);
        }
    }

    pub(super) fn cancel_confirm_dialog(&mut self, ws: &mut WindowState) {
        ws.confirm_dialog = None;
        ws.wheel.reset();
        ws.lane_wheel.reset();
        ws.accessibility_pending = true;
    }

    pub(super) fn dispatch_confirm_action_arms(
        &mut self,
        ws: &mut WindowState,
        action: ConfirmAction,
    ) {
        match action {
            ConfirmAction::CloseWindow { drop_panes } => {
                // Carries the requester's intent: confirming a `close_window`
                // action must still clear the session, and confirming a
                // titlebar close must still leave it restorable. Without this
                // the answer depended on whether the prompt happened to
                // appear.
                self.close_window_now(ws, drop_panes);
            }
            ConfirmAction::CloseTabHolding(panes) => {
                // Re-resolve the tab now: the prompt may have been on screen
                // for a while, and an exiting shell can drop a tab and shift
                // every index after it. `None` means the tab this prompt was
                // about is already gone, so there is nothing left to close.
                let Some(idx) = ws.mux.tab_index_of_any_pane(&panes) else {
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                };
                let last = ws.mux.close_tab_at(idx);
                self.fire_tab_close_event(ws, idx);
                self.save_session(ws);
                if last {
                    self.request_window_close(ws.seq);
                    return;
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ConfirmAction::ClosePane(target) => {
                // Close the pane the PROMPT was about, not whatever happens to
                // be focused now. While the prompt was up the target's own
                // shell may have exited and promoted a sibling into focus —
                // confirming then closed the sibling, which can be a tmux or
                // agent session the user never selected. A target that is
                // already gone means there is nothing left to close.
                // Re-focusing the target moves the ACTIVE TAB to wherever it
                // lives. If the user changed tabs while the prompt was up (the
                // ctl API can, even with a modal on screen), snapping them back
                // to the closed pane's tab is not something they asked for —
                // so remember where they were and put them back.
                let viewing = ws.mux.tab_anchor_panes(ws.mux.active);
                if !ws.mux.focus_pane(target) {
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                self.fire_pane_close_event(ws, target);
                let was_last = ws.mux.close_focused();
                // Resolve the viewed tab again after a removal shifts indices.
                if let Some(viewing) = ws.mux.tab_index_of_any_pane(&viewing) {
                    ws.mux.active = viewing;
                }
                // Honor close_focused()'s return like the
                // keybind ClosePane path — `true` means the last pane closed,
                // so exit; otherwise redraw the collapsed layout (the renderer
                // cache + focus id are stale until a frame is scheduled).
                if was_last {
                    self.save_session(ws);
                    self.request_window_close(ws.seq);
                    return;
                }
                self.save_session(ws);
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ConfirmAction::RebindKeybind {
                trig,
                act,
                action_name,
            } => {
                self.apply_keybind_rebind(trig, act, action_name);
            }
        }
    }

    /// Route a key through the same confirmation state machine used by real
    /// winit keyboard events. Unsupported keys are swallowed by the modal.
    pub(super) fn confirm_dialog_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        _event_loop: &ActiveEventLoop,
    ) {
        if confirmation_space_key(key) {
            if !ws.mods.control_key()
                && !ws.mods.alt_key()
                && !ws.mods.super_key()
                && let Some(dialog) = ws.confirm_dialog.as_mut()
            {
                dialog.space_pressed = Some(dialog.focus_idx);
                dialog.armed_press = None;
            }
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        let page = self.confirmation_layout(ws).map_or(1, |layout| {
            (layout.body_rect.3 / layout.body_line).floor().max(1.0) as isize
        });
        let scroll = match key {
            Key::Named(NamedKey::ArrowUp) => Some(-1),
            Key::Named(NamedKey::ArrowDown) => Some(1),
            Key::Named(NamedKey::PageUp) => Some(-page),
            Key::Named(NamedKey::PageDown) => Some(page),
            Key::Named(NamedKey::Home) => Some(isize::MIN),
            Key::Named(NamedKey::End) => Some(isize::MAX),
            _ => None,
        };
        if let Some(delta) = scroll {
            self.confirm_scroll(ws, delta, false);
            return;
        }
        let key = match key {
            Key::Named(NamedKey::Escape) => Some(ConfirmKey::Escape),
            Key::Named(NamedKey::Enter) => Some(ConfirmKey::Enter),
            Key::Named(NamedKey::Tab) => Some(if ws.mods.shift_key() {
                ConfirmKey::ShiftTab
            } else {
                ConfirmKey::Tab
            }),
            Key::Named(NamedKey::ArrowLeft) => Some(ConfirmKey::Left),
            Key::Named(NamedKey::ArrowRight) => Some(ConfirmKey::Right),
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && !ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && !ws.mods.super_key() =>
            {
                match s.to_ascii_lowercase().as_str() {
                    "y" => Some(ConfirmKey::Yes),
                    "n" => Some(ConfirmKey::No),
                    "h" => Some(ConfirmKey::Left),
                    "l" => Some(ConfirmKey::Right),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(key) = key
            && let Some(state) = ws.confirm_dialog.as_ref()
        {
            let result = confirm_dialog_keypress(state.focus_idx, state.buttons.len(), key);
            match result {
                ConfirmKeyResult::Move(index) => {
                    if let Some(dialog) = ws.confirm_dialog.as_mut() {
                        dialog.focus_idx = index;
                        dialog.space_pressed = None;
                        dialog.armed_press = None;
                    }
                    self.confirm_scroll(ws, 0, true);
                }
                ConfirmKeyResult::Confirm => {
                    let index = state
                        .buttons
                        .iter()
                        .position(|button| matches!(button, ConfirmButton::Confirm { .. }));
                    if let Some(index) = index {
                        self.resolve_confirm_button(ws, index);
                    }
                }
                ConfirmKeyResult::Cancel => self.cancel_confirm_dialog(ws),
                ConfirmKeyResult::Ignore => {}
            }
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
    }

    pub(super) fn confirm_wheel(&self, ws: &mut WindowState, lines: i32) -> bool {
        let Some(layout) = self.confirmation_layout(ws) else {
            return false;
        };
        let viewport = if layout.whole_scroll {
            layout.rect
        } else {
            layout.body_rect
        };
        if rect_contains(viewport, ws.cursor.x as f32, ws.cursor.y as f32) {
            self.confirm_scroll(ws, lines.saturating_neg() as isize, false);
        }
        true
    }

    /// Key handling for the title-edit overlay (`Edit tab/window/pane title`
    /// and `Edit pane group`). Esc cancels, Enter applies via
    /// [`Self::apply_title_edit`]; editing and clipboard commands use the shared
    /// editor, and printable input replaces the selection or inserts at the caret.
    ///
    /// Shared by the `KeyboardInput` arm and `dispatch_ui_key`, so end-to-end
    /// tests type into this modal through the same rules as a real keystroke.
    pub(super) fn title_edit_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        if self.modal_editor_key(ws, key) {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                // The chrome strip `tab_bar_h` materialised for this modal
                // disappears now, so the content rectangle changes and the PTYs
                // must follow it.
                ws.editing_title = None;
                ws.pending_resize = true;
            }
            Key::Named(NamedKey::Enter) => {
                self.apply_title_edit(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(state) = ws.editing_title.as_mut() {
                    crate::modal_input::backspace(&mut state.input);
                }
            }
            Key::Character(s) if crate::modal_input::is_paste_chord(Some(s.as_str()), ws.mods) => {
                if let Some(pasted) = self.modal_clipboard_text()
                    && let Some(state) = ws.editing_title.as_mut()
                {
                    crate::modal_input::push_text(&mut state.input, &pasted);
                }
            }
            _ => {
                if let Some(s) = crate::modal_input::accept_text(text, ws.mods)
                    && let Some(state) = ws.editing_title.as_mut()
                {
                    crate::modal_input::push_text(&mut state.input, s);
                }
            }
        }
    }

    /// Clipboard text for a modal field. The shared editor bounds and normalizes it.
    pub(super) fn modal_clipboard_text(&mut self) -> Option<String> {
        let clipboard = self.clipboard.as_mut()?;
        match clipboard.get_text() {
            Ok(text) => (!text.is_empty()).then_some(text),
            Err(error) => {
                log::warn!("modal paste: clipboard read failed: {error}");
                None
            }
        }
    }

    /// Command-palette key handling: fuzzy-filter as you type, `Tab`/`↑↓`
    /// to move the selection, `Enter` to run it, `Esc` to cancel.
    /// Quick-select hint key handling: type the label of a target to act on
    /// it (open URLs, copy paths/hashes/IPs); `Esc` cancels.
    /// Vi-mode key dispatcher backed by alacritty_terminal's native vi cursor
    /// and selection. The engine owns scrollback/reflow coordinates and keeps
    /// them stable as output rotates or evicts history.
    pub(super) fn vi_mode_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.exit_vi_mode(ws);
            return;
        }
        let Some(owner) = ws.vi_mode else {
            return;
        };
        let Some(pane) = ws.mux.panes.get(&owner.pane_id) else {
            ws.vi_mode = None;
            return;
        };
        let mut term = pane
            .term
            .term
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !term.mode().contains(kettle_core::TermMode::VI) {
            drop(term);
            ws.vi_mode = None;
            return;
        }

        let ch = crate::modal_input::accept_text(text, ws.mods)
            .and_then(|s| s.chars().next())
            .unwrap_or('\0');
        let motion = match (ch, key) {
            ('h', _) | ('\0', Key::Named(NamedKey::ArrowLeft)) => Some(kettle_core::ViMotion::Left),
            ('l', _) | ('\0', Key::Named(NamedKey::ArrowRight)) => {
                Some(kettle_core::ViMotion::Right)
            }
            ('k', _) | ('\0', Key::Named(NamedKey::ArrowUp)) => Some(kettle_core::ViMotion::Up),
            ('j', _) | ('\0', Key::Named(NamedKey::ArrowDown)) => Some(kettle_core::ViMotion::Down),
            ('0', _) | ('\0', Key::Named(NamedKey::Home)) => Some(kettle_core::ViMotion::First),
            ('^', _) => Some(kettle_core::ViMotion::FirstOccupied),
            ('$', _) | ('\0', Key::Named(NamedKey::End)) => Some(kettle_core::ViMotion::Last),
            ('H', _) => Some(kettle_core::ViMotion::High),
            ('M', _) => Some(kettle_core::ViMotion::Middle),
            ('L', _) => Some(kettle_core::ViMotion::Low),
            ('b', _) => Some(kettle_core::ViMotion::WordLeft),
            ('w', _) => Some(kettle_core::ViMotion::WordRight),
            ('e', _) => Some(kettle_core::ViMotion::WordRightEnd),
            ('B', _) => Some(kettle_core::ViMotion::SemanticLeft),
            ('W', _) => Some(kettle_core::ViMotion::SemanticRight),
            ('E', _) => Some(kettle_core::ViMotion::SemanticRightEnd),
            ('%', _) => Some(kettle_core::ViMotion::Bracket),
            ('{', _) => Some(kettle_core::ViMotion::ParagraphUp),
            ('}', _) => Some(kettle_core::ViMotion::ParagraphDown),
            _ => None,
        };
        if let Some(motion) = motion {
            term.vi_motion(motion);
            return;
        }

        match ch {
            'g' => {
                use kettle_core::Dimensions as _;
                let point = kettle_core::Point::new(term.topmost_line(), kettle_core::Column(0));
                term.vi_goto_point(point);
            }
            'G' => {
                use kettle_core::Dimensions as _;
                let point = kettle_core::Point::new(term.bottommost_line(), kettle_core::Column(0));
                term.vi_goto_point(point);
            }
            'v' => {
                if owner.visual {
                    term.selection = None;
                } else {
                    let point = term.vi_mode_cursor.point;
                    let mut selection = kettle_core::Selection::new(
                        kettle_core::SelectionType::Simple,
                        point,
                        kettle_core::Side::Left,
                    );
                    selection.update(point, kettle_core::Side::Right);
                    term.selection = Some(selection);
                }
                if let Some(state) = ws.vi_mode.as_mut() {
                    state.visual = !owner.visual;
                }
            }
            'y' => {
                let yanked = owner
                    .visual
                    .then(|| term.selection_to_string())
                    .flatten()
                    .unwrap_or_default();
                drop(term);
                let yanked = match kettle_core::scrub_card_markers(yanked) {
                    Ok(text) => text,
                    Err(error) => {
                        log::warn!("vi-mode yank: could not prepare text: {error}");
                        return;
                    }
                };
                if !yanked.is_empty() {
                    let Some(clip) = self.clipboard.as_mut() else {
                        log::warn!(
                            "vi-mode yank: clipboard unavailable (selection of {} bytes not copied)",
                            yanked.len()
                        );
                        return;
                    };
                    if let Err(error) = clip.set_text(yanked) {
                        log::warn!("vi-mode yank: clipboard set_text failed: {error}");
                        return;
                    }
                }
                let Ok(mut term) = pane.term.term.lock() else {
                    return;
                };
                term.selection = None;
                term.toggle_vi_mode();
                drop(term);
                ws.vi_mode = None;
            }
            _ => {}
        }
    }

    pub(super) fn hint_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.hint_state = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some((_, typed)) = ws.hint_state.as_mut() {
                    typed.pop();
                }
            }
            _ => {
                let Some(ch) = crate::modal_input::accept_text(text, ws.mods)
                    .and_then(|t| t.chars().next())
                    .filter(|c| c.is_ascii_alphabetic())
                    .map(|c| c.to_ascii_lowercase())
                else {
                    return;
                };
                // Extend the typed prefix only if it still matches a label.
                let chosen = {
                    let Some((targets, typed)) = ws.hint_state.as_mut() else {
                        return;
                    };
                    let cand = format!("{typed}{ch}");
                    if targets.iter().any(|t| t.label.starts_with(&cand)) {
                        *typed = cand;
                    }
                    let exact: Vec<&HintTarget> =
                        targets.iter().filter(|t| t.label == *typed).collect();
                    (exact.len() == 1).then(|| exact[0].clone())
                };
                if let Some(h) = chosen {
                    ws.hint_state = None;
                    // A label typed with Shift picks the other action. The
                    // modifier, not the letter's case, so Caps Lock is not Shift.
                    let alternate = ws.mods.shift_key();
                    self.act_hint(ws, &h, alternate);
                }
            }
        }
    }

    pub(super) fn act_hint(&mut self, ws: &mut WindowState, h: &HintTarget, alternate: bool) {
        // Quick select for previews previews what was picked, its pane's
        // gate permitting, from the pane's own directory.
        if std::mem::take(&mut ws.hint_preview) {
            if !hint_previews(&h.what) {
                return;
            }
            if let HintWhat::Text { kind, text, .. } = &h.what {
                let url = match kind {
                    kettle_core::hints::Kind::Url => Some(text.clone()),
                    _ => hint_path_url(
                        text,
                        ws.mux
                            .panes
                            .get(&h.pane)
                            .and_then(|pane| pane.term.current_dir_or_native())
                            .as_deref(),
                        crate::mux::home_dir_string().as_deref(),
                    ),
                };
                let tr = self.ui_text;
                match url {
                    Some(url) => self.preview_pane_link(ws, h.pane, &url),
                    None => fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitlePreviewFailed),
                        tr.text(kettle_i18n::Text::NotifyBodyPreviewUnplaced),
                    ),
                }
            }
            return;
        }
        let (kind, text, bounded) = match &h.what {
            // A card opens its item in the viewer, Shift or not.
            HintWhat::Card(nonce) => {
                self.open_card(ws, h.pane, *nonce);
                return;
            }
            HintWhat::Text {
                kind,
                text,
                bounded,
            } => (*kind, text, *bounded),
        };
        let pane = ws.mux.panes.get(&h.pane);
        // A pane that exited took its directory with it, so nothing local can
        // be resolved for it, and a remote pane's paths name files elsewhere.
        // Behind a multiplexer the path opens once the user confirms (see
        // `open_pane_link`).
        let remote = matches!(pane.map(pane_path_origin), None | Some(PathOrigin::Remote));
        let action = hint_action(kind, alternate, remote || !bounded);
        let path_url = match action {
            HintAction::OpenPath => hint_path_url(
                text,
                pane.and_then(|pane| pane.term.current_dir_or_native())
                    .as_deref(),
                crate::mux::home_dir_string().as_deref(),
            ),
            _ => None,
        };
        match (action, path_url) {
            // Route through open_pane_link so a hint honors the pane's origin,
            // the URL safety check, Lua URL handlers and the custom URL
            // handler, like a click.
            (HintAction::OpenUrl, _) => self.open_pane_link(ws, Some(h.pane), text),
            (HintAction::OpenPath, Some(url)) => self.open_pane_link(ws, Some(h.pane), &url),
            // A path Kettle will not open (a climb with `..`, an unknown base,
            // any path on Windows) is copied instead, as for a plain label.
            (HintAction::OpenPath, None) | (HintAction::Copy, _) => {
                if let Some(cb) = self.clipboard.as_mut()
                    && let Err(e) = cb.set_text(text.clone())
                {
                    // Log instead of silently swallowing.
                    log::warn!("clipboard set_text failed (hint copy): {e}");
                }
            }
        }
    }

    pub(super) fn palette_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        text: Option<&str>,
        event_loop: &ActiveEventLoop,
    ) {
        if self.modal_editor_key(ws, key) {
            return;
        }
        let cmds = palette_entries(self.update_notice().as_ref(), &self.ui_text);
        let Some((q, sel)) = ws.palette_input.as_mut() else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.palette_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                crate::modal_input::backspace(q);
                *sel = 0;
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                let n = kettle_config::palette::rank_entries(q, &cmds, &self.ui_text).len();
                if n > 0 {
                    *sel = (*sel + 1) % n;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = kettle_config::palette::rank_entries(q, &cmds, &self.ui_text).len();
                if n > 0 {
                    *sel = (*sel + n - 1) % n;
                }
            }
            Key::Named(NamedKey::Enter) => {
                let ranked = kettle_config::palette::rank_entries(q, &cmds, &self.ui_text);
                let action = ranked.get(*sel).map(|&i| cmds[i].1.clone());
                ws.palette_input = None;
                if let Some(a) = action {
                    self.handle_action(ws, a, event_loop);
                }
            }
            // `vim-menu-nav`: Ctrl+j/Ctrl+k (plus the telescope/fzf
            // Ctrl+n/Ctrl+p idiom) move the selection, while bare letters keep
            // typing into the query. Other Ctrl-chords fall through to the
            // catch-all unchanged.
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                let n = kettle_config::palette::rank_entries(q, &cmds, &self.ui_text).len();
                if n > 0 {
                    *sel = match s.as_str() {
                        "j" | "n" => (*sel + 1) % n,
                        _ => (*sel + n - 1) % n,
                    };
                }
            }
            Key::Character(s) if crate::modal_input::is_paste_chord(Some(s.as_str()), ws.mods) => {
                if let Some(pasted) = self.modal_clipboard_text()
                    && crate::modal_input::push_text(q, &pasted)
                {
                    *sel = 0;
                }
            }
            _ => {
                if let Some(t) = crate::modal_input::accept_text(text, ws.mods)
                    && crate::modal_input::push_text(q, t)
                {
                    *sel = 0;
                }
            }
        }
    }

    /// Apply a Settings-captured keybind rebind live and persist it.
    /// `action_name` is the config token (`Action::from_name`'s input,
    /// e.g. `"copy"`) for the action `act` already decodes to.
    ///
    /// A rebind REPLACES the action's binding, so this first drops every
    /// OTHER chord currently mapped to `act` (else the Keybinds row could show
    /// a stale chord, since the display scans the `keybinds` HashMap
    /// non-deterministically). It persists an `unbind` for each dropped chord
    /// before the new `bind`, so a reload doesn't resurrect the old chord
    /// alongside the new one.
    ///
    /// Shared by the direct (no-conflict) rebind path and
    /// `ConfirmAction::RebindKeybind`, so the two paths can never diverge. A
    /// chord already bound to a DIFFERENT action goes through a confirm dialog
    /// first instead of being stolen silently.
    pub(super) fn apply_keybind_rebind(
        &mut self,
        trig: Trigger,
        act: Action,
        action_name: &str,
    ) -> bool {
        let label = trig.label();
        let stale: Vec<String> = self
            .cfg
            .keybinds
            .iter()
            .filter(|(t, v)| **v == act && **t != trig)
            .map(|(t, _)| t.label())
            .collect();
        self.cfg.keybinds.retain(|_, v| *v != act);
        self.cfg.keybinds.insert(trig, act);
        // The saved line makes this the user's chord on the next reload; mark
        // it now so `keybind-yield` treats it the same way this session.
        self.cfg.keybinds_declared.insert(trig);
        // Persist: unbind the old chord(s), then bind the new one. This
        // also naturally covers stealing a chord from another action —
        // appending `trig=action_name` after any earlier `trig=<other>`
        // line means the last-one-wins reload resolves `trig` to the new
        // action, matching the live HashMap `insert` above.
        let path = self
            .config_path
            .clone()
            .or_else(kettle_config::Config::default_path);
        let saved = persist_keybind_rebind(path.as_deref(), &stale, &label, action_name);
        if !saved {
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleKeybindNotSaved),
                self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
            );
        }
        saved
    }

    /// Keyboard routing while the settings overlay is open.
    /// ↑/↓ move between fields, Tab/Shift+Tab switch category, ←/→ change the
    /// focused field's value, Space/Enter activate (toggle / cycle forward),
    /// Esc closes. Every change persists to the config file via `persist_pref`
    /// and reloads live, so the effect is immediate (matching the right-click
    /// Preferences toggles).
    pub(super) fn settings_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        _event_loop: &ActiveEventLoop,
    ) {
        let cats = crate::settings::categories(&self.gpu_choices);
        if cats.is_empty() {
            ws.settings_nav = None;
            return;
        }
        let (mut cat, mut fld, capturing) = match &ws.settings_nav {
            Some(n) => (n.category.min(cats.len() - 1), n.field, n.capturing),
            None => return,
        };
        let field_count = cats[cat].fields.len();
        if field_count == 0 {
            return;
        }
        if fld >= field_count {
            fld = field_count - 1;
        }

        // Chord-capture mode — the focused Keybind field is waiting
        // for the user to press a chord. Esc cancels. A bare modifier press maps
        // to `None` via `to_kkey`, so we simply stay in capture until a real key
        // arrives. Any other chord is bound to the action live AND appended to
        // the config file so it survives restart.
        if capturing {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                if let Some(n) = ws.settings_nav.as_mut() {
                    n.capturing = false;
                }
                return;
            }
            if let Some(kk) = to_kkey(key) {
                let mods = to_mods(ws.mods);
                // Refuse a modifier-less binding to a
                // text/essential key — it would shadow that key in normal typing
                // everywhere, persisted, with no in-overlay unbind. Stay in
                // capture mode and tell the user instead of soft-bricking.
                if !keybind_chord_is_safe(mods, kk) {
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleKeybindNeedsModifier),
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyBodyKeybindModifier),
                    );
                    return;
                }
                if let Some(action) = crate::settings::keybind_action(&cats[cat].fields[fld])
                    && let Some(act) = Action::from_name(action)
                {
                    let trig = Trigger::new(mods, kk);
                    // A chord already bound to a DIFFERENT action must not be
                    // stolen silently, since the old action's ONLY binding
                    // could vanish with no warning and no undo path. Like the
                    // modifier-safety check above, route it through a confirm
                    // dialog instead of applying it; Cancel is the safe default.
                    if let Some(stolen_from) =
                        self.cfg.keybinds.get(&trig).filter(|v| **v != act).cloned()
                    {
                        let chord_label = trig.label();
                        let stolen_name =
                            crate::settings::confirmation_action_label(&stolen_from, &self.ui_text);
                        let new_name =
                            crate::settings::confirmation_action_label(&act, &self.ui_text);
                        let body = if stolen_name == new_name {
                            self.ui_text
                                .text(kettle_i18n::Text::ConfirmRebindGeneric)
                                .to_string()
                        } else {
                            self.ui_text.confirm_rebind_body(&stolen_name, &new_name)
                        };
                        self.install_confirm_dialog(
                            ws,
                            ConfirmDialogState {
                                heading: self.ui_text.confirm_rebind_title(&chord_label),
                                body,
                                buttons: vec![
                                    ConfirmButton::Cancel,
                                    ConfirmButton::Confirm {
                                        label: self
                                            .ui_text
                                            .text(kettle_i18n::Text::ConfirmButtonReassign)
                                            .to_string(),
                                        destructive: false,
                                    },
                                ],
                                focus_idx: 0,
                                scroll: 0,
                                armed_press: None,
                                space_pressed: None,
                                on_confirm: ConfirmAction::RebindKeybind {
                                    trig,
                                    act,
                                    action_name: action,
                                },
                            },
                        );
                    } else {
                        self.apply_keybind_rebind(trig, act, action);
                    }
                }
                if let Some(n) = ws.settings_nav.as_mut() {
                    n.capturing = false;
                }
            }
            return;
        }

        match key {
            Key::Named(NamedKey::Escape) => {
                ws.settings_nav = None;
                self.reset_blink_phase(ws);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                fld = crate::settings::next_enabled_field(&self.cfg, &cats[cat].fields, fld, 1)
            }
            Key::Named(NamedKey::ArrowUp) => {
                fld = crate::settings::next_enabled_field(&self.cfg, &cats[cat].fields, fld, -1)
            }
            Key::Named(NamedKey::Tab) => {
                let n = cats.len();
                cat = if ws.mods.shift_key() {
                    (cat + n - 1) % n
                } else {
                    (cat + 1) % n
                };
                fld = 0;
            }
            key if confirmation_space_key(key)
                || matches!(
                    key,
                    Key::Named(NamedKey::ArrowRight | NamedKey::ArrowLeft | NamedKey::Enter)
                ) =>
            {
                let dir = match key {
                    Key::Named(NamedKey::ArrowRight) => 1,
                    Key::Named(NamedKey::ArrowLeft) => -1,
                    _ => 0, // Space / Enter = activate (cycle forward / toggle)
                };
                self.settings_adjust(ws, &cats, cat, fld, dir);
            }
            // `vim-menu-nav`: j/k mirror ↓/↑, h/l mirror ←/→, g/G jump to the
            // first/last field, Ctrl+d / Ctrl+u move half a page. Chord-capture
            // mode (handled before this match) already consumed the key, so
            // capturing a chord like bare `j` (refused anyway) or `Ctrl+d`
            // still reaches the capture branch.
            Key::Character(s)
                if self.cfg.vim_menu_nav && !ws.mods.alt_key() && !ws.mods.super_key() =>
            {
                let half = (field_count / 2).max(1);
                // Case-fold so CapsLock can't kill the nav layer; g-vs-G
                // derives from the physical Shift state.
                let folded = s.to_ascii_lowercase();
                match (folded.as_str(), ws.mods.control_key()) {
                    ("j", false) => {
                        fld = crate::settings::next_enabled_field(
                            &self.cfg,
                            &cats[cat].fields,
                            fld,
                            1,
                        )
                    }
                    ("k", false) => {
                        fld = crate::settings::next_enabled_field(
                            &self.cfg,
                            &cats[cat].fields,
                            fld,
                            -1,
                        )
                    }
                    ("g", false) => {
                        fld = if ws.mods.shift_key() {
                            field_count - 1
                        } else {
                            0
                        };
                    }
                    ("h", false) => self.settings_adjust(ws, &cats, cat, fld, -1),
                    ("l", false) => self.settings_adjust(ws, &cats, cat, fld, 1),
                    ("d", true) => fld = (fld + half).min(field_count - 1),
                    ("u", true) => fld = fld.saturating_sub(half),
                    _ => {}
                }
            }
            _ => {}
        }
        if let Some(n) = ws.settings_nav.as_mut() {
            n.category = cat;
            n.field = fld;
        }
    }

    /// ←/→ (`dir = ±1`) or Space/Enter (`dir = 0`) on the focused settings
    /// row. Keybind rows enter chord-capture on activate (±1 no-ops); value
    /// rows persist the stepped value, mirror padding, and live-reload.
    /// vim-menu-nav's `h`/`l` share this exact code path.
    pub(super) fn settings_adjust(
        &mut self,
        ws: &mut WindowState,
        cats: &[crate::settings::Category],
        cat: usize,
        fld: usize,
        dir: i32,
    ) {
        let field = &cats[cat].fields[fld];
        // A gated/inapplicable row never changes (it's drawn dimmed).
        if crate::settings::field_disabled(&self.cfg, field.key) {
            return;
        }
        if crate::settings::is_keybind(field) {
            // Activate on a keybind row → enter chord-capture; ←/→ no-op.
            if dir == 0
                && let Some(n) = ws.settings_nav.as_mut()
            {
                n.category = cat;
                n.field = fld;
                n.capturing = true;
            }
            return;
        }
        if dir == 0 && crate::settings::opens_theme_picker(field) {
            self.open_theme_picker(ws);
            return;
        }
        // A Text row (the image path) opens an inline prompt on activate
        // (dir 0); ←/→ no-op. A gated/disabled Text row can't be edited.
        if crate::settings::is_text(field) {
            if dir == 0 && !crate::settings::field_disabled(&self.cfg, field.key) {
                if let Some(n) = ws.settings_nav.as_mut() {
                    n.category = cat;
                    n.field = fld;
                }
                self.open_settings_text_edit(ws, field.key);
            }
            return;
        }
        // Scope the cfg borrow so reload_config (&mut self) is free.
        let (key_str, new_val) = {
            (
                field.key,
                crate::settings::next_value(&self.cfg, field, dir),
            )
        };
        // The GPU device picker is one row but persists THREE keys
        // (vendor/device/name). "auto" clears the pin; "<vendor>:<device>:<name>"
        // sets it. GPU changes apply on the NEXT launch (the wgpu device/surface
        // graph can't hot-swap and every window shares one adapter), so we flag
        // a restart affordance instead of rebuilding the renderer live.
        if key_str == "gpu" {
            // Fold all three `persist_pref` results together (false on an
            // unexpected `new_val` shape). A partial or total write failure
            // (read-only config, momentarily locked file, no write permission)
            // must not claim "restart to apply" when the pin never reached
            // disk and would silently revert to auto on the next launch.
            let saved = if new_val == "auto" {
                let a = self.persist_pref("gpu-vendor-id", "0");
                let b = self.persist_pref("gpu-device-id", "0");
                let c = self.persist_pref("gpu-name", "");
                a && b && c
            } else if let Some((v, rest)) = new_val.split_once(':')
                && let Some((d, name)) = rest.split_once(':')
            {
                let a = self.persist_pref("gpu-vendor-id", &format!("0x{v}"));
                let b = self.persist_pref("gpu-device-id", &format!("0x{d}"));
                let c = self.persist_pref("gpu-name", name);
                a && b && c
            } else {
                false
            };
            if saved {
                ws.settings_restart_pending = true;
            } else {
                fire_notify(
                    self.ui_text
                        .text(kettle_i18n::Text::NotifyTitleSettingNotSaved),
                    self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
                );
            }
            self.reload_config(ws);
            return;
        }
        // Notify if the Settings change can't
        // be written — it's live this session but lost on restart.
        let saved = self.persist_pref(key_str, &new_val);
        if !saved {
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleSettingNotSaved),
                self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
            );
        }
        // The single "Window padding" control sets *uniform* padding, so
        // mirror the value to the Y axis. Leaving `window-padding-y` at its
        // default gives visibly lopsided padding.
        //
        // Report a failed mirror write like the general path above. The X
        // write can succeed while this one fails (e.g. the config file becomes
        // read-only or briefly locked between the two calls), which would
        // leave lopsided padding after restart with no error shown.
        if key_str == "window-padding-x" && !self.persist_pref("window-padding-y", &new_val) {
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleSettingNotSaved),
                self.ui_text
                    .text(kettle_i18n::Text::NotifyBodyPaddingPartial),
            );
        }
        // The remaining GPU policy keys (power preference / backend /
        // force-software) also only take effect on restart.
        if saved
            && matches!(
                key_str,
                "gpu-power-preference" | "gpu-backend" | "gpu-force-software"
            )
        {
            ws.settings_restart_pending = true;
        }
        if saved
            && matches!(
                key_str,
                "background-opacity" | "background-type" | "window-blur"
            )
        {
            // Winit's alpha-capable window attribute is fixed at creation.
            // Renderer colors reload immediately, but crossing the opaque /
            // translucent boundary or enabling native material needs a fresh
            // window before the OS compositor can show the result.
            ws.settings_restart_pending = true;
        }
        self.reload_config(ws);
        // A theme pick needs no `save_session()`. `persist_pref` above durably
        // writes the config `theme =` line, the single source of truth on restart.
    }

    /// Open the inline text prompt for a [`crate::settings::FieldKind::Text`]
    /// row, pre-filled with the current config value.
    pub(super) fn open_settings_text_edit(&mut self, ws: &mut WindowState, key: &'static str) {
        let cur = match key {
            "background-image" => self.cfg.background_image.clone(),
            _ => String::new(),
        };
        ws.settings_text_edit = begin_settings_text_edit(key, &cur);
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Keyboard routing while the settings inline text prompt is open.
    /// Esc cancels, Enter persists and reloads; editing commands use the shared editor.
    pub(super) fn settings_text_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        text: Option<&str>,
    ) {
        if self.modal_editor_key(ws, key) {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.settings_text_edit = None;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(e) = ws.settings_text_edit.as_mut() {
                    crate::modal_input::backspace(&mut e.buf);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(e) = ws.settings_text_edit.take() {
                    let val = e.buf.trim().to_string();
                    if !self.persist_pref(e.key, &val) {
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleSettingNotSaved),
                            self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
                        );
                    }
                    self.reload_config(ws);
                }
            }
            Key::Character(s) if crate::modal_input::is_paste_chord(Some(s.as_str()), ws.mods) => {
                if let Some(pasted) = self.modal_clipboard_text()
                    && let Some(e) = ws.settings_text_edit.as_mut()
                {
                    crate::modal_input::push_text(&mut e.buf, &pasted);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            _ => {
                if let Some(t) = crate::modal_input::accept_text(text, ws.mods)
                    && let Some(e) = ws.settings_text_edit.as_mut()
                {
                    crate::modal_input::push_text(&mut e.buf, t);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
        }
    }

    /// Terminator parity, `layoutlauncher.py`:
    /// keyboard routing while the layout picker overlay is open.
    /// Same shape as `palette_key` but ranks against
    /// the names captured when it opened and dispatches by spawning
    /// `kettle --layout NAME` as a new window.
    pub(super) fn layout_picker_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        text: Option<&str>,
    ) {
        if self.modal_editor_key(ws, key) {
            return;
        }
        let layouts = &ws.layout_picker_entries;
        let Some((q, sel)) = ws.layout_picker_input.as_mut() else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.layout_picker_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                crate::modal_input::backspace(q);
                *sel = 0;
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                let n = rank_layouts(q, layouts).len();
                if n > 0 {
                    *sel = (*sel + 1) % n;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = rank_layouts(q, layouts).len();
                if n > 0 {
                    *sel = (*sel + n - 1) % n;
                }
            }
            // `vim-menu-nav`: Ctrl+j/k (+ Ctrl+n/p) move the selection; bare
            // letters keep typing into the filter.
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                let n = rank_layouts(q, layouts).len();
                if n > 0 {
                    *sel = match s.as_str() {
                        "j" | "n" => (*sel + 1) % n,
                        _ => (*sel + n - 1) % n,
                    };
                }
            }
            Key::Named(NamedKey::Enter) => {
                let ranked = rank_layouts(q, layouts);
                let name = ranked.get(*sel).map(|&i| layouts[i].clone());
                ws.layout_picker_input = None;
                if let Some(name) = name {
                    let exe = std::env::current_exe()
                        .unwrap_or_else(|_| std::path::PathBuf::from("kettle"));
                    // The layout opens as its own Kettle process, alive as
                    // long as its windows, so its reaper waits that long. It
                    // is spawned on the reaper's thread: if that thread cannot
                    // start, no child is left unreaped. Not capped, as trigger
                    // commands are: each is one deliberate pick.
                    if let Err(error) = std::thread::Builder::new()
                        .name("kettle-layout".into())
                        .spawn(move || {
                            if let Err(error) = std::process::Command::new(&exe)
                                .arg("--layout")
                                .arg(&name)
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn()
                                .and_then(|mut child| child.wait())
                            {
                                log::warn!(
                                    "layout picker: spawn/wait `kettle --layout {name}`: {error}"
                                );
                            }
                        })
                    {
                        log::warn!("layout picker: reaper failed to start: {error}");
                    }
                }
            }
            Key::Character(s) if crate::modal_input::is_paste_chord(Some(s.as_str()), ws.mods) => {
                if let Some(pasted) = self.modal_clipboard_text()
                    && crate::modal_input::push_text(q, &pasted)
                {
                    *sel = 0;
                }
            }
            _ => {
                if let Some(t) = crate::modal_input::accept_text(text, ws.mods)
                    && crate::modal_input::push_text(q, t)
                {
                    *sel = 0;
                }
            }
        }
    }

    /// Whether the saved `language` would show Kettle in another language than
    /// this process's, which keeps the one it started with.
    pub(super) fn language_change_pending(&self) -> bool {
        language_change_pending(
            self.cfg.language,
            self.ui_text.language(),
            kettle_i18n::os_locale,
        )
    }

    /// Open the theme picker on the current theme. Opened from Settings, the
    /// picker sits over the panel, which comes back when the picker closes.
    pub(super) fn open_theme_picker(&mut self, ws: &mut WindowState) {
        let settings = ws.settings_nav.take();
        self.close_all_modals(ws);
        ws.settings_nav = settings;
        let live = (self.cfg.theme_name.clone(), self.cfg.theme.clone());
        if let Some((name, theme)) = begin_theme_picker_session(ws, &mut self.windows, live) {
            self.set_runtime_theme_state(ws, name, theme);
        }
    }

    /// The theme picker's keys. Moving the selection previews that theme (see
    /// `sync_theme_preview`); Enter keeps it and Esc restores the theme the
    /// picker opened on.
    pub(super) fn theme_picker_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        if self.modal_editor_key(ws, key) {
            return;
        }
        let Some(state) = ws.theme_picker_input.as_mut() else {
            return;
        };
        let count = |state: &crate::window_state::ThemePickerState| {
            theme_picker_matches(&state.query, &state.opened_on, &self.ui_text).len()
        };
        let step = |state: &mut crate::window_state::ThemePickerState, forward: bool, n: usize| {
            if n > 0 {
                state.selected = if forward {
                    (state.selected + 1) % n
                } else {
                    (state.selected + n - 1) % n
                };
            }
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.theme_picker_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                crate::modal_input::backspace(&mut state.query);
                state.selected = 0;
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                let n = count(state);
                step(state, true, n);
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = count(state);
                step(state, false, n);
            }
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                let n = count(state);
                step(state, matches!(s.as_str(), "j" | "n"), n);
            }
            Key::Named(NamedKey::Enter) => {
                let pick = theme_picker_selection(state, &self.ui_text);
                ws.theme_picker_input = None;
                if let Some(name) = pick {
                    self.commit_theme(ws, name);
                }
            }
            Key::Character(s) if crate::modal_input::is_paste_chord(Some(s.as_str()), ws.mods) => {
                if let Some(pasted) = self.modal_clipboard_text()
                    && let Some(state) = ws.theme_picker_input.as_mut()
                    && crate::modal_input::push_text(&mut state.query, &pasted)
                {
                    state.selected = 0;
                }
            }
            _ => {
                if let Some(t) = crate::modal_input::accept_text(text, ws.mods)
                    && crate::modal_input::push_text(&mut state.query, t)
                {
                    state.selected = 0;
                }
            }
        }
    }

    /// Keep `name` as the theme. Dropping the preview's baseline makes the
    /// post-event `sync_theme_preview` keep this pick instead of restoring the
    /// theme the picker opened on.
    pub(super) fn commit_theme(&mut self, ws: &mut WindowState, name: &str) {
        ws.theme_preview = None;
        self.set_runtime_theme_name(ws, name);
        // Theme is config-governed, so persist it to the config file, not the
        // session. A session-pinned theme would OVERRIDE the config/compile-time
        // default on restore, so neither a default change nor a fresh-config
        // user would see the new theme. Notify if the write fails so the pick
        // isn't silently lost on the next launch.
        if !self.persist_pref("theme", name) {
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleThemeNotSaved),
                self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
            );
        }
    }

    pub(super) fn ssh_key(&mut self, ws: &mut WindowState, key: &Key, text: Option<&str>) {
        if self.modal_editor_key(ws, key) {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                ws.ssh_input = None;
                self.reset_blink_phase(ws);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some((query, selected)) = ws.ssh_input.as_mut() {
                    crate::modal_input::backspace(query);
                    *selected = 0;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some((query, selected)) = ws.ssh_input.as_mut() {
                    let count = rank_ssh_hosts(query, &self.cfg.ssh_hosts).len();
                    if count > 0 {
                        *selected = (*selected + 1) % count;
                    }
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some((query, selected)) = ws.ssh_input.as_mut() {
                    let count = rank_ssh_hosts(query, &self.cfg.ssh_hosts).len();
                    if count > 0 {
                        *selected = (*selected + count - 1) % count;
                    }
                }
            }
            Key::Named(NamedKey::Tab) => {
                // Complete to the highlighted configured host, not merely the
                // first fuzzy result, so keyboard navigation and the input line
                // cannot disagree about what Enter will connect to.
                if let Some((query, selected)) = ws.ssh_input.as_mut() {
                    let ranked = rank_ssh_hosts(query, &self.cfg.ssh_hosts);
                    if let Some(&index) = ranked.get(*selected) {
                        query.replace_all(
                            &self.cfg.ssh_hosts[index].0,
                            crate::modal_input::MAX_MODAL_INPUT_BYTES,
                        );
                        *selected = 0;
                    }
                }
            }
            Key::Named(NamedKey::Enter) => {
                let (typed, selected) = ws.ssh_input.take().unwrap_or_default();
                let target = selected_ssh_target(&typed, selected, &self.cfg.ssh_hosts);
                if let Some(target) = target {
                    let area = self.area(ws);
                    let (cols, rows) = self.grid_of(ws, area);
                    let geometry = self.pty_geometry_for_grid(ws, cols, rows);
                    if let Err(e) =
                        ws.mux
                            .new_ssh_tab_geometry(&self.cfg, geometry, self.waker(), &target)
                    {
                        log::error!("ssh launch failed: {e}");
                    }
                    self.resize_all(ws);
                    self.save_session(ws);
                }
            }
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && ws.mods.control_key()
                    && !ws.mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                if let Some((query, selected)) = ws.ssh_input.as_mut() {
                    let count = rank_ssh_hosts(query, &self.cfg.ssh_hosts).len();
                    if count > 0 {
                        *selected = match s.as_str() {
                            "j" | "n" => (*selected + 1) % count,
                            _ => (*selected + count - 1) % count,
                        };
                    }
                }
            }
            Key::Character(s) if crate::modal_input::is_paste_chord(Some(s.as_str()), ws.mods) => {
                if let Some(pasted) = self.modal_clipboard_text()
                    && let Some((query, selected)) = ws.ssh_input.as_mut()
                    && crate::modal_input::push_text(query, &pasted)
                {
                    *selected = 0;
                }
            }
            _ => {
                // Modifier filtering, control-character filtering and the byte
                // cap all live in `modal_input` now, shared with the palette,
                // the title editors, the layout picker and the Settings prompt.
                if let Some(t) = crate::modal_input::accept_text(text, ws.mods)
                    && let Some((query, selected)) = ws.ssh_input.as_mut()
                    && crate::modal_input::push_text(query, t)
                {
                    *selected = 0;
                }
            }
        }
    }
}
