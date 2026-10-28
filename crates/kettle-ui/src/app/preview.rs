//! Media previews on screen: lanes, the centered viewer, cards, playback and their pointer and keyboard handling.

use super::*;

impl App {
    /// Note the card a primary press at the pointer would take, for the hand
    /// cursor and the hover outline: one settled under it, with no Shift,
    /// dialog, menu or viewer in the way. One that has not settled yet
    /// leaves a wake for when it does, so a still pointer gets the hand
    /// then.
    pub(super) fn note_card_hover(&self, ws: &mut WindowState) -> bool {
        let now = std::time::Instant::now();
        let card = ws
            .native_pointer
            .filter(|_| !ws.mods.shift_key() && !self.pointer_modal_open(ws))
            .and_then(|pointer| {
                ws.renderer
                    .as_ref()?
                    .card_at(pointer.x as f32, pointer.y as f32)
            });
        let settled = card.filter(|card| ws.card_sightings.settled(Some(*card), now).is_some());
        ws.card_settle_wake = card
            .filter(|_| settled.is_none())
            .and_then(|card| ws.card_sightings.settles_at(&card, now));
        if ws.card_hover != settled {
            ws.card_hover = settled;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        settled.is_some()
    }

    /// Open the focused pane's media shelf in its preview lane, on its
    /// newest item, or close the lane if it has one. Only a user action
    /// opens a lane; a push never does.
    pub(super) fn open_media_shelf(&mut self, ws: &mut WindowState) {
        let Some(pane) = ws.mux.active_focus() else {
            return;
        };
        if ws.preview_panels.contains_key(&pane) {
            self.close_preview(ws, pane);
            return;
        }
        let Some(item) = ws
            .mux
            .panes
            .get(&pane)
            .and_then(|state| state.media_shelf.latest())
            .map(|item| item.id)
        else {
            return;
        };
        self.close_all_modals(ws);
        self.show_media_item(ws, pane, item);
    }

    /// Open a manual preview in its chosen presentation and mark it viewed.
    pub(super) fn show_media_item(&mut self, ws: &mut WindowState, pane: u64, item: u64) {
        let overlay = ws.preview_overlay.is_some_and(|open| open.pane == pane)
            || (!ws.preview_panels.contains_key(&pane)
                && self.cfg.media_open_mode == kettle_config::MediaOpenMode::Overlay);
        if overlay {
            // An asynchronous render must not interrupt another tab or dialog.
            let visible = self
                .pane_layout(ws, self.area(ws))
                .iter()
                .any(|(id, _)| *id == pane);
            if !visible || self.non_search_modal_open(ws) || ws.search.open {
                return;
            }
            if let Some(open) = ws.preview_overlay.filter(|open| open.pane != pane) {
                self.close_preview(ws, open.pane);
            }
        }
        // The item already showing keeps its mode, canvas and place, and
        // what was last said about it goes: it may just have been read again.
        let panel = match ws.preview_panels.get(&pane) {
            Some(panel) if panel.item == item => crate::window_state::PreviewPanel {
                notice: None,
                page: None,
                ..*panel
            },
            // Another item: the sharper pixels were the last one's.
            _ => {
                ws.lane_crops.remove(&pane);
                crate::window_state::PreviewPanel::new(item)
            }
        };
        let had_panel = ws.preview_panels.insert(pane, panel).is_some();
        let side = match self.cfg.preview_lane_side {
            kettle_config::PreviewLaneSide::Bottom => crate::pane_partition::LaneSide::Bottom,
            kettle_config::PreviewLaneSide::Right => crate::pane_partition::LaneSide::Right,
        };
        let resized = if overlay {
            if ws.preview_overlay.is_none() {
                ws.preview_overlay = Some(crate::media::Overlay {
                    pane,
                    origin: ws.mux.active_focus(),
                    control: None,
                    theater: false,
                    fullscreen: false,
                });
            }
            self.release_held_buttons(ws, |_| true);
            self.finish_selection_gesture(ws);
            ws.mux.focus_pane(pane);
            ws.mux.close_lane(pane)
        } else {
            ws.mux.open_lane(pane, side)
        };
        // A pane too small for even the strip shows no lane: say so rather
        // than keep one nobody can see, and leave the item unseen. Measured
        // where the pane is, or will be: a preview the user asked for may
        // finish after they switched tabs or zoomed another pane.
        let shown = if overlay {
            self.preview_lane_geometry_of(ws, pane)
                .is_some_and(|geometry| geometry.full)
        } else {
            ws.mux
                .partition_of(pane, self.area(ws), self.layout_style(ws))
                .and_then(|partition| partition.lane)
                .is_some_and(|lane| lane.rect().is_some())
        };
        if !shown && !had_panel {
            ws.preview_panels.remove(&pane);
            self.restore_overlay_origin(ws, pane);
            let resized = ws.mux.close_lane(pane) || resized;
            self.lanes_changed(ws, resized);
            let tr = self.ui_text;
            fire_notify(
                tr.text(kettle_i18n::Text::NotifyTitlePreviewNoRoom),
                tr.text(kettle_i18n::Text::NotifyBodyPreviewNoRoom),
            );
            return;
        }
        if let Some(state) = ws.mux.panes.get_mut(&pane) {
            state.media_shelf.viewed(item);
        }
        if overlay {
            self.focus_preview(ws);
        }
        self.lanes_changed(ws, resized);
    }

    pub(super) fn restore_overlay_origin(&mut self, ws: &mut WindowState, pane: u64) {
        if let Some(open) = ws.preview_overlay.filter(|open| open.pane == pane) {
            if open.fullscreen
                && let Some(window) = &ws.window
            {
                window.set_fullscreen(None);
            }
            ws.preview_overlay = None;
            if ws.preview_focus.is_some_and(|focus| focus.pane == pane) {
                self.exit_preview_focus(ws);
            }
            if ws.mux.active_focus() == Some(pane)
                && let Some(origin) = open.origin
                && ws.mux.panes.get(&origin).is_some_and(|pane| !pane.closed)
                && self
                    .pane_layout(ws, self.area(ws))
                    .iter()
                    .any(|(pane, _)| *pane == origin)
            {
                ws.mux.focus_pane(origin);
            }
        }
    }

    pub(super) fn pin_preview(&mut self, ws: &mut WindowState, pane: u64) {
        if !ws.preview_overlay.is_some_and(|open| open.pane == pane) {
            return;
        }
        self.restore_overlay_origin(ws, pane);
        let side = match self.cfg.preview_lane_side {
            kettle_config::PreviewLaneSide::Bottom => crate::pane_partition::LaneSide::Bottom,
            kettle_config::PreviewLaneSide::Right => crate::pane_partition::LaneSide::Right,
        };
        let resized = ws.mux.open_lane(pane, side);
        self.lanes_changed(ws, resized);
    }

    pub(super) fn toggle_preview_theater(&mut self, ws: &mut WindowState, pane: u64) {
        if !preview_item(ws, pane)
            .is_some_and(|item| item.kind.media() == Some(kettle_media::MediaKind::Video))
        {
            return;
        }
        if let Some(open) = ws.preview_overlay.as_mut().filter(|open| open.pane == pane) {
            open.theater = !open.theater;
            self.lanes_changed(ws, false);
        }
    }

    pub(super) fn toggle_preview_fullscreen(&mut self, ws: &mut WindowState, pane: u64) {
        if !ws.preview_overlay.is_some_and(|open| open.fullscreen)
            && !preview_item(ws, pane)
                .is_some_and(|item| item.kind.media() == Some(kettle_media::MediaKind::Video))
        {
            return;
        }
        if let Some(open) = ws.preview_overlay.as_mut().filter(|open| open.pane == pane)
            && let Some(window) = &ws.window
        {
            let fullscreen = open.toggle_fullscreen(window.fullscreen().is_some());
            window.set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
            self.lanes_changed(ws, false);
        }
    }

    /// After a lane opened, closed, collapsed or changed item: give the
    /// terminals their new size when the geometry moved, and repaint.
    pub(super) fn lanes_changed(&mut self, ws: &mut WindowState, resized: bool) {
        if resized {
            self.resize_all(ws);
        }
        self.lanes_moved(ws);
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// Close the preview, restoring terminal room only for a pinned lane.
    pub(super) fn close_preview(&mut self, ws: &mut WindowState, pane: u64) {
        self.restore_overlay_origin(ws, pane);
        // Motion banked toward a scroll of the lane goes with it, as do its
        // sharper pixels. A press on it still owns the pointer until its
        // release, which is the lane's.
        ws.lane_wheel.reset();
        ws.lane_crops.remove(&pane);
        self.end_lane_preview(ws, pane);
        ws.lane_playbacks.remove(&pane);
        let had_panel = ws.preview_panels.remove(&pane).is_some();
        let resized = ws.mux.close_lane(pane);
        if had_panel || resized {
            self.lanes_changed(ws, resized);
        }
    }

    /// The stills job a silent preview of what `pane`'s lane shows asks
    /// for: the item, its generation, the job and the longest reply it can
    /// send. `None` unless the lane shows a video with a length, rendered,
    /// whose source it can read again.
    pub(super) fn lane_preview_job(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Option<(u64, u64, crate::media::JobSpec, usize)> {
        let panel = ws.preview_panels.get(&pane)?;
        let item = preview_item(ws, pane)?;
        lane_previews_item(item, panel)?;
        let info = item.video?;
        let stills = crate::media::preview_stills(&info)?;
        let limit = crate::media::preview_reply_limit(&stills)?;
        let mut spec = item.source.spec.clone();
        spec.kind = kettle_media::JobKind::VideoStills(stills);
        Some((item.id, item.generation, spec, limit))
    }

    /// Start `pane`'s silent preview, or stop the one that is made or plays:
    /// the lane's button, key and action.
    pub(super) fn toggle_lane_preview(&mut self, ws: &mut WindowState, pane: u64) {
        if ws
            .lane_previews
            .get(&pane)
            .is_some_and(crate::media::LanePreview::active)
        {
            if let Some(preview) = ws.lane_previews.get_mut(&pane)
                && !preview.stop()
            {
                ws.lane_previews.remove(&pane);
                self.media.withdraw_lane_preview(pane);
            }
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = None;
            }
            return self.lanes_changed(ws, false);
        }
        self.start_lane_preview(ws, pane);
    }

    /// Start a silent preview of the video `pane`'s lane shows: from frames
    /// it holds for the item as it is, else from one stills job, which reads
    /// the item's source again. That happens only on the user's own
    /// gesture, never while a control client's request or a Lua script's
    /// action is handled, and never for a file found changed, which a
    /// reload reads again.
    pub(super) fn start_lane_preview(&mut self, ws: &mut WindowState, pane: u64) {
        if self.ctl_driving || self.lua_driving {
            return;
        }
        let start = crate::media::PreviewStart::User;
        let Some((item, generation, spec, limit)) = self.lane_preview_job(ws, pane) else {
            return;
        };
        let reads_a_file = matches!(spec.input, crate::media::SourceInput::Path { .. });
        if reads_a_file && preview_item(ws, pane).is_some_and(|item| item.source.changed) {
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = Some(crate::window_state::LaneNotice::Changed);
            }
            return self.lanes_changed(ws, false);
        }
        let now = std::time::Instant::now();
        if let Some(panel) = ws.preview_panels.get_mut(&pane) {
            panel.notice = None;
        }
        // One decoder a lane: a video playing there lets its session go,
        // keeping its frame for when the preview stops.
        if let Some(playback) = ws.lane_playbacks.get_mut(&pane) {
            playback.park();
        }
        // Frames held for the item as it is play again without a job.
        if ws
            .lane_previews
            .get_mut(&pane)
            .filter(|preview| preview.serves(item, generation))
            .is_some_and(|preview| preview.replay(now, start))
        {
            return self.lanes_changed(ws, false);
        }
        self.end_lane_preview(ws, pane);
        self.preview_tickets += 1;
        let ticket = self.preview_tickets;
        let deadline = now + crate::media::PREVIEW_FIRST_FRAME_WITHIN;
        let control =
            kettle_media::client::RenderControl::with_deadline(deadline).limit_reply(limit);
        let render = crate::media::LaneRender {
            window: ws.seq,
            pane,
            item,
            generation,
            tile: None,
            page: None,
            preview: Some(ticket),
        };
        let push = crate::media::Push::new(
            crate::media::Requester::Lane(render),
            crate::media::Route {
                pane,
                window: ws.seq,
                verified: true,
            },
            crate::media::Draft {
                key: None,
                title: String::new(),
                provenance: crate::media::Provenance::User,
                inline: None,
                inline_refused: None,
                gallery: None,
            },
            spec,
            control.clone(),
        );
        ws.lane_previews.insert(
            pane,
            crate::media::LanePreview::loading(item, generation, ticket, control, now, start),
        );
        self.media
            .admit(crate::media::Sender::LanePreview(pane), deadline, push);
        self.media_tick(ws);
        self.lanes_changed(ws, false);
    }

    /// What opens the video `pane`'s lane shows, from `start_ms`: the item,
    /// its generation and length, and the request, its frames fitted to the
    /// room the lane's picture has now. `None` unless the lane shows a video
    /// with a length, rendered, read from a file it can read again.
    pub(super) fn lane_play_open(
        &self,
        ws: &WindowState,
        pane: u64,
        start_ms: u64,
    ) -> Option<(u64, u64, u64, kettle_media::PlayOpen)> {
        let panel = ws.preview_panels.get(&pane)?;
        let item = preview_item(ws, pane)?;
        lane_previews_item(item, panel)?;
        let info = item.video?;
        let crate::media::SourceInput::Path {
            path,
            authorization,
        } = &item.source.spec.input
        else {
            return None;
        };
        let edge = |value: f32| {
            if value.is_finite() {
                (value.round() as u32).clamp(1, kettle_media::MAX_PLAY_EDGE)
            } else {
                1
            }
        };
        let room = self
            .preview_lane_geometry_of(ws, pane)
            .map(|geometry| geometry.content)
            .filter(|content| content.2 >= 1.0 && content.3 >= 1.0)
            .map_or((item.size.0 as f32, item.size.1 as f32), |content| {
                (content.2, content.3)
            });
        Some((
            item.id,
            item.generation,
            info.duration_ms,
            kettle_media::PlayOpen {
                source: kettle_media::Source::Path {
                    path: path.clone(),
                    authorization: authorization.clone(),
                },
                max_width: edge(room.0),
                max_height: edge(room.1),
                start_ms: start_ms.min(info.duration_ms),
                max_fps_milli: kettle_media::MAX_PLAY_FPS_MILLI,
                // Its sound, as the user last left it for this item; none
                // on macOS for a container only an external decoder reads,
                // since only Apple's decoder plays sound.
                audio: if apple_reads(info.container) {
                    ws.lane_playbacks
                        .get(&pane)
                        .filter(|playback| playback.serves(item.id, item.generation))
                        .map_or(
                            kettle_media::AudioRequest::On,
                            crate::media::LanePlayback::audio_request,
                        )
                } else {
                    kettle_media::AudioRequest::Off
                },
            },
        ))
    }

    /// The lane's play button, Space while it holds the keyboard, and
    /// `video_play`: stop a silent preview that plays, else play or pause
    /// the video itself, opening it where it stands when it has no session.
    pub(super) fn toggle_lane_play(&mut self, ws: &mut WindowState, pane: u64) {
        if ws
            .lane_previews
            .get(&pane)
            .is_some_and(crate::media::LanePreview::active)
        {
            return self.toggle_lane_preview(ws, pane);
        }
        let now = std::time::Instant::now();
        let current = preview_item(ws, pane).map(|item| (item.id, item.generation));
        let changed = preview_item(ws, pane).is_some_and(|item| item.source.changed);
        let playback = ws.lane_playbacks.get_mut(&pane).filter(|playback| {
            current.is_some_and(|(item, generation)| playback.serves(item, generation))
        });
        // A control client or a Lua script may pause a video, but only the
        // user plays one, and never from a file found changed, which a
        // reload reads again: resuming reads the file as opening it does.
        let pauses = playback
            .as_ref()
            .is_some_and(|playback| playback.button() == crate::media::PlaybackButton::Pause);
        if !pauses && (self.ctl_driving || self.lua_driving) {
            return;
        }
        if !pauses && changed {
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = Some(crate::window_state::LaneNotice::Changed);
            }
            return self.lanes_changed(ws, false);
        }
        let at_ms = match playback {
            Some(playback) => match playback.toggle(now) {
                crate::media::PlaybackToggled::Done => {
                    if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                        panel.notice = None;
                    }
                    return self.lanes_changed(ws, false);
                }
                crate::media::PlaybackToggled::Open { at_ms } => at_ms,
            },
            None => 0,
        };
        self.open_lane_playback(ws, pane, at_ms, true);
    }

    /// The lane's sound button, M while it holds the keyboard, and
    /// `video_mute`: mute the sound of the video playing in the lane, or let
    /// it be heard again, the video playing on. A control client or a Lua
    /// script may mute it, but only the user lets it be heard.
    pub(super) fn toggle_lane_sound(&mut self, ws: &mut WindowState, pane: u64) {
        let current = preview_item(ws, pane).map(|item| (item.id, item.generation));
        let Some(playback) = ws.lane_playbacks.get_mut(&pane).filter(|playback| {
            current.is_some_and(|(item, generation)| playback.serves(item, generation))
        }) else {
            return;
        };
        if playback.muted() && (self.ctl_driving || self.lua_driving) {
            return;
        }
        if playback.toggle_mute() {
            ws.accessibility_pending = true;
            self.lanes_changed(ws, false);
        }
    }

    /// Open the video `pane`'s lane shows from `at_ms`, playing when
    /// `play`, in a session of its own. Only on the user's own gesture:
    /// never while a control client's request or a Lua script's action is
    /// handled, and never for a file found changed, which a reload reads
    /// again. Its silent preview ends first, and a lane past the most that
    /// play at once gives its session up, the one played least recently.
    pub(super) fn open_lane_playback(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        at_ms: u64,
        play: bool,
    ) {
        use crate::window_state::LaneNotice;
        if self.ctl_driving || self.lua_driving {
            return;
        }
        let Some((item, generation, duration_ms, open)) = self.lane_play_open(ws, pane, at_ms)
        else {
            return;
        };
        let tell = |ws: &mut WindowState, notice: Option<LaneNotice>| {
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = notice;
            }
        };
        if preview_item(ws, pane).is_some_and(|item| item.source.changed) {
            tell(ws, Some(LaneNotice::Changed));
            return self.lanes_changed(ws, false);
        }
        let Some(client) = self.startup.media.clone() else {
            tell(ws, Some(LaneNotice::VideoFailed));
            return self.lanes_changed(ws, false);
        };
        self.end_lane_preview(ws, pane);
        // The lane's own session goes first: it is the one replaced.
        if let Some(playback) = ws.lane_playbacks.get_mut(&pane) {
            playback.park();
        }
        self.make_room_for_playback(ws);
        let proxy = self.proxy.clone();
        let wake: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            let _ = proxy.send_event(UserEvent::LanePlayback);
        });
        let now = std::time::Instant::now();
        match client.play(&open, wake) {
            Ok(session) => {
                let session: Box<dyn crate::media::PlaybackSession> = Box::new(session);
                match ws
                    .lane_playbacks
                    .get_mut(&pane)
                    .filter(|playback| playback.serves(item, generation))
                {
                    Some(playback) => playback.reopen(session, open.start_ms, play, now),
                    None => {
                        ws.lane_playbacks.insert(
                            pane,
                            crate::media::LanePlayback::new(
                                item,
                                generation,
                                duration_ms,
                                session,
                                open.start_ms,
                                play,
                                now,
                            ),
                        );
                    }
                }
                // Asked for no sound: only an external decoder plays it.
                if open.audio == kettle_media::AudioRequest::Off
                    && let Some(playback) = ws.lane_playbacks.get_mut(&pane)
                {
                    playback.play_silently();
                }
                tell(ws, None);
            }
            Err(failure) => tell(ws, Some(playback_failure_notice(failure))),
        }
        ws.accessibility_pending = true;
        self.lanes_changed(ws, false);
    }

    /// Park playbacks, least recently played first, until fewer than the
    /// most that play at once have a session, here and in every other
    /// window: each keeps its frame and where it stands.
    pub(super) fn make_room_for_playback(&mut self, ws: &mut WindowState) {
        loop {
            let mut live = 0;
            let mut oldest: Option<(std::time::Instant, Option<u64>, u64)> = None;
            let mut consider = |touched, window, pane| {
                live += 1;
                if oldest.is_none_or(|(at, _, _)| touched < at) {
                    oldest = Some((touched, window, pane));
                }
            };
            for (&pane, playback) in &ws.lane_playbacks {
                if playback.has_session() {
                    consider(playback.touched, None, pane);
                }
            }
            for (&seq, window) in &self.windows {
                for (&pane, playback) in &window.lane_playbacks {
                    if playback.has_session() {
                        consider(playback.touched, Some(seq), pane);
                    }
                }
            }
            if live < crate::media::MAX_PLAYING {
                return;
            }
            let Some((_, window, pane)) = oldest else {
                return;
            };
            let window = match window {
                None => &mut *ws,
                Some(seq) => match self.windows.get_mut(&seq) {
                    Some(window) => window,
                    None => return,
                },
            };
            if let Some(playback) = window.lane_playbacks.get_mut(&pane) {
                playback.park();
            }
            window.accessibility_pending = true;
            if let Some(handle) = &window.window {
                handle.request_redraw();
            }
        }
    }

    /// An assistive technology's move along `pane`'s bar: to the value it
    /// sets, in seconds, or a step either way, the video opened there and
    /// playing if it played. Only while the lane shows the bar.
    pub(super) fn seek_lane_by_accessibility(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        request: &accesskit::ActionRequest,
    ) {
        let shown = self
            .preview_lane_projections(ws)
            .into_iter()
            .find(|lane| lane.pane == pane)
            .is_some_and(|lane| {
                lane.scrubber.is_some()
                    && self
                        .preview_lane_geometry(ws, &lane)
                        .is_some_and(|geometry| geometry.scrubber.is_some())
            });
        let Some(playback) = ws.lane_playbacks.get_mut(&pane).filter(|_| shown) else {
            return;
        };
        let at = playback.position_ms();
        let step = LANE_SEEK_STEP.as_millis() as u64;
        let target = match (request.action, &request.data) {
            (AccessibilityAction::SetValue, Some(accesskit::ActionData::NumericValue(seconds)))
                if seconds.is_finite() =>
            {
                (seconds.max(0.0) * 1000.0).round() as u64
            }
            (AccessibilityAction::Increment, _) => at.saturating_add(step),
            (AccessibilityAction::Decrement, _) => at.saturating_sub(step),
            _ => return,
        };
        let (at_ms, play) = playback.seek_target(target);
        self.open_lane_playback(ws, pane, at_ms, play);
    }

    /// The release of a drag along `pane`'s bar: open the video where it
    /// ended, playing if it played.
    pub(super) fn seek_lane_playback(&mut self, ws: &mut WindowState, pane: u64) {
        let Some((at_ms, play)) = ws
            .lane_playbacks
            .get_mut(&pane)
            .and_then(crate::media::LanePlayback::release_scrub)
        else {
            return;
        };
        self.open_lane_playback(ws, pane, at_ms, play);
    }

    /// Take what each lane's session has: its newest frame and where it is.
    /// One that ended keeps its last frame; one that failed says why.
    pub(super) fn poll_lane_playbacks(&mut self, ws: &mut WindowState) {
        let budget = kettle_core::GraphicsBudget::previews();
        let mut redraw = false;
        let mut failed = Vec::new();
        let mut reopen = Vec::new();
        for (&pane, playback) in &mut ws.lane_playbacks {
            let before = (playback.button(), playback.notice());
            redraw |= playback.poll(&budget);
            if let Some((at_ms, play)) = playback.take_reopen() {
                reopen.push((pane, at_ms, play));
            }
            let after = (playback.button(), playback.notice());
            if before != after {
                ws.accessibility_pending = true;
                if let Some(crate::media::PlaybackNotice::Failed(failure)) = after.1 {
                    failed.push((pane, failure));
                }
            }
        }
        for (pane, failure) in failed {
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = Some(playback_failure_notice(failure));
            }
            // A file found changed is read no more for its item, played or
            // previewed, until a reload replaces it.
            if failure == kettle_media::FailureCode::Changed
                && let Some(playback) = ws.lane_playbacks.get(&pane)
                && let Some(item) = ws
                    .mux
                    .panes
                    .get_mut(&pane)
                    .and_then(|pane| pane.media_shelf.get_mut(playback.item))
                    .filter(|item| item.generation == playback.generation)
            {
                item.source.changed = true;
            }
        }
        // A session Apple's decoder turned down opens again, silent, as the
        // user had it.
        for (pane, at_ms, play) in reopen {
            self.open_lane_playback(ws, pane, at_ms, play);
        }
        if redraw && let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// Keep each lane's session while this window's loop runs, pause it
    /// while its lane cannot be seen, and drop a playback whose lane shows
    /// another item. When the loop should come round again to keep them.
    pub(super) fn tick_lane_playbacks(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        if ws.lane_playbacks.is_empty() {
            return None;
        }
        let stale: Vec<u64> = ws
            .lane_playbacks
            .iter()
            .filter(|(pane, playback)| {
                preview_item(ws, **pane)
                    .is_none_or(|item| !playback.serves(item.id, item.generation))
            })
            .map(|(pane, _)| *pane)
            .collect();
        for pane in stale {
            ws.lane_playbacks.remove(&pane);
        }
        let hidden = window_is_render_hidden(ws);
        let visible: std::collections::HashSet<u64> = if hidden {
            std::collections::HashSet::new()
        } else {
            ws.lane_playbacks
                .keys()
                .copied()
                .filter(|&pane| {
                    self.preview_lane_geometry_of(ws, pane)
                        .is_some_and(|geometry| geometry.full && geometry.image.is_some())
                })
                .collect()
        };
        // A drag along a bar that ended without its release, by a menu
        // opening or another press, leaves the video where it was.
        let scrubbing = ws.lane_drag.filter(|drag| drag.scrub).map(|drag| drag.pane);
        let mut live = false;
        for (pane, playback) in &mut ws.lane_playbacks {
            if scrubbing != Some(*pane) {
                playback.cancel_scrub();
            }
            playback.set_hidden(!visible.contains(pane));
            playback.keep();
            live |= playback.has_session();
        }
        let _ = now;
        live.then_some(LANE_PLAYBACK_KEEP)
    }

    /// Let `pane`'s preview go, frames and all: a job still making frames is
    /// cancelled, and one still waiting is withdrawn.
    pub(super) fn end_lane_preview(&mut self, ws: &mut WindowState, pane: u64) {
        if let Some(mut preview) = ws.lane_previews.remove(&pane) {
            preview.stop();
            self.media.withdraw_lane_preview(pane);
        }
    }

    /// Take a lane's silent preview frames. They stay the lane's, shown in
    /// the poster's place, only while the lane still waits on this job for
    /// the item as it was, read from the source the item came from, and
    /// only a sheet laid out as the job asked; the item and its cards keep
    /// their poster. A failure says why in the lane, and the poster stays.
    pub(super) fn finish_lane_preview(
        &mut self,
        ws: &mut WindowState,
        render: crate::media::LaneRender,
        ticket: u64,
        spec: crate::media::JobSpec,
        result: Result<kettle_media::RenderOutput, kettle_media::client::RenderError>,
    ) {
        use crate::window_state::LaneNotice;
        use kettle_media::FailureCode;
        use kettle_media::client::RenderError;
        let waited = self
            .window_by_seq(ws, render.window)
            .and_then(|window| window.lane_previews.get(&render.pane))
            .is_some_and(|preview| {
                preview.waits_on() == Some(ticket) && preview.serves(render.item, render.generation)
            });
        if !waited {
            return;
        }
        let fail = |app: &mut Self, ws: &mut WindowState, notice| {
            if let Some(window) = app.window_by_seq(ws, render.window) {
                window.lane_previews.remove(&render.pane);
            }
            app.tell_lane(ws, render.window, render.pane, render.item, notice);
        };
        let output = match result {
            Ok(output) => output,
            // Stopped: its lane already let it go.
            Err(RenderError::Cancelled) => return,
            Err(RenderError::Failure(FailureCode::Changed)) => {
                self.mark_preview_changed(ws, &render);
                return fail(self, ws, LaneNotice::Changed);
            }
            Err(RenderError::Failure(failure)) => {
                return fail(self, ws, preview_failure_notice(failure));
            }
        };
        let rendered = output.rendered;
        let verdict = self
            .window_by_seq(ws, render.window)
            .and_then(|window| window.mux.panes.get(&render.pane))
            .map_or(LaneRenderVerdict::Gone, |pane| {
                lane_render_verdict(pane.media_shelf.items(), &render, &rendered.digest)
            });
        match verdict {
            LaneRenderVerdict::Gone => {
                if let Some(window) = self.window_by_seq(ws, render.window) {
                    window.lane_previews.remove(&render.pane);
                }
                return;
            }
            LaneRenderVerdict::Changed => {
                self.mark_preview_changed(ws, &render);
                return fail(self, ws, LaneNotice::Changed);
            }
            LaneRenderVerdict::Same => {}
        }
        // Frames that came after their two seconds are as late as none.
        let now = std::time::Instant::now();
        if self
            .window_by_seq(ws, render.window)
            .and_then(|window| window.lane_previews.get(&render.pane))
            .is_some_and(|preview| preview.overdue(now))
        {
            return fail(self, ws, LaneNotice::PreviewTimedOut);
        }
        let kettle_media::JobKind::VideoStills(stills) = spec.kind else {
            return fail(self, ws, LaneNotice::PreviewFailed);
        };
        let Some(layout) = crate::media::PreviewLayout::of(&stills, &rendered) else {
            return fail(self, ws, LaneNotice::PreviewFailed);
        };
        let Ok(sheet) =
            self.admit_preview_pixels(ws, rendered.width, rendered.height, rendered.rgba)
        else {
            return fail(self, ws, LaneNotice::PreviewFailed);
        };
        let Some(frames) = crate::media::PreviewFrames::new(sheet, layout) else {
            return fail(self, ws, LaneNotice::PreviewFailed);
        };
        let Some(window) = self.window_by_seq(ws, render.window) else {
            return;
        };
        if window
            .lane_previews
            .get_mut(&render.pane)
            .is_some_and(|preview| preview.ready(ticket, frames, now))
        {
            window.accessibility_pending = true;
            if let Some(handle) = &window.window {
                handle.request_redraw();
            }
        }
    }

    /// Move every lane preview on to `now` and say how long until one next
    /// needs it. A preview goes when its lane closes, shows another item,
    /// or its item changes; one whose frames are late says so; frames play
    /// only while their lane shows its picture in a visible window; and,
    /// with `video-preview-hover` on and motion allowed, the real pointer
    /// resting on a video lane's picture starts a hover loop, once a rest.
    pub(super) fn tick_lane_previews(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        if ws.lane_previews.is_empty() && !self.cfg.video_preview_hover {
            ws.lane_hover = None;
            return None;
        }
        let stale: Vec<u64> = ws
            .lane_previews
            .iter()
            .filter(|(pane, preview)| {
                preview_item(ws, **pane)
                    .is_none_or(|item| !preview.serves(item.id, item.generation))
            })
            .map(|(pane, _)| *pane)
            .collect();
        for pane in stale {
            self.end_lane_preview(ws, pane);
        }
        let hidden = window_is_render_hidden(ws);
        let hovered = self.hovered_lane_picture(ws);
        let at = ws.native_pointer.map(|pointer| (pointer.x, pointer.y));
        ws.lane_hover = next_hover(ws.lane_hover, hovered, at, now);
        let motion = self.cfg.video_preview_hover && self.motion_allowed();
        let visible: std::collections::HashSet<u64> = if hidden {
            std::collections::HashSet::new()
        } else {
            ws.lane_previews
                .keys()
                .copied()
                .filter(|&pane| {
                    self.preview_lane_geometry_of(ws, pane)
                        .is_some_and(|geometry| geometry.full && geometry.image.is_some())
                })
                .collect()
        };
        let mut wake: Option<std::time::Instant> = None;
        let mut redraw = false;
        let mut late = Vec::new();
        for (&pane, preview) in &mut ws.lane_previews {
            let tick = preview.tick(
                now,
                crate::media::PreviewSeen {
                    visible: visible.contains(&pane),
                    hovered: hovered == Some(pane),
                    motion,
                },
            );
            redraw |= tick.redraw;
            if tick.timed_out {
                late.push(pane);
            }
            wake = earliest(wake, preview.wake());
        }
        for pane in late {
            if let Some(item) = ws.lane_previews.remove(&pane).map(|preview| preview.item) {
                self.media.withdraw_lane_preview(pane);
                self.tell_lane(
                    ws,
                    ws.seq,
                    pane,
                    item,
                    crate::window_state::LaneNotice::PreviewTimedOut,
                );
                redraw = true;
            }
        }
        // A rest long enough loops the frames a preview the user started
        // left, once; a hover never makes frames, which would read the file.
        if motion
            && let Some(hover) = ws.lane_hover.filter(|hover| !hover.used)
            && !ws.lane_playbacks.contains_key(&hover.pane)
            && ws
                .lane_previews
                .get(&hover.pane)
                .is_some_and(|preview| !preview.active())
        {
            let due = hover.since + crate::media::PREVIEW_HOVER_DWELL;
            if now >= due {
                ws.lane_hover = Some(crate::window_state::LaneHover {
                    used: true,
                    ..hover
                });
                if let Some(preview) = ws.lane_previews.get_mut(&hover.pane)
                    && preview.replay(now, crate::media::PreviewStart::Hover)
                {
                    wake = earliest(wake, preview.wake());
                    redraw = true;
                }
            } else {
                wake = earliest(wake, Some(due));
            }
        }
        if redraw {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        wake.map(|at| {
            at.saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        })
    }

    /// The lane whose video picture the real pointer rests on, never a
    /// position a control client moved.
    pub(super) fn hovered_lane_picture(&self, ws: &WindowState) -> Option<u64> {
        // Nothing under a dialog or a menu, or in a window not drawn, is in
        // reach of the pointer.
        if window_is_render_hidden(ws) || self.pointer_modal_open(ws) || ws.context_menu.is_some() {
            return None;
        }
        let pointer = ws.native_pointer?;
        let (x, y) = (pointer.x as f32, pointer.y as f32);
        let (pane, _, _) = self.preview_lane_at(ws, x, y)?;
        let geometry = self.preview_lane_geometry_of(ws, pane)?;
        let picture = geometry.shown()?;
        let offered = ws
            .preview_panels
            .get(&pane)
            .zip(preview_item(ws, pane))
            .is_some_and(|(panel, item)| lane_previews_item(item, panel).is_some());
        (offered && rect_contains(picture, x, y)).then_some(pane)
    }

    /// What a screen reader hears that `pane`'s lane last did: its notice,
    /// or that a silent preview is being made. A preview that plays is said
    /// by its button's name, never frame by frame.
    pub(super) fn lane_announcement(&self, ws: &WindowState, pane: u64) -> Option<String> {
        // A video opening says so, as a preview being made does; one that
        // plays is said by its button's name, never frame by frame.
        // A video playing without sound says why, once.
        match ws
            .lane_playbacks
            .get(&pane)
            .and_then(crate::media::LanePlayback::notice)
        {
            Some(crate::media::PlaybackNotice::Opening) => {
                return Some(
                    self.ui_text
                        .text(kettle_i18n::Text::MediaLaneNoticeVideoLoading)
                        .to_string(),
                );
            }
            Some(crate::media::PlaybackNotice::Silent(why)) => {
                return Some(self.ui_text.text(silence_text(why)).to_string());
            }
            _ => {}
        }
        lane_announced(
            ws.lane_previews.get(&pane),
            ws.preview_panels.get(&pane).and_then(|panel| panel.notice),
            &self.ui_text,
        )
    }

    /// Whether Kettle may start motion on its own: `reduce-motion`, and for
    /// `auto` the system's Reduce Motion where it has one; where it has
    /// none, motion is held back.
    pub(super) fn motion_allowed(&self) -> bool {
        crate::motion::allowed(self.cfg.reduce_motion, || {
            self.system_motion
                .get_or_init(|| crate::motion::SystemMotion::new(self.waker()))
                .allows()
        })
    }

    /// Show `pane`'s item as its source, or rendered again; only an item
    /// whose source the lane holds has the choice.
    pub(super) fn switch_preview_mode(&mut self, ws: &mut WindowState, pane: u64) {
        use kettle_render::MediaLaneMode as Mode;
        let has_text = preview_item(ws, pane).is_some_and(|item| item.source.text().is_some());
        let Some(panel) = ws.preview_panels.get_mut(&pane).filter(|_| has_text) else {
            return;
        };
        panel.mode = match panel.mode {
            Mode::Rendered => Mode::Source,
            Mode::Source => Mode::Rendered,
        };
        panel.notice = None;
        self.lanes_changed(ws, false);
    }

    /// Show `pane`'s item on the next canvas: the pane's background, white,
    /// then a checkerboard. A raster's or an SVG's is drawn behind its
    /// pixels; a diagram's colors follow its canvas, so it is rendered
    /// again for it, from the same source.
    pub(super) fn next_preview_canvas(&mut self, ws: &mut WindowState, pane: u64) {
        use kettle_render::MediaCanvas as C;
        let Some((item, panel)) = preview_item(ws, pane).zip(ws.preview_panels.get(&pane)) else {
            return;
        };
        if item.image().is_none() {
            return;
        }
        let next = match chosen_canvas(item, panel) {
            C::Theme => C::White,
            C::White => C::Checker,
            C::Checker => C::Theme,
        };
        let diagram = crate::media::draws_as_diagram(item.kind);
        if let Some(panel) = ws.preview_panels.get_mut(&pane) {
            panel.canvas = Some(next);
            panel.notice = None;
        }
        if diagram {
            self.render_preview_again(ws, pane, media_canvas(next));
        }
        self.lanes_changed(ws, false);
    }

    /// Render `pane`'s item again from its source on `canvas`, for its lane.
    /// The result replaces the item's pixels only if the item is still the
    /// one shown, unreplaced, and its source unchanged (see
    /// [`Self::finish_lane_render`]). A file is read again only on the user's
    /// own gesture, never while a control client's request is handled.
    pub(super) fn render_preview_again(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        canvas: kettle_media::Canvas,
    ) {
        let Some(item) = preview_item(ws, pane) else {
            return;
        };
        let mut spec = item.source.spec.clone();
        let reads_a_file = matches!(spec.input, crate::media::SourceInput::Path { .. });
        // A file found changed is read again only by a reload.
        if reads_a_file && item.source.changed {
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = Some(crate::window_state::LaneNotice::Changed);
            }
            return;
        }
        if self.ctl_driving && reads_a_file
            || matches!(spec.input, crate::media::SourceInput::Released)
        {
            if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                panel.notice = Some(crate::window_state::LaneNotice::RenderFailed);
            }
            return;
        }
        // A gallery page asked for and not yet shown is what the lane will
        // show: it is asked for again on this canvas, taking the waiting
        // request's place rather than leaving it to be dropped unsaid.
        let (id, generation) = (item.id, item.generation);
        let waited = ws
            .preview_panels
            .get(&pane)
            .and_then(|panel| page_waited_on(panel, generation))
            .and_then(|ask| item.source.page_spec(ask.page).map(|spec| (ask.page, spec)));
        let page = match waited {
            Some((page, page_spec)) => {
                spec = page_spec;
                self.page_tickets += 1;
                let ask = crate::media::PageAsk {
                    page,
                    ticket: self.page_tickets,
                };
                if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                    panel.page = Some((generation, ask));
                }
                Some(ask)
            }
            None => None,
        };
        spec.canvas = canvas;
        let render = crate::media::LaneRender {
            window: ws.seq,
            pane,
            item: id,
            generation,
            preview: None,
            tile: None,
            page,
        };
        self.render_for_lane(ws, render, spec, crate::window_state::LaneNotice::Rendering);
    }

    /// Ask the worker for `render` of `spec`, for `pane`'s lane, which says
    /// `notice` (what it is rendering) until it comes back. A newer render
    /// for the lane takes the place of one still waiting.
    pub(super) fn render_for_lane(
        &mut self,
        ws: &mut WindowState,
        render: crate::media::LaneRender,
        spec: crate::media::JobSpec,
        notice: crate::window_state::LaneNotice,
    ) {
        let pane = render.pane;
        let deadline = std::time::Instant::now() + kettle_ctl::show::SHOW_SERVER_DEADLINE;
        let push = crate::media::Push::new(
            crate::media::Requester::Lane(render),
            crate::media::Route {
                pane,
                window: ws.seq,
                verified: true,
            },
            crate::media::Draft {
                key: None,
                title: String::new(),
                provenance: crate::media::Provenance::User,
                inline: None,
                inline_refused: None,
                gallery: None,
            },
            spec,
            kettle_media::client::RenderControl::with_deadline(deadline),
        );
        if let Some(panel) = ws.preview_panels.get_mut(&pane) {
            panel.notice = Some(notice);
        }
        self.media
            .admit(crate::media::Sender::Lane(pane), deadline, push);
        self.media_tick(ws);
    }

    /// Ask for the page of `pane`'s gallery or document `step` pages from the
    /// one asked for last and not yet shown, else from the one shown. A
    /// gallery's page renders from the pages kept, never the file, and a
    /// document's from the bytes a request carried, while they are kept; a
    /// document shown by its path reads that file again, as a reload does:
    /// only on the user's own gesture, never a control client's, and not once
    /// the file is found changed. It renders on the canvas shown and replaces
    /// the shown page once it comes back (see [`Self::finish_lane_render`]);
    /// until then the lane shows its page and says it is rendering. Past the
    /// first or last page nothing happens; back to the page shown, what was
    /// asked for is let go.
    pub(super) fn page_preview(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        step: crate::media::PageStep,
    ) {
        let Some(item) = preview_item(ws, pane) else {
            return;
        };
        let Some((shown, count)) = item.source.shown_page() else {
            return;
        };
        let document = item.source.is_document();
        let changed = item.source.changed;
        let (id, generation) = (item.id, item.generation);
        let asked = ws
            .preview_panels
            .get(&pane)
            .and_then(|panel| page_waited_on(panel, generation))
            .map(|ask| ask.page);
        // A page already on its way is not asked for again, as Home or End
        // pressed twice would.
        let Some(page) =
            gallery_step(shown, count, asked, step).filter(|&page| asked != Some(page))
        else {
            return;
        };
        // On the canvas the user chose, which a canvas render still waiting
        // would have drawn: this request takes its place.
        let canvas = ws
            .preview_panels
            .get(&pane)
            .map(|panel| media_canvas(chosen_canvas(item, panel)));
        let spec = item.source.page_spec(page).map(|mut spec| {
            if let Some(canvas) = canvas {
                spec.canvas = canvas;
            }
            spec
        });
        let Some(panel) = ws.preview_panels.get_mut(&pane) else {
            return;
        };
        if page == shown {
            panel.page = None;
            panel.notice = None;
            return self.lanes_changed(ws, false);
        }
        let Some(spec) = spec else {
            return;
        };
        // A page read from the file again is read only as a reload would be.
        let reads_a_file = matches!(spec.input, crate::media::SourceInput::Path { .. });
        if reads_a_file && (changed || self.ctl_driving) {
            panel.notice = Some(if changed {
                crate::window_state::LaneNotice::Changed
            } else {
                crate::window_state::LaneNotice::RenderFailed
            });
            return self.lanes_changed(ws, false);
        }
        self.page_tickets += 1;
        let ask = crate::media::PageAsk {
            page,
            ticket: self.page_tickets,
        };
        panel.page = Some((generation, ask));
        let render = crate::media::LaneRender {
            window: ws.seq,
            pane,
            item: id,
            generation,
            preview: None,
            tile: None,
            page: Some(ask),
        };
        let notice = if document {
            crate::window_state::LaneNotice::RenderingDocumentPage
        } else {
            crate::window_state::LaneNotice::RenderingPage
        };
        self.render_for_lane(ws, render, spec, notice);
        self.lanes_changed(ws, false);
    }

    /// Whether `render` is of the gallery page `pane`'s lane asked for last,
    /// or of the page it shows (drawn again on another canvas), for the item
    /// and generation it shows; true for any other render.
    pub(super) fn lane_wants(
        &mut self,
        ws: &mut WindowState,
        render: &crate::media::LaneRender,
    ) -> bool {
        let Some(ask) = render.page else {
            return true;
        };
        let Some(window) = self.window_by_seq(ws, render.window) else {
            return false;
        };
        let shown = window
            .mux
            .panes
            .get(&render.pane)
            .and_then(|pane| pane.media_shelf.get(render.item))
            .filter(|item| item.generation == render.generation)
            .and_then(|item| item.source.shown_page())
            .map(|(shown, _)| shown);
        window
            .preview_panels
            .get(&render.pane)
            .is_some_and(|panel| lane_takes_page(panel, render, ask, shown))
    }

    /// Let go of the gallery page request `render` made, if it is the one
    /// its lane still waits on; a request asked again since is not.
    pub(super) fn forget_lane_page(
        &mut self,
        ws: &mut WindowState,
        render: &crate::media::LaneRender,
    ) {
        let Some(ask) = render.page else {
            return;
        };
        if let Some(panel) = self
            .window_by_seq(ws, render.window)
            .and_then(|window| window.preview_panels.get_mut(&render.pane))
            .filter(|panel| {
                panel.item == render.item && page_waited_on(panel, render.generation) == Some(ask)
            })
        {
            panel.page = None;
        }
    }

    /// Copy the displayed pixels or source on the clipboard thread.
    pub(super) fn copy_preview(&mut self, ws: &mut WindowState, pane: u64) {
        let Some(mode) = preview_item(ws, pane)
            .zip(ws.preview_panels.get(&pane))
            .map(|(item, panel)| lane_mode(item, panel))
        else {
            return;
        };
        self.copy_preview_in_mode(ws, pane, mode);
    }

    /// Copy retained pixels or source without changing the preview's mode.
    pub(super) fn copy_preview_in_mode(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        mode: kettle_render::MediaLaneMode,
    ) {
        use crate::window_state::LaneNotice;
        let Some((item, panel)) = preview_item(ws, pane).zip(ws.preview_panels.get(&pane)) else {
            return;
        };
        let content = match mode {
            kettle_render::MediaLaneMode::Source => item
                .source
                .text_backing()
                .map(|text| crate::media::CopyContent::Text(Arc::clone(text))),
            kettle_render::MediaLaneMode::Rendered => {
                let frame = lane_frame(
                    item,
                    panel,
                    ws.lane_previews.get(&pane),
                    ws.lane_playbacks.get(&pane),
                );
                lane_copy_image(item, frame).map(crate::media::CopyContent::Image)
            }
        };
        let Some(content) = content else {
            return;
        };
        if self.preview_copy.is_none() {
            let proxy = self.proxy.clone();
            self.preview_copy = crate::media::CopyService::with_platform_clipboard(
                std::sync::Arc::new(move || {
                    let _ = proxy.send_event(UserEvent::MediaRendered);
                }),
            );
        }
        let started = self
            .preview_copy
            .as_ref()
            .map_or(crate::media::CopyStarted::Unavailable, |copy| {
                copy.copy(ws.seq, pane, item.id, item.generation, content)
            });
        let notice = match started {
            crate::media::CopyStarted::Started => None,
            crate::media::CopyStarted::Busy => Some(LaneNotice::CopyBusy),
            crate::media::CopyStarted::Unavailable => Some(LaneNotice::CopyFailed),
        };
        if let Some(panel) = ws.preview_panels.get_mut(&pane) {
            panel.notice = notice;
        }
        self.lanes_changed(ws, false);
    }

    /// Scroll the source the lane under the pointer shows by `steps`: a line
    /// per scrollback line, toward the end for the wheel down; sideways for
    /// a horizontal swipe, or the wheel with Shift held. Over a lane showing
    /// no source it does nothing.
    pub(super) fn scroll_preview_lane(&mut self, ws: &mut WindowState, steps: input::WheelSteps) {
        let Some((pane, _, _)) = self.preview_lane_at(ws, ws.cursor.x as f32, ws.cursor.y as f32)
        else {
            return;
        };
        let Some(total) = preview_item(ws, pane)
            .zip(ws.preview_panels.get(&pane))
            .filter(|(item, panel)| lane_mode(item, panel) == kettle_render::MediaLaneMode::Source)
            .map(|(item, _)| item.source.text_rows())
        else {
            return;
        };
        let (rows, columns) = source_scroll_by(steps, ws.mods.shift_key());
        if let Some(panel) = ws.preview_panels.get_mut(&pane) {
            let scroll = scrolled_source(panel.scroll, rows, columns, total);
            if scroll != panel.scroll {
                panel.scroll = scroll;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
        }
    }

    /// Change how `pane`'s lane shows its rendered item: `change` gets the
    /// view, the item's fitted rectangle and the content it shows in.
    /// Whether the lane shows a rendered item to change.
    pub(super) fn change_preview_view(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        change: impl FnOnce(
            kettle_render::MediaViewport,
            kettle_render::Rect4,
            kettle_render::Rect4,
        ) -> kettle_render::MediaViewport,
    ) -> bool {
        let Some(geometry) = self.preview_lane_geometry_of(ws, pane) else {
            return false;
        };
        if geometry.video {
            return false;
        }
        let (Some(fit), Some(panel)) = (geometry.fit, ws.preview_panels.get_mut(&pane)) else {
            return false;
        };
        let view = change(panel.view, fit, geometry.content);
        if view != panel.view {
            panel.view = view;
            self.want_lane_crop(ws, pane);
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        true
    }

    /// What shows of a zoomed item may have moved, or another item may show:
    /// each zoomed lane asks again for sharper pixels once it settles.
    pub(super) fn lanes_moved(&mut self, ws: &mut WindowState) {
        let zoomed: Vec<u64> = ws
            .preview_panels
            .iter()
            .filter(|(_, panel)| !panel.view.is_fit())
            .map(|(pane, _)| *pane)
            .collect();
        let control = self.ctl_driving;
        let now = std::time::Instant::now();
        for pane in zoomed {
            ws.lane_crops.entry(pane).or_default().again(now, control);
        }
    }

    /// `pane`'s lane view changed: ask for sharper pixels once it settles,
    /// for the view of whoever changed it.
    pub(super) fn want_lane_crop(&mut self, ws: &mut WindowState, pane: u64) {
        let control = self.ctl_driving;
        ws.lane_crops
            .entry(pane)
            .or_default()
            .view_changed(std::time::Instant::now(), control);
    }

    /// Ask for the sharper pixels whose time has come at `now`; how long
    /// until the next is due.
    pub(super) fn ask_due_lane_crops(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        let due: Vec<(u64, bool)> = ws
            .lane_crops
            .iter_mut()
            .filter_map(|(pane, crop)| crop.take_due(now).map(|control| (*pane, control)))
            .collect();
        for (pane, control) in due {
            self.ask_lane_crop(ws, pane, control);
        }
        ws.lane_crops
            .values()
            .filter_map(crate::media::LaneCrop::due)
            .min()
            .map(|at| {
                at.saturating_duration_since(now)
                    .max(std::time::Duration::from_millis(1))
            })
    }

    /// Ask the worker for the part of `pane`'s zoomed item in view at the
    /// size it shows, unless the lane's own pixels, or the sharper ones it
    /// holds, show it well enough; with nothing sharper needed, let those go.
    /// A file is read only for the user's own view: never when a control
    /// client changed it last (`control`), nor once the file changed since
    /// the item was rendered, until a reload.
    pub(super) fn ask_lane_crop(&mut self, ws: &mut WindowState, pane: u64, control: bool) {
        // Whatever this asks, or finds no need to, a render asked for
        // before is no longer wanted.
        if let Some(crop) = ws.lane_crops.get_mut(&pane) {
            crop.asked = None;
        }
        let Some(geometry) = self.preview_lane_geometry_of(ws, pane) else {
            return;
        };
        let Some(item) = preview_item(ws, pane) else {
            return;
        };
        let (Some(placed), Some(visible), Some(layout), Some(held), Some(kind)) = (
            geometry.image,
            geometry.visible(),
            item.layout,
            item.image().map(|image| (image.width, image.height)),
            item.kind.media(),
        ) else {
            return;
        };
        let plan = crate::media::plan_crop(
            kind,
            &layout,
            held,
            (f64::from(placed.2), f64::from(placed.3)),
            visible,
            item.source.spec.target.scale,
        );
        let (id, generation, changed) = (item.id, item.generation, item.source.changed);
        let mut spec = item.source.spec.clone();
        let crop = ws.lane_crops.entry(pane).or_default();
        let Some(target) = plan else {
            crop.tile = None;
            return;
        };
        let reads_file = matches!(spec.input, crate::media::SourceInput::Path { .. });
        if (reads_file && (changed || control))
            || matches!(spec.input, crate::media::SourceInput::Released)
            || crop.tile.as_ref().is_some_and(|tile| {
                tile.serves(id, generation, spec.canvas) && tile.suffices(visible, &target)
            })
        {
            return;
        }
        self.crop_tickets += 1;
        let ticket = self.crop_tickets;
        crop.asked = Some(ticket);
        spec.target = target;
        let render = crate::media::LaneRender {
            window: ws.seq,
            pane,
            item: id,
            generation,
            preview: None,
            tile: Some(ticket),
            page: None,
        };
        let deadline = std::time::Instant::now() + kettle_ctl::show::SHOW_SERVER_DEADLINE;
        let push = crate::media::Push::new(
            crate::media::Requester::Lane(render),
            crate::media::Route {
                pane,
                window: ws.seq,
                verified: true,
            },
            crate::media::Draft {
                key: None,
                title: String::new(),
                provenance: crate::media::Provenance::User,
                inline: None,
                inline_refused: None,
                gallery: None,
            },
            spec,
            kettle_media::client::RenderControl::with_deadline(deadline),
        );
        self.media
            .admit(crate::media::Sender::LaneTile(pane), deadline, push);
        self.media_tick(ws);
    }

    /// Zoom `pane`'s rendered item by `factor` about the content's center.
    pub(super) fn zoom_preview(&mut self, ws: &mut WindowState, pane: u64, factor: f64) {
        self.change_preview_view(ws, pane, |view, fit, content| {
            view.zoomed(factor, fit, content)
        });
    }

    /// Show `pane`'s rendered item fitted to its lane again.
    pub(super) fn fit_preview(&mut self, ws: &mut WindowState, pane: u64) {
        self.change_preview_view(ws, pane, |_, _, _| kettle_render::MediaViewport::FIT);
    }

    /// The wheel over the lane under the pointer: over its rendered item it
    /// zooms at the pointer with Cmd held on macOS, Ctrl elsewhere, and pans
    /// otherwise; over its source it scrolls. A lane showing neither takes
    /// the wheel and does nothing with it.
    pub(super) fn wheel_over_lane(
        &mut self,
        ws: &mut WindowState,
        delta: &winit::event::MouseScrollDelta,
    ) {
        let pointer = (ws.cursor.x as f32, ws.cursor.y as f32);
        let Some((pane, _, _)) = self.preview_lane_at(ws, pointer.0, pointer.1) else {
            return;
        };
        let wheel = lane_wheel(
            delta,
            lane_zoom_modifier(ws.mods),
            ws.mods.shift_key(),
            self.menu_cell(ws),
            self.cfg.scroll_multiplier,
        );
        let rendered = self.change_preview_view(ws, pane, |view, fit, content| match wheel {
            LaneWheel::Zoom(factor) => view.zoomed_at(factor, pointer, fit, content),
            LaneWheel::Pan(by) => view.panned(by, fit, content),
        });
        if rendered {
            ws.lane_wheel.reset();
            return;
        }
        let steps = ws.lane_wheel.feed(delta, self.cfg.scroll_multiplier);
        self.scroll_preview_lane(ws, steps);
    }

    /// Whole wheel `steps` a control client sent over the lane under the
    /// pointer: a rendered item zooms or pans by them as by the wheel (see
    /// [`lane_wheel_steps`]); a source scrolls.
    pub(super) fn wheel_steps_over_lane(&mut self, ws: &mut WindowState, steps: input::WheelSteps) {
        let Some((pane, _, _)) = self.preview_lane_at(ws, ws.cursor.x as f32, ws.cursor.y as f32)
        else {
            return;
        };
        let pointer = (ws.cursor.x as f32, ws.cursor.y as f32);
        let wheel = lane_wheel_steps(
            steps,
            lane_zoom_modifier(ws.mods),
            ws.mods.shift_key(),
            self.menu_cell(ws),
        );
        let rendered = self.change_preview_view(ws, pane, |view, fit, content| match wheel {
            LaneWheel::Zoom(factor) => view.zoomed_at(factor, pointer, fit, content),
            LaneWheel::Pan(by) => view.panned(by, fit, content),
        });
        if !rendered {
            self.scroll_preview_lane(ws, steps);
        }
    }

    /// A pinch over the lane under the pointer zooms its rendered item at
    /// the pointer.
    pub(super) fn pinch_over_lane(&mut self, ws: &mut WindowState, delta: f64) {
        let pointer = (ws.cursor.x as f32, ws.cursor.y as f32);
        if let Some((pane, _, _)) = self.preview_lane_at(ws, pointer.0, pointer.1) {
            let factor = pinch_zoom(delta);
            self.change_preview_view(ws, pane, |view, fit, content| {
                view.zoomed_at(factor, pointer, fit, content)
            });
        }
    }

    /// Follow the pointer for a press on a lane's rendered item: past a few
    /// logical pixels from the press it pans the item. Whether a press of
    /// the lane's is held, which then owns the pointer's motion.
    pub(super) fn drag_preview_lane(&mut self, ws: &mut WindowState) -> bool {
        let Some(mut drag) = ws.lane_drag else {
            return false;
        };
        // A menu or dialog that opened since owns the pointer.
        if ws.context_menu.is_some() || self.non_search_modal_open(ws) {
            ws.lane_drag = None;
            return false;
        }
        let at = (ws.cursor.x, ws.cursor.y);
        // A drag along a playing video's bar moves where it says the video
        // is; the release opens it there.
        if drag.scrub {
            let fraction = self
                .preview_lane_geometry_of(ws, drag.pane)
                .and_then(|geometry| geometry.scrub_at(at.0 as f32));
            if let (Some(fraction), Some(playback)) =
                (fraction, ws.lane_playbacks.get_mut(&drag.pane))
            {
                playback.scrub(fraction, std::time::Instant::now());
                self.lanes_changed(ws, false);
            }
            ws.lane_drag = Some(drag);
            return true;
        }
        let scale = ws
            .window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor());
        if !drag.panning && past_drag_slop(drag.from, at, scale) {
            drag.panning = true;
        }
        if drag.panning {
            let by = (at.0 - drag.last.0, at.1 - drag.last.1);
            drag.last = at;
            self.change_preview_view(ws, drag.pane, |view, fit, content| {
                view.panned(by, fit, content)
            });
        }
        ws.lane_drag = Some(drag);
        true
    }

    /// The release of a press a lane took for a drag: a drag along a playing
    /// video's bar opens the video where it ended; a pan is already done.
    pub(super) fn end_lane_drag(
        &mut self,
        ws: &mut WindowState,
        drag: crate::window_state::LaneDrag,
    ) {
        if drag.scrub {
            self.seek_lane_playback(ws, drag.pane);
        }
    }

    /// Follow the pointer for a press on a lane's edge: the lane takes the
    /// share of its pane the pointer leaves it, repainting at once and
    /// resizing the PTYs at most every [`LANE_RESIZE_PTY_INTERVAL`]. Whether
    /// such a press is held, which then owns the pointer's motion.
    ///
    /// [`LANE_RESIZE_PTY_INTERVAL`]: crate::window_state::LANE_RESIZE_PTY_INTERVAL
    pub(super) fn resize_preview_lane(&mut self, ws: &mut WindowState) -> bool {
        let Some(mut resize) = ws.lane_resize else {
            return false;
        };
        // A menu or dialog that opened since owns the pointer.
        if ws.context_menu.is_some() || self.non_search_modal_open(ws) {
            self.end_lane_resize(ws);
            return false;
        }
        let (area, style) = (self.area(ws), self.layout_style(ws));
        let pointer = (ws.cursor.x as f32, ws.cursor.y as f32);
        if ws.mux.drag_lane_edge(resize.pane, area, style, pointer) {
            let now = std::time::Instant::now();
            if now.duration_since(resize.resized) >= crate::window_state::LANE_RESIZE_PTY_INTERVAL {
                self.resize_all(ws);
                resize.resized = now;
                resize.pending = false;
            } else {
                resize.pending = true;
            }
            self.lanes_changed(ws, false);
        }
        ws.lane_resize = Some(resize);
        true
    }

    /// End a lane's edge drag, resizing the PTYs for where it ended.
    pub(super) fn end_lane_resize(&mut self, ws: &mut WindowState) {
        if ws.lane_resize.take().is_some_and(|resize| resize.pending) {
            self.resize_all(ws);
        }
    }

    /// Resize the PTYs for a lane's edge drag that has waited its interval
    /// at `now`; how long until one that has not is due.
    pub(super) fn flush_lane_resize(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        let resize = ws.lane_resize.as_mut().filter(|resize| resize.pending)?;
        let due = resize.resized + crate::window_state::LANE_RESIZE_PTY_INTERVAL;
        if due > now {
            return Some(due - now);
        }
        resize.resized = now;
        resize.pending = false;
        self.resize_all(ws);
        None
    }

    /// The cursor over a lane: resize arrows over its edge or while it is
    /// dragged, a pointing hand over a control, a hand over a rendered item
    /// that zooms, grabbing while it pans, and the arrow elsewhere in it.
    pub(super) fn lane_cursor_icon(&self, ws: &WindowState) -> Option<CursorIcon> {
        let edge_icon = |pane: u64| match ws.mux.lane(pane).map(|lane| lane.side) {
            Some(crate::pane_partition::LaneSide::Right) => CursorIcon::ColResize,
            _ => CursorIcon::RowResize,
        };
        if let Some(resize) = ws.lane_resize {
            return Some(edge_icon(resize.pane));
        }
        if ws.lane_drag.is_some_and(|drag| drag.panning) {
            return Some(CursorIcon::Grabbing);
        }
        let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let (pane, rect, collapsed) = self.preview_lane_at(ws, x, y)?;
        let geometry = self
            .preview_lane_panel(ws, pane, rect, collapsed)
            .and_then(|lane| self.preview_lane_geometry(ws, &lane));
        Some(
            match geometry
                .as_ref()
                .and_then(|geometry| geometry.hit_test(x, y))
            {
                Some(kettle_render::MediaLaneHit::Resize) => edge_icon(pane),
                Some(kettle_render::MediaLaneHit::Inside) => match geometry {
                    Some(geometry)
                        if geometry.fit.is_some() && rect_contains(geometry.content, x, y) =>
                    {
                        CursorIcon::Grab
                    }
                    _ => CursorIcon::Default,
                },
                Some(_) => CursorIcon::Pointer,
                None => CursorIcon::Default,
            },
        )
    }

    /// Give the focused pane's preview lane the keyboard, while it shows its
    /// item expanded with room for it: keys then move, zoom, fit or copy
    /// what it shows, and reach nothing else, until Esc. An input method's
    /// composition ends, and it stays off meanwhile.
    pub(super) fn focus_preview(&mut self, ws: &mut WindowState) {
        let Some(pane) = ws.mux.active_focus() else {
            return;
        };
        let Some(item) = ws.preview_panels.get(&pane).map(|panel| panel.item) else {
            return;
        };
        if !self
            .preview_lane_geometry_of(ws, pane)
            .is_some_and(|geometry| geometry.full)
        {
            return;
        }
        ws.preview_focus = Some(crate::media::PreviewFocus { pane, item });
        end_modal_composition(ws);
        if let Some(window) = &ws.window {
            window.set_ime_allowed(false);
        }
        self.lanes_changed(ws, false);
    }

    /// Release a pinned lane's keyboard. A viewer remains modal until closed,
    /// including across focus loss or a refused file drop.
    pub(super) fn exit_preview_focus(&mut self, ws: &mut WindowState) {
        if ws.preview_overlay.is_some() {
            return;
        }
        if ws.preview_focus.take().is_none() {
            return;
        }
        end_modal_composition(ws);
        if let Some(window) = &ws.window {
            window.set_ime_allowed(true);
        }
        self.lanes_changed(ws, false);
    }

    /// A press anywhere but on the lane holding the keyboard gives the
    /// keyboard back first, then goes where it would.
    pub(super) fn leave_preview_focus_for_press(&mut self, ws: &mut WindowState) {
        if ws.preview_overlay.is_some()
            || ws
                .context_menu
                .as_ref()
                .and_then(ContextMenuState::preview_target)
                .is_some()
        {
            return;
        }
        let Some(focus) = ws.preview_focus else {
            return;
        };
        let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        if self.preview_lane_at(ws, x, y).map(|(pane, _, _)| pane) != Some(focus.pane) {
            self.exit_preview_focus(ws);
        }
    }

    /// Whether a preview lane holds the keyboard in the window `pane` is in.
    pub(super) fn preview_holds_keyboard(&self, ws: &WindowState, pane: u64) -> bool {
        if ws.mux.panes.contains_key(&pane) {
            return ws.preview_focus.is_some();
        }
        self.windows
            .values()
            .find(|window| window.mux.panes.contains_key(&pane))
            .is_some_and(|window| window.preview_focus.is_some())
    }

    /// Keep preview focus while its item is visible and only its own menu covers it.
    pub(super) fn check_preview_focus(&mut self, ws: &mut WindowState) {
        if ws
            .context_menu
            .as_ref()
            .and_then(ContextMenuState::preview_target)
            .is_some_and(|target| !self.preview_menu_target_is_current(ws, target))
        {
            ws.context_menu = None;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        let Some(focus) = ws.preview_focus else {
            return;
        };
        let own_menu = ws
            .context_menu
            .as_ref()
            .and_then(ContextMenuState::preview_target)
            .is_some_and(|target| target.pane == focus.pane && target.item == focus.item);
        let usable = (top_modal(ws) == Some(KeyModal::Preview)
            || (top_modal(ws) == Some(KeyModal::ContextMenu) && own_menu))
            && ws.mux.active_focus() == Some(focus.pane)
            && ws
                .preview_panels
                .get(&focus.pane)
                .is_some_and(|panel| panel.item == focus.item)
            && self
                .preview_lane_geometry_of(ws, focus.pane)
                .is_some_and(|geometry| geometry.full);
        if !usable {
            if ws
                .preview_overlay
                .is_some_and(|open| open.pane == focus.pane)
            {
                self.close_preview(ws, focus.pane);
            } else {
                self.exit_preview_focus(ws);
            }
        }
    }

    /// A key while a lane holds the keyboard: Esc gives it back, arrows move
    /// the picture or scroll a source, Page Up and Page Down show a gallery's
    /// diagram before or after, `+`, `-` and `0` zoom and fit, and `c`
    /// copies; every other key does nothing. A held key that acts once a
    /// press does not repeat.
    pub(super) fn handle_viewer_key(
        &mut self,
        ws: &mut WindowState,
        key: &Key,
        repeat: bool,
    ) -> bool {
        use crate::media::ViewerKey;
        let Some(open) = ws.preview_overlay else {
            return false;
        };
        let Some(action) = crate::media::viewer_key(key, held_modifiers(ws)) else {
            return false;
        };
        if repeat && !matches!(action, ViewerKey::Focus(_)) {
            return true;
        }
        match action {
            ViewerKey::Focus(forward) => {
                let shown: Vec<u64> = self
                    .shown_preview_controls(ws, open.pane)
                    .into_iter()
                    .map(|(part, _, _)| part)
                    .collect();
                if let Some(open) = &mut ws.preview_overlay {
                    open.step_control(&shown, forward);
                }
                self.lanes_changed(ws, false);
            }
            ViewerKey::Pin => self.pin_preview(ws, open.pane),
            ViewerKey::Activate => {
                let hit = open.control.and_then(|part| {
                    self.shown_preview_controls(ws, open.pane)
                        .into_iter()
                        .find(|(at, _, _)| *at == part)
                        .map(|(_, _, hit)| hit)
                });
                if hit == Some(kettle_render::MediaLaneHit::Scrub)
                    && key == &Key::Named(NamedKey::Space)
                {
                    self.toggle_lane_play(ws, open.pane);
                } else if let Some(hit) = hit {
                    self.act_on_preview(ws, open.pane, hit);
                } else if open.control.is_none() && key == &Key::Named(NamedKey::Space) {
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn preview_focus_key(&mut self, ws: &mut WindowState, key: &Key, repeat: bool) {
        if self.handle_viewer_key(ws, key, repeat) {
            return;
        }
        use crate::media::PreviewKey as K;
        let Some(focus) = ws.preview_focus else {
            return;
        };
        let pane = focus.pane;
        let action = crate::media::preview_key(key, held_modifiers(ws));
        if repeat && action.once() {
            return;
        }
        let source = preview_item(ws, pane)
            .zip(ws.preview_panels.get(&pane))
            .is_some_and(|(item, panel)| {
                lane_mode(item, panel) == kettle_render::MediaLaneMode::Source
            });
        // A video playing in the lane, paused in place, or stopped where it
        // stood shows its frame, which never pans: left and right step it.
        let playing = preview_item(ws, pane).is_some_and(|item| {
            ws.lane_playbacks
                .get(&pane)
                .is_some_and(|playback| playback.serves(item.id, item.generation))
        });
        match action {
            K::Exit if ws.preview_overlay.is_some_and(|open| open.fullscreen) => {
                if let Some(open) = &mut ws.preview_overlay {
                    open.fullscreen = false;
                }
                if let Some(window) = &ws.window {
                    window.set_fullscreen(None);
                }
                self.lanes_changed(ws, false);
            }
            K::Exit if ws.preview_overlay.is_some() => self.close_preview(ws, pane),
            K::Exit => self.exit_preview_focus(ws),
            K::Move(right, 0) if playing && !source => {
                let step = LANE_SEEK_STEP.as_millis() as u64;
                if let Some(playback) = ws.lane_playbacks.get_mut(&pane) {
                    let at = playback.position_ms();
                    let target = if right < 0 {
                        at.saturating_sub(step)
                    } else {
                        at.saturating_add(step)
                    };
                    let (at_ms, play) = playback.seek_target(target);
                    self.open_lane_playback(ws, pane, at_ms, play);
                }
            }
            K::Move(right, down) if source => {
                if let Some(total) = preview_item(ws, pane).map(|item| item.source.text_rows())
                    && let Some(panel) = ws.preview_panels.get_mut(&pane)
                {
                    panel.scroll = scrolled_source(panel.scroll, down, right, total);
                }
            }
            K::Move(right, down) => {
                // Three cells a press, as a wheel's notch pans; the picture
                // moves the other way, showing what lies toward the arrow.
                let cell = self.menu_cell(ws);
                let step = f64::from(input::LINES_PER_NOTCH);
                let by = (
                    -f64::from(right) * step * f64::from(cell.0),
                    -f64::from(down) * step * f64::from(cell.1),
                );
                self.change_preview_view(ws, pane, |view, fit, content| {
                    view.panned(by, fit, content)
                });
            }
            K::ZoomIn => self.zoom_preview(ws, pane, kettle_render::MEDIA_ZOOM_STEP),
            K::ZoomOut => self.zoom_preview(ws, pane, 1.0 / kettle_render::MEDIA_ZOOM_STEP),
            K::Fit if source => {
                if let Some(panel) = ws.preview_panels.get_mut(&pane) {
                    panel.scroll = (0, 0);
                }
            }
            K::Fit => self.fit_preview(ws, pane),
            K::Page(step) => self.page_preview(ws, pane, step),
            K::Copy => self.copy_preview(ws, pane),
            K::Play => self.toggle_lane_play(ws, pane),
            K::Mute => self.toggle_lane_sound(ws, pane),
            K::Theater => self.toggle_preview_theater(ws, pane),
            K::Fullscreen => self.toggle_preview_fullscreen(ws, pane),
            K::Menu => {
                self.open_preview_menu(ws, pane, None);
            }
            K::Swallow => {}
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// Collapse `pane`'s lane to its strip, or expand it.
    pub(super) fn toggle_preview(&mut self, ws: &mut WindowState, pane: u64) {
        let Some(lane) = self.preview_panel_of(ws, pane) else {
            return;
        };
        // A small pane can show a strip while its saved request stays expanded.
        self.set_preview_expanded(ws, pane, lane.collapsed);
    }

    pub(super) fn set_preview_expanded(&mut self, ws: &mut WindowState, pane: u64, expanded: bool) {
        let resized = ws.mux.set_lane_expanded(pane, expanded);
        self.lanes_changed(ws, resized);
    }

    /// Take a primary press on a settled inline card, before focus, links,
    /// selection and mouse reporting, so nothing behind it gets the press.
    /// Shift passes the press on, as does a card that only just appeared.
    pub(super) fn press_card(&mut self, ws: &mut WindowState, px: f32, py: f32, bcode: u8) -> bool {
        if !matches!(bcode, 0 | 2) || ws.mods.shift_key() {
            return false;
        }
        let card = ws
            .renderer
            .as_ref()
            .and_then(|renderer| renderer.card_at(px, py));
        let Some(card) = ws.card_sightings.settled(card, std::time::Instant::now()) else {
            return false;
        };
        if bcode == 2 {
            // A right-press on a card opens the card's menu, not the
            // terminal's; its release is Kettle's own gesture's.
            return self.open_card_menu(ws, card, (px, py));
        }
        ws.card_press = Some(card);
        true
    }

    /// End a press a card took. Released over the same card, it opens the
    /// card's item in the viewer; anywhere else, or once a dialog or the
    /// viewer has opened over it, it is dropped. Either way the terminal saw
    /// neither the press nor the release.
    pub(super) fn release_card(
        &mut self,
        ws: &mut WindowState,
        px: f32,
        py: f32,
        bcode: u8,
    ) -> bool {
        if bcode != 0 {
            return false;
        }
        let Some((pane, nonce)) = ws.card_press.take() else {
            return false;
        };
        let over = ws
            .renderer
            .as_ref()
            .and_then(|renderer| renderer.card_at(px, py));
        let covered = self.pointer_modal_open(ws);
        if card_release_opens((pane, nonce), over, covered) {
            self.open_card(ws, pane, nonce);
        }
        true
    }

    /// Open the item `pane`'s card `nonce` shows in the pane's preview lane,
    /// for a click, a hint or assistive technology. Nothing opens while a
    /// dialog or a menu owns the pointer, or once the card's item has left
    /// the shelf.
    pub(super) fn open_card(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        nonce: kettle_core::InlineNonce,
    ) -> bool {
        if self.pointer_modal_open(ws) {
            return false;
        }
        let Some(item) = self.media.cards.item(pane, nonce) else {
            return false;
        };
        if !ws
            .mux
            .panes
            .get(&pane)
            .is_some_and(|state| state.media_shelf.items().iter().any(|it| it.id == item))
        {
            return false;
        }
        self.close_all_modals(ws);
        self.show_media_item(ws, pane, item);
        // Opening a card is what the tip taught.
        self.end_cards_tip(ws);
        true
    }

    /// End the tip that a click opens a card, and take it off its card in
    /// whichever window holds that pane.
    pub(super) fn end_cards_tip(&mut self, ws: &mut WindowState) {
        let Some(pane) = self.cards_tip.end() else {
            return;
        };
        for window in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
            if let Some(state) = window.mux.panes.get_mut(&pane) {
                state.inline_cards.set_tip(None);
                if let Some(handle) = &window.window {
                    handle.request_redraw();
                }
            }
        }
    }

    /// Open the menu of the card `nonce` in `pane` at (`px`, `py`): open its
    /// item in Kettle, and open a copy of it in the permitted image viewer,
    /// or of a video in the permitted video player, where this platform has
    /// one. False when the card's item has left the shelf, so the press goes
    /// where it would have.
    pub(super) fn open_card_menu(
        &mut self,
        ws: &mut WindowState,
        (pane, nonce): (u64, kettle_core::InlineNonce),
        (px, py): (f32, f32),
    ) -> bool {
        let Some(item) = self.media.cards.item(pane, nonce) else {
            return false;
        };
        let Some((generation, outside)) = ws.mux.panes.get(&pane).and_then(|state| {
            state
                .media_shelf
                .items()
                .iter()
                .find(|it| it.id == item)
                .map(|it| (it.generation, crate::media::Outside::of(it)))
        }) else {
            return false;
        };
        let target = MediaTarget {
            pane,
            nonce,
            item,
            generation,
        };
        let tr = self.ui_text;
        let mut items = vec![ContextMenuItem::Media {
            label: tr.text(kettle_i18n::Text::MenuOpenInKettle),
            target,
            action: MediaMenuAction::View,
        }];
        if let Some(outside) = outside {
            items.push(ContextMenuItem::Media {
                label: outside.label(&tr),
                target,
                action: MediaMenuAction::OpenOutside(outside),
            });
        }
        self.show_context_menu(ws, items, px, py);
        true
    }

    /// Act on a card menu row, if the card still shows the item the menu
    /// named: open it as a click would, or a copy of it outside Kettle.
    pub(super) fn act_on_media(
        &mut self,
        ws: &mut WindowState,
        target: MediaTarget,
        action: MediaMenuAction,
    ) {
        if self.media.cards.item(target.pane, target.nonce) != Some(target.item) {
            return;
        }
        let Some(item) = ws.mux.panes.get(&target.pane).and_then(|state| {
            state
                .media_shelf
                .items()
                .iter()
                .find(|it| it.id == target.item && it.generation == target.generation)
        }) else {
            return;
        };
        match action {
            MediaMenuAction::View => {
                self.open_card(ws, target.pane, target.nonce);
            }
            MediaMenuAction::OpenOutside(outside) => self.open_item_outside(item, outside),
        }
    }

    /// The item `pane`'s lane shows, if it has one and the item is still
    /// on the shelf.
    pub(super) fn preview_item<'a>(
        &self,
        ws: &'a WindowState,
        pane: u64,
    ) -> Option<&'a crate::media::ShelfItem> {
        let panel = ws.preview_panels.get(&pane)?;
        ws.mux
            .panes
            .get(&pane)?
            .media_shelf
            .items()
            .iter()
            .find(|it| it.id == panel.item)
    }

    /// What `pane`'s lane offers to open its item in outside Kettle: none
    /// when this platform has nothing permitted for it, an image's pixels
    /// were let go, or a PDF's bytes were. A video opens from its file, not
    /// its poster, a PDF from its file or bytes, not its page, and a file
    /// Kettle does not draw is shown in its folder.
    pub(super) fn preview_opens_outside(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Option<crate::media::Outside> {
        use crate::media::Outside;
        let item = self.preview_item(ws, pane)?;
        let outside = Outside::of(item)?;
        match outside {
            Outside::Video(_) => true,
            Outside::Document(_) => crate::media::DocumentSource::of(item).is_some(),
            Outside::Image(_) => item.image().is_some(),
            Outside::Reveal(_) => crate::media::RevealTarget::of(item).is_some(),
        }
        .then_some(outside)
    }

    /// Open `pane`'s lane item outside Kettle, when it offers to.
    pub(super) fn open_preview_outside(&self, ws: &WindowState, pane: u64) {
        let Some(outside) = self.preview_opens_outside(ws, pane) else {
            return;
        };
        if let Some(item) = self.preview_item(ws, pane) {
            self.open_item_outside(item, outside);
        }
    }

    /// Open a copy of `item` `outside`: a PNG of an image's pixels in the
    /// image viewer, a private copy of a video's file in the video player,
    /// or a private copy of a PDF in the document viewer. A notice says why
    /// if it does not open.
    pub(super) fn open_item_outside(
        &self,
        item: &crate::media::ShelfItem,
        outside: crate::media::Outside,
    ) {
        let tr = self.ui_text;
        let failed = move |why: crate::media::OpenFailure| {
            let (title, body) = why.notice(outside);
            fire_notify(tr.text(title), tr.text(body));
        };
        match outside {
            crate::media::Outside::Image(viewer) => match item.image() {
                Some(image) => crate::media::open_externally(
                    std::sync::Arc::clone(&self.viewer_copies),
                    image.clone(),
                    viewer,
                    failed,
                ),
                // Its pixels were let go to stay within the budget.
                None => failed(crate::media::OpenFailure::Copy),
            },
            crate::media::Outside::Video(player) => match crate::media::VideoSource::of(item) {
                Some(source) => crate::media::open_video(
                    std::sync::Arc::clone(&self.video_copies),
                    source,
                    player,
                    failed,
                ),
                // Shown from no file Kettle can name: nothing to copy.
                None => failed(crate::media::OpenFailure::Changed),
            },
            crate::media::Outside::Document(reader) => {
                match crate::media::DocumentSource::of(item) {
                    Some(source) => crate::media::open_document(
                        std::sync::Arc::clone(&self.document_copies),
                        source,
                        reader,
                        failed,
                    ),
                    // Its bytes were let go to stay within the budget.
                    None => failed(crate::media::OpenFailure::Copy),
                }
            }
            crate::media::Outside::Reveal(revealer) => match crate::media::RevealTarget::of(item) {
                Some(target) => crate::media::reveal(target, revealer, failed),
                None => failed(crate::media::OpenFailure::Changed),
            },
        }
    }

    /// Show the shelf item `step` places along from the one `pane`'s lane
    /// shows, stopping at either end.
    pub(super) fn step_preview(&mut self, ws: &mut WindowState, pane: u64, step: isize) {
        let Some(panel) = ws.preview_panels.get(&pane).copied() else {
            return;
        };
        let Some(items) = ws
            .mux
            .panes
            .get(&pane)
            .map(|state| state.media_shelf.items())
        else {
            return;
        };
        let Some(at) = items.iter().position(|item| item.id == panel.item) else {
            return;
        };
        let Some(next) = at
            .checked_add_signed(step)
            .and_then(|index| items.get(index))
            .map(|item| item.id)
        else {
            return;
        };
        self.show_media_item(ws, pane, next);
    }

    /// Close every lane that has nothing to show: its pane closed or left
    /// this window, or its item left the shelf. A lane without a panel, or a
    /// panel without a lane, goes too. Run on each pass of the event loop.
    pub(super) fn prune_previews(&mut self, ws: &mut WindowState) {
        let before = ws.preview_panels.len();
        let live: Vec<u64> = ws
            .preview_panels
            .keys()
            .copied()
            .filter(|pane| {
                (ws.mux.lane(*pane).is_some()
                    || ws.preview_overlay.is_some_and(|open| open.pane == *pane))
                    && ws.mux.panes.get(pane).is_some_and(|state| !state.closed)
                    && self.preview_item(ws, *pane).is_some()
            })
            .collect();
        ws.preview_panels.retain(|pane, _| live.contains(pane));
        if let Some(open) = ws.preview_overlay.filter(|open| !live.contains(&open.pane)) {
            self.restore_overlay_origin(ws, open.pane);
        }
        ws.lane_crops.retain(|pane, _| live.contains(pane));
        let gone: Vec<u64> = ws
            .lane_previews
            .keys()
            .copied()
            .filter(|pane| !live.contains(pane))
            .collect();
        for pane in gone {
            self.end_lane_preview(ws, pane);
        }
        ws.lane_playbacks.retain(|pane, _| live.contains(pane));
        let resized = ws.mux.retain_lanes(|pane| live.contains(&pane));
        if resized || ws.preview_panels.len() != before {
            self.lanes_changed(ws, resized);
        }
    }

    /// The active tab's preview lanes, projected for painting: each pane
    /// whose lane has room to show more than a badge.
    pub(super) fn preview_lane_projections(
        &self,
        ws: &WindowState,
    ) -> Vec<kettle_render::MediaLanePanel> {
        self.pane_layout(ws, self.area(ws))
            .into_iter()
            .filter_map(|(pane, partition)| {
                let (rect, collapsed) = self.preview_placement(ws, pane, partition)?;
                self.preview_lane_panel(ws, pane, rect, collapsed)
            })
            .collect()
    }

    pub(super) fn preview_placement(
        &self,
        ws: &WindowState,
        pane: u64,
        partition: crate::pane_partition::PanePartition,
    ) -> Option<(Rect, bool)> {
        if let Some(open) = ws.preview_overlay.filter(|open| open.pane == pane) {
            if open.theater || open.fullscreen {
                let surface = ws.renderer.as_ref()?.surface_size();
                let rect = (0.0, 0.0, surface.0 as f32, surface.1 as f32);
                return if open.fullscreen {
                    Some((rect, false))
                } else {
                    crate::media::centered_preview_rect(rect).map(|rect| (rect, false))
                };
            }
            return crate::media::centered_preview_rect(partition.terminal)
                .map(|rect| (rect, false));
        }
        let share = partition.lane?;
        Some((
            share.rect()?,
            matches!(share, crate::pane_partition::LaneShare::Strip(_)),
        ))
    }

    /// `pane`'s lane in `rect`, projected for painting.
    pub(super) fn preview_lane_panel(
        &self,
        ws: &WindowState,
        pane: u64,
        rect: Rect,
        collapsed: bool,
    ) -> Option<kettle_render::MediaLanePanel> {
        let panel = ws.preview_panels.get(&pane)?;
        let items = ws.mux.panes.get(&pane)?.media_shelf.items();
        let index = items.iter().position(|item| item.id == panel.item)?;
        let item = &items[index];
        let tr = &self.ui_text;
        let (kind, sender) = self.media_item_kind_and_sender(item);
        let text = item.source.text();
        let mode = lane_mode(item, panel);
        let frame = lane_frame(
            item,
            panel,
            ws.lane_previews.get(&pane),
            ws.lane_playbacks.get(&pane),
        );
        let copy_image = frame.is_some() || item.image().is_some();
        let mut lane = kettle_render::MediaLanePanel {
            focused_control: None,
            pin_label: ws
                .preview_overlay
                .filter(|open| open.pane == pane)
                .map(|_| tr.text(kettle_i18n::Text::MediaViewerPinShort).to_string()),
            play: None,
            play_feedback: None,
            sound: None,
            frame,
            scrubber: None,
            pane,
            rect,
            collapsed,
            title: item.title.clone(),
            detail: media_item_detail(*tr, kind, item),
            sender,
            // Where keys go: the terminal, or this lane while it holds the
            // keyboard.
            hint: tr
                .text(
                    if ws.preview_overlay.is_some_and(|open| open.pane == pane) {
                        if item.kind.media() == Some(kettle_media::MediaKind::Video) {
                            kettle_i18n::Text::MediaViewerVideoHint
                        } else {
                            kettle_i18n::Text::MediaViewerOverlayHint
                        }
                    } else if ws.preview_focus.is_some_and(|focus| focus.pane == pane) {
                        kettle_i18n::Text::MediaLaneHintFocused
                    } else {
                        kettle_i18n::Text::MediaLaneHint
                    },
                )
                .to_string(),
            position: (index + 1, items.len()),
            // The page asked for and not yet shown, else the one shown: where
            // a step counts from.
            page: item.source.shown_page().map(|(shown, count)| {
                let asked = page_waited_on(panel, item.generation).map_or(shown, |ask| ask.page);
                (
                    usize::try_from(asked).map_or(usize::MAX, |asked| asked.saturating_add(1)),
                    count,
                )
            }),
            document: item.source.is_document(),
            image: item.image().cloned(),
            // What the lane says where no picture is: why a file was not
            // drawn, or that pixels were let go.
            status: tr
                .text(match item.kind.file().map(|facts| facts.why) {
                    Some(crate::media::FileWhy::Unsupported) => {
                        kettle_i18n::Text::MediaViewerFileUnsupported
                    }
                    Some(crate::media::FileWhy::NoDecoder) => {
                        kettle_i18n::Text::MediaViewerFileNoDecoder
                    }
                    Some(crate::media::FileWhy::TooLarge) => {
                        kettle_i18n::Text::MediaViewerFileTooLarge
                    }
                    None => kettle_i18n::Text::MediaViewerReleased,
                })
                .to_string(),
            open_outside: self.preview_opens_outside(ws, pane).is_some(),
            canvas: preview_canvas(item, panel),
            mode,
            source: text.map(|_| kettle_render::MediaLaneSource::default()),
            copy: match mode {
                kettle_render::MediaLaneMode::Rendered => copy_image,
                kettle_render::MediaLaneMode::Source => true,
            },
            notice: panel
                .notice
                .map(|notice| lane_notice_text(notice, tr).to_string()),
            reload: preview_reloads(item),
            view: panel.view,
            // Sharper pixels for this item as it is now, if the lane holds
            // them.
            // An expanded lane resizes by the edge it shares with its
            // terminal.
            edge: ws
                .mux
                .lane(pane)
                .filter(|_| !collapsed)
                .map(|lane| match lane.side {
                    crate::pane_partition::LaneSide::Bottom => kettle_render::MediaLaneEdge::Top,
                    crate::pane_partition::LaneSide::Right => kettle_render::MediaLaneEdge::Left,
                }),
            tile: ws
                .lane_crops
                .get(&pane)
                .and_then(|crop| crop.tile.as_ref())
                .filter(|tile| tile.serves(item.id, item.generation, item.source.spec.canvas))
                .map(|tile| kettle_render::MediaLaneTile {
                    image: tile.image.clone(),
                    coverage: tile.coverage,
                }),
        };
        // A video's silent preview: its button, its frame in the poster's
        // place while it plays, and what it is doing in the footer.
        if lane_previews_item(item, panel).is_some() {
            let preview = ws
                .lane_previews
                .get(&pane)
                .filter(|preview| preview.serves(item.id, item.generation));
            lane.play = Some(match preview {
                Some(preview) if preview.active() => kettle_render::MediaLanePlay::Stop,
                _ => kettle_render::MediaLanePlay::Play,
            });
            if let Some(preview) = preview.filter(|preview| preview.active()) {
                match preview.frame() {
                    Some(frame) => {
                        lane.notice = Some(tr.media_lane_notice_preview_playing(
                            &kettle_render::duration_label(frame.at_ms),
                            &kettle_render::duration_label(frame.length_ms),
                        ));
                    }
                    None => {
                        lane.notice = Some(
                            tr.text(kettle_i18n::Text::MediaLaneNoticePreviewLoading)
                                .to_string(),
                        );
                    }
                }
            }
        }
        if let Some(info) = item
            .video
            .filter(|info| lane.play.is_some() && info.duration_ms > 0)
        {
            lane.scrubber = Some(kettle_render::MediaLaneScrubber {
                seekable: false,
                position_ms: 0,
                duration_ms: info.duration_ms,
                label: tr.media_lane_video_time(
                    &kettle_render::duration_label(0),
                    &kettle_render::duration_label(info.duration_ms),
                ),
            });
        }
        // A video playing in its lane, paused in place, or stopped where it
        // stood, while no silent preview plays: its frame in the poster's
        // place, its bar, and what it does in the footer.
        let previewing = ws
            .lane_previews
            .get(&pane)
            .is_some_and(|preview| preview.serves(item.id, item.generation) && preview.active());
        if lane_previews_item(item, panel).is_some()
            && !previewing
            && let Some(playback) = ws
                .lane_playbacks
                .get(&pane)
                .filter(|playback| playback.serves(item.id, item.generation))
        {
            lane.play = Some(match playback.button() {
                crate::media::PlaybackButton::Pause => kettle_render::MediaLanePlay::Pause,
                crate::media::PlaybackButton::Play => kettle_render::MediaLanePlay::Play,
            });
            // Only sound that can play has a button.
            lane.sound = match playback.sound() {
                Some(crate::media::PlaybackSound::Heard) => {
                    Some(kettle_render::MediaLaneSound::Heard)
                }
                Some(crate::media::PlaybackSound::Muted) => {
                    Some(kettle_render::MediaLaneSound::Muted)
                }
                Some(crate::media::PlaybackSound::Silent(_)) | None => None,
            };
            let (at, length) = (playback.position_ms(), playback.duration_ms());
            lane.scrubber = Some(kettle_render::MediaLaneScrubber {
                seekable: true,
                position_ms: at,
                duration_ms: length,
                label: tr.media_lane_video_time(
                    &kettle_render::duration_label(at),
                    &kettle_render::duration_label(length),
                ),
            });
            match playback.notice() {
                Some(crate::media::PlaybackNotice::Opening) => {
                    lane.notice = Some(
                        tr.text(kettle_i18n::Text::MediaLaneNoticeVideoLoading)
                            .to_string(),
                    );
                }
                Some(crate::media::PlaybackNotice::Muted) => {
                    lane.notice = Some(
                        tr.text(kettle_i18n::Text::MediaLaneNoticeVideoMuted)
                            .to_string(),
                    );
                }
                Some(crate::media::PlaybackNotice::Silent(why)) => {
                    lane.notice = Some(tr.text(silence_text(why)).to_string());
                }
                Some(crate::media::PlaybackNotice::Failed(failure)) => {
                    lane.notice =
                        Some(lane_notice_text(playback_failure_notice(failure), tr).to_string());
                }
                None => {}
            }
        }
        // The source rows in view, for the room the lane has: only those are
        // read out of the text.
        if mode == kettle_render::MediaLaneMode::Source
            && text.is_some()
            && let Some(geometry) = self.preview_lane_geometry(ws, &lane)
        {
            let glyph = self.overlay_text_cell_width(ws).max(1.0);
            let columns = (geometry.image_area.2 / glyph).ceil() as usize + 1;
            lane.source = Some(lane_source(
                &item.source,
                panel.scroll,
                geometry.source_rows,
                columns,
            ));
        }
        if let Some(control) = ws
            .preview_overlay
            .filter(|open| open.pane == pane)
            .and_then(|open| open.control)
            && let Some(geometry) = self.preview_lane_geometry(ws, &lane)
        {
            lane.focused_control = lane_interactive_controls(&lane, &geometry)
                .into_iter()
                .find(|(part, _, _)| *part == control)
                .map(|(_, rect, _)| rect);
        }
        lane.play_feedback = match lane.play {
            Some(kettle_render::MediaLanePlay::Play) => Some(kettle_render::MediaLanePlay::Play),
            Some(kettle_render::MediaLanePlay::Pause)
                if ws.lane_playbacks.get(&pane).is_some_and(|playback| {
                    playback.touched.elapsed() < std::time::Duration::from_millis(600)
                }) =>
            {
                Some(kettle_render::MediaLanePlay::Play)
            }
            _ => None,
        };
        Some(lane)
    }

    /// How the viewer and assistive technology describe a shelf item: its
    /// kind, and who sent it.
    pub(super) fn media_item_kind_and_sender(
        &self,
        item: &crate::media::ShelfItem,
    ) -> (&'static str, kettle_render::MediaLaneSender) {
        let tr = &self.ui_text;
        let kind = tr.text(match item.kind {
            crate::media::ItemKind::Media(kind) => match kind {
                kettle_media::MediaKind::Raster => kettle_i18n::Text::MediaViewerKindImage,
                kettle_media::MediaKind::Svg => kettle_i18n::Text::MediaViewerKindSvg,
                kettle_media::MediaKind::Mermaid => kettle_i18n::Text::MediaViewerKindMermaid,
                kettle_media::MediaKind::Video => kettle_i18n::Text::MediaViewerKindVideo,
                kettle_media::MediaKind::Markdown => kettle_i18n::Text::MediaViewerKindMarkdown,
                kettle_media::MediaKind::Pdf => kettle_i18n::Text::MediaViewerKindPdf,
            },
            crate::media::ItemKind::File(_) => kettle_i18n::Text::MediaViewerKindFile,
        });
        let sender = match &item.provenance {
            crate::media::Provenance::Verified => kettle_render::MediaLaneSender {
                text: tr.text(kettle_i18n::Text::MediaViewerFromPane).to_string(),
                program: None,
                signer: None,
            },
            crate::media::Provenance::User => kettle_render::MediaLaneSender {
                text: tr.text(kettle_i18n::Text::MediaViewerFromUser).to_string(),
                program: None,
                signer: None,
            },
            crate::media::Provenance::Unverified(sender) => {
                let pid = u64::from(sender.pid);
                let line = |program: &str, signer: Option<&str>| match signer {
                    Some(signer) => tr.media_viewer_unverified_signed(program, pid, signer),
                    None => tr.media_viewer_unverified(program, pid),
                };
                let signer = sender.signer.as_deref();
                match sender.executable.as_deref() {
                    Some(path) => media_viewer_sender_line(line, path, signer),
                    None => media_viewer_sender_line(
                        line,
                        tr.text(kettle_i18n::Text::MediaViewerUnknownProgram),
                        signer,
                    ),
                }
            }
        };
        (kind, sender)
    }

    /// Where `lane`'s controls are.
    pub(super) fn preview_lane_geometry(
        &self,
        ws: &WindowState,
        lane: &kettle_render::MediaLanePanel,
    ) -> Option<kettle_render::MediaLaneGeometry> {
        kettle_render::media_lane_geometry_scaled(
            lane,
            self.menu_cell(ws),
            (
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            ),
            ws.window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32),
        )
    }

    /// `pane`'s lane geometry as it shows in the active tab, when it has
    /// room to draw.
    pub(super) fn preview_diagnostics(
        &self,
        ws: &WindowState,
        pane: u64,
        rect: Option<Rect>,
        collapsed: bool,
        state: &str,
    ) -> serde_json::Value {
        let mut lane = serde_json::json!({
            "pane": pane,
            "rect": rect.map(rect_json),
            "state": state,
        });
        if let Some(open) = ws.preview_overlay.filter(|open| open.pane == pane) {
            lane["presentation"] = if open.fullscreen {
                "fullscreen"
            } else if open.theater {
                "theater"
            } else {
                "pane"
            }
            .into();
        }
        // What the lane shows and where its controls are: names and
        // rectangles only, never a title, a source or pixels.
        if let Some(panel) =
            rect.and_then(|rect| self.preview_lane_panel(ws, pane, rect, collapsed))
        {
            lane["mode"] = match panel.mode {
                kettle_render::MediaLaneMode::Rendered => "rendered",
                kettle_render::MediaLaneMode::Source => "source",
            }
            .into();
            lane["canvas"] = match panel.canvas {
                kettle_render::MediaCanvas::Theme => "theme",
                kettle_render::MediaCanvas::White => "white",
                kettle_render::MediaCanvas::Checker => "checker",
            }
            .into();
            lane["zoom"] = panel.view.zoom().into();
            if let Some(geometry) = self.preview_lane_geometry(ws, &panel) {
                // Where the content is and the item is drawn in it:
                // rectangles, never pixels.
                lane["content"] = rect_json(geometry.content);
                lane["image"] = geometry.image.map(rect_json).into();
                lane["sharp"] = geometry.tile.map(rect_json).into();
                lane["edge"] = geometry.resize.map(rect_json).into();
                lane["transport"] = geometry.transport.map(rect_json).into();
                lane["timeline"] = geometry.transport_timeline.map(rect_json).into();
                lane["time_label"] = geometry.scrub_label.map(rect_json).into();
                lane["scrubbing"] = ws
                    .lane_drag
                    .is_some_and(|drag| drag.pane == pane && drag.scrub)
                    .into();
                lane["hovered_control"] = ws
                    .native_pointer
                    .filter(|_| !self.non_search_modal_open(ws))
                    .and_then(|p| geometry.hit_test(p.x as f32, p.y as f32))
                    .filter(|hit| {
                        matches!(
                            hit,
                            kettle_render::MediaLaneHit::Play
                                | kettle_render::MediaLaneHit::Sound
                                | kettle_render::MediaLaneHit::Scrub
                                | kettle_render::MediaLaneHit::Theater
                                | kettle_render::MediaLaneHit::Fullscreen
                        )
                    })
                    .map(lane_control_name)
                    .into();
                lane["keyboard"] = if ws.preview_focus.is_some_and(|focus| focus.pane == pane) {
                    "preview"
                } else {
                    "terminal"
                }
                .into();
                let mut controls: serde_json::Map<String, serde_json::Value> =
                    lane_controls(&geometry)
                        .into_iter()
                        .filter_map(|(_, rect, hit)| {
                            Some((lane_control_name(hit).to_string(), rect_json(rect?)))
                        })
                        .collect();
                if let Some(bar) = geometry.scrubber {
                    controls.insert(
                        lane_control_name(kettle_render::MediaLaneHit::Scrub).to_string(),
                        rect_json(bar),
                    );
                }
                lane["controls"] = controls.into();
            }
            // A video playing in the lane, paused in place, or
            // stopped where it stood: where it is, never a frame.
            if let Some(bar) = panel.scrubber.as_ref().filter(|bar| bar.seekable) {
                let state = match (
                    panel.play,
                    ws.lane_playbacks
                        .get(&pane)
                        .is_some_and(crate::media::LanePlayback::has_session),
                ) {
                    (Some(kettle_render::MediaLanePlay::Pause), _) => "playing",
                    (_, true) => "paused",
                    (_, false) => "stopped",
                };
                let sound = ws
                    .lane_playbacks
                    .get(&pane)
                    .and_then(crate::media::LanePlayback::sound)
                    .map(playback_sound_name);
                lane["playback"] = serde_json::json!({
                    "state": state,
                    "position_ms": bar.position_ms,
                    "duration_ms": bar.duration_ms,
                    "frame": panel.frame.is_some(),
                    "sound": sound,
                });
            }
        }
        lane
    }

    pub(super) fn preview_panel_of(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Option<kettle_render::MediaLanePanel> {
        let partition = self
            .pane_layout(ws, self.area(ws))
            .into_iter()
            .find(|(id, _)| *id == pane)?
            .1;
        let (rect, collapsed) = self.preview_placement(ws, pane, partition)?;
        self.preview_lane_panel(ws, pane, rect, collapsed)
    }

    pub(super) fn preview_lane_geometry_of(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Option<kettle_render::MediaLaneGeometry> {
        let lane = self.preview_panel_of(ws, pane)?;
        self.preview_lane_geometry(ws, &lane)
    }

    pub(super) fn shown_preview_controls(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Vec<(u64, Rect, kettle_render::MediaLaneHit)> {
        let Some(lane) = self.preview_panel_of(ws, pane) else {
            return Vec::new();
        };
        self.preview_lane_geometry(ws, &lane)
            .map(|geometry| lane_interactive_controls(&lane, &geometry))
            .unwrap_or_default()
    }

    pub(super) fn preview_is_reachable(&self, ws: &WindowState, pane: u64) -> bool {
        !self.non_search_modal_open(ws) && ws.preview_overlay.is_none_or(|open| open.pane == pane)
    }

    /// The lane under `(x, y)` in the active tab, with its pane and whether
    /// it shows as a strip: the whole lane, whether or not its panel has room
    /// to draw.
    pub(super) fn preview_lane_at(
        &self,
        ws: &WindowState,
        x: f32,
        y: f32,
    ) -> Option<(u64, Rect, bool)> {
        self.pane_layout(ws, self.area(ws))
            .into_iter()
            .find_map(|(pane, partition)| {
                if ws
                    .preview_overlay
                    .is_some_and(|overlay| overlay.pane != pane)
                {
                    return None;
                }
                let (rect, strip) = self.preview_placement(ws, pane, partition)?;
                (x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3)
                    .then_some((pane, rect, strip))
            })
    }

    /// Whether the wheel at the pointer is a preview lane's: over a lane that
    /// nothing covers, so the terminal beside it never scrolls from a wheel
    /// over the lane. A menu or dialog open over it takes the wheel instead.
    pub(super) fn lane_takes_wheel(&self, ws: &WindowState) -> bool {
        ws.context_menu.is_none()
            && !self.non_search_modal_open(ws)
            && (ws.preview_overlay.is_some() || self.preview_lane_under_pointer(ws))
    }

    pub(super) fn preview_lane_under_pointer(&self, ws: &WindowState) -> bool {
        self.preview_lane_at(ws, ws.cursor.x as f32, ws.cursor.y as f32)
            .is_some()
    }

    /// Keep presses in the preview: left operates controls, right opens its menu.
    pub(super) fn press_preview_lane(
        &mut self,
        ws: &mut WindowState,
        x: f32,
        y: f32,
        bcode: u8,
    ) -> bool {
        let Some((pane, rect, collapsed)) = self.preview_lane_at(ws, x, y) else {
            if bcode == 0
                && let Some(open) = ws.preview_overlay
            {
                self.close_preview(ws, open.pane);
                return true;
            }
            return ws.preview_overlay.is_some();
        };
        if bcode == 2 {
            self.open_preview_menu(ws, pane, Some((x, y)));
            return true;
        }
        if bcode != 0 {
            return true;
        }
        if let Some(open) = &mut ws.preview_overlay {
            open.control = None;
        }
        let geometry = self
            .preview_lane_panel(ws, pane, rect, collapsed)
            .and_then(|lane| self.preview_lane_geometry(ws, &lane));
        let hit = geometry
            .as_ref()
            .and_then(|geometry| geometry.hit_test(x, y));
        // A press on a playing video's bar moves it there, and a drag
        // along the bar after it, until the release opens the video there.
        if hit == Some(kettle_render::MediaLaneHit::Scrub)
            && let Some(fraction) = geometry.as_ref().and_then(|geometry| geometry.scrub_at(x))
            && let Some(playback) = ws.lane_playbacks.get_mut(&pane)
        {
            playback.scrub(fraction, std::time::Instant::now());
            let at = (f64::from(x), f64::from(y));
            ws.lane_drag = Some(crate::window_state::LaneDrag {
                pane,
                from: at,
                last: at,
                panning: false,
                scrub: true,
            });
            self.lanes_changed(ws, false);
            return true;
        }
        // A press on the lane's edge drags it.
        if hit == Some(kettle_render::MediaLaneHit::Resize) {
            ws.lane_resize = Some(crate::window_state::LaneResize {
                pane,
                resized: std::time::Instant::now() - crate::window_state::LANE_RESIZE_PTY_INTERVAL,
                pending: false,
            });
            return true;
        }
        // A press on the content of a lane showing a rendered item may
        // become a drag that pans it.
        if hit == Some(kettle_render::MediaLaneHit::Inside)
            && let Some(geometry) = geometry
            && geometry.fit.is_some()
            && rect_contains(geometry.content, x, y)
        {
            let at = (f64::from(x), f64::from(y));
            ws.lane_drag = Some(crate::window_state::LaneDrag {
                pane,
                from: at,
                last: at,
                panning: false,
                scrub: false,
            });
        }
        if let Some(hit) = hit {
            self.act_on_preview(ws, pane, hit);
        }
        true
    }

    /// Do what a lane control does, from the pointer or assistive technology.
    pub(super) fn act_on_preview(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        hit: kettle_render::MediaLaneHit,
    ) {
        use kettle_render::MediaLaneHit as Hit;
        match hit {
            Hit::Close => self.close_preview(ws, pane),
            Hit::Toggle => self.toggle_preview(ws, pane),
            Hit::Pin => self.pin_preview(ws, pane),
            Hit::Previous => self.step_preview(ws, pane, -1),
            Hit::Next => self.step_preview(ws, pane, 1),
            Hit::PreviousPage => self.page_preview(ws, pane, crate::media::PageStep::By(-1)),
            Hit::NextPage => self.page_preview(ws, pane, crate::media::PageStep::By(1)),
            Hit::OpenOutside => self.open_preview_outside(ws, pane),
            Hit::Mode => self.switch_preview_mode(ws, pane),
            Hit::Canvas => self.next_preview_canvas(ws, pane),
            Hit::Copy => self.copy_preview(ws, pane),
            Hit::Reload => self.reload_preview(ws, pane),
            Hit::Play => self.toggle_lane_play(ws, pane),
            Hit::Sound => self.toggle_lane_sound(ws, pane),
            Hit::Theater => self.toggle_preview_theater(ws, pane),
            Hit::Fullscreen => self.toggle_preview_fullscreen(ws, pane),
            Hit::ZoomOut => self.zoom_preview(ws, pane, 1.0 / kettle_render::MEDIA_ZOOM_STEP),
            Hit::ZoomIn => self.zoom_preview(ws, pane, kettle_render::MEDIA_ZOOM_STEP),
            Hit::Fit => self.fit_preview(ws, pane),
            // The edge moves with a drag, and the bar with a press or a
            // drag, not a click.
            Hit::Resize | Hit::Scrub | Hit::Inside => {}
        }
    }
}
