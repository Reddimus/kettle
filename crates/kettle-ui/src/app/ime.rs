use super::*;

pub(super) fn normalized_ime_preedit(
    text: String,
    selection: Option<(usize, usize)>,
) -> Option<(String, Option<(usize, usize)>)> {
    (!text.is_empty()).then_some((text, selection))
}

fn current_ime_owner(ws: &WindowState) -> crate::window_state::ImePreeditOwner {
    if ws.search.open {
        crate::window_state::ImePreeditOwner::Search
    } else if modal_holds_cursor_steady(ws) {
        crate::window_state::ImePreeditOwner::Other
    } else {
        ws.mux
            .active_focus()
            .map(crate::window_state::ImePreeditOwner::Terminal)
            .unwrap_or(crate::window_state::ImePreeditOwner::Other)
    }
}

pub(super) fn update_ime_preedit(
    ws: &mut WindowState,
    text: String,
    selection: Option<(usize, usize)>,
) -> bool {
    use crate::window_state::ImePreeditSession;
    let owner = current_ime_owner(ws);
    let generation = ws.ime_focus_generation;
    if let Some(session) = ws.ime_preedit_owner.as_mut()
        && matches!(
            session.owner,
            crate::window_state::ImePreeditOwner::Terminal(_)
        )
        && (session.owner != owner || session.generation != generation)
    {
        if text.is_empty() && !session.empty_after_focus {
            session.empty_after_focus = true;
            ws.ime_preedit = None;
            return true;
        }
        if !session.empty_after_focus {
            ws.ime_preedit = None;
            return true;
        }
        // The first empty Preedit may lead into the old Commit. A later
        // Preedit starts fresh input, including Wayland's empty one before
        // a commit-only input, so the cancelled owner no longer guards it.
        ws.ime_preedit_owner = None;
    }
    let next = normalized_ime_preedit(text, selection);
    if ws.ime_preedit == next {
        return false;
    }
    match (next.is_some(), ws.ime_preedit_owner) {
        // A composition begins here, unless one under way here goes on.
        (true, session)
            if session.is_none_or(|session| {
                session.ended.is_some()
                    || session.owner != owner
                    || session.generation != generation
            }) =>
        {
            ws.ime_preedit_owner = Some(ImePreeditSession {
                owner,
                generation,
                ended: None,
                empty_after_focus: false,
            });
        }
        // Emptied where it began: cancelled, or leading into a Commit for
        // this same surface, so there is nothing to guard.
        (false, Some(session))
            if session.ended.is_none()
                && session.owner == owner
                && session.generation == generation =>
        {
            ws.ime_preedit_owner = None;
        }
        // Emptied after focus moved: a Commit for the old surface may follow.
        (false, Some(session)) if session.ended.is_none() => {
            ws.ime_preedit_owner = Some(ImePreeditSession {
                ended: Some(generation),
                ..session
            });
        }
        _ => {}
    }
    ws.ime_preedit = next;
    true
}

pub(super) fn take_ime_commit(ws: &mut WindowState) -> bool {
    ws.ime_preedit = None;
    let owner = current_ime_owner(ws);
    let generation = ws.ime_focus_generation;
    ws.ime_preedit_owner.take().is_none_or(|session| {
        // A modal session that ended before focus moved on again is stale.
        // Terminal sessions keep rejecting their old composition.
        (!matches!(
            session.owner,
            crate::window_state::ImePreeditOwner::Terminal(_)
        ) && session.ended.is_some_and(|ended| ended != generation))
            || (session.owner == owner && session.generation == generation)
    })
}

pub(super) fn ime_preedit_focus(text: &str, selection: Option<(usize, usize)>) -> usize {
    use unicode_segmentation::UnicodeSegmentation as _;

    let requested = selection
        .map_or(text.len(), |(_, focus)| focus)
        .min(text.len());
    let mut boundary = requested;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    if boundary == text.len() {
        return boundary;
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index <= boundary)
        .last()
        .unwrap_or(0)
}

impl App {
    pub(super) fn commit_ime_text(
        &mut self,
        ws: &mut WindowState,
        text: &str,
        event_loop: &ActiveEventLoop,
    ) {
        if text.is_empty() {
            return;
        }
        let key = Key::Character(text.into());
        if ws.context_menu.is_some() || ws.vi_mode.is_some() || ws.hint_state.is_some() {
            return;
        }
        // The modal handlers below filter typed text by the modifiers that
        // produced it. A committed composition has none: the input method
        // decides when to commit, and whatever is latched at that instant did
        // not type the phrase. Committing with `⌘Space` (switch input source)
        // would otherwise drop the whole thing. Present the handlers with the
        // empty modifier set for the duration and restore it afterwards, so
        // `ws.mods` stays truthful for the next real keystroke.
        let latched_mods = ws.mods;
        ws.mods = ModifiersState::empty();
        // The modal outranks whatever raised it. Rebinding a key onto a chord
        // that is already taken raises a confirm dialog from inside the
        // Settings overlay. If the Settings arms below claimed the keyboard
        // first, the question could not be answered, by `y`/`n` or by anything
        // else, while the panel that asked it stayed open. The dialog's own
        // handler (the named-key path) reads `y`/`n`; this arm only has to
        // stop the text from going somewhere else.
        if ws.confirm_dialog.is_some() {
            ws.mods = latched_mods;
            return;
        } else if ws.palette_input.is_some() {
            self.palette_key(ws, &key, Some(text), event_loop);
        } else if ws.theme_picker_input.is_some() {
            self.theme_picker_key(ws, &key, Some(text));
        } else if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
            self.settings_text_key(ws, &key, Some(text));
        } else if ws.settings_nav.is_some() {
            ws.mods = latched_mods;
            return;
        } else if ws.layout_picker_input.is_some() {
            self.layout_picker_key(ws, &key, Some(text));
        } else if ws.ssh_input.is_some() {
            self.ssh_key(ws, &key, Some(text));
        } else if ws.editing_title.is_some() {
            // Route through the shared helper rather than extending the buffer
            // inline, so an IME commit cannot bypass the 4 KiB cap the typed
            // path enforces.
            if let Some(accepted) = crate::modal_input::accept_committed_text(text)
                && let Some(state) = ws.editing_title.as_mut()
            {
                crate::modal_input::push_text(&mut state.input, accepted);
            }
        } else if ws.search.open {
            self.search_key(ws, &key, None, Some(text), None);
        } else {
            ws.mods = latched_mods;
            let _ = self.write_terminal_input(ws, text.as_bytes());
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        ws.mods = latched_mods;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn update_ime_cursor_area(&self, ws: &mut WindowState) {
        let Some(window) = &ws.window else {
            return;
        };
        let (cw, ch) = ws
            .renderer
            .as_ref()
            .map(|renderer| (renderer.cell_w, renderer.cell_h))
            .unwrap_or((8.0, 16.0));
        let (preedit, preedit_focus) = ws
            .ime_preedit
            .as_ref()
            .map(|(text, selection)| (text.as_str(), ime_preedit_focus(text, *selection)))
            .unwrap_or(("", 0));
        let preedit_before_focus = &preedit[..preedit_focus];
        if let Some(field) = self.modal_text_projection(ws)
            && let Some(renderer) = ws.renderer.as_mut()
        {
            let layout = renderer.measure_text_field(&field);
            let (x, y, w, h) = layout.caret;
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y)),
                winit::dpi::PhysicalSize::new(f64::from(w.max(1.0)), f64::from(h.max(1.0))),
            );
            return;
        }
        let search_cursor = self.search_geometry(ws).map(|geometry| {
            let cw = self.overlay_text_cell_width(ws);
            let mut projected = ws.search.query().to_string();
            let replace = ws
                .search
                .editor
                .selection()
                .unwrap_or(ws.search.editor.cursor()..ws.search.editor.cursor());
            projected.replace_range(replace.clone(), preedit);
            let cursor_byte = replace.start.saturating_add(preedit_focus);
            let columns = kettle_render::search_bar_columns(geometry.editor.2, cw).max(1);
            let scroll = crate::search_input::SearchEditor::visible_scroll_for(
                &projected,
                cursor_byte,
                ws.search.editor.horizontal_scroll(),
                columns.saturating_sub(2),
            );
            let cursor_column = crate::search_input::display_width(&projected[..cursor_byte])
                .saturating_sub(scroll);
            let left = geometry.editor.0 + cw;
            let right = (geometry.editor.0 + geometry.editor.2 - cw).max(left);
            // The committed text before the composition is placed where it
            // was painted, so a CJK fallback glyph wider or narrower than two
            // cells does not drift the candidate window; the composition
            // itself counts cells.
            let stops = ws
                .renderer
                .as_ref()
                .map_or(&[][..], |renderer| renderer.search_editor_stops());
            let painted = stops
                .binary_search_by_key(&replace.start, |stop| stop.0)
                .ok()
                .map(|index| stops[index].1 + preedit_before_focus.width() as f32 * cw);
            (
                painted
                    .unwrap_or(geometry.editor.0 + (cursor_column + 1) as f32 * cw)
                    .clamp(left, right),
                geometry.editor.1 + ((geometry.editor.3 - ch) * 0.5).max(0.0),
            )
        });
        let (x, y) = if let Some((x, y)) = search_cursor {
            (x, y)
        } else {
            let Some((row, col)) = self.terminal_cursor_cell(ws) else {
                return;
            };
            let Some(rect) = self.focused_terminal(ws, self.area(ws)) else {
                return;
            };
            let grid_origin = (rect.0 + self.cfg.padding_x, rect.1 + self.cfg.padding_y);
            (
                grid_origin.0 + col as f32 * cw,
                grid_origin.1 + row as f32 * ch,
            )
        };
        let size = window.inner_size();
        let x = x.clamp(0.0, size.width.saturating_sub(1) as f32);
        let y = y.clamp(0.0, size.height.saturating_sub(1) as f32);
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y)),
            winit::dpi::PhysicalSize::new(f64::from(cw.max(1.0)), f64::from(ch.max(1.0))),
        );
    }
}
