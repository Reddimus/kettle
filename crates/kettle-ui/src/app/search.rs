//! The search bar: queries, matches, its editor and its pointer handling.

use super::*;

impl App {
    /// Height reserved by the responsive search lane. The renderer owns the
    /// layout formula; the app reuses it for PTY sizing and hit-testing so no
    /// terminal row can be painted underneath the controls.
    pub(super) fn search_bar_h(&self, ws: &WindowState) -> f32 {
        if !ws.search.open {
            return 0.0;
        }
        let Some(renderer) = ws.renderer.as_ref() else {
            return 26.0;
        };
        let (width, height) = renderer.surface_size();
        kettle_render::search_bar_geometry(
            width as f32,
            height as f32,
            renderer.overlay_text_cell_width(),
            renderer.cell_h,
            &self.ui_text,
        )
        .reserved_height
    }

    pub(super) fn update_search(&mut self, ws: &mut WindowState) {
        if !ws.search.open {
            return;
        }
        const TYPING_SEARCH_LINES: usize = 1_000;
        const BACKGROUND_CHUNK_LINES: usize = 1_000;
        const VISIBLE_CONTEXT_LINES: i32 = 100;
        const MAX_LOGICAL_LINE_EXPANSION: usize = kettle_core::MAX_SEARCH_LOGICAL_LINE_CONTEXT;

        let Some(pane_id) = ws.search.target_pane else {
            self.close_search(ws);
            return;
        };
        let Some(pane) = ws.mux.panes.get(&pane_id) else {
            self.close_search(ws);
            return;
        };

        // Compile untrusted input before taking the terminal-grid lock. Compilation has explicit
        // engine limits, but it still does allocation and Unicode automata construction that must
        // not delay the PTY reader.
        if ws.search.compiled_revision != Some(ws.search.revision) {
            let query = ws.search.query().to_string();
            match kettle_core::CompiledSearch::compile(
                &query,
                map_case_sensitivity(ws.search.case_mode),
            ) {
                Ok(compiled) => {
                    ws.search.set_compiled(compiled);
                    ws.search.compiled_revision = Some(ws.search.revision);
                    ws.search.status = if query.is_empty() {
                        kettle_render::SearchStatus::Typing
                    } else {
                        kettle_render::SearchStatus::Searching
                    };
                }
                Err(error) => {
                    ws.search.set_compiled(None);
                    ws.search.compiled_revision = Some(ws.search.revision);
                    ws.search.status = search_compile_error_status(error);
                }
            }
        }
        if ws.search.compiled.is_none() {
            ws.search.visible.clear();
            ws.search.visible_scan = None;
            ws.search.visible_turn_pending = false;
            // Empty and rejected patterns have no deferred work. Leaving a due retry armed makes
            // about_to_wait request a 1 ms redraw forever even though every frame exits here.
            ws.search.background = None;
            ws.search.unlimited_retry_at = None;
            ws.search.quiet_retry_pending = false;
            return;
        }

        let output_generation = pane.term.output_generation();
        let Ok(mut term) = pane.term.term.lock() else {
            ws.search.set_status_at(
                kettle_render::SearchStatus::Searching,
                std::time::Instant::now(),
            );
            return;
        };
        use kettle_core::Dimensions as _;

        let now = std::time::Instant::now();
        let top_line = term.topmost_line().0;
        let bottom_line = term.bottommost_line().0;
        let last_column = term.last_column().0;
        let screen_lines = term.screen_lines();
        let display_offset = term.grid().display_offset();
        let default_direction = if ws.search.invert {
            kettle_core::SearchDirection::Reverse
        } else {
            kettle_core::SearchDirection::Forward
        };
        let current_token =
            kettle_core::SearchScanToken::capture(&term, ws.search.revision, output_generation);
        ws.search.set_scan_token(current_token);

        // Reflow invalidates grid coordinates and restarts from the viewport. Plain PTY output is
        // different: retain a chunked job and advance its token so a process that prints every
        // few milliseconds cannot keep a deep-history search behind the 500 ms typing debounce.
        // An output-drifted exhausted pass is verified again after the stream becomes quiet before
        // we claim No match/Start/End.
        if let Some(previous_token) = ws.search.scan_token
            && previous_token != current_token
        {
            ws.search.focused = None;
            ws.search.revealed_focus = None;
            ws.search.visible.clear();
            ws.search.visible_scan = None;
            ws.search.visible_key = None;
            let reflowed = previous_token.query_revision != current_token.query_revision
                || previous_token.layout.columns != current_token.layout.columns
                || previous_token.layout.screen_lines != current_token.layout.screen_lines;
            let rejected = matches!(
                ws.search.status,
                kettle_render::SearchStatus::Invalid
                    | kettle_render::SearchStatus::TooComplex
                    | kettle_render::SearchStatus::TooLong
            );
            if reflowed {
                ws.search.typing_scanned_revision = None;
                ws.search.background = None;
                ws.search.anchor = Some(search_viewport_anchor(
                    display_offset,
                    screen_lines,
                    last_column,
                    default_direction,
                ));
                ws.search.unlimited_retry_at = (!rejected && !ws.search.query().is_empty())
                    .then_some(if ws.search.quiet_retry_pending {
                        now + std::time::Duration::from_millis(500)
                    } else {
                        now
                    });
            } else if let Some(job) = ws.search.background.as_mut() {
                continue_background_after_output(
                    job,
                    current_token,
                    top_line,
                    bottom_line,
                    last_column,
                );
                if !job.navigation {
                    ws.search.typing_scanned_revision = None;
                }
            } else {
                ws.search.typing_scanned_revision = None;
                ws.search.anchor = Some(search_viewport_anchor(
                    display_offset,
                    screen_lines,
                    last_column,
                    default_direction,
                ));
                ws.search.unlimited_retry_at = (!rejected && !ws.search.query().is_empty())
                    .then_some(if ws.search.quiet_retry_pending {
                        now + std::time::Duration::from_millis(500)
                    } else {
                        now
                    });
            }
            if !rejected && !ws.search.query().is_empty() {
                ws.search.set_status_at(
                    kettle_render::SearchStatus::Searching,
                    std::time::Instant::now(),
                );
            }
        }
        ws.search.scan_token = Some(current_token);
        ws.search.anchor.get_or_insert_with(|| {
            search_viewport_anchor(display_offset, screen_lines, last_column, default_direction)
        });

        let prioritize_visible = std::mem::take(&mut ws.search.visible_turn_pending);
        let mut foreground_scan_ran = false;
        let mut nearby_yielded_this_turn = false;

        // Live typing stays bounded to nearby history. The endpoint is expanded
        // to the logical line boundary so a regex can cross every soft wrap in
        // that line without crossing a hard newline.
        if !prioritize_visible && should_start_nearby_search(&ws.search) {
            let anchor = ws.search.anchor.expect("search anchor initialized");
            let edge = bounded_search_edge(
                anchor,
                default_direction,
                top_line,
                bottom_line,
                last_column,
                TYPING_SEARCH_LINES,
            );
            let edge = match default_direction {
                kettle_core::SearchDirection::Forward => {
                    kettle_core::search::bounded_line_search_right(
                        &term,
                        edge,
                        MAX_LOGICAL_LINE_EXPANSION,
                    )
                    .0
                }
                kettle_core::SearchDirection::Reverse => {
                    kettle_core::search::bounded_line_search_left(
                        &term,
                        edge,
                        MAX_LOGICAL_LINE_EXPANSION,
                    )
                    .0
                }
            };
            let result = ws
                .search
                .compiled
                .as_mut()
                .expect("compiled search checked")
                .find_in_range(
                    &term,
                    kettle_core::SearchBounds::new(anchor, edge),
                    default_direction,
                    1,
                );
            foreground_scan_ran = true;
            ws.search.focused = (!result.accuracy_limited)
                .then(|| result.matches.first().copied())
                .flatten();
            ws.search.revealed_focus = None;
            if result.accuracy_limited {
                ws.search.typing_scanned_revision = Some(ws.search.revision);
                if ws
                    .search
                    .background
                    .as_ref()
                    .is_some_and(|job| !job.navigation)
                {
                    ws.search.background = None;
                }
                ws.search.status = kettle_render::SearchStatus::Limited;
                ws.search.unlimited_retry_at = None;
            } else if ws.search.focused.is_some() {
                ws.search.typing_scanned_revision = Some(ws.search.revision);
                if ws
                    .search
                    .background
                    .as_ref()
                    .is_some_and(|job| !job.navigation)
                {
                    ws.search.background = None;
                }
                ws.search.status = kettle_render::SearchStatus::Match;
                ws.search.unlimited_retry_at = None;
            } else if let Some(cursor) = result.continuation {
                ws.search.background = Some(crate::search_input::BackgroundSearch {
                    token: current_token,
                    direction: default_direction,
                    cursor,
                    edge,
                    wrap_anchor: anchor,
                    wrapped: false,
                    nearby: true,
                    navigation: false,
                    had_focus: false,
                    output_drifted: false,
                });
                nearby_yielded_this_turn = true;
                ws.search.set_status_at(
                    kettle_render::SearchStatus::Searching,
                    std::time::Instant::now(),
                );
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            } else {
                ws.search.typing_scanned_revision = Some(ws.search.revision);
                let at_edge = match default_direction {
                    kettle_core::SearchDirection::Forward => edge.line >= bottom_line,
                    kettle_core::SearchDirection::Reverse => edge.line <= top_line,
                };
                if at_edge {
                    if ws.search.wrap {
                        ws.search.set_status_at(
                            kettle_render::SearchStatus::Searching,
                            std::time::Instant::now(),
                        );
                    } else {
                        ws.search.status = kettle_render::SearchStatus::NoMatch;
                        ws.search.unlimited_retry_at = None;
                    }
                } else {
                    ws.search.set_status_at(
                        kettle_render::SearchStatus::Searching,
                        std::time::Instant::now(),
                    );
                }
            }
        }

        // After 500 ms idle, continue in logical-line-aligned chunks. Only one
        // chunk runs per event-loop turn. A reflow or query change cancels
        // stale work; plain output advances the job's scan token instead.
        let background_due = !prioritize_visible
            && !foreground_scan_ran
            && !nearby_yielded_this_turn
            && (ws.search.background.is_some()
                || ws
                    .search
                    .unlimited_retry_at
                    .is_some_and(|deadline| now >= deadline));
        let quiet_retry_due = background_due
            && ws.search.background.is_none()
            && ws.search.quiet_retry_pending
            && ws
                .search
                .unlimited_retry_at
                .is_some_and(|deadline| now >= deadline);
        if quiet_retry_due {
            activate_quiet_search_retry(&mut ws.search);
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        } else if background_due && (ws.search.background.is_some() || ws.search.focused.is_none())
        {
            if ws.search.background.is_none() {
                let anchor = ws.search.anchor.expect("search anchor initialized");
                let searched_edge = bounded_search_edge(
                    anchor,
                    default_direction,
                    top_line,
                    bottom_line,
                    last_column,
                    TYPING_SEARCH_LINES,
                );
                let searched_edge = match default_direction {
                    kettle_core::SearchDirection::Forward => {
                        kettle_core::search::bounded_line_search_right(
                            &term,
                            searched_edge,
                            MAX_LOGICAL_LINE_EXPANSION,
                        )
                        .0
                    }
                    kettle_core::SearchDirection::Reverse => {
                        kettle_core::search::bounded_line_search_left(
                            &term,
                            searched_edge,
                            MAX_LOGICAL_LINE_EXPANSION,
                        )
                        .0
                    }
                };
                let edge = match default_direction {
                    kettle_core::SearchDirection::Forward => {
                        kettle_core::SearchPoint::new(bottom_line, last_column)
                    }
                    kettle_core::SearchDirection::Reverse => {
                        kettle_core::SearchPoint::new(top_line, 0)
                    }
                };
                if let Some(cursor) = search_point_after(
                    searched_edge,
                    default_direction,
                    top_line,
                    bottom_line,
                    last_column,
                ) {
                    ws.search.background = Some(crate::search_input::BackgroundSearch {
                        token: current_token,
                        direction: default_direction,
                        cursor,
                        edge,
                        wrap_anchor: anchor,
                        wrapped: false,
                        nearby: false,
                        navigation: false,
                        had_focus: false,
                        output_drifted: false,
                    });
                } else if ws.search.wrap {
                    ws.search.background = wrapped_background_search(
                        current_token,
                        default_direction,
                        anchor,
                        (top_line, bottom_line, last_column),
                        false,
                        false,
                        false,
                    );
                    if ws.search.background.is_some() {
                        ws.search.set_status_at(
                            kettle_render::SearchStatus::Searching,
                            std::time::Instant::now(),
                        );
                    } else {
                        ws.search.status = kettle_render::SearchStatus::NoMatch;
                    }
                } else {
                    ws.search.status = kettle_render::SearchStatus::NoMatch;
                }
                ws.search.unlimited_retry_at = None;
            }

            if let Some(mut job) = ws.search.background.take()
                && job.token == current_token
            {
                let chunk_edge = bounded_search_edge(
                    job.cursor,
                    job.direction,
                    top_line,
                    bottom_line,
                    last_column,
                    BACKGROUND_CHUNK_LINES,
                );
                let expanded_chunk_edge = match job.direction {
                    kettle_core::SearchDirection::Forward => {
                        kettle_core::search::bounded_line_search_right(
                            &term,
                            chunk_edge,
                            MAX_LOGICAL_LINE_EXPANSION,
                        )
                        .0
                    }
                    kettle_core::SearchDirection::Reverse => {
                        kettle_core::search::bounded_line_search_left(
                            &term,
                            chunk_edge,
                            MAX_LOGICAL_LINE_EXPANSION,
                        )
                        .0
                    }
                };
                let chunk_edge = match job.direction {
                    kettle_core::SearchDirection::Forward => expanded_chunk_edge.min(job.edge),
                    kettle_core::SearchDirection::Reverse => expanded_chunk_edge.max(job.edge),
                };
                let result = ws
                    .search
                    .compiled
                    .as_mut()
                    .expect("compiled search checked")
                    .find_in_range(
                        &term,
                        kettle_core::SearchBounds::new(job.cursor, chunk_edge),
                        job.direction,
                        1,
                    );
                foreground_scan_ran = true;
                if result.accuracy_limited {
                    // Exact highlights inside bounded chunks are still safe, but an omitted
                    // cross-boundary match could precede them. Do not advertise a wrong
                    // first/next result or wrap past it.
                    ws.search.status = kettle_render::SearchStatus::Limited;
                    ws.search.typing_scanned_revision = Some(ws.search.revision);
                    ws.search.unlimited_retry_at = None;
                } else if let Some(found) = result.matches.first().copied() {
                    ws.search.focused = Some(found);
                    ws.search.revealed_focus = None;
                    ws.search.status = if job.output_drifted {
                        // The span exists in the currently locked grid, but output between
                        // earlier chunks can make its global first/next ordering uncertain.
                        kettle_render::SearchStatus::Limited
                    } else if job.wrapped {
                        kettle_render::SearchStatus::Wrapped
                    } else {
                        kettle_render::SearchStatus::Match
                    };
                    if prepare_initial_search_retry_after_output(
                        &mut ws.search,
                        &job,
                        search_viewport_anchor(
                            display_offset,
                            screen_lines,
                            last_column,
                            default_direction,
                        ),
                    ) {
                        ws.search.unlimited_retry_at =
                            Some(now + std::time::Duration::from_millis(500));
                    }
                } else if let Some(continuation) = result.continuation {
                    job.cursor = continuation;
                    ws.search.background = Some(job);
                    ws.search.set_status_at(
                        kettle_render::SearchStatus::Searching,
                        std::time::Instant::now(),
                    );
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                } else if job.nearby {
                    if job.output_drifted {
                        let (status, retry_at) = finished_background_search(&job, now);
                        ws.search.status = status;
                        ws.search.unlimited_retry_at = retry_at;
                        prepare_initial_search_retry_after_output(
                            &mut ws.search,
                            &job,
                            search_viewport_anchor(
                                display_offset,
                                screen_lines,
                                last_column,
                                default_direction,
                            ),
                        );
                    } else {
                        ws.search.typing_scanned_revision = Some(ws.search.revision);
                        let at_edge = match job.direction {
                            kettle_core::SearchDirection::Forward => job.edge.line >= bottom_line,
                            kettle_core::SearchDirection::Reverse => job.edge.line <= top_line,
                        };
                        if at_edge && !ws.search.wrap {
                            ws.search.status = kettle_render::SearchStatus::NoMatch;
                            ws.search.unlimited_retry_at = None;
                        } else {
                            ws.search.set_status_at(
                                kettle_render::SearchStatus::Searching,
                                std::time::Instant::now(),
                            );
                        }
                    }
                } else if chunk_edge == job.edge {
                    if !job.wrapped && ws.search.wrap {
                        ws.search.background = wrapped_background_search(
                            current_token,
                            job.direction,
                            job.wrap_anchor,
                            (top_line, bottom_line, last_column),
                            job.navigation,
                            job.had_focus,
                            job.output_drifted,
                        );
                        if ws.search.background.is_some() {
                            ws.search.set_status_at(
                                kettle_render::SearchStatus::Searching,
                                std::time::Instant::now(),
                            );
                        } else {
                            let (status, retry_at) = finished_background_search(&job, now);
                            ws.search.status = status;
                            ws.search.unlimited_retry_at = retry_at;
                            prepare_initial_search_retry_after_output(
                                &mut ws.search,
                                &job,
                                search_viewport_anchor(
                                    display_offset,
                                    screen_lines,
                                    last_column,
                                    default_direction,
                                ),
                            );
                        }
                    } else {
                        let (status, retry_at) = finished_background_search(&job, now);
                        ws.search.status = status;
                        ws.search.unlimited_retry_at = retry_at;
                        prepare_initial_search_retry_after_output(
                            &mut ws.search,
                            &job,
                            search_viewport_anchor(
                                display_offset,
                                screen_lines,
                                last_column,
                                default_direction,
                            ),
                        );
                    }
                } else if let Some(next) = search_point_after(
                    chunk_edge,
                    job.direction,
                    top_line,
                    bottom_line,
                    last_column,
                ) {
                    job.cursor = next;
                    ws.search.background = Some(job);
                    ws.search.set_status_at(
                        kettle_render::SearchStatus::Searching,
                        std::time::Instant::now(),
                    );
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                } else {
                    let (status, retry_at) = finished_background_search(&job, now);
                    ws.search.status = status;
                    ws.search.unlimited_retry_at = retry_at;
                    prepare_initial_search_retry_after_output(
                        &mut ws.search,
                        &job,
                        search_viewport_anchor(
                            display_offset,
                            screen_lines,
                            last_column,
                            default_direction,
                        ),
                    );
                }
            }
        }

        // Reveal only when focus changes. A user can still wheel away while
        // search stays open without the next idle frame snapping them back.
        if let Some(focused) = ws.search.focused
            && ws.search.revealed_focus != Some(focused)
        {
            let history = term.grid().history_size();
            let old_offset = term.grid().display_offset();
            let wanted = kettle_core::search::reveal_offset(
                focused.start.line,
                old_offset,
                history,
                term.screen_lines(),
            );
            if wanted != old_offset {
                term.scroll_display(kettle_core::Scroll::Delta(
                    wanted as i32 - old_offset as i32,
                ));
            }
            ws.search.revealed_focus = Some(focused);
            ws.search.visible_key = None;
        }

        // Highlight only the viewport and a small logical-line context. This is
        // bounded independently of scrollback size and invalidates on viewport
        // movement without recompiling the regex.
        let display_offset = term.grid().display_offset();
        let layout = kettle_core::SearchLayout::capture(&term);
        let visible_key = crate::search_input::VisibleSearchKey {
            revision: ws.search.revision,
            pane: pane_id,
            output_generation,
            columns: layout.columns,
            screen_lines: layout.screen_lines,
            history_size: layout.history_size,
            display_offset,
            focused: ws.search.focused,
        };
        if ws.search.visible_key != Some(visible_key) {
            if foreground_scan_ran {
                ws.search.visible_turn_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
                return;
            }
            let offset = i32::try_from(display_offset).unwrap_or(i32::MAX);
            let viewport_top = (-offset).max(top_line);
            let viewport_bottom = i32::try_from(term.screen_lines().saturating_sub(1))
                .unwrap_or(i32::MAX)
                .saturating_sub(offset)
                .min(bottom_line);
            let context_top = viewport_top
                .saturating_sub(VISIBLE_CONTEXT_LINES)
                .max(top_line);
            let context_bottom = viewport_bottom
                .saturating_add(VISIBLE_CONTEXT_LINES)
                .min(bottom_line);
            let viewport_start = kettle_core::SearchPoint::new(viewport_top, 0);
            let viewport_end = kettle_core::SearchPoint::new(viewport_bottom, last_column);
            let (context_start, left_truncated) = kettle_core::search::bounded_line_search_left(
                &term,
                kettle_core::SearchPoint::new(context_top, 0),
                MAX_LOGICAL_LINE_EXPANSION,
            );
            let (context_end, right_truncated) = kettle_core::search::bounded_line_search_right(
                &term,
                kettle_core::SearchPoint::new(context_bottom, last_column),
                MAX_LOGICAL_LINE_EXPANSION,
            );
            let (viewport_logical_start, viewport_left_truncated) =
                kettle_core::search::bounded_line_search_left(
                    &term,
                    viewport_start,
                    MAX_LOGICAL_LINE_EXPANSION,
                );
            let (viewport_logical_end, viewport_right_truncated) =
                kettle_core::search::bounded_line_search_right(
                    &term,
                    viewport_end,
                    MAX_LOGICAL_LINE_EXPANSION,
                );

            // Scan the viewport before surrounding context, one deterministic core work budget
            // per frame. Dense pre-viewport matches can therefore never consume the cap before a
            // visible/active match, and an exact work yield is resumed rather than mislabeled as
            // Results limited or cached as complete.
            let mut ranges = vec![kettle_core::SearchBounds::new(viewport_start, viewport_end)];
            if viewport_logical_start < viewport_start || viewport_logical_end > viewport_end {
                ranges.push(kettle_core::SearchBounds::new(
                    viewport_logical_start,
                    viewport_logical_end,
                ));
            }
            if let Some(before_end) = search_point_after(
                viewport_logical_start,
                kettle_core::SearchDirection::Reverse,
                top_line,
                bottom_line,
                last_column,
            ) && context_start <= before_end
            {
                ranges.push(kettle_core::SearchBounds::new(context_start, before_end));
            }
            if let Some(after_start) = search_point_after(
                viewport_logical_end,
                kettle_core::SearchDirection::Forward,
                top_line,
                bottom_line,
                last_column,
            ) && after_start <= context_end
            {
                ranges.push(kettle_core::SearchBounds::new(after_start, context_end));
            }

            if ws
                .search
                .visible_scan
                .as_ref()
                .is_none_or(|scan| scan.key != visible_key)
            {
                ws.search.visible.clear();
                ws.search.visible_truncated = false;
                ws.search.visible_scan = Some(crate::search_input::VisibleSearchScan::new(
                    visible_key,
                    ranges,
                    ws.search.focused,
                    left_truncated
                        || right_truncated
                        || viewport_left_truncated
                        || viewport_right_truncated,
                ));
            }

            let (bounds, remaining) = {
                let scan = ws
                    .search
                    .visible_scan
                    .as_ref()
                    .expect("visible scan initialized");
                (
                    scan.current_bounds().expect("visible scan has a range"),
                    kettle_core::MAX_SEARCH_MATCHES.saturating_sub(scan.matches.len()),
                )
            };
            let batch = ws.search.compiled_for_highlights().find_in_range(
                &term,
                bounds,
                kettle_core::SearchDirection::Forward,
                remaining,
            );
            let finished = if remaining == 0 {
                true
            } else {
                let scan = ws
                    .search
                    .visible_scan
                    .as_mut()
                    .expect("visible scan initialized");
                scan.apply_batch(batch)
            };

            if finished {
                let mut scan = ws
                    .search
                    .visible_scan
                    .take()
                    .expect("visible scan initialized");
                scan.matches.sort_by_key(|span| (span.start, span.end));
                ws.search.visible = scan.matches;
                ws.search.visible_truncated = scan.truncated;
                ws.search.visible_key = Some(visible_key);
            } else if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
    }

    pub(super) fn open_search(&mut self, ws: &mut WindowState) {
        self.close_all_modals(ws);
        self.release_held_buttons(ws, |_| true);
        ws.card_press = None;
        ws.last_mouse_cell = None;
        ws.scrollbar_drag_offset = None;
        ws.dragging_split = None;
        ws.tab_drag_active = false;
        ws.tab_drag_press = None;
        ws.tab_pressed_idx = None;
        ws.detach_drag = crate::detach::DragState::default();
        ws.drag_press = None;
        ws.pane_drag = None;
        if self.torn_drag.as_ref().is_some_and(|drag| {
            drag.seq == ws.seq
                || drag.carrier == ws.seq
                || drag.dock.is_some_and(|(seq, _)| seq == ws.seq)
        }) {
            self.abandon_torn_drag(Some(ws));
        }
        if ws.ime_preedit_owner.is_some() {
            // The composition belongs to the previously focused surface. Keep
            // its owner marker for the late Commit guard, but stop painting it
            // in the newly opened search editor.
            ws.ime_preedit = None;
        }
        let Some(pane_id) = ws.mux.active_focus() else {
            return;
        };
        self.attach_search_to_pane(ws, pane_id, None);
    }

    /// Build the bar's state for `pane_id`. `carried` is the editor text and
    /// toggles an already-open bar brings along when it follows focus to
    /// another pane; a fresh open starts from the pane's remembered query and
    /// the configured defaults.
    pub(super) fn attach_search_to_pane(
        &mut self,
        ws: &mut WindowState,
        pane_id: u64,
        carried: Option<SearchCarryOver>,
    ) {
        let invert = carried
            .as_ref()
            .map_or(self.cfg.invert_search, |c| c.invert);
        let direction = if invert {
            kettle_core::SearchDirection::Reverse
        } else {
            kettle_core::SearchDirection::Forward
        };
        let display_offset = ws
            .mux
            .panes
            .get(&pane_id)
            .and_then(|pane| {
                pane.term
                    .term
                    .lock()
                    .ok()
                    .map(|term| term.grid().display_offset())
            })
            .unwrap_or(0);
        let remembered = ws.search_queries.get(&pane_id).cloned().unwrap_or_default();
        let defaults = SearchCarryOver {
            query: remembered,
            wrap: self.cfg.search_wrap,
            case_mode: self.cfg.search_case_sensitive,
            invert: self.cfg.invert_search,
            focused_control: kettle_render::SearchControl::Editor,
        };
        ws.search = fresh_search_state(
            pane_id,
            carried.unwrap_or(defaults),
            display_offset,
            std::time::Instant::now(),
        );
        self.resize_all(ws);
        // Reserving the responsive Search lane can shrink the terminal by several rows. In a
        // scrolled-back grid Alacritty increases display_offset to preserve the viewed content,
        // so derive the scan anchor only after that resize has settled.
        ws.search.anchor = ws.mux.panes.get(&pane_id).and_then(|pane| {
            let term = pane.term.term.lock().ok()?;
            use kettle_core::Dimensions as _;
            Some(search_viewport_anchor(
                term.grid().display_offset(),
                term.screen_lines(),
                term.last_column().0,
                direction,
            ))
        });
    }

    /// Move the open bar to the focused pane. The bar is window-level and the
    /// grid stays clickable while it is open, so a click that focuses another
    /// pane must also make typing search that pane. The query and toggles come
    /// along. The old pane keeps the query in its remembered-query slot and,
    /// with no focused result, gets its pre-search viewport back, as closing
    /// would. The new pane is scanned afresh.
    pub(super) fn retarget_search_to_focus(&mut self, ws: &mut WindowState) {
        if !ws.search.open {
            return;
        }
        let Some(pane_id) = ws.mux.active_focus() else {
            return;
        };
        if search_retarget(ws.search.target_pane, Some(pane_id)).is_none() {
            return;
        }
        let carried = SearchCarryOver {
            query: ws.search.query().to_string(),
            wrap: ws.search.wrap,
            case_mode: ws.search.case_mode,
            invert: ws.search.invert,
            focused_control: ws.search.focused_control,
        };
        self.remember_search_query(ws);
        self.restore_search_viewport(ws);
        self.attach_search_to_pane(ws, pane_id, Some(carried));
    }

    /// Park the bar's query in its target pane's remembered-query slot.
    pub(super) fn remember_search_query(&mut self, ws: &mut WindowState) {
        if let Some(pane_id) = ws.search.target_pane
            && ws.mux.panes.contains_key(&pane_id)
        {
            ws.search_queries
                .insert(pane_id, ws.search.query().to_string());
        }
    }

    /// With no focused result, put the target pane's viewport back where it
    /// was before the bar opened. A focused result stays where it is: grid
    /// growth already preserves the visible content, so no coordinate
    /// arithmetic is needed and the pre-grow span is intentionally stale.
    pub(super) fn restore_search_viewport(&mut self, ws: &mut WindowState) {
        if ws.search.focused.is_some() {
            return;
        }
        let fallback_offset = ws.search.pre_open_display_offset;
        if let Some(pane_id) = ws.search.target_pane
            && let Some(pane) = ws.mux.panes.get(&pane_id)
            && let Ok(mut term) = pane.term.term.lock()
        {
            let current = term.grid().display_offset();
            let wanted = fallback_offset
                .unwrap_or(current)
                .min(term.grid().history_size());
            if wanted != current {
                term.scroll_display(kettle_core::Scroll::Delta(wanted as i32 - current as i32));
            }
        }
    }

    pub(super) fn close_search(&mut self, ws: &mut WindowState) {
        if !ws.search.open {
            return;
        }
        ws.ime_focus_generation = ws.ime_focus_generation.wrapping_add(1);
        self.remember_search_query(ws);
        if matches!(
            ws.ime_preedit_owner.map(|session| session.owner),
            Some(crate::window_state::ImePreeditOwner::Search)
        ) {
            // Keep the owner marker until Commit/Disabled so a late commit is
            // recognized and discarded by the IME event path.
            ws.ime_preedit = None;
        }
        ws.search.open = false;
        ws.search.background = None;
        ws.search.unlimited_retry_at = None;
        ws.search.quiet_retry_pending = false;
        ws.search.visible.clear();
        ws.search.visible_scan = None;
        ws.search.visible_turn_pending = false;
        // Dropping the lane grows the grid, and Alacritty adjusts
        // display_offset to keep the viewed content, so the viewport is
        // restored only after that resize has settled.
        self.resize_all(ws);
        self.restore_search_viewport(ws);
        self.reset_blink_phase(ws);
        // The remembered query now has a single pane-scoped owner. Drop the
        // closed editor, compiled DFA, coordinates, and cache immediately so a
        // dead pane cannot retain sensitive text for the process lifetime.
        ws.search = crate::search_input::SearchState::default();
    }

    /// Track the search control under the pointer for the hover fill, and
    /// redraw only when it changes.
    pub(super) fn update_search_hover(&mut self, ws: &mut WindowState) {
        let hovered = self
            .search_geometry(ws)
            .and_then(|geometry| geometry.hit_test(ws.cursor.x as f32, ws.cursor.y as f32))
            .filter(|control| *control != kettle_render::SearchControl::Editor);
        if ws.search.hovered_control != hovered {
            ws.search.hovered_control = hovered;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
    }

    /// Whether the pointer is inside the open search bar's reserved lane.
    pub(super) fn cursor_in_search_bar(&self, ws: &WindowState) -> bool {
        self.search_geometry(ws).is_some_and(|geometry| {
            rect_contains(geometry.rect, ws.cursor.x as f32, ws.cursor.y as f32)
        })
    }

    pub(super) fn persist_search_pref(&self, key: &str, value: &str) {
        if !self.persist_pref(key, value) {
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleSearchNotSaved),
                self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
            );
        }
    }

    pub(super) fn scroll_search_viewport(&mut self, ws: &mut WindowState, lines: i32) -> bool {
        let Some(pane_id) = ws.search.target_pane else {
            return true;
        };
        if let Some(pane) = ws.mux.panes.get(&pane_id)
            && let Ok(mut term) = pane.term.term.lock()
        {
            term.scroll_display(Scroll::Delta(lines));
            ws.search.visible_key = None;
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    /// Keys for the open search bar. Returns whether the bar used the key.
    ///
    /// The bar claims the keys a native find field would: typing, caret and
    /// word movement, deletion, the clipboard, undo, and its own Enter, Tab,
    /// F3, `Cmd/Ctrl+G` and Escape. Anything else, such as a new tab, font
    /// zoom or a split's resize, goes on to Kettle's shortcuts (see
    /// `search_bar_shortcut`), as the menus of a native app keep working
    /// while its find field has focus.
    pub(super) fn search_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        physical_key: Option<&PhysicalKey>,
        text: Option<&str>,
        unshifted_key: Option<&Key>,
    ) -> bool {
        let macos = cfg!(target_os = "macos");
        let mods = ws.mods;
        let shortcut = if macos {
            mods.super_key()
        } else {
            mods.control_key()
        };
        // The shortcut key alone, with or without Shift.
        let shortcut_only = shortcut
            && !mods.alt_key()
            && if macos {
                !mods.control_key()
            } else {
                !mods.super_key()
            };
        // Linux leaves Ctrl+Shift chords to the terminal's own shortcuts
        // (`Ctrl+Shift+A` splits, `Ctrl+Shift+X` zooms), apart from the copy
        // and paste pair every Linux terminal uses.
        let shifted_shortcut = !macos && mods.shift_key();
        let word_modifier = if macos {
            option_physically_held(ws)
        } else {
            mods.control_key()
        };
        // Caret movement takes Shift and the word modifier; macOS adds Cmd
        // for the line ends. `Cmd+Opt+Arrow` and Linux's `Alt+Arrow` move
        // focus between splits instead.
        let caret_chord = if macos {
            !mods.control_key() && !(mods.super_key() && mods.alt_key())
        } else {
            !mods.alt_key() && !mods.super_key()
        };
        let no_chord = !mods.control_key() && !mods.super_key() && !mods.alt_key();
        let selecting = mods.shift_key();
        let editor = ws.search.focused_control == kettle_render::SearchControl::Editor;
        // The find shortcut pressed again, with the bar already open, returns
        // to the query and selects it. It is whatever `start_search` is bound
        // to, so a rebound shortcut works too.
        if matches!(
            resolve_keybind_action(&self.cfg.keybinds, Some(key), None, mods, unshifted_key),
            Some((_, Action::StartSearch))
        ) {
            ws.search.focused_control = kettle_render::SearchControl::Editor;
            ws.search.editor.select_all();
            self.sync_search_editor_scroll(ws);
            return true;
        }
        let bound = resolve_keybind_action(
            &self.cfg.keybinds,
            Some(key),
            physical_key,
            mods,
            unshifted_key,
        )
        .is_some();
        let typed = search_bar_text(
            crate::modal_input::accept_text(text, ws.mods),
            mods,
            bound,
            cfg!(windows),
        );
        if !search_bar_claims(key, mods, macos, editor, self.cfg.vim_menu_nav) && typed.is_none() {
            return false;
        }
        let letter = |s: &str, want: &str| s.eq_ignore_ascii_case(want);
        let mut edited = false;
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close_search(ws);
            }
            Key::Named(NamedKey::Enter) if no_chord => {
                if editor {
                    let go_back = mods.shift_key() ^ ws.search.invert;
                    self.search_step_match(ws, go_back);
                } else {
                    self.activate_search_control(ws, ws.search.focused_control);
                }
            }
            Key::Named(NamedKey::F3) if no_chord => {
                self.search_step_match(ws, mods.shift_key());
            }
            Key::Named(NamedKey::Tab) if no_chord => {
                let controls = kettle_render::SearchControl::ALL;
                let current = controls
                    .iter()
                    .position(|control| *control == ws.search.focused_control)
                    .unwrap_or(0);
                let next = if mods.shift_key() {
                    (current + controls.len() - 1) % controls.len()
                } else {
                    (current + 1) % controls.len()
                };
                ws.search.focused_control = controls[next];
                ws.search.editor.clear_selection();
            }
            Key::Character(s)
                if self.cfg.vim_menu_nav
                    && mods.control_key()
                    && !mods.alt_key()
                    && matches!(s.as_str(), "j" | "k" | "n" | "p") =>
            {
                self.search_step_match(ws, matches!(s.as_str(), "k" | "p"));
            }
            // Cmd/Ctrl+G steps like Enter, as in browsers and macOS text views.
            Key::Character(s) if shortcut_only && letter(s, "g") => {
                self.search_step_match(ws, mods.shift_key());
            }
            Key::Character(s) if shortcut_only && letter(s, "z") => {
                ws.search.focused_control = kettle_render::SearchControl::Editor;
                edited = if mods.shift_key() {
                    ws.search.editor.redo()
                } else {
                    ws.search.editor.undo()
                };
            }
            Key::Character(s) if shortcut_only && !shifted_shortcut && letter(s, "a") => {
                ws.search.focused_control = kettle_render::SearchControl::Editor;
                ws.search.editor.select_all();
            }
            Key::Character(s) if shortcut_only && letter(s, "c") => self.search_copy(ws),
            Key::Character(s) if shortcut_only && !shifted_shortcut && letter(s, "x") => {
                ws.search.focused_control = kettle_render::SearchControl::Editor;
                edited = ws.search.editor.cut_selection(
                    kettle_core::MAX_SEARCH_QUERY_BYTES,
                    |selected| {
                        let Some(clipboard) = self.clipboard.as_mut() else {
                            return false;
                        };
                        match clipboard.set_text(selected.to_string()) {
                            Ok(()) => true,
                            Err(error) => {
                                log::warn!("search cut: clipboard write failed: {error}");
                                false
                            }
                        }
                    },
                );
            }
            Key::Character(s) if shortcut_only && letter(s, "v") => self.search_paste(ws),
            // The text-field keys every macOS text field shares: Ctrl+A / E to
            // the ends, Ctrl+B / F by a character, Ctrl+D / H delete forward and
            // back, Ctrl+K deletes to the end.
            Key::Character(s)
                if macos
                    && editor
                    && mods.control_key()
                    && !mods.super_key()
                    && !mods.alt_key()
                    && !mods.shift_key()
                    && matches!(
                        s.to_ascii_lowercase().as_str(),
                        "a" | "e" | "b" | "f" | "d" | "h" | "k"
                    ) =>
            {
                let search_editor = &mut ws.search.editor;
                match s.to_ascii_lowercase().as_str() {
                    "a" => search_editor.move_home(false),
                    "e" => search_editor.move_end(false),
                    "b" => search_editor.move_left(false, false),
                    "f" => search_editor.move_right(false, false),
                    "d" => edited = search_editor.delete(),
                    "h" => edited = search_editor.backspace(),
                    _ => edited = search_editor.delete_to_end(),
                }
            }
            Key::Named(NamedKey::ArrowLeft) if editor && caret_chord => {
                if macos && shortcut {
                    ws.search.editor.move_home(selecting);
                } else {
                    ws.search.editor.move_left(selecting, word_modifier);
                }
            }
            Key::Named(NamedKey::ArrowRight) if editor && caret_chord => {
                if macos && shortcut {
                    ws.search.editor.move_end(selecting);
                } else {
                    ws.search.editor.move_right(selecting, word_modifier);
                }
            }
            // `Ctrl+Home` / `Ctrl+End` scroll the terminal instead.
            Key::Named(NamedKey::Home) if editor && no_chord => {
                ws.search.editor.move_home(selecting);
            }
            Key::Named(NamedKey::End) if editor && no_chord => {
                ws.search.editor.move_end(selecting);
            }
            Key::Named(NamedKey::Backspace) => {
                ws.search.focused_control = kettle_render::SearchControl::Editor;
                edited = if macos && shortcut {
                    ws.search.editor.delete_to_start()
                } else if word_modifier {
                    ws.search.editor.delete_word_backward()
                } else {
                    ws.search.editor.backspace()
                };
            }
            Key::Named(NamedKey::Delete) => {
                ws.search.focused_control = kettle_render::SearchControl::Editor;
                edited = if macos && shortcut {
                    ws.search.editor.delete_to_end()
                } else if word_modifier {
                    ws.search.editor.delete_word_forward()
                } else {
                    ws.search.editor.delete()
                };
            }
            // winit reports the space bar as a named key; `dispatch_ui_key`
            // sends it as a character. Either one presses a focused button.
            Key::Named(NamedKey::Space) if !editor && no_chord => {
                self.activate_search_control(ws, ws.search.focused_control);
            }
            Key::Character(s) if s == " " && !editor && no_chord => {
                self.activate_search_control(ws, ws.search.focused_control);
            }
            _ => {
                // Share the modal text-entry rule with the sibling handlers.
                // The explicit shortcut arms above claim ⌘A/⌘C/⌘X/⌘V; the
                // helper rejects every other Command chord (so ⌘Q never types
                // a `q`) and control characters.
                let Some(t) = typed else {
                    return false;
                };
                ws.search.focused_control = kettle_render::SearchControl::Editor;
                let outcome = ws
                    .search
                    .editor
                    .type_text(t, kettle_core::search::MAX_SEARCH_QUERY_BYTES);
                edited = outcome.changed;
                if outcome.truncated {
                    ws.search.status = kettle_render::SearchStatus::TooLong;
                }
            }
        }
        if edited {
            ws.search.note_edit(std::time::Instant::now());
        }
        if ws.search.open {
            self.sync_search_editor_scroll(ws);
        }
        true
    }

    /// Copy for the search bar: a selection inside the query wins; otherwise
    /// the grid selection the user dragged out under the bar. A grid press
    /// clears the query's selection, so a stale range cannot shadow a fresh
    /// drag (Terminator copies the terminal selection here).
    pub(super) fn search_copy(&mut self, ws: &mut WindowState) {
        if let Some(selected) = ws.search.editor.selected_text() {
            if let Some(clipboard) = self.clipboard.as_mut()
                && let Err(error) = clipboard.set_text(selected.to_string())
            {
                log::warn!("search copy: clipboard write failed: {error}");
            }
        } else {
            self.copy_selection(ws);
        }
    }

    /// Paste the clipboard into the query.
    pub(super) fn search_paste(&mut self, ws: &mut WindowState) {
        ws.search.focused_control = kettle_render::SearchControl::Editor;
        let Some(clipboard) = self.clipboard.as_mut() else {
            return;
        };
        match clipboard.get_text() {
            Ok(paste) => {
                let outcome = ws
                    .search
                    .editor
                    .insert(&paste, kettle_core::MAX_SEARCH_QUERY_BYTES);
                if outcome.changed {
                    ws.search.note_edit(std::time::Instant::now());
                }
                if outcome.truncated {
                    ws.search.status = kettle_render::SearchStatus::TooLong;
                }
            }
            Err(error) => log::warn!("search paste: clipboard read failed: {error}"),
        }
    }

    /// Run the Kettle shortcut for a key the search bar did not use, as a
    /// native app's menus keep working while its find field has focus. The
    /// keyboard stays with the bar, so shortcuts that type into the terminal
    /// do nothing, and Copy and Paste act on the query. Returns whether a
    /// shortcut ran.
    pub(super) fn search_bar_shortcut(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        physical_key: Option<&PhysicalKey>,
        repeat: bool,
        event_loop: &ActiveEventLoop,
        unshifted_key: Option<&Key>,
    ) -> bool {
        let Some((trigger, action)) = resolve_keybind_action(
            &self.cfg.keybinds,
            Some(key),
            physical_key,
            ws.mods,
            unshifted_key,
        ) else {
            return false;
        };
        match search_bar_shortcut_kind(&action) {
            SearchBarShortcut::Typing => false,
            SearchBarShortcut::Copy => {
                self.search_copy(ws);
                true
            }
            SearchBarShortcut::Paste => {
                self.search_paste(ws);
                self.sync_search_editor_scroll(ws);
                true
            }
            SearchBarShortcut::Run => {
                // An adaptive focus chord with no split in that direction
                // would go to the program, which does not have the keyboard.
                if self.adaptive_focus_chord_falls_through(ws, trigger, &action) {
                    return false;
                }
                // A held toggle chord toggles once, as on the main keyboard
                // path; its repeats are still the bar's to swallow.
                if !(repeat && toggles_once_per_press(&action)) {
                    self.handle_action(ws, action, event_loop);
                }
                true
            }
        }
    }

    /// Advance the search selection one match forward (`go_back = false`) or
    /// backward, honoring `search-wrap` (`false` stops at the ends instead of
    /// cycling). Enter, F3, the Previous/Next controls, and vim-menu-nav's
    /// Ctrl+j/k/n/p all share this stepping.
    pub(super) fn search_step_match(&mut self, ws: &mut WindowState, go_back: bool) {
        let query = ws.search.query().to_string();
        if query.is_empty() {
            ws.search.status = kettle_render::SearchStatus::Typing;
            return;
        }
        if ws.search.compiled_revision != Some(ws.search.revision) {
            match kettle_core::CompiledSearch::compile(
                &query,
                map_case_sensitivity(ws.search.case_mode),
            ) {
                Ok(compiled) => {
                    ws.search.set_compiled(compiled);
                    ws.search.compiled_revision = Some(ws.search.revision);
                }
                Err(error) => {
                    ws.search.status = search_compile_error_status(error);
                    return;
                }
            }
        }
        if ws.search.compiled.is_none() {
            return;
        }
        // Explicit Previous/Next supersedes an idle quiet-verification even if this request is
        // already at a non-wrapping terminal edge and returns before constructing a job.
        cancel_deferred_search_retry(&mut ws.search);
        let Some(pane_id) = ws.search.target_pane else {
            return;
        };
        let Some(pane) = ws.mux.panes.get(&pane_id) else {
            ws.search.status = kettle_render::SearchStatus::NoMatch;
            return;
        };
        let Ok(term) = pane.term.term.lock() else {
            ws.search.set_status_at(
                kettle_render::SearchStatus::Searching,
                std::time::Instant::now(),
            );
            return;
        };
        use kettle_core::Dimensions as _;
        let direction = if go_back {
            kettle_core::SearchDirection::Reverse
        } else {
            kettle_core::SearchDirection::Forward
        };
        let token = kettle_core::SearchScanToken::capture(
            &term,
            ws.search.revision,
            pane.term.output_generation(),
        );
        ws.search.set_scan_token(token);
        ws.search.invalidate_stale_navigation(token);
        let top_line = term.topmost_line().0;
        let bottom_line = term.bottommost_line().0;
        let last_column = term.last_column().0;
        let had_focus = ws.search.focused.is_some();
        let edge_origin = match (direction, ws.search.focused) {
            (kettle_core::SearchDirection::Forward, Some(span)) => span.end,
            (kettle_core::SearchDirection::Reverse, Some(span)) => span.start,
            (_, None) => search_viewport_anchor(
                term.grid().display_offset(),
                term.screen_lines(),
                last_column,
                direction,
            ),
        };
        let mut forced_wrap = false;
        let origin = if ws.search.focused.is_some() {
            match search_point_after(edge_origin, direction, top_line, bottom_line, last_column) {
                Some(point) => point,
                None if ws.search.wrap => {
                    forced_wrap = true;
                    match direction {
                        kettle_core::SearchDirection::Forward => {
                            kettle_core::SearchPoint::new(top_line, 0)
                        }
                        kettle_core::SearchDirection::Reverse => {
                            kettle_core::SearchPoint::new(bottom_line, last_column)
                        }
                    }
                }
                None => {
                    ws.search.status = if go_back {
                        kettle_render::SearchStatus::Start
                    } else {
                        kettle_render::SearchStatus::End
                    };
                    return;
                }
            }
        } else {
            edge_origin
        };
        let edge = match direction {
            kettle_core::SearchDirection::Forward => {
                kettle_core::SearchPoint::new(bottom_line, last_column)
            }
            kettle_core::SearchDirection::Reverse => kettle_core::SearchPoint::new(top_line, 0),
        };
        drop(term);

        // Enter/F3 navigation uses the same one-chunk-per-event-loop continuation as the idle
        // deep scan. A miss in a multi-million-line history must never block winit while a full
        // edge-to-edge RegexIter runs synchronously.
        ws.search.scan_token = Some(token);
        ws.search.typing_scanned_revision = Some(ws.search.revision);
        ws.search.background = Some(crate::search_input::BackgroundSearch {
            token,
            direction,
            cursor: origin,
            edge,
            wrap_anchor: origin,
            wrapped: forced_wrap,
            nearby: false,
            navigation: true,
            had_focus,
            output_drifted: false,
        });
        ws.search.set_status_at(
            kettle_render::SearchStatus::Searching,
            std::time::Instant::now(),
        );
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn activate_search_control(
        &mut self,
        ws: &mut WindowState,
        control: kettle_render::SearchControl,
    ) {
        match control {
            kettle_render::SearchControl::Editor => {
                ws.search.focused_control = kettle_render::SearchControl::Editor;
            }
            kettle_render::SearchControl::Previous => self.search_step_match(ws, true),
            kettle_render::SearchControl::Next => self.search_step_match(ws, false),
            kettle_render::SearchControl::Wrap => {
                ws.search.wrap = !ws.search.wrap;
                self.cfg.search_wrap = ws.search.wrap;
                self.persist_search_pref(
                    "search-wrap",
                    if ws.search.wrap { "true" } else { "false" },
                );
                search_wrap_toggled(&mut ws.search, std::time::Instant::now());
            }
            kettle_render::SearchControl::Case => {
                ws.search.case_mode = match ws.search.case_mode {
                    kettle_config::SearchCaseSensitivity::Smart => {
                        kettle_config::SearchCaseSensitivity::Always
                    }
                    kettle_config::SearchCaseSensitivity::Always => {
                        kettle_config::SearchCaseSensitivity::Never
                    }
                    kettle_config::SearchCaseSensitivity::Never => {
                        kettle_config::SearchCaseSensitivity::Smart
                    }
                };
                self.cfg.search_case_sensitive = ws.search.case_mode;
                let value = match ws.search.case_mode {
                    kettle_config::SearchCaseSensitivity::Smart => "smart",
                    kettle_config::SearchCaseSensitivity::Always => "always",
                    kettle_config::SearchCaseSensitivity::Never => "never",
                };
                self.persist_search_pref("search-case-sensitive", value);
                // The match set changes, so search again, from the match or
                // viewport the user is looking at rather than from where Search
                // opened.
                Self::anchor_search_to_viewport(ws);
                let anchor = ws.search.anchor;
                ws.search.note_edit(std::time::Instant::now());
                ws.search.anchor = anchor;
            }
            kettle_render::SearchControl::Invert => {
                ws.search.invert = !ws.search.invert;
                self.cfg.invert_search = ws.search.invert;
                self.persist_search_pref(
                    "invert-search",
                    if ws.search.invert { "true" } else { "false" },
                );
                // Invert only changes which way Enter steps. A later search
                // starts from the matching edge of the viewport; the match on
                // screen stays.
                if ws.search.focused.is_none() {
                    ws.search.note_edit(std::time::Instant::now());
                }
                Self::anchor_search_to_viewport(ws);
            }
            kettle_render::SearchControl::Close => self.close_search(ws),
        }
    }

    /// Point the next search at where the user is looking (see
    /// `search_anchor_for`).
    pub(super) fn anchor_search_to_viewport(ws: &mut WindowState) {
        let viewport = ws
            .search
            .target_pane
            .and_then(|pane_id| ws.mux.panes.get(&pane_id))
            .and_then(|pane| {
                use kettle_core::Dimensions as _;
                let term = pane.term.term.lock().ok()?;
                Some((
                    term.grid().display_offset(),
                    term.screen_lines(),
                    term.last_column().0,
                ))
            });
        if let Some(anchor) = search_anchor_for(ws.search.focused, ws.search.invert, viewport) {
            ws.search.anchor = Some(anchor);
        }
    }

    pub(super) fn sync_search_editor_scroll(&self, ws: &mut WindowState) {
        let Some(renderer) = ws.renderer.as_ref() else {
            return;
        };
        let (width, height) = renderer.surface_size();
        let geometry = kettle_render::search_bar_geometry(
            width as f32,
            height as f32,
            renderer.overlay_text_cell_width(),
            renderer.cell_h,
            &self.ui_text,
        );
        let columns = kettle_render::search_bar_columns(
            geometry.editor.2,
            renderer.overlay_text_cell_width(),
        )
        .max(1);
        ws.search
            .editor
            .ensure_cursor_visible(columns.saturating_sub(2));
    }

    pub(super) fn search_geometry(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::SearchBarGeometry> {
        if !ws.search.open {
            return None;
        }
        let renderer = ws.renderer.as_ref()?;
        let (width, height) = renderer.surface_size();
        Some(kettle_render::search_bar_geometry(
            width as f32,
            height as f32,
            renderer.overlay_text_cell_width(),
            renderer.cell_h,
            &self.ui_text,
        ))
    }

    pub(super) fn search_mouse_press(&mut self, ws: &mut WindowState) -> bool {
        let Some(geometry) = self.search_geometry(ws) else {
            return false;
        };
        let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let Some(control) = geometry.hit_test(x, y) else {
            return false;
        };
        // Control input can press without a move first; the press itself
        // establishes what is under the pointer.
        self.update_search_hover(ws);
        if control != kettle_render::SearchControl::Editor {
            // A button activates on release over it, like a native button, so
            // a press can be taken back by moving off before letting go. A
            // pointer click also leaves keyboard focus where it was, as macOS
            // buttons and VS Code's find widget do, so typing goes on in the
            // query and the editor keeps its caret.
            ws.search.dragging_editor = false;
            ws.search.pressed_control = Some(control);
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return true;
        }
        ws.search.focused_control = control;

        // The caret goes to the nearest boundary between characters; a word
        // or line selection uses the character under the pointer.
        let boundary = self.search_caret_point_at(ws, x, geometry);
        let under = self.search_char_point_at(ws, x, geometry);
        let clicks = Self::click_count(ws, usize::MAX, under.click_key());
        match clicks {
            2 => {
                ws.search.editor.select_word_at(under);
                ws.search.dragging_editor = false;
            }
            3 => {
                ws.search.editor.select_all();
                ws.search.dragging_editor = false;
            }
            _ => {
                ws.search
                    .editor
                    .set_cursor_at(boundary, ws.mods.shift_key());
                ws.search.dragging_editor = true;
            }
        }
        self.sync_search_editor_scroll(ws);
        true
    }

    /// Where in the query a click at `x` puts the caret.
    ///
    /// Over the visible text it is the nearest glyph boundary the renderer
    /// painted, as a query byte, so the caret lands where it is drawn even
    /// when a fallback font gives CJK or emoji a width other than whole cells.
    /// Past either edge of the editor it counts cells, so a drag there walks
    /// a scrolled query. An IME composition paints text the editor does not
    /// hold yet, so it counts cells too.
    pub(super) fn search_caret_point_at(
        &self,
        ws: &WindowState,
        x: f32,
        geometry: kettle_render::SearchBarGeometry,
    ) -> crate::search_input::QueryPoint {
        let cell_width = ws
            .renderer
            .as_ref()
            .map_or(8.0, |renderer| renderer.overlay_text_cell_width());
        let by_cells =
            crate::search_input::QueryPoint::Column(crate::search_input::pointer_query_column(
                ws.search.editor.horizontal_scroll(),
                ((x - geometry.editor.0) / cell_width + 0.5).floor() as isize,
            ));
        if !Self::search_pointer_over_painted_query(ws, x, geometry) {
            return by_cells;
        }
        let stops = ws
            .renderer
            .as_ref()
            .map_or(&[][..], |renderer| renderer.search_editor_stops());
        crate::search_input::nearest_stop_byte(stops, x)
            .map_or(by_cells, crate::search_input::QueryPoint::Byte)
    }

    /// The character under `x`, for a word or line selection. Like the
    /// caret, it follows the painted glyphs over the visible text and counts
    /// cells elsewhere.
    pub(super) fn search_char_point_at(
        &self,
        ws: &WindowState,
        x: f32,
        geometry: kettle_render::SearchBarGeometry,
    ) -> crate::search_input::QueryPoint {
        let cell_width = ws
            .renderer
            .as_ref()
            .map_or(8.0, |renderer| renderer.overlay_text_cell_width());
        let by_cells =
            crate::search_input::QueryPoint::Column(crate::search_input::pointer_query_column(
                ws.search.editor.horizontal_scroll(),
                ((x - geometry.editor.0) / cell_width).floor() as isize,
            ));
        if !Self::search_pointer_over_painted_query(ws, x, geometry) {
            return by_cells;
        }
        let clusters = ws
            .renderer
            .as_ref()
            .map_or(&[][..], |renderer| renderer.search_editor_clusters());
        crate::search_input::cluster_byte_at(clusters, x)
            .map_or(by_cells, crate::search_input::QueryPoint::Byte)
    }

    /// Whether `x` is over the editor's text area and the painted query is
    /// the editor's text. An IME composition paints text the editor does not
    /// hold yet.
    pub(super) fn search_pointer_over_painted_query(
        ws: &WindowState,
        x: f32,
        geometry: kettle_render::SearchBarGeometry,
    ) -> bool {
        if ws.ime_preedit.is_some() {
            return false;
        }
        let Some(renderer) = ws.renderer.as_ref() else {
            return false;
        };
        // A right-to-left query starts at its right end, so cells counted
        // from the left edge would pick the wrong end; it always maps through
        // the painted glyphs, the nearest of which is the right end to take.
        renderer.search_editor_rtl()
            || (x >= geometry.editor.0 + renderer.overlay_text_cell_width() * 0.5
                && x <= geometry.editor.0 + geometry.editor.2
                    - renderer.overlay_text_cell_width() * 0.5)
    }

    /// End a button press: activate the button if the pointer is still over
    /// it, otherwise drop the press.
    pub(super) fn search_button_release(&mut self, ws: &mut WindowState) {
        let Some(pressed) = ws.search.pressed_control.take() else {
            return;
        };
        let over = self
            .search_geometry(ws)
            .and_then(|geometry| geometry.hit_test(ws.cursor.x as f32, ws.cursor.y as f32));
        if over == Some(pressed) {
            self.activate_search_control(ws, pressed);
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn search_mouse_drag(&mut self, ws: &mut WindowState) -> bool {
        if !ws.search.open || !ws.search.dragging_editor {
            return false;
        }
        let Some(geometry) = self.search_geometry(ws) else {
            return true;
        };
        let point = self.search_caret_point_at(ws, ws.cursor.x as f32, geometry);
        ws.search.editor.set_cursor_at(point, true);
        self.sync_search_editor_scroll(ws);
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }
}
