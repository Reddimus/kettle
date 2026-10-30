//! Pasting text, files and screenshots, the paste queue, and the pasted-media receipt.

use super::*;

impl App {
    pub(super) fn media_paste_receipt_geometry(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::MediaPasteReceiptGeometry> {
        let (receipt, completion) = self.media_paste_receipt_projection(ws)?;
        kettle_render::notification_geometry(
            self.update_toast_projection(ws).map(|toast| toast.geometry),
            Some(&receipt),
            completion.as_ref(),
            self.menu_cell(ws),
            self.overlay_text_cell_width(ws),
            self.overlay_text_line_height(ws),
        )
        .receipt
    }

    pub(super) fn media_paste_receipt_hovered(&self, ws: &WindowState) -> bool {
        self.media_paste_receipt_geometry(ws)
            .is_some_and(|geometry| {
                rect_contains(geometry.rect, ws.cursor.x as f32, ws.cursor.y as f32)
            })
    }

    pub(super) fn update_media_paste_receipt_hover(&self, ws: &mut WindowState) -> bool {
        let hovered = self.media_paste_receipt_hovered(ws);
        let changed = ws
            .media_paste_receipt
            .as_mut()
            .is_some_and(|receipt| receipt.set_hover(hovered, std::time::Instant::now()));
        if changed {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        hovered
    }

    /// Consume presses over receipt chrome. A primary press opens or dismisses
    /// as appropriate; other buttons do nothing rather than reaching terminal
    /// content hidden behind the card.
    /// Shared by native and ctl mouse input so automation cannot click through.
    pub(super) fn activate_media_paste_receipt_at(
        &mut self,
        ws: &mut WindowState,
        x: f32,
        y: f32,
        bcode: u8,
    ) -> bool {
        let Some((receipt, completion)) = self.media_paste_receipt_projection(ws) else {
            return false;
        };
        let (pane_x, pane_y, pane_w, pane_h) = receipt.pane_rect;
        if receipt.right_gutter > 0.0
            && x >= pane_x + pane_w - receipt.right_gutter
            && x <= pane_x + pane_w
            && y >= pane_y
            && y <= pane_y + pane_h
        {
            return false;
        }
        let Some(geometry) = kettle_render::notification_geometry(
            self.update_toast_projection(ws).map(|toast| toast.geometry),
            Some(&receipt),
            completion.as_ref(),
            self.menu_cell(ws),
            self.overlay_text_cell_width(ws),
            self.overlay_text_line_height(ws),
        )
        .receipt
        else {
            return false;
        };
        let Some(hit) = geometry.hit_test(x, y) else {
            return false;
        };
        if bcode == 0 {
            if matches!(
                hit,
                kettle_render::MediaPasteReceiptHit::Dismiss
                    | kettle_render::MediaPasteReceiptHit::Body
            ) {
                ws.media_paste_receipt = None;
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            } else if hit == kettle_render::MediaPasteReceiptHit::Open {
                self.open_media_paste_receipt(ws);
            }
        }
        true
    }

    /// Preview the retained bitmap in its pane without changing the command line.
    /// Video receipts remain informational.
    pub(super) fn open_media_paste_receipt(&mut self, ws: &mut WindowState) {
        let Some(active) = ws.media_paste_receipt.as_ref().filter(|receipt| {
            ws.mux.active_focus() == Some(receipt.pane_id)
                && matches!(
                    &receipt.kind,
                    crate::window_state::MediaPasteReceiptKind::Image { .. }
                )
        }) else {
            return;
        };
        let pane = active.pane_id;
        let path = active.path.clone();
        if !self.pasted_images.path_still_matches(&path) {
            ws.media_paste_receipt = None;
            ws.accessibility_pending = true;
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleImagePreviewUnavailable),
                self.ui_text.text(kettle_i18n::Text::NotifyBodyImageGone),
            );
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        self.pull_preview(ws, pane, &path, None);
    }

    /// A receipt describes the command line at the instant Kettle pasted it.
    /// The next authored input can edit, submit, or cancel that line, so keeping
    /// the card would turn a receipt into stale success chrome. A later media
    /// paste installs its own fresh receipt after delivery.
    pub(super) fn dismiss_media_paste_receipt_on_input(ws: &mut WindowState) {
        drop(Self::take_media_paste_receipt_on_input(ws));
    }

    pub(super) fn take_media_paste_receipt_on_input(
        ws: &mut WindowState,
    ) -> Option<crate::window_state::MediaPasteReceiptState> {
        let (receipt, _) = Self::take_media_paste_state_on_input(ws);
        receipt
    }

    pub(super) fn take_media_paste_state_on_input(
        ws: &mut WindowState,
    ) -> (
        Option<crate::window_state::MediaPasteReceiptState>,
        Option<crate::window_state::PendingVideoPasteReceipt>,
    ) {
        let previous = ws.media_paste_receipt.take();
        let pending = ws.pending_video_paste_receipt.take();
        if previous.is_some() {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        (previous, pending)
    }

    fn dismiss_media_paste_state_for_input_pane(ws: &mut WindowState, pane_id: u64) {
        let receipt = ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.pane_id == pane_id);
        let pending = ws
            .pending_video_paste_receipt
            .as_ref()
            .is_some_and(|pending| pending.pane_id == pane_id);
        if receipt && pending {
            Self::dismiss_media_paste_receipt_on_input(ws);
            return;
        }
        if receipt {
            ws.media_paste_receipt = None;
        }
        if pending {
            ws.pending_video_paste_receipt = None;
        }
        if receipt || pending {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
    }

    pub(super) fn dismiss_media_paste_receipt_for_pane(
        &mut self,
        ws: &mut WindowState,
        pane_id: u64,
    ) {
        if ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.pane_id == pane_id)
            || ws
                .pending_video_paste_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.pane_id == pane_id)
        {
            Self::dismiss_media_paste_state_for_input_pane(ws, pane_id);
            return;
        }
        if let Some(window) = self.windows.values_mut().find(|window| {
            window
                .media_paste_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.pane_id == pane_id)
                || window
                    .pending_video_paste_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.pane_id == pane_id)
        }) {
            Self::dismiss_media_paste_state_for_input_pane(window, pane_id);
        }
    }

    pub(super) fn media_paste_receipt_owners(ws: &WindowState) -> crate::mux::PasteReceiptOwners {
        crate::mux::PasteReceiptOwners {
            receipt: ws
                .media_paste_receipt
                .as_ref()
                .map(|receipt| receipt.pane_id),
            pending: ws
                .pending_video_paste_receipt
                .as_ref()
                .map(|pending| pending.pane_id),
        }
    }

    pub(super) fn dismiss_media_paste_receipt_after_input(
        ws: &mut WindowState,
        pane_id: u64,
        result: PaneInputResult,
    ) {
        if result.is_queued() {
            Self::dismiss_media_paste_state_for_input_pane(ws, pane_id);
        }
    }

    pub(super) fn dismiss_media_paste_receipt_after_delivery(
        ws: &mut WindowState,
        delivery: PaneInputDelivery,
    ) {
        let owners = Self::media_paste_receipt_owners(ws);
        if delivery.receipt_accepted
            && let Some(pane_id) = owners.receipt
        {
            Self::dismiss_media_paste_state_for_input_pane(ws, pane_id);
        }
        if delivery.pending_accepted
            && let Some(pane_id) = owners.pending
        {
            Self::dismiss_media_paste_state_for_input_pane(ws, pane_id);
        }
    }

    pub(super) fn paste_clipboard(&mut self, ws: &mut WindowState) {
        // A copied file arrives as CF_HDROP or `text/uri-list`, not text. Paste
        // its quoted path and translate it for WSL. A text clipboard falls
        // through to the normal paste path below.
        if self.cfg.paste_files.enabled()
            && let Some(paths) = self.clipboard_file_paste_paths()
        {
            let video = self.cfg.paste_video_preview.then(|| {
                crate::video_preview::VideoPasteRequest::from_user_paths(
                    &paths,
                    crate::video_preview::VideoPasteSource::Clipboard,
                )
            });
            self.paste_paths(ws, paths, false, video.flatten());
            return;
        }
        // Bitmap paste: a screenshot puts raw pixels on the clipboard with no
        // file and no text behind it, so neither the file branch nor the text
        // path can see it. Materialize it as a PNG and paste the path, reusing
        // the same shell-quoting / WSL-translation pipeline. Ordered after the
        // file branch so a genuine copied file always wins.
        if self.cfg.paste_images.enabled() && self.paste_clipboard_image(ws) {
            return;
        }
        let text = self
            .clipboard
            .as_mut()
            .and_then(|c| c.get_text().ok())
            .unwrap_or_default();
        self.paste_text(ws, text);
    }

    /// Paste the clipboard's bitmap as the path of a temporary PNG. False
    /// when the clipboard holds no image, or it cannot be stored, so the
    /// paste falls through to text rather than silently doing nothing — the
    /// failure mode this whole path exists to remove.
    ///
    /// A large screenshot takes long enough to encode to stall the window,
    /// so the PNG is made on the encoder thread and the paste waits in
    /// [`App::queued_pastes`], with any made after it.
    pub(super) fn paste_clipboard_image(&mut self, ws: &mut WindowState) -> bool {
        let Some(image) = self.clipboard.as_mut().and_then(|c| c.get_image().ok()) else {
            return false;
        };
        // Read now: by the time the PNG is ready the clipboard may have changed.
        let fallback = self
            .clipboard
            .as_mut()
            .and_then(|c| c.get_text().ok())
            .filter(|text| local_paste_within_limit(text))
            .unwrap_or_default();
        if !self.paste_queue_admits(fallback.len()) {
            self.report_input_result(PaneInputResult::Oversize);
            return true;
        }
        let png = match self.pasted_images.reserve_png(
            image.width,
            image.height,
            image.bytes.len(),
            self.cfg.paste_image_preview,
        ) {
            Ok(png) => png,
            Err(error) => {
                log::warn!("clipboard image paste failed: {error}");
                return false;
            }
        };
        let id = self.png_jobs;
        self.png_jobs = self.png_jobs.wrapping_add(1);
        let rgba = image.bytes.into_owned();
        let submitted = match self.png_encoder() {
            Some(encoder) => encoder.submit(id, png, rgba),
            None => Err((png, rgba)),
        };
        if let Err((png, rgba)) = submitted {
            // No encoder thread: encode here, as a paste always did once.
            let encoded = png.encode(&rgba, &|| false);
            return match self.pasted_images.keep_png(encoded) {
                Ok(path) => {
                    self.paste_paths(ws, vec![path], false, None);
                    true
                }
                Err(error) => {
                    log::warn!("clipboard image paste failed: {error}");
                    false
                }
            };
        }
        self.queue_paste(
            ws,
            QueuedPasteContent::Image {
                id,
                fallback,
                path: None,
            },
        );
        true
    }

    /// The clipboard-image encoder, started on first use.
    pub(super) fn png_encoder(&mut self) -> Option<&crate::paste_image::PngEncoder> {
        if self.png_encoder.is_none() {
            let proxy = self.proxy.clone();
            match crate::paste_image::PngEncoder::new(Box::new(move || {
                let _ = proxy.send_event(UserEvent::PastedImageEncoded);
            })) {
                Ok(encoder) => self.png_encoder = Some(encoder),
                Err(error) => log::warn!("clipboard image encoder could not start: {error}"),
            }
        }
        self.png_encoder.as_ref()
    }

    pub(super) fn defer_terminal_input(
        &mut self,
        ws: &mut WindowState,
        event: Option<&winit::event::KeyEvent>,
        bytes: &[u8],
        prepare_input: bool,
        focused_only: bool,
    ) -> bool {
        if self.queued_pastes.is_empty() {
            return false;
        }
        let recipients = self.broadcast_recipients(ws);
        // A key release goes to the panes its press reached, as one sent at
        // once does, even if focus or the broadcast scope moved since.
        let pressed = event
            .filter(|_| !prepare_input)
            .and_then(|event| ws.terminal_key_recipients.remove(&event.physical_key));
        let ids: Vec<u64> = if let Some(pressed) = pressed {
            pressed.into_iter().collect()
        } else if ws.mux.is_broadcast_on() && !focused_only {
            recipients.clone()
        } else {
            ws.mux.active_focus().into_iter().collect()
        };
        let mut release_bytes = Vec::new();
        let targets: Vec<(u64, Vec<u8>)> = ids
            .into_iter()
            .filter_map(|id| {
                let pane = ws.mux.panes.get(&id).or_else(|| {
                    self.windows
                        .values()
                        .find_map(|other| other.mux.panes.get(&id))
                })?;
                let bytes = if let Some(event) = event {
                    let mods = pty_key_modifiers(ws, &event.logical_key);
                    let mode = pane.effective_key_mode(
                        self.cfg.modify_other_keys,
                        key_is_modified_enter(&event.logical_key, mods),
                    );
                    if prepare_input
                        && let Some(release) = key_release_bytes(&self.cfg, event, mods, mode)
                    {
                        release_bytes.push(((event.physical_key, id), release));
                    }
                    encode_key_event_for_mode(&self.cfg, event, mods, mode)?
                } else {
                    bytes.to_vec()
                };
                (!bytes.is_empty()).then_some((id, bytes))
            })
            .collect();
        let reached: Vec<u64> = targets.iter().map(|(id, _)| *id).collect();
        release_bytes.retain(|((_, id), _)| reached.contains(id));
        let release_bytes: Vec<_> = release_bytes
            .into_iter()
            .map(|(key, bytes)| {
                let release = ws
                    .terminal_key_release_bytes
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| TerminalKeyRelease::new(bytes, false));
                (key, release)
            })
            .collect();
        let paste = QueuedPaste {
            key_presses: release_bytes
                .iter()
                .map(|((_, id), release)| (*id, release.clone()))
                .collect(),
            key_releases: Vec::new(),
            window: ws.seq,
            target: paste_target(&ws.mux),
            receipt_pane: ws.mux.active_focus(),
            broadcast: ws.mux.broadcast.clone(),
            recipients,
            content: QueuedPasteContent::Input {
                targets,
                prepare_input,
            },
        };
        let result = defer_authored_input(&mut self.queued_pastes, paste);
        // Keep the release with its press's admission state. Replay may
        // reject the press after policy or queue capacity changes.
        if let Some(event) = event.filter(|_| prepare_input)
            && result.is_some_and(|result| result.is_queued())
        {
            for (key, bytes) in release_bytes {
                ws.terminal_key_release_bytes.entry(key).or_insert(bytes);
            }
            ws.terminal_key_recipients
                .entry(event.physical_key)
                .or_default()
                .extend(reached);
        }
        if let Some(result) = result {
            self.report_input_result(result);
        }
        true
    }

    pub(super) fn write_focused_terminal_input(&mut self, ws: &mut WindowState, bytes: &[u8]) {
        if self.defer_terminal_input(ws, None, bytes, true, true) {
            return;
        }
        self.sync_pane_focus_reports(ws);
        if let Some(id) = ws.mux.active_focus()
            && let Some(pane) = ws.mux.panes.get(&id)
        {
            let result = pane.feed_input(bytes);
            self.report_input_result(result);
            Self::dismiss_media_paste_receipt_after_input(ws, id, result);
        }
    }

    pub(super) fn deliver_deferred_input(
        ws: &mut WindowState,
        targets: &[(u64, Vec<u8>)],
        key_presses: &[(u64, TerminalKeyRelease)],
        prepare_input: bool,
        scroll_to_bottom: bool,
    ) -> PaneInputResult {
        let mut result = PaneInputResult::Queued;
        for (id, bytes) in targets {
            let Some(pane) = ws.mux.panes.get(id) else {
                continue;
            };
            let delivered = pane.feed_input(bytes);
            if delivered.is_queued() && !bytes.is_empty() {
                for (_, release) in key_presses.iter().filter(|(pane, _)| pane == id) {
                    release.mark_accepted();
                }
            }
            if delivered.is_queued() && prepare_input {
                ws.last_typed = Some(std::time::Instant::now());
                if scroll_to_bottom && let Ok(mut term) = pane.term.term.lock() {
                    term.scroll_display(Scroll::Bottom);
                }
            }
            if prepare_input {
                Self::dismiss_media_paste_receipt_after_input(ws, *id, delivered);
            }
            result = result.merge(delivered);
        }
        result
    }

    /// Whether one more paste of `bytes` may wait in the queue.
    pub(super) fn paste_queue_admits(&self, bytes: usize) -> bool {
        let waiting = self
            .queued_pastes
            .iter()
            .map(|paste| paste.content.bytes())
            .fold(bytes, usize::saturating_add);
        self.queued_pastes.len() < QUEUED_PASTES_MAX && waiting <= QUEUED_PASTE_BYTES_MAX
    }

    /// Queue `content` to paste where a paste made now would go, once the
    /// images ahead of it are encoded. Refused when too much is waiting.
    pub(super) fn queue_paste(&mut self, ws: &WindowState, content: QueuedPasteContent) {
        if !matches!(content, QueuedPasteContent::Image { .. })
            && !self.paste_queue_admits(content.bytes())
        {
            self.discard_queued_paste(content);
            self.report_input_result(PaneInputResult::Oversize);
            return;
        }
        let recipients = self.broadcast_recipients(ws);
        self.queued_pastes.push_back(QueuedPaste {
            key_presses: Vec::new(),
            key_releases: Vec::new(),
            window: ws.seq,
            target: paste_target(&ws.mux),
            receipt_pane: ws.mux.active_focus(),
            broadcast: ws.mux.broadcast.clone(),
            recipients,
            content,
        });
    }

    /// Every pane, in this window and the others, that a broadcast from `ws`
    /// reaches now, in order. Empty with broadcast off. `ws` must be checked
    /// out of `self.windows`, as during dispatch.
    pub(super) fn broadcast_recipients(&self, ws: &WindowState) -> Vec<u64> {
        if !ws.mux.is_broadcast_on() {
            return Vec::new();
        }
        let mut panes = ws.mux.broadcast_target_ids();
        if Mux::scope_crosses_windows(&ws.mux.broadcast) {
            for other in self.windows.values() {
                panes.extend(other.mux.foreign_target_ids(&ws.mux.broadcast));
            }
        }
        panes.sort_unstable();
        panes.dedup();
        panes
    }

    /// Take the PNGs the encoder finished, then deliver the pastes at the
    /// front of the queue that are ready, in order.
    pub(super) fn drain_queued_pastes(&mut self, el: &ActiveEventLoop) {
        while let Some((id, encoded)) = self.png_encoder.as_ref().and_then(|e| e.try_recv()) {
            let kept = self.pasted_images.keep_png(encoded);
            let waiting =
                self.queued_pastes
                    .iter_mut()
                    .find_map(|paste| match &mut paste.content {
                        QueuedPasteContent::Image {
                            id: queued, path, ..
                        } if *queued == id => Some(path),
                        _ => None,
                    });
            match (waiting, kept) {
                (Some(path), kept) => *path = Some(kept),
                // Its paste is gone, so its PNG goes too.
                (None, Ok(path)) => {
                    let _ = self.pasted_images.remove(&path);
                }
                (None, Err(_)) => {}
            }
        }
        while let Some(paste) = pop_ready_paste(&mut self.queued_pastes) {
            self.deliver_queued_paste(el, paste);
        }
    }

    pub(super) fn deliver_queued_paste(&mut self, el: &ActiveEventLoop, paste: QueuedPaste) {
        let QueuedPaste {
            window,
            target,
            receipt_pane,
            broadcast,
            recipients,
            content,
            key_presses,
            key_releases,
        } = paste;
        // A single-pane paste follows its pane, which may have changed window.
        let window = target
            .and_then(|pane| {
                self.windows
                    .iter()
                    .find_map(|(&seq, ws)| ws.mux.panes.contains_key(&pane).then_some(seq))
            })
            .unwrap_or(window);
        let Some(mut ws) = self.windows.remove(&window) else {
            self.discard_queued_paste(content);
            self.send_key_releases(None, &key_releases);
            return;
        };
        // Broadcast goes where its scope reaches now. A different scope, or
        // the same scope over other panes (another tab, a group that changed),
        // could reach panes the paste was never meant for.
        if !matches!(content, QueuedPasteContent::Input { .. })
            && (ws.mux.broadcast != broadcast || self.broadcast_recipients(&ws) != recipients)
        {
            self.discard_queued_paste(content);
            self.send_key_releases(Some(&ws), &key_releases);
            self.finish_window_dispatch(el, window, ws);
            return;
        }
        self.sync_pane_focus_reports(&mut ws);
        match content {
            QueuedPasteContent::Image {
                path: Some(Ok(path)),
                ..
            } => self.paste_paths_to(&mut ws, target, receipt_pane, vec![path], false, None),
            QueuedPasteContent::Image {
                path: Some(Err(error)),
                fallback,
                ..
            } => {
                log::warn!("clipboard image paste failed: {error}");
                self.paste_text_to(&mut ws, target, &fallback);
            }
            QueuedPasteContent::Image { path: None, .. } => {}
            QueuedPasteContent::Text(text) => self.paste_text_to(&mut ws, target, &text),
            QueuedPasteContent::Input {
                targets,
                prepare_input,
            } => {
                let mut result = Self::deliver_deferred_input(
                    &mut ws,
                    &targets,
                    &key_presses,
                    prepare_input,
                    self.cfg.scroll_on_keystroke,
                );
                for other in self.windows.values_mut() {
                    result = result.merge(Self::deliver_deferred_input(
                        other,
                        &targets,
                        &key_presses,
                        prepare_input,
                        self.cfg.scroll_on_keystroke,
                    ));
                }
                self.report_input_result(result);
            }
            QueuedPasteContent::Paths {
                paths,
                trailing_space,
                video,
            } => self.paste_paths_to(&mut ws, target, receipt_pane, paths, trailing_space, video),
        }
        self.send_key_releases(Some(&ws), &key_releases);
        self.finish_window_dispatch(el, window, ws);
    }

    /// Let go of what a paste that will not be delivered holds.
    pub(super) fn discard_queued_paste(&mut self, content: QueuedPasteContent) {
        match content {
            QueuedPasteContent::Image {
                path: Some(Ok(path)),
                ..
            } => {
                self.pasted_images
                    .discard_previews_for_paths(std::slice::from_ref(&path));
                let _ = self.pasted_images.remove(&path);
            }
            QueuedPasteContent::Paths { paths, .. } => {
                self.pasted_images.discard_previews_for_paths(&paths);
            }
            QueuedPasteContent::Image { .. }
            | QueuedPasteContent::Text(_)
            | QueuedPasteContent::Input { .. } => {}
        }
    }

    /// Read a file list from the clipboard (CF_HDROP on Windows,
    /// `text/uri-list` elsewhere), or `None` if the clipboard holds no files.
    pub(super) fn clipboard_file_paste_paths(&mut self) -> Option<Vec<std::path::PathBuf>> {
        self.clipboard
            .as_mut()?
            .get()
            .file_list()
            .ok()
            .filter(|paths| !paths.is_empty())
    }

    /// Paste selected X11 text, falling back to the full clipboard pipeline
    /// when PRIMARY is empty or unavailable. Other platforms have one clipboard.
    pub(super) fn paste_primary(&mut self, ws: &mut WindowState) {
        #[cfg(target_os = "linux")]
        {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            let primary = self
                .clipboard
                .as_mut()
                .and_then(|c| c.get().clipboard(LinuxClipboardKind::Primary).text().ok())
                .filter(|text| !text.is_empty());
            if let Some(text) = primary {
                self.paste_text(ws, text);
                return;
            }
        }
        self.paste_clipboard(ws);
    }

    /// Shared paste path — size validation, broadcast scoping, bracketed-paste
    /// wrap, and stable single-pane targeting. Extracted so `paste_clipboard`
    /// and `paste_primary` (and any future paste channel) can't drift on the
    /// safety/scoping rules.
    pub(super) fn paste_text(&mut self, ws: &mut WindowState, text: String) {
        if !self.queued_pastes.is_empty() && !text.is_empty() && local_paste_within_limit(&text) {
            self.queue_paste(ws, QueuedPasteContent::Text(text));
            return;
        }
        let target = paste_target(&ws.mux);
        self.paste_text_to(ws, target, &text);
    }

    pub(super) fn paste_text_to(&mut self, ws: &mut WindowState, target: Option<u64>, text: &str) {
        if text.is_empty() {
            return;
        }
        // Reject rather than silently truncate. A truncated shell command,
        // patch, or encoded file can be more dangerous than no paste, and the
        // explicit notice tells the user to pass the data by file instead.
        if !local_paste_within_limit(text) {
            self.report_input_result(PaneInputResult::Oversize);
            return;
        }
        self.deliver_text_paste(ws, target, text);
    }

    pub(super) fn deliver_text_paste(
        &mut self,
        ws: &mut WindowState,
        target: Option<u64>,
        text: &str,
    ) {
        self.sync_pane_focus_reports(ws);
        if text.is_empty() {
            return;
        }
        // A paste is user input even when it comes from the pointer.
        self.reset_blink_phase(ws);
        // Broadcast owns a group scope, not one pane. It deliberately ignores
        // the optional single-pane target and retains its per-pane mode
        // encoding plus cross-window named-group fan-out.
        if ws.mux.is_broadcast_on() {
            if let Some(rec) = self.recorder.as_mut() {
                rec.record_marker(&format!("kettle:paste len={}", text.chars().count()));
            }
            let receipt_pane = Self::media_paste_receipt_owners(ws);
            let local = ws.mux.broadcast_paste_delivery(text, receipt_pane);
            Self::dismiss_media_paste_receipt_after_delivery(ws, local);
            let mut result = local.result;
            if crate::mux::Mux::scope_crosses_windows(&ws.mux.broadcast) {
                let scope = ws.mux.broadcast.clone();
                for other in self.windows.values_mut() {
                    let receipt_pane = Self::media_paste_receipt_owners(other);
                    let delivery =
                        other
                            .mux
                            .broadcast_paste_foreign_delivery(&scope, text, receipt_pane);
                    Self::dismiss_media_paste_receipt_after_delivery(other, delivery);
                    result = result.merge(delivery.result);
                }
            }
            self.report_input_result(result);
            return;
        }
        // A broadcast prompt carries no pane id. If its scope was disabled
        // while the modal was open, fail closed instead of choosing focus.
        if target.is_none_or(|id| !ws.mux.panes.contains_key(&id)) {
            return;
        }
        // Record that a paste happened and its length — NEVER the
        // pasted content (a common secret vector). The per-key hook captures
        // the Ctrl+V chord; this marker captures the size without the bytes.
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_marker(&format!("kettle:paste len={}", text.chars().count()));
        }
        if let Some(result) = paste_text_into_target(&mut ws.mux, target, text) {
            self.report_input_result(result);
            if let Some(pane_id) = target {
                Self::dismiss_media_paste_receipt_after_input(ws, pane_id, result);
            }
        }
    }

    pub(super) fn paste_paths(
        &mut self,
        ws: &mut WindowState,
        paths: Vec<std::path::PathBuf>,
        trailing_space: bool,
        video: Option<crate::video_preview::VideoPasteRequest>,
    ) {
        if paths.is_empty() {
            return;
        }
        if !self.queued_pastes.is_empty() {
            self.queue_paste(
                ws,
                QueuedPasteContent::Paths {
                    paths,
                    trailing_space,
                    video,
                },
            );
            return;
        }
        let target = paste_target(&ws.mux);
        let receipt_pane = ws.mux.active_focus();
        self.paste_paths_to(ws, target, receipt_pane, paths, trailing_space, video);
    }

    pub(super) fn paste_paths_to(
        &mut self,
        ws: &mut WindowState,
        target: Option<u64>,
        receipt_pane: Option<u64>,
        paths: Vec<std::path::PathBuf>,
        trailing_space: bool,
        video: Option<crate::video_preview::VideoPasteRequest>,
    ) {
        let argv = target
            .and_then(|pane| ws.mux.panes.get(&pane))
            .map_or_else(|| ws.mux.focused_argv(), |pane| pane.argv.clone());
        if !self.paths_type_exactly(ws, &argv, &paths) {
            self.pasted_images.discard_previews_for_paths(&paths);
            log::warn!(
                "paste: a file name cannot be typed exactly into a target shell; pasted nothing"
            );
            fire_notify(
                self.ui_text
                    .text(kettle_i18n::Text::NotifyTitleInputNotDelivered),
                self.ui_text.text(kettle_i18n::Text::NotifyBodyPathNotTyped),
            );
            return;
        }
        let mut preview = match crate::mux::format_paths_for_paste(&argv, &paths) {
            Ok(preview) => preview,
            Err(error) => {
                self.pasted_images.discard_previews_for_paths(&paths);
                self.report_input_result(error);
                return;
            }
        };
        if trailing_space {
            preview.push(' ');
        }
        let checked = (|| {
            if !ws.mux.is_broadcast_on() {
                return Ok(local_paste_within_limit(&preview));
            }
            if !ws.mux.broadcast_paste_paths_within_limit(
                &paths,
                trailing_space,
                LOCAL_PASTE_MAX,
            )? {
                return Ok(false);
            }
            if Mux::scope_crosses_windows(&ws.mux.broadcast) {
                for other in self.windows.values() {
                    if !other.mux.broadcast_paste_paths_foreign_within_limit(
                        &ws.mux.broadcast,
                        &paths,
                        trailing_space,
                        LOCAL_PASTE_MAX,
                    )? {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })();
        let rejection = match checked {
            Ok(true) => None,
            Ok(false) => Some(PaneInputResult::Oversize),
            Err(error) => Some(error),
        };
        if let Some(error) = rejection {
            self.pasted_images.discard_previews_for_paths(&paths);
            self.report_input_result(error);
            return;
        }

        self.deliver_path_paste(ws, target, receipt_pane, &paths, trailing_space, video);
    }

    /// Whether every path can be typed exactly into every pane the paste
    /// reaches: the target, or each pane broadcast reaches, in every window
    /// its scope crosses. See [`crate::mux::path_types_exactly`].
    fn paths_type_exactly(
        &self,
        ws: &WindowState,
        argv: &[String],
        paths: &[std::path::PathBuf],
    ) -> bool {
        let fits = |argv: &[String]| {
            paths
                .iter()
                .all(|path| crate::mux::path_types_exactly(argv, path))
        };
        if !ws.mux.is_broadcast_on() {
            return fits(argv);
        }
        let pane_fits = |mux: &Mux, id: u64| mux.panes.get(&id).is_none_or(|pane| fits(&pane.argv));
        ws.mux
            .broadcast_target_ids()
            .into_iter()
            .all(|id| pane_fits(&ws.mux, id))
            && (!Mux::scope_crosses_windows(&ws.mux.broadcast)
                || self.windows.values().all(|other| {
                    other
                        .mux
                        .foreign_target_ids(&ws.mux.broadcast)
                        .into_iter()
                        .all(|id| pane_fits(&other.mux, id))
                }))
    }

    pub(super) fn deliver_path_paste(
        &mut self,
        ws: &mut WindowState,
        target: Option<u64>,
        receipt_pane: Option<u64>,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        video: Option<crate::video_preview::VideoPasteRequest>,
    ) {
        self.sync_pane_focus_reports(ws);
        // Path delivery authors new terminal input. Keep the old receipt only
        // long enough to coalesce the separate DroppedFile events one OS drag
        // emits; every other outcome drops it here.
        let previous = Self::take_media_paste_state_on_input(ws);
        self.reset_blink_phase(ws);
        if ws.mux.is_broadcast_on() {
            if let Some(recorder) = self.recorder.as_mut() {
                recorder.record_marker(&format!("kettle:paste paths={}", paths.len()));
            }
            let scope = ws.mux.broadcast.clone();
            let mut delivery = ws.mux.broadcast_paste_paths_delivery(
                paths,
                trailing_space,
                LOCAL_PASTE_MAX,
                receipt_pane,
            );
            if Mux::scope_crosses_windows(&scope) {
                for other in self.windows.values_mut() {
                    let foreign =
                        Self::deliver_foreign_path_paste(other, &scope, paths, trailing_space);
                    delivery.result = delivery.result.merge(foreign.result);
                    delivery.accepted |= foreign.accepted;
                }
            }
            self.report_input_result(delivery.result);
            self.show_media_paste_receipt(
                ws,
                receipt_pane,
                paths,
                delivery.receipt_accepted,
                video,
                previous,
            );
            return;
        }

        if target.is_none_or(|id| !ws.mux.panes.contains_key(&id)) {
            self.show_media_paste_receipt(ws, receipt_pane, paths, false, video, previous);
            return;
        }
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.record_marker(&format!("kettle:paste paths={}", paths.len()));
        }
        match paste_paths_into_target(&mut ws.mux, target, paths, trailing_space) {
            Some(delivery) => {
                self.report_input_result(delivery.result);
                self.show_media_paste_receipt(
                    ws,
                    receipt_pane,
                    paths,
                    delivery.receipt_accepted,
                    video,
                    previous,
                );
            }
            None => self.show_media_paste_receipt(ws, receipt_pane, paths, false, video, previous),
        }
    }

    pub(super) fn deliver_foreign_path_paste(
        ws: &mut WindowState,
        scope: &crate::mux::BroadcastScope,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
    ) -> PaneInputDelivery {
        let receipt_pane = Self::media_paste_receipt_owners(ws);
        let delivery = ws.mux.broadcast_paste_paths_foreign_delivery(
            scope,
            paths,
            trailing_space,
            LOCAL_PASTE_MAX,
            receipt_pane,
        );
        Self::dismiss_media_paste_receipt_after_delivery(ws, delivery);
        delivery
    }

    pub(super) fn show_media_paste_receipt(
        &mut self,
        ws: &mut WindowState,
        receipt_pane: Option<u64>,
        paths: &[std::path::PathBuf],
        accepted_by_receipt_pane: bool,
        video: Option<crate::video_preview::VideoPasteRequest>,
        previous: (
            Option<crate::window_state::MediaPasteReceiptState>,
            Option<crate::window_state::PendingVideoPasteReceipt>,
        ),
    ) {
        let (previous_receipt, previous_pending) = previous;
        let image_preview = if paths.len() == 1 {
            // Move bounded pixels out even when presentation is rejected. A
            // managed PNG gets one chance to create a receipt.
            self.pasted_images.take_preview_for_path(&paths[0])
        } else {
            self.pasted_images.discard_previews_for_paths(paths);
            None
        };
        if !accepted_by_receipt_pane {
            ws.media_paste_receipt = previous_receipt;
            ws.pending_video_paste_receipt = previous_pending;
            if ws.media_paste_receipt.is_some() {
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            log::debug!("media paste receipt skipped: initiating pane did not accept the path");
            return;
        }
        let Some(pane_id) = receipt_pane.filter(|id| ws.mux.panes.contains_key(id)) else {
            log::debug!("media paste receipt skipped: initiating pane is no longer live");
            return;
        };
        let Some(pane) = ws.mux.panes.get(&pane_id) else {
            log::debug!("media paste receipt skipped: initiating pane lookup failed");
            return;
        };
        let remote = pane.remote_context.is_some();
        let prefer_top = pane.term.term.lock().ok().is_some_and(|term| {
            let cursor_row = term.renderable_content().cursor.point.line.0.max(0) as usize;
            cursor_row >= term.screen_lines() / 2
        });
        let now = std::time::Instant::now();
        let handled = if let Some(preview) = image_preview.filter(|_| self.cfg.paste_image_preview)
        {
            ws.media_paste_receipt = Some(crate::window_state::MediaPasteReceiptState::new_image(
                pane_id,
                paths[0].clone(),
                preview,
                remote,
                prefer_top,
                now,
            ));
            true
        } else if let Some(request) = video.filter(|_| self.cfg.paste_video_preview) {
            if let Some(mut pending) = previous_pending
                && pending.merge_drop(pane_id, &request, now)
            {
                ws.pending_video_paste_receipt = Some(pending);
                true
            } else {
                let generation = self.next_video_preview_generation;
                self.next_video_preview_generation = generation.wrapping_add(1).max(1);
                let queued =
                    self.video_previewer
                        .request(ws.seq, pane_id, generation, request.clone());
                if queued {
                    ws.pending_video_paste_receipt =
                        Some(crate::window_state::PendingVideoPasteReceipt {
                            pane_id,
                            generation,
                            request,
                            remote,
                            prefer_top,
                            created_at: now,
                            previous_receipt,
                        });
                }
                queued
            }
        } else {
            false
        };
        if !handled {
            log::debug!("media paste receipt skipped: no eligible preview payload");
            return;
        }
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        log::debug!("media paste receipt queued or shown for pane {pane_id}");
    }

    pub(super) fn media_paste_receipt_overlay_projection(
        &self,
        ws: &WindowState,
        terminal_surface: bool,
        now: std::time::Instant,
    ) -> Option<kettle_render::MediaPasteReceiptOverlay> {
        // The open media viewer hides the receipt until it closes: the viewer
        // owns every press, and the receipt's thumbnail and labels would
        // otherwise paint over it.
        if !terminal_surface || !ws.window_focused {
            return None;
        }
        let receipt = ws.media_paste_receipt.as_ref()?;
        let (kind, openable) = match &receipt.kind {
            crate::window_state::MediaPasteReceiptKind::Image {
                original_width,
                original_height,
            } if self.cfg.paste_image_preview => (
                kettle_render::MediaPasteReceiptKind::Image {
                    original_width: *original_width,
                    original_height: *original_height,
                },
                true,
            ),
            crate::window_state::MediaPasteReceiptKind::Video {
                candidate,
                preview_pending,
                ..
            } if self.cfg.paste_video_preview => (
                kettle_render::MediaPasteReceiptKind::Video {
                    extension: candidate.extension.clone(),
                    size: candidate.size,
                    count: candidate.count,
                    preview_pending: *preview_pending,
                },
                false,
            ),
            _ => return None,
        };
        let pane_id = ws.mux.active_focus()?;
        if pane_id != receipt.pane_id || !receipt.live(now) {
            return None;
        }
        let pane = ws.mux.panes.get(&pane_id)?;
        let (columns, rows, has_history) = {
            let term = pane.term.term.lock().ok()?;
            (
                term.columns(),
                term.screen_lines(),
                term.grid().history_size() > 0,
            )
        };
        let area = self.area(ws);
        let pane_rect = self.focused_terminal(ws, area)?;
        let grid_origin = (
            pane_rect.0 + self.cfg.padding_x,
            pane_rect.1 + self.cfg.padding_y,
        );
        let renderer = ws.renderer.as_ref()?;
        let scrollbar_present = self.cfg.scrollbar == kettle_config::ScrollbarMode::Always
            || (self.cfg.scrollbar != kettle_config::ScrollbarMode::Never && has_history);
        let right_gutter = if scrollbar_present {
            let scale = ws
                .window
                .as_ref()
                .map(|window| window.scale_factor() as f32)
                .unwrap_or(1.0);
            (self.cfg.scrollbar_width.clamp(2.0, 40.0) * scale).max(12.0 * scale)
        } else {
            0.0
        };
        Some(kettle_render::MediaPasteReceiptOverlay {
            tr: self.ui_text,
            pane_rect,
            grid_rect: (
                grid_origin.0,
                grid_origin.1,
                columns as f32 * renderer.cell_w,
                rows as f32 * renderer.cell_h,
            ),
            right_gutter,
            image: receipt.image.clone(),
            kind,
            openable,
            remote: receipt.remote,
            expanded: receipt.expanded(now),
            prefer_top: receipt.prefer_top,
        })
    }

    pub(super) fn media_paste_receipt_projection(
        &self,
        ws: &WindowState,
    ) -> Option<(
        kettle_render::MediaPasteReceiptOverlay,
        Option<kettle_render::CompletionOverlay>,
    )> {
        // This is the pointer-motion path. Keep the overwhelmingly common case
        // allocation- and lock-free, then project only the two overlays whose
        // geometry can affect the receipt rather than building every piece of
        // window chrome.
        ws.media_paste_receipt.as_ref()?;
        let terminal_surface = terminal_surface_available(ws);
        let receipt = self.media_paste_receipt_overlay_projection(
            ws,
            terminal_surface,
            std::time::Instant::now(),
        )?;
        let completion = self.completion_overlay_projection(ws, terminal_surface);
        Some((receipt, completion))
    }
}

/// Queue input typed while a paste ahead of it is still being prepared, so
/// it reaches the panes after that paste. `None` when nothing is waiting and
/// the input can go now. Input for the same panes as the entry before it
/// joins that entry, so a held key cannot fill the queue's entries.
pub(super) fn defer_authored_input(
    queue: &mut std::collections::VecDeque<QueuedPaste>,
    input: QueuedPaste,
) -> Option<PaneInputResult> {
    if queue.is_empty() {
        return None;
    }
    if !input.key_presses.is_empty() {
        let recipients = queue
            .iter()
            .filter(|paste| paste.window == input.window)
            .map(|paste| paste.key_presses.len())
            .fold(input.key_presses.len(), usize::saturating_add);
        if recipients > MAX_OWED_KEY_RECIPIENTS {
            return Some(PaneInputResult::Oversize);
        }
    }
    let bytes = queue
        .iter()
        .map(|paste| paste.content.bytes())
        .fold(input.content.bytes(), usize::saturating_add);
    if bytes > QUEUED_PASTE_BYTES_MAX {
        return Some(PaneInputResult::Oversize);
    }
    if let (
        Some(QueuedPaste {
            content:
                QueuedPasteContent::Input {
                    targets: queued,
                    prepare_input: queued_prepare,
                },
            key_presses,
            key_releases,
            ..
        }),
        QueuedPasteContent::Input {
            targets,
            prepare_input,
        },
    ) = (queue.back_mut(), &input.content)
        && key_releases.is_empty()
        && input.key_releases.is_empty()
        && queued_prepare == prepare_input
        && queued.len() == targets.len()
        && queued.iter().zip(targets).all(|(a, b)| a.0 == b.0)
    {
        for (id, press) in &input.key_presses {
            if !key_presses.iter().any(|(kept, release)| {
                *kept == *id && Arc::ptr_eq(&release.press_accepted, &press.press_accepted)
            }) {
                key_presses.push((*id, press.clone()));
            }
        }
        for ((_, queued), (_, bytes)) in queued.iter_mut().zip(targets) {
            queued.extend_from_slice(bytes);
        }
        return Some(PaneInputResult::Queued);
    }
    if queue.len() >= QUEUED_PASTES_MAX {
        return Some(PaneInputResult::Oversize);
    }
    queue.push_back(input);
    Some(PaneInputResult::Queued)
}

const MAX_OWED_KEY_RECIPIENTS: usize = 1024;

/// Releases take no new queue slot or byte admission: their presses already
/// reserved the obligation. The tail fences later input behind each release.
pub(super) fn defer_key_releases(
    queue: &mut std::collections::VecDeque<QueuedPaste>,
    releases: &mut Vec<(u64, PhysicalKey, TerminalKeyRelease)>,
) -> bool {
    if let Some(paste) = queue.back_mut() {
        paste.key_releases.append(releases);
        true
    } else {
        false
    }
}

pub(super) fn pop_ready_paste(
    queue: &mut std::collections::VecDeque<QueuedPaste>,
) -> Option<QueuedPaste> {
    if queue.front().is_some_and(|paste| !paste.content.encoding()) {
        queue.pop_front()
    } else {
        None
    }
}

/// A paste made while a clipboard image was still being encoded. It waits
/// its turn so pastes land in the order they were made.
pub(super) struct QueuedPaste {
    pub(super) window: u64,
    pub(super) target: Option<u64>,
    pub(super) receipt_pane: Option<u64>,
    pub(super) broadcast: crate::mux::BroadcastScope,
    /// The panes a broadcast paste reaches, every window's, when it was made.
    pub(super) recipients: Vec<u64>,
    pub(super) content: QueuedPasteContent,
    pub(super) key_presses: Vec<(u64, TerminalKeyRelease)>,
    pub(super) key_releases: Vec<(u64, PhysicalKey, TerminalKeyRelease)>,
}

pub(super) enum QueuedPasteContent {
    /// The PNG of encoder job `id`, once it is kept or has failed, and the
    /// clipboard's text to paste instead if it failed.
    Image {
        id: u64,
        fallback: String,
        path: Option<std::io::Result<std::path::PathBuf>>,
    },
    Text(String),
    Input {
        targets: Vec<(u64, Vec<u8>)>,
        prepare_input: bool,
    },
    Paths {
        paths: Vec<std::path::PathBuf>,
        trailing_space: bool,
        video: Option<crate::video_preview::VideoPasteRequest>,
    },
}

impl QueuedPasteContent {
    /// The bytes it holds while it waits.
    fn bytes(&self) -> usize {
        match self {
            Self::Image { fallback, .. } => fallback.len(),
            Self::Text(text) => text.len(),
            Self::Input { targets, .. } => targets
                .iter()
                .map(|(_, bytes)| bytes.len())
                .fold(0, usize::saturating_add),
            Self::Paths { paths, .. } => paths
                .iter()
                .map(|path| path.as_os_str().len())
                .fold(0, usize::saturating_add),
        }
    }

    /// Whether it is an image still being encoded.
    fn encoding(&self) -> bool {
        matches!(self, Self::Image { path: None, .. })
    }
}

/// Most pastes that may wait behind encoding images, and their text bytes.
pub(super) const QUEUED_PASTES_MAX: usize = 64;
pub(super) const QUEUED_PASTE_BYTES_MAX: usize = 4 * LOCAL_PASTE_MAX;

/// Terminator parity (`terminal.py:real_copy_clipboard` +
/// `config.py:smart_copy`): what `Action::Copy` writes to the clipboard.
///
///   * `selection`: the selected text, or `None` when the pane has none.
///   * `smart_copy`: `cfg.smart_copy`. With no selection, `true` (default)
///     keeps the existing clipboard and `false` clobbers it with an empty
///     string (Terminator's deliberate-UX-choice mode).
///
/// Returns the text to write, or `None` to leave the clipboard untouched.
/// On the `Some("")` clobber path the caller writes the empty string but does
/// not count it as a real copy, so the `clear_select_on_copy` follow-up is
/// skipped (no selection existed to clear). Pure; unit-testable without a
/// clipboard fixture.
pub(super) fn copy_clipboard_decision(
    selection: Option<String>,
    smart_copy: bool,
) -> Result<Option<String>, std::collections::TryReserveError> {
    match (selection, smart_copy) {
        (Some(s), _) => kettle_core::scrub_card_markers(s).map(Some),
        (None, false) => Ok(Some(String::new())),
        (None, true) => Ok(None),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PasteSource {
    Clipboard,
    Primary,
}

/// Terminator parity for `putty_paste_style_source_clipboard`: right-click
/// PuTTY paste defaults to the PRIMARY selection on Linux, matching terminal
/// mouse-paste convention. Users who expect Windows/PuTTY-style clipboard
/// paste can opt into the regular clipboard source.
pub(super) fn putty_paste_source(source_clipboard: bool) -> PasteSource {
    if source_clipboard {
        PasteSource::Clipboard
    } else {
        PasteSource::Primary
    }
}

/// 4 MiB limit on a *local* clipboard paste. The OSC 52 cap above guards
/// against a hostile remote program pushing an unbounded payload into
/// the system clipboard; this guards the reverse direction — a user
/// accidentally pastes a multi-GB file and kettle would otherwise feed
/// every byte into the PTY in one shot, freezing the terminal until
/// the program at the other end (cat? vim?) drained the pipe. 4 MiB
/// fits any realistic code-review / log-snippet paste with room to spare.
/// Larger payloads are rejected with visible feedback rather than truncated,
/// since silently shortening a command, patch, or encoded file is unsafe.
const LOCAL_PASTE_MAX: usize = 4 << 20;

pub(super) fn local_paste_within_limit(text: &str) -> bool {
    text.len() <= LOCAL_PASTE_MAX
}

/// The pane a single-pane paste goes to: the focused one. Broadcast names no
/// pane and keeps its live group semantics.
fn paste_target(mux: &Mux) -> Option<u64> {
    if mux.is_broadcast_on() {
        None
    } else {
        mux.active_focus()
    }
}

/// Apply a terminal control, such as Reset's RIS or Clear Scrollback's
/// CSI 3 J, to each target pane's terminal.
///
/// Nothing is written to the child, so read-only panes are included. This is
/// VTE's model, which Terminator's Reset uses: `vte_terminal_reset` changes the
/// terminal and ignores `input-enabled`.
pub(super) fn apply_terminal_control(mux: &Mux, targets: &[u64], bytes: &[u8]) {
    for id in targets {
        if let Some(pane) = mux.panes.get(id)
            && !pane.term.apply_local_output(bytes)
        {
            log::debug!("pane {id}: terminal control dropped; its reader is busy or stopped");
        }
    }
}

/// Deliver a confirmed non-broadcast text paste to the pane named when the
/// prompt opened. Mode lookup and delivery use the same stable id.
fn paste_text_into_target(
    mux: &mut Mux,
    target: Option<u64>,
    text: &str,
) -> Option<PaneInputResult> {
    let pane = mux.panes.get_mut(&target?)?;
    let bracketed = pane
        .term
        .term
        .lock()
        .ok()
        .map(|term| term.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
        .unwrap_or(false);
    let bytes = input::paste_payload(text, bracketed);
    Some(pane.feed_input(&bytes))
}

/// Deliver a confirmed non-broadcast path paste to the pane named when the
/// prompt opened. Shell formatting, mode lookup, and delivery all use it.
pub(super) fn paste_paths_into_target(
    mux: &mut Mux,
    target: Option<u64>,
    paths: &[std::path::PathBuf],
    trailing_space: bool,
) -> Option<PaneInputDelivery> {
    let pane = mux.panes.get_mut(&target?)?;
    let mut text = match crate::mux::format_paths_for_paste(&pane.argv, paths) {
        Ok(text) => text,
        Err(error) => return Some(PaneInputDelivery::rejected(error)),
    };
    if trailing_space {
        text.push(' ');
    }
    let bracketed = pane
        .term
        .term
        .lock()
        .ok()
        .map(|term| term.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
        .unwrap_or(false);
    let bytes = input::paste_payload(&text, bracketed);
    let result = pane.feed_input(&bytes);
    Some(PaneInputDelivery {
        result,
        accepted: result.is_queued(),
        receipt_accepted: result.is_queued(),
        pending_accepted: false,
    })
}

/// The window waiting for this pane's video preview generation: the window
/// that requested it, or the one its tab has moved to since.
pub(super) fn video_preview_target_window(
    windows: &std::collections::BTreeMap<u64, WindowState>,
    hint: u64,
    pane: u64,
    generation: u64,
) -> Option<u64> {
    if windows
        .get(&hint)
        .is_some_and(|window| window.expects_video_preview(pane, generation))
    {
        return Some(hint);
    }
    windows.iter().find_map(|(&seq, window)| {
        window
            .expects_video_preview(pane, generation)
            .then_some(seq)
    })
}
