//! Pasting text, files and screenshots, the paste queue, and the pasted-media receipt.

use super::*;

/// Placement inputs shared by receipt activation and scrollback-pill precedence.
pub(super) struct MediaPasteReceiptPlacement {
    pub toast: Option<kettle_render::UpdateToastGeometry>,
    pub notices: Vec<kettle_render::UpdateToastOverlay>,
    pub receipt: kettle_render::MediaPasteReceiptOverlay,
    pub completion: Option<kettle_render::CompletionOverlay>,
    pub cell: (f32, f32),
    pub text_cell: (f32, f32),
}

impl MediaPasteReceiptPlacement {
    pub(super) fn geometry(&self) -> Option<kettle_render::MediaPasteReceiptGeometry> {
        kettle_render::notification_geometry_with_notices(
            self.toast,
            &self.notices,
            Some(&self.receipt),
            self.completion.as_ref(),
            self.cell,
            self.text_cell.0,
            self.text_cell.1,
        )
        .receipt
    }
}

impl App {
    pub(super) fn media_paste_receipt_geometry(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::MediaPasteReceiptGeometry> {
        let (receipt, completion) = self.media_paste_receipt_projection(ws)?;
        self.media_paste_receipt_placement(ws, receipt, completion)
            .geometry()
    }

    pub(super) fn media_paste_receipt_placement(
        &self,
        ws: &WindowState,
        receipt: kettle_render::MediaPasteReceiptOverlay,
        completion: Option<kettle_render::CompletionOverlay>,
    ) -> MediaPasteReceiptPlacement {
        MediaPasteReceiptPlacement {
            toast: self.update_toast_projection(ws).map(|toast| toast.geometry),
            notices: self
                .persistent_notices(ws)
                .into_iter()
                .map(|(_, notice)| notice)
                .collect(),
            receipt,
            completion,
            cell: self.menu_cell(ws),
            text_cell: (
                self.overlay_text_cell_width(ws),
                self.overlay_text_line_height(ws),
            ),
        }
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
        let Some(geometry) = self
            .media_paste_receipt_placement(ws, receipt, completion)
            .geometry()
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

    #[cfg(target_os = "linux")]
    fn read_paste_clipboard(&mut self, target: ClipboardType) -> Option<String> {
        let result = self
            .clipboard
            .as_mut()
            .map(|clipboard| get_clipboard_text_bounded(clipboard, target, LOCAL_PASTE_MAX + 1))
            .unwrap_or(Err(arboard::Error::ContentNotAvailable));
        self.checked_paste_text(result)
    }

    /// The text a paste read, or `None` after telling the user why it was
    /// refused. An empty clipboard is empty text.
    fn checked_paste_text(&self, result: Result<String, arboard::Error>) -> Option<String> {
        match clipboard_paste_result(result) {
            Ok(text) => Some(text),
            Err(reason) => {
                self.notify_paste_refused(reason);
                None
            }
        }
    }

    pub(super) fn paste_clipboard(&mut self, ws: &mut WindowState) {
        // One order on every platform (`crate::clipboard`): a copied file
        // (CF_HDROP, `text/uri-list`) pastes its quoted path, translated for
        // WSL; then text; then a bitmap. Text wins over a bitmap because a
        // source that offers both, such as spreadsheet cells, means the text.
        use crate::clipboard::{PasteContent, PasteWant, UserRead};
        // A paste already waiting for the clipboard is not asked again: a
        // held shortcut repeats, and each repeat would queue one more read.
        if ws.clipboard_wait.is_some() {
            return;
        }
        let want = PasteWant {
            files: self.cfg.paste_files.enabled(),
            images: self.cfg.paste_images.enabled(),
        };
        let read = match self.clipboard.as_mut() {
            Some(clipboard) => clipboard.paste_for_user(want, LOCAL_PASTE_MAX + 1),
            None => UserRead::Now(PasteContent::Text(Err(arboard::Error::ContentNotAvailable))),
        };
        match read {
            UserRead::Now(content) => self.apply_clipboard_paste(ws, content),
            UserRead::Later(answer) => {
                let destination = PasteDestination::capture(ws, self.broadcast_recipients(ws));
                self.await_clipboard(
                    ws,
                    Wait::Pane {
                        destination,
                        answer,
                    },
                );
            }
        }
    }

    /// Pastes what the clipboard gave a terminal paste.
    fn apply_clipboard_paste(
        &mut self,
        ws: &mut WindowState,
        content: crate::clipboard::PasteContent,
    ) {
        use crate::clipboard::{PasteContent, PasteRefusal};
        match content {
            PasteContent::Files(paths) => {
                let video = self.cfg.paste_video_preview.then(|| {
                    crate::video_preview::VideoPasteRequest::from_user_paths(
                        &paths,
                        crate::video_preview::VideoPasteSource::Clipboard,
                    )
                });
                self.paste_paths(ws, paths, false, video.flatten());
            }
            PasteContent::Text(text) => {
                if let Some(text) = self.checked_paste_text(text) {
                    self.paste_text(ws, text);
                }
            }
            // A screenshot puts pixels on the clipboard with no file and no
            // text behind it. Materialize it as a PNG and paste the path,
            // through the same shell quoting and WSL translation.
            PasteContent::Image(image) => self.paste_clipboard_image(ws, image),
            PasteContent::FilesOff => {
                log::debug!("paste: copied files ignored, paste-files is off");
            }
            PasteContent::Refused(refusal) => self.notify_paste_refused(match refusal {
                PasteRefusal::Files => kettle_i18n::Text::NotifyBodyClipboardFilesUnreadable,
                PasteRefusal::Image => kettle_i18n::Text::NotifyBodyClipboardImageUnreadable,
            }),
        }
    }

    /// Leaves a paste's answer to come later: the window stops waiting for
    /// the clipboard, and the user is told the paste is still coming and how
    /// to stop it. The notice cannot be withdrawn once the paste lands or is
    /// dropped (the desktop notification worker has no call for it), so it
    /// states the rule rather than a state that could go stale.
    pub(super) fn await_clipboard(&self, ws: &mut WindowState, wait: Wait) {
        log::info!("paste: still waiting for the clipboard");
        ws.clipboard_wait = Some(ClipboardWait(wait));
        fire_notify(
            self.ui_text
                .text(kettle_i18n::Text::NotifyTitleClipboardWaiting),
            self.ui_text
                .text(kettle_i18n::Text::NotifyBodyClipboardWaiting),
        );
    }

    /// Drops any paste waiting in `ws`, and any peer window's waiting
    /// broadcast paste whose recipients include a pane in `ws`.
    pub(super) fn drop_clipboard_wait(&mut self, ws: &mut WindowState) {
        ws.clipboard_wait = None;
        drop_peer_clipboard_waits(ws, self.windows.values_mut());
    }

    /// Whether `key` with the window's modifiers pastes into the destination
    /// `ws.clipboard_wait` is already waiting for, so pressing paste again
    /// joins that wait instead of dropping it.
    ///
    /// A modal field's or find bar's wait is joined only by the platform paste
    /// chord while that modal still has the keyboard. A terminal pane's wait
    /// is joined only by a Paste or Paste Selection keybind when no modal has
    /// the keyboard: `Cmd+V` in a dialog or field over a waiting pane paste
    /// drops the pane wait instead of joining it.
    pub(super) fn key_joins_clipboard_wait(
        &self,
        ws: &WindowState,
        key: &Key,
        physical: Option<&PhysicalKey>,
        unshifted: Option<&Key>,
    ) -> bool {
        key_joins_wait(&self.cfg.keybinds, ws, key, physical, unshifted)
    }

    /// Delivers the answer a waiting paste has had, to where the paste was
    /// asked for, if the keyboard is still there. Returns when to look again
    /// while the answer is still to come.
    pub(super) fn poll_clipboard_wait(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        match take_clipboard_answer(ws, now, |ws| self.broadcast_recipients(ws))? {
            ClipboardDelivery::Waiting(next) => return Some(next),
            // Nothing the user did in this window moved it, or the wait
            // would have been dropped then: say why nothing was pasted when
            // there was an answer to lose or an error to report.
            ClipboardDelivery::Dropped(reason) => {
                if let Some(reason) = reason {
                    self.notify_paste_refused(reason);
                }
            }
            ClipboardDelivery::Pane(content) => self.apply_clipboard_paste(ws, content),
            ClipboardDelivery::Search(text) => {
                self.search_paste_answer(ws, text);
                self.sync_search_editor_scroll(ws);
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            ClipboardDelivery::Field(text) => self.modal_paste_answer(ws, text),
        }
        None
    }

    /// Tell the user a paste brought nothing, and why. The reason is a fixed
    /// message: nothing from the clipboard is logged or shown.
    pub(super) fn notify_paste_refused(&self, reason: kettle_i18n::Text) {
        log::warn!(
            "terminal input rejected: {}",
            kettle_i18n::Translator::default().text(reason)
        );
        fire_notify(
            self.ui_text
                .text(kettle_i18n::Text::NotifyTitleInputNotDelivered),
            self.ui_text.text(reason),
        );
    }

    /// Paste the clipboard's bitmap as the path of a temporary PNG. The
    /// clipboard held no text, so a bitmap that cannot be stored pastes
    /// nothing and says so.
    ///
    /// A large screenshot takes long enough to encode to stall the window,
    /// so the PNG is made on the encoder thread and the paste waits in
    /// [`App::queued_pastes`], with any made after it. A PNG another program
    /// supplied is decoded there too, under limits: the window thread reads
    /// only the size its header declares.
    pub(super) fn paste_clipboard_image(
        &mut self,
        ws: &mut WindowState,
        image: crate::clipboard::PasteImage,
    ) {
        use crate::clipboard::PasteImage;
        use crate::paste_image::PngSource;
        if !self.paste_queue_admits(0) {
            self.report_input_result(PaneInputResult::Oversize);
            return;
        }
        let preview = self.cfg.paste_image_preview;
        let (reserved, pixels) = match image {
            PasteImage::Rgba(image) => (
                self.pasted_images.reserve_png(
                    image.width,
                    image.height,
                    image.bytes.len(),
                    preview,
                ),
                PngSource::Rgba(image.bytes.into_owned()),
            ),
            PasteImage::Png(png) => (
                self.pasted_images.reserve_png_from(&png, preview),
                PngSource::Png(png),
            ),
        };
        let png = match reserved {
            Ok(png) => png,
            Err(error) => {
                log::warn!("clipboard image paste failed: {error}");
                self.notify_paste_refused(kettle_i18n::Text::NotifyBodyClipboardImageUnreadable);
                return;
            }
        };
        let id = self.png_jobs;
        self.png_jobs = self.png_jobs.wrapping_add(1);
        let submitted = match self.png_encoder() {
            Some(encoder) => encoder.submit(id, png, pixels),
            None => Err((png, pixels)),
        };
        if let Err((png, pixels)) = submitted {
            // No encoder thread: encode here, as a paste always did once.
            let encoded = png.encode_source(&pixels, &|| false);
            match self.pasted_images.keep_png(encoded) {
                Ok(path) => self.paste_paths(ws, vec![path], false, None),
                Err(error) => {
                    log::warn!("clipboard image paste failed: {error}");
                    self.notify_paste_refused(
                        kettle_i18n::Text::NotifyBodyClipboardImageUnreadable,
                    );
                }
            }
            return;
        }
        self.queue_paste(
            ws,
            QueuedPasteContent::Image {
                id,
                fallback: String::new(),
                path: None,
            },
        );
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
            accessibility_input: event
                .filter(|_| prepare_input)
                .map(|event| {
                    AccessibilityInput::from_key(&event.logical_key, event.text.as_deref())
                })
                .unwrap_or_default(),
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
        accessibility_input: AccessibilityInput,
    ) -> PaneInputResult {
        let mut result = PaneInputResult::Queued;
        let mut reached = Vec::new();
        for (id, bytes) in targets {
            let Some(pane) = ws.mux.panes.get(id) else {
                continue;
            };
            // Keys replayed in one block after a paste are not one typed
            // keystroke: they never teach or arm the typed-echo settle.
            let delivered = pane.feed_paste(bytes);
            if delivered.is_queued() && !bytes.is_empty() {
                for (_, release) in key_presses.iter().filter(|(pane, _)| pane == id) {
                    release.mark_accepted();
                }
                if prepare_input {
                    Self::apply_accessibility_input(ws, &[*id], accessibility_input);
                }
            }
            if delivered.is_queued() && prepare_input {
                reached.push(*id);
                if scroll_to_bottom && let Ok(mut term) = pane.term.term.lock() {
                    term.scroll_display(Scroll::Bottom);
                }
            }
            if prepare_input {
                Self::dismiss_media_paste_receipt_after_input(ws, *id, delivered);
            }
            result = result.merge(delivered);
        }
        if !reached.is_empty() {
            note_keystroke(ws, std::time::Instant::now(), &reached);
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
            accessibility_input: AccessibilityInput::default(),
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
            accessibility_input,
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
        if confirmation_refuses_queued_input(&ws) {
            self.discard_queued_paste(content);
            self.send_key_releases(Some(&ws), &key_releases);
            self.finish_window_dispatch(el, window, ws);
            return;
        }
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
                if fallback.is_empty() {
                    self.notify_paste_refused(
                        kettle_i18n::Text::NotifyBodyClipboardImageUnreadable,
                    );
                } else {
                    self.paste_text_to(&mut ws, target, &fallback);
                }
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
                    accessibility_input,
                );
                for other in self.windows.values_mut() {
                    result = result.merge(Self::deliver_deferred_input(
                        other,
                        &targets,
                        &key_presses,
                        prepare_input,
                        self.cfg.scroll_on_keystroke,
                        accessibility_input,
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

    /// Paste selected Linux PRIMARY text, falling back to the full clipboard pipeline
    /// when PRIMARY is empty or unavailable. Other platforms have one clipboard.
    pub(super) fn paste_primary(&mut self, ws: &mut WindowState) {
        #[cfg(target_os = "linux")]
        {
            match self.read_paste_clipboard(ClipboardType::Selection) {
                Some(text) if !text.is_empty() => {
                    self.paste_text(ws, text);
                    return;
                }
                None => return,
                _ => {}
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
        // The PTY reader can change modes after this lock is released. Keep
        // the safety decision and encoding on the same per-pane snapshot.
        let modes = self.text_paste_modes(ws, target);
        if self.confirm_text_paste(ws, target, text, &modes) {
            return;
        }
        self.deliver_text_paste(ws, target, text, &modes);
    }

    pub(super) fn text_paste_modes(
        &self,
        ws: &WindowState,
        target: Option<u64>,
    ) -> std::collections::HashMap<u64, bool> {
        let ids = if ws.mux.is_broadcast_on() {
            self.broadcast_recipients(ws)
        } else {
            target.into_iter().collect()
        };
        text_paste_modes(ids.into_iter().filter_map(|id| {
            ws.mux
                .panes
                .get(&id)
                .or_else(|| {
                    self.windows
                        .values()
                        .find_map(|other| other.mux.panes.get(&id))
                })
                .map(|pane| (id, pane))
        }))
    }

    fn confirm_text_paste(
        &mut self,
        ws: &mut WindowState,
        target: Option<u64>,
        text: &str,
        modes: &std::collections::HashMap<u64, bool>,
    ) -> bool {
        let needs_confirmation =
            text_paste_needs_confirmation(self.cfg.paste_protection, text, modes);
        if needs_confirmation {
            let dialog = text_paste_confirmation(
                target,
                text,
                ws.mux.broadcast.clone(),
                self.broadcast_recipients(ws),
                self.paste_ask(modes),
                self.ui_text,
            );
            self.show_paste_question(ws, dialog);
        }
        needs_confirmation
    }

    /// The paste question as this platform and policy ask it, for `modes`.
    fn paste_ask(&self, modes: &std::collections::HashMap<u64, bool>) -> PasteAsk {
        PasteAsk {
            policy: self.cfg.paste_protection,
            unbracketed: modes.values().any(|bracketed| !bracketed),
            access_keys: confirm_access_keys(cfg!(target_os = "macos")),
            delayed_suspend: cfg!(target_os = "macos"),
            reasked: false,
        }
    }

    fn show_paste_question(&mut self, ws: &mut WindowState, dialog: ConfirmDialogState) {
        self.install_confirm_dialog(ws, dialog);
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// Carry out an answered paste question, as [`text_paste_dispatch`]
    /// decides on the recipients as they are now.
    pub(super) fn answer_text_paste(&mut self, ws: &mut WindowState, answer: ConfirmAction) {
        let ConfirmAction::Paste { target, text, .. } = &answer else {
            return;
        };
        let modes = self.text_paste_modes(ws, *target);
        let now = PasteNow {
            broadcast: ws.mux.broadcast.clone(),
            recipients: self.broadcast_recipients(ws),
            ask: self.paste_ask(&modes),
            modes,
        };
        match text_paste_dispatch(&answer, now, self.ui_text) {
            PasteDispatch::Drop => {}
            PasteDispatch::Deliver(modes) => self.deliver_text_paste(ws, *target, text, &modes),
            PasteDispatch::AskAgain(dialog) => self.show_paste_question(ws, *dialog),
        }
    }

    pub(super) fn deliver_text_paste(
        &mut self,
        ws: &mut WindowState,
        target: Option<u64>,
        text: &str,
        modes: &std::collections::HashMap<u64, bool>,
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
            let local = ws.mux.broadcast_paste_delivery(text, receipt_pane, modes);
            Self::dismiss_media_paste_receipt_after_delivery(ws, local);
            let mut result = local.result;
            if crate::mux::Mux::scope_crosses_windows(&ws.mux.broadcast) {
                let scope = ws.mux.broadcast.clone();
                for other in self.windows.values_mut() {
                    let receipt_pane = Self::media_paste_receipt_owners(other);
                    let delivery = other.mux.broadcast_paste_foreign_delivery(
                        &scope,
                        text,
                        receipt_pane,
                        modes,
                    );
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
        if let Some(result) = paste_text_into_target(&mut ws.mux, target, text, modes) {
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
            .map_or_else(|| ws.mux.focused_argv(), |pane| pane.path_argv().to_vec());
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
        let mut preview = match crate::mux::format_paths_for_paste_bounded(
            &argv,
            &paths,
            LOCAL_PASTE_MAX.saturating_sub(usize::from(trailing_space)),
        ) {
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
        let pane_fits =
            |mux: &Mux, id: u64| mux.panes.get(&id).is_none_or(|pane| fits(pane.path_argv()));
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
            kettle_render::scrollbar_hit_width(self.cfg.scrollbar_width, scale)
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
            accessibility_input,
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
        && queued.iter().zip(targets).all(|(a, b)| {
            a.0 == b.0
                && a.1.len().saturating_add(b.1.len()) <= crate::mux::MAX_USER_INPUT_MESSAGE_BYTES
        })
    {
        for (id, press) in &input.key_presses {
            if !key_presses.iter().any(|(kept, release)| {
                *kept == *id && Arc::ptr_eq(&release.press_accepted, &press.press_accepted)
            }) {
                key_presses.push((*id, press.clone()));
            }
        }
        accessibility_input.append(input.accessibility_input);
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
    pub(super) accessibility_input: AccessibilityInput,
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
    /// The PNG of encoder job `id`, once it is kept or has failed, and text
    /// to paste instead if it failed. A clipboard image has none: text wins
    /// before an image is tried, so its failure is said, not replaced.
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

/// A paste whose clipboard answer the window stopped waiting for (macOS,
/// `crate::clipboard::LateAnswer`). It is delivered when the answer comes,
/// only where it was asked for. A fresh key press, a click, a drop, text an
/// input method composes, or an assistive-technology edit or focus change in
/// the window drops it first, so a paste never lands behind what the user
/// did next. A key that pastes joins the wait instead.
pub(crate) struct ClipboardWait(pub(super) Wait);

pub(super) enum Wait {
    /// A terminal paste, for the destination it was asked for.
    Pane {
        destination: PasteDestination,
        answer: crate::clipboard::LateAnswer<crate::clipboard::PasteContent>,
    },
    /// A paste into the text field of the modal that had the keyboard.
    Field {
        modal: KeyModal,
        answer: crate::clipboard::LateAnswer<Result<String, arboard::Error>>,
    },
}

/// Where a terminal paste goes, as a paste delivered now would decide it:
/// the focused pane, or the broadcast scope and every pane it reaches, in
/// any window, with whatever modal and preview lane hold the keyboard. A late
/// answer is pasted only while all of it is unchanged; a paste confirmation
/// holds its consent to the same rule (`text_paste_dispatch`).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PasteDestination {
    target: Option<u64>,
    broadcast: crate::mux::BroadcastScope,
    recipients: Vec<u64>,
    modal: Option<KeyModal>,
    preview: Option<crate::media::PreviewFocus>,
}

impl PasteDestination {
    /// `recipients` is `App::broadcast_recipients` for `ws`.
    pub(super) fn capture(ws: &WindowState, recipients: Vec<u64>) -> Self {
        Self {
            target: paste_target(&ws.mux),
            broadcast: ws.mux.broadcast.clone(),
            recipients,
            modal: top_modal(ws),
            preview: ws.preview_focus,
        }
    }
}

/// How often the answer to a waiting paste is looked for.
const CLIPBOARD_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// What became of a waiting paste at a poll.
pub(super) enum ClipboardDelivery {
    /// No answer yet: look again then.
    Waiting(std::time::Instant),
    /// The wait ended after its destination changed without input in this
    /// window (another window's broadcast toggle, a menu-bar tab switch, a
    /// pane that exited): nothing is pasted, and the user is told why when
    /// there was an answer to lose or an error to report. An empty clipboard
    /// stays quiet.
    Dropped(Option<kettle_i18n::Text>),
    Pane(crate::clipboard::PasteContent),
    Search(Result<String, arboard::Error>),
    Field(Result<String, arboard::Error>),
}

/// Drops any peer window's waiting terminal paste whose broadcast recipients
/// include a pane in `ws`, so a cross-window group paste cannot land behind
/// input in another window of the group.
pub(super) fn drop_peer_clipboard_waits<'a>(
    ws: &WindowState,
    peers: impl IntoIterator<Item = &'a mut WindowState>,
) {
    for other in peers {
        let targets_here = matches!(
            other.clipboard_wait,
            Some(ClipboardWait(Wait::Pane { ref destination, .. }))
                if destination
                    .recipients
                    .iter()
                    .any(|id| ws.mux.panes.contains_key(id))
        );
        if targets_here {
            other.clipboard_wait = None;
        }
    }
}

/// Whether a wheel event carries non-zero scroll motion (as opposed to a
/// zero-delta gesture-end event).
pub(super) fn wheel_delta_nonzero(delta: &winit::event::MouseScrollDelta) -> bool {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(x, y) => *x != 0.0 || *y != 0.0,
        winit::event::MouseScrollDelta::PixelDelta(pos) => pos.x != 0.0 || pos.y != 0.0,
    }
}

fn dropped_pane_reason(content: crate::clipboard::PasteContent) -> Option<kettle_i18n::Text> {
    use crate::clipboard::{PasteContent, PasteRefusal};
    use kettle_i18n::Text;
    match content {
        PasteContent::Files(paths) if paths.is_empty() => None,
        PasteContent::Files(_) | PasteContent::Image(_) => {
            Some(Text::NotifyBodyClipboardPasteMoved)
        }
        PasteContent::FilesOff => None,
        PasteContent::Refused(PasteRefusal::Files) => {
            Some(Text::NotifyBodyClipboardFilesUnreadable)
        }
        PasteContent::Refused(PasteRefusal::Image) => {
            Some(Text::NotifyBodyClipboardImageUnreadable)
        }
        PasteContent::Text(result) => match clipboard_paste_result(result) {
            Ok(text) if text.is_empty() => None,
            Ok(_) => Some(Text::NotifyBodyClipboardPasteMoved),
            Err(reason) => Some(reason),
        },
    }
}

fn dropped_field_reason(result: Result<String, arboard::Error>) -> Option<kettle_i18n::Text> {
    match modals::field_paste(result) {
        Ok(Some(_)) => Some(kettle_i18n::Text::NotifyBodyClipboardPasteMoved),
        Ok(None) => None,
        Err(reason) => Some(reason),
    }
}

/// Takes the answer a waiting paste has had, with where it goes. A paste
/// still waiting stays in `ws`. A terminal paste goes only to an unchanged
/// [`PasteDestination`], whose recipients `recipients` gives for `ws` now;
/// a field's paste goes to its modal while that modal is on top.
pub(super) fn take_clipboard_answer(
    ws: &mut WindowState,
    now: std::time::Instant,
    recipients: impl FnOnce(&WindowState) -> Vec<u64>,
) -> Option<ClipboardDelivery> {
    let ClipboardWait(wait) = ws.clipboard_wait.take()?;
    Some(match wait {
        Wait::Pane {
            destination,
            answer,
        } => match answer.poll(now) {
            None => {
                let next = clipboard_poll_after(now, answer.deadline());
                ws.clipboard_wait = Some(ClipboardWait(Wait::Pane {
                    destination,
                    answer,
                }));
                ClipboardDelivery::Waiting(next)
            }
            Some(content) if PasteDestination::capture(ws, recipients(ws)) == destination => {
                ClipboardDelivery::Pane(content)
            }
            Some(content) => ClipboardDelivery::Dropped(dropped_pane_reason(content)),
        },
        Wait::Field { modal, answer } => match answer.poll(now) {
            None => {
                let next = clipboard_poll_after(now, answer.deadline());
                ws.clipboard_wait = Some(ClipboardWait(Wait::Field { modal, answer }));
                ClipboardDelivery::Waiting(next)
            }
            Some(text) if top_modal(ws) != Some(modal) => {
                ClipboardDelivery::Dropped(dropped_field_reason(text))
            }
            Some(text) if modal == KeyModal::Search => ClipboardDelivery::Search(text),
            Some(text) => ClipboardDelivery::Field(text),
        },
    })
}

fn clipboard_poll_after(
    now: std::time::Instant,
    deadline: std::time::Instant,
) -> std::time::Instant {
    (now + CLIPBOARD_POLL).min(deadline.max(now + std::time::Duration::from_millis(1)))
}

/// Whether a key event drops a paste still waiting for the clipboard: a
/// fresh press of any key but a modifier. A held key's repeats do not, and
/// neither does a key that pastes (`pastes`): pressing the paste shortcut
/// again joins the paste that waits instead of asking the clipboard again.
pub(super) fn key_drops_clipboard_wait(
    state: ElementState,
    repeat: bool,
    logical: &Key,
    physical: PhysicalKey,
    pastes: bool,
) -> bool {
    state == ElementState::Pressed
        && !repeat
        && !pastes
        && !crate::cursor_exit_log::is_modifier(logical, physical)
}

/// Whether `key` joins `ws.clipboard_wait`: a modal field's or find bar's wait
/// is joined only by the paste chord while that modal is on top, and a pane's
/// wait is joined only by a Paste or Paste Selection keybind when no modal has
/// the keyboard.
pub(super) fn key_joins_wait(
    keybinds: &Bindings,
    ws: &WindowState,
    key: &Key,
    physical: Option<&PhysicalKey>,
    unshifted: Option<&Key>,
) -> bool {
    let Some(ClipboardWait(wait)) = &ws.clipboard_wait else {
        return false;
    };
    match wait {
        Wait::Field { modal, .. } => top_modal(ws) == Some(*modal) && chord_pastes(ws, key),
        Wait::Pane { .. } => {
            top_modal(ws).is_none()
                && resolve_keybind_action(keybinds, Some(key), physical, ws.mods, unshifted)
                    .is_some_and(|(_, action)| {
                        matches!(
                            action,
                            kettle_config::Action::Paste | kettle_config::Action::PastePrimary
                        )
                    })
        }
    }
}

/// Whether `key` is the platform paste chord where the chord pastes: in a
/// modal field's shared editor or the find bar.
pub(super) fn chord_pastes(ws: &WindowState, key: &Key) -> bool {
    let text_key = match key {
        Key::Character(text) => Some(text.as_str()),
        _ => None,
    };
    (modal_editor(ws).is_some() || top_modal(ws) == Some(KeyModal::Search))
        && crate::modal_input::is_paste_chord(text_key, ws.mods)
}

/// Whether an assistive-technology request drops a paste still waiting for
/// the clipboard: one that edits text or moves focus or presses a control,
/// as a key or a click would.
pub(super) fn accessibility_drops_clipboard_wait(action: AccessibilityAction) -> bool {
    matches!(
        action,
        AccessibilityAction::SetValue
            | AccessibilityAction::ReplaceSelectedText
            | AccessibilityAction::SetTextSelection
            | AccessibilityAction::Focus
            | AccessibilityAction::Click
    )
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

/// Deliver to the stable target using the mode sampled for the safety decision.
pub(super) fn paste_text_into_target(
    mux: &mut Mux,
    target: Option<u64>,
    text: &str,
    modes: &std::collections::HashMap<u64, bool>,
) -> Option<PaneInputResult> {
    let id = target?;
    let pane = mux.panes.get_mut(&id)?;
    if pane.pty_input_failed() {
        return Some(PaneInputResult::Failed);
    }
    if pane.read_only {
        return Some(PaneInputResult::ReadOnly);
    }
    let bracketed = *modes.get(&id)?;
    let bytes = input::paste_payload(text, bracketed);
    Some(pane.feed_paste(&bytes))
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
    let mut text = match crate::mux::format_paths_for_paste_bounded(
        pane.path_argv(),
        paths,
        LOCAL_PASTE_MAX.saturating_sub(usize::from(trailing_space)),
    ) {
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
    let result = pane.feed_paste(&bytes);
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

pub(super) fn clipboard_paste_result(
    result: Result<String, arboard::Error>,
) -> Result<String, kettle_i18n::Text> {
    use kettle_i18n::Text;
    match result {
        Err(arboard::Error::LimitExceeded) => Err(Text::NotifyBodyClipboardLimitExceeded),
        Err(arboard::Error::Timeout) => Err(Text::NotifyBodyClipboardTimeout),
        Ok(text) if !local_paste_within_limit(&text) => Err(Text::NotifyBodyClipboardLimitExceeded),
        other => Ok(other.unwrap_or_default()),
    }
}

/// Queued input that comes ready while a confirmation is unanswered is
/// discarded, never delivered behind the question. Once the question is
/// gone, by any answer, later input is judged on its own.
pub(super) fn confirmation_refuses_queued_input(ws: &WindowState) -> bool {
    ws.confirm_dialog.is_some()
}

/// A bounded window of pasted text for the Confirm paste dialog.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PastePreview {
    /// The window as shown: each control character spelled in ASCII (`ESC`,
    /// `^C`, `CSI`, `U+202E`), a line break as `\n`, a tab as spaces, and an
    /// ellipsis where the text goes on.
    pub text: String,
    /// The bytes of `text` that spell a control character.
    pub tokens: Vec<std::ops::Range<usize>>,
    /// The same window with each control character as words a screen reader
    /// says, so `ESC` in the paste cannot be mistaken for the typed word.
    pub spoken: String,
}

/// Most columns, and most lines, of pasted text a preview shows. A column is
/// one character of what is drawn: a tab is four, `ESC` three, `U+202E` six.
const PREVIEW_CHARS: usize = 160;
const PREVIEW_LINES: usize = 3;
/// Most columns shown before the control a moved preview window is for.
const PREVIEW_LEAD: usize = 40;

/// How one character of pasted text is drawn in the preview.
enum Shown {
    /// CR or LF.
    Break,
    /// Text as it is; a tab as spaces.
    Plain(String),
    /// A control, spelled in ASCII and drawn as a chip.
    Token(String),
}

impl Shown {
    fn of(c: char) -> Self {
        match c {
            '\r' | '\n' => Self::Break,
            // A tab is ordinary whitespace: no chip and no caret alarm.
            '\t' => Self::Plain("    ".to_string()),
            '\x1b' => Self::Token("ESC".to_string()),
            '\x07' => Self::Token("BEL".to_string()),
            '\x7f' => Self::Token("DEL".to_string()),
            c if c <= '\x1f' => Self::Token(format!("^{}", char::from(c as u8 + 64))),
            c if c.is_control() || super::title::is_bidi_format_char(c) => {
                Self::Token(control_code_name(c))
            }
            c => Self::Plain(c.to_string()),
        }
    }

    /// Columns the character takes in the window's budget.
    fn width(&self) -> usize {
        match self {
            Self::Break => 0,
            Self::Plain(text) | Self::Token(text) => text.chars().count(),
        }
    }
}

/// What a screen reader says for control `c`, drawn as `token`.
fn spoken_control(c: char, token: &str, tr: &kettle_i18n::Translator) -> String {
    use kettle_i18n::Text as T;
    match c {
        '\x1b' => tr.text(T::ConfirmPasteA11yEscape).to_string(),
        '\x07' => tr.confirm_paste_a11y_control(tr.text(T::ConfirmPasteFindingBell)),
        '\x7f' => tr.confirm_paste_a11y_control(tr.text(T::ConfirmPasteFindingDelete)),
        '\x08' => tr.confirm_paste_a11y_control(tr.text(T::ConfirmPasteFindingBackspace)),
        c if c <= '\x1f' => {
            tr.confirm_paste_a11y_control(&format!("Ctrl+{}", char::from(c as u8 + 64)))
        }
        _ => tr.confirm_paste_a11y_control(token),
    }
}

/// Keep preview work and dialog size independent of clipboard size.
///
/// The window opens at the start of the text. `anchor` is the byte range of
/// the most serious control in it; when the opening window ends before that
/// control does, the window moves to it instead, so nothing that comes first
/// can hide an end marker further down: not a harmless line break, and not a
/// run of tabs or of characters that are drawn wide.
pub(super) fn paste_preview(
    text: &str,
    anchor: Option<(usize, usize)>,
    tr: &kettle_i18n::Translator,
) -> PastePreview {
    let (head, shown) = preview_window(text, 0, tr);
    let Some((start, end)) = anchor.filter(|&(start, end)| {
        start <= end && end <= text.len() && text.is_char_boundary(start) && shown < end
    }) else {
        return head;
    };
    // Lead in with the rest of the control's line, a bounded way back. The
    // bound is in drawn columns, the unit the window is cut in: a lead
    // counted in characters could fill the window with tabs or `U+202E`s and
    // push the control out of it again.
    let mut from = start;
    let mut lead = 0;
    for (index, c) in text[..start].char_indices().rev() {
        let shown = Shown::of(c);
        lead += shown.width();
        if matches!(shown, Shown::Break) || lead > PREVIEW_LEAD {
            break;
        }
        from = index;
    }
    let (moved, shown) = preview_window(text, from, tr);
    if shown >= end {
        return moved;
    }
    // Whatever the lead was, the control itself is shown: it opens the window.
    preview_window(text, start, tr).0
}

/// The preview of `text` from byte `from`, and the byte it reached.
fn preview_window(text: &str, from: usize, tr: &kettle_i18n::Translator) -> (PastePreview, usize) {
    let mut preview = PastePreview::default();
    if from > 0 {
        preview.text.push('…');
        preview.spoken.push('…');
    }
    let mut used = 0;
    let mut lines = 1;
    let mut shown = from;
    let mut chars = text[from..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        let mut end = from + offset + c.len_utf8();
        let piece = Shown::of(c);
        if matches!(piece, Shown::Break) {
            if c == '\r' && chars.peek().is_some_and(|&(_, next)| next == '\n') {
                chars.next();
                end += 1;
            }
            // A line break that ends the text ends its last line. It draws
            // no empty line and is no fourth line to cut at; the summary says
            // the text ends with one.
            if end == text.len() {
                shown = end;
                break;
            }
            lines += 1;
        }
        let width = piece.width();
        if used + width > PREVIEW_CHARS || lines > PREVIEW_LINES {
            preview.text.push('…');
            preview.spoken.push('…');
            break;
        }
        match piece {
            Shown::Break => {
                preview.text.push('\n');
                preview.spoken.push('\n');
            }
            Shown::Plain(visible) => {
                preview.text.push_str(&visible);
                preview.spoken.push_str(&visible);
            }
            Shown::Token(token) => {
                let start = preview.text.len();
                preview.text.push_str(&token);
                preview.tokens.push(start..preview.text.len());
                if !preview.spoken.is_empty() && !preview.spoken.ends_with([' ', '\n']) {
                    preview.spoken.push(' ');
                }
                preview.spoken.push_str(&spoken_control(c, &token, tr));
                preview.spoken.push(' ');
            }
        }
        used += width;
        shown = end;
    }
    let spoken = preview.spoken.trim_end().len();
    preview.spoken.truncate(spoken);
    (preview, shown)
}

/// How a C1 control or a text-direction character is spelled: the standard
/// short name of the common C1 codes, else its code point.
fn control_code_name(c: char) -> String {
    match c {
        '\u{9b}' => "CSI".to_string(),
        '\u{9d}' => "OSC".to_string(),
        '\u{90}' => "DCS".to_string(),
        '\u{9c}' => "ST".to_string(),
        c => format!("U+{:04X}", u32::from(c)),
    }
}

/// How the findings list spells a kind of control, matching the preview, and
/// what the kind does.
fn finding_text(
    kind: crate::paste_safety::Kind,
    tr: &kettle_i18n::Translator,
) -> (String, &'static str) {
    use crate::paste_safety::Kind;
    use kettle_i18n::Text as T;
    let (token, meaning) = match kind {
        Kind::EndMarker => (END_MARKER_NAME.to_string(), T::ConfirmPasteFindingEndMarker),
        Kind::EndMarkerC1 => (
            END_MARKER_C1_NAME.to_string(),
            T::ConfirmPasteFindingEndMarkerC1,
        ),
        Kind::Interrupt => ("^C".to_string(), T::ConfirmPasteFindingInterrupt),
        Kind::Suspend => ("^Z".to_string(), T::ConfirmPasteFindingSuspend),
        Kind::Quit => ("^\\".to_string(), T::ConfirmPasteFindingQuit),
        Kind::EndOfInput => ("^D".to_string(), T::ConfirmPasteFindingEndOfInput),
        Kind::Escape => ("ESC".to_string(), T::ConfirmPasteFindingEscape),
        Kind::Bidi(c) => (control_code_name(c), T::ConfirmPasteFindingBidi),
        Kind::C1(c) => (control_code_name(c), T::ConfirmPasteFindingC1),
        Kind::Backspace => ("^H".to_string(), T::ConfirmPasteFindingBackspace),
        Kind::Delete => ("DEL".to_string(), T::ConfirmPasteFindingDelete),
        Kind::Bell => ("BEL".to_string(), T::ConfirmPasteFindingBell),
        Kind::Control(c) => (
            format!("^{}", char::from(c as u8 + 64)),
            T::ConfirmPasteFindingControl,
        ),
    };
    (token, tr.text(meaning))
}

/// Whether the paste encoder removes this kind of control whichever button
/// is chosen, so it never reaches a program.
///
/// The end marker goes in every mode. ESC and the signal bytes go only
/// inside a bracketed paste, and the dialog may say so only when every
/// recipient is bracketed: under broadcast, one program without bracketed
/// paste still receives them. Which bytes those are is the encoder's own
/// rule, not a second list here.
fn encoder_removes(kind: crate::paste_safety::Kind, ask: PasteAsk) -> bool {
    use crate::paste_safety::Kind;
    match kind {
        Kind::EndMarker | Kind::EndMarkerC1 => true,
        kind => {
            !ask.unbracketed
                && kind
                    .byte()
                    .is_some_and(|byte| input::bracketed_paste_drops(byte, ask.delayed_suspend))
        }
    }
}

/// The end marker as the dialog names it, in its 7-bit and its C1 form. The
/// preview shows the same letters: a chip for the introducer, then the rest.
const END_MARKER_NAME: &str = "ESC[201~";
const END_MARKER_C1_NAME: &str = "CSI 201~";

/// Most findings rows; the kinds beyond them are counted in one last row.
const FINDING_ROWS: usize = 4;

pub(super) fn paste_as_one_line(text: &str) -> String {
    text.replace("\r\n", " ")
        .chars()
        .map(|c| {
            if c.is_ascii_control() && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Why a paste question is asked, sampled with the modes that decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PasteAsk {
    pub policy: kettle_config::PasteProtection,
    /// A program receiving the paste has bracketed paste off.
    pub unbracketed: bool,
    /// Show and answer access keys. Never on macOS.
    pub access_keys: bool,
    /// The platform has a delayed-suspend byte (0x19) that a bracketed paste
    /// drops: macOS.
    pub delayed_suspend: bool,
    /// Kettle asks again because a recipient left bracketed paste after the
    /// question was answered, and the answer was not used.
    pub reasked: bool,
}

pub(super) fn text_paste_confirmation(
    target: Option<u64>,
    text: &str,
    broadcast: BroadcastScope,
    recipients: Vec<u64>,
    ask: PasteAsk,
    tr: kettle_i18n::Translator,
) -> ConfirmDialogState {
    use crate::paste_safety::Kind;
    use kettle_i18n::Text as T;
    let found = crate::paste_safety::analyse(text);
    let kinds = crate::paste_safety::findings(text);
    // With controls in the text, the sanitised paste is the recommended
    // choice after Cancel.
    let controls = !kinds.is_empty();
    // The raw paste carries the warning colour when it would hand the
    // program a control: one the encoder does not remove in this mode. Text
    // with an end marker keeps it too. Kettle removes the marker, but such
    // text was written to break out of a paste, and the raw choice is the
    // one that sends everything else in it untouched.
    let marked = kinds
        .iter()
        .any(|finding| matches!(finding.kind, Kind::EndMarker | Kind::EndMarkerC1));
    let raw_is_risky = marked
        || kinds
            .iter()
            .any(|finding| !encoder_removes(finding.kind, ask));
    let mut buttons = vec![ConfirmButton::Cancel];
    if controls {
        buttons.push(ConfirmButton::PasteWithoutControls {
            label: tr.text(T::ConfirmPasteWithoutControls).to_string(),
        });
    }
    let paste = ConfirmButton::Confirm {
        label: tr.text(T::MenuPaste).to_string(),
        destructive: raw_is_risky,
    };
    let one_line = ConfirmButton::PasteOneLine {
        label: tr.text(T::ConfirmPasteOneLine).to_string(),
    };
    if controls {
        buttons.extend([one_line, paste]);
    } else {
        buttons.extend([paste, one_line]);
    }
    let key = |message: T| tr.text(message).chars().next().filter(|_| ask.access_keys);
    let access_keys = buttons
        .iter()
        .map(|button| {
            key(match button {
                ConfirmButton::Cancel => T::ConfirmPasteKeyCancel,
                ConfirmButton::PasteWithoutControls { .. } => T::ConfirmPasteKeyWithoutControls,
                ConfirmButton::PasteOneLine { .. } => T::ConfirmPasteKeyOneLine,
                ConfirmButton::Confirm { .. } => T::ConfirmPasteKeyPaste,
            })
        })
        .collect();

    // The first sentence says why Kettle asked, naming the marker it found.
    // Its 7-bit form ranks first, so that is the one named when both occur.
    let marker = kinds.iter().find_map(|finding| match finding.kind {
        Kind::EndMarker => Some(END_MARKER_NAME),
        Kind::EndMarkerC1 => Some(END_MARKER_C1_NAME),
        _ => None,
    });
    let breaks = text.contains(['\n', '\r']);
    let lines_reason = || tr.text(T::ConfirmPasteReasonLines).to_string();
    let mut reason = Vec::new();
    // A question asked again says first why the answer was not used: the
    // dialog may otherwise look like the one just answered.
    if ask.reasked {
        reason.push(tr.text(T::ConfirmPasteReasonReasked).to_string());
    }
    if let Some(marker) = marker {
        reason.push(tr.confirm_paste_reason_marker(marker));
        // Removing the marker does not make the line breaks safe where
        // bracketed paste is off: each still arrives as Enter.
        if ask.unbracketed && breaks {
            reason.push(lines_reason());
        }
    } else if ask.unbracketed && breaks {
        reason.push(lines_reason());
    } else if ask.unbracketed || ask.policy != kettle_config::PasteProtection::Always {
        reason.push(tr.text(T::ConfirmPasteReasonControls).to_string());
    } else {
        reason.push(tr.text(T::ConfirmPasteReasonAlways).to_string());
    }
    let body = reason.join("\n");

    // A final line break ends the last line; it does not start another.
    let trailing = text.ends_with(['\n', '\r']);
    let lines = tr.confirm_paste_lines(found.lines - u64::from(trailing));
    let mut summary = vec![if trailing {
        tr.confirm_paste_trailing_break(&lines)
    } else {
        lines
    }];
    if found.escapes > 0 {
        summary.push(tr.confirm_paste_escapes(found.escapes));
    }
    if found.controls > 0 {
        summary.push(tr.confirm_paste_controls(found.controls));
    }

    let preview = paste_preview(text, kinds.first().map(|finding| finding.first), &tr);
    let more = match kinds.len().saturating_sub(FINDING_ROWS) {
        0 => String::new(),
        rest => tr.confirm_paste_more_kinds(rest as u64),
    };
    let policy = tr.confirm_paste_policy(tr.text(match ask.policy {
        kettle_config::PasteProtection::Auto => T::SettingsValueAutomatic,
        kettle_config::PasteProtection::Always => T::SettingsValueAlways,
        kettle_config::PasteProtection::Off => T::SettingsValueOff,
    }));
    let mut description = vec![body.clone(), summary.join(" \u{b7} ")];
    // With more kinds than rows, the rows go first to the end marker, which
    // is why Kettle asked, and to the controls that reach the program. ESC
    // and the signal bytes that a bracketed paste removes never push a live
    // control into the count of further kinds. The rows chosen keep their
    // order of severity.
    let (first, removed): (Vec<_>, Vec<_>) = kinds.iter().enumerate().partition(|(_, finding)| {
        matches!(finding.kind, Kind::EndMarker | Kind::EndMarkerC1)
            || !encoder_removes(finding.kind, ask)
    });
    let mut rows: Vec<_> = first
        .into_iter()
        .chain(removed)
        .take(FINDING_ROWS)
        .collect();
    rows.sort_by_key(|&(rank, _)| rank);
    let findings = rows
        .into_iter()
        .map(|(_, finding)| finding)
        .map(|finding| {
            let (token, meaning) = finding_text(finding.kind, &tr);
            // A row says what happens to the control in this mode: one the
            // encoder removes is not presented as a live risk.
            let removed = encoder_removes(finding.kind, ask);
            let meaning = if removed {
                tr.confirm_paste_finding_removed(meaning)
            } else {
                meaning.to_string()
            };
            description.push(tr.confirm_paste_a11y_finding(&token, &meaning, finding.count));
            kettle_render::ConfirmDialogFinding {
                token,
                meaning,
                count: format!("\u{d7}{}", finding.count),
                removed,
            }
        })
        .collect();
    if !more.is_empty() {
        description.push(more.clone());
    }
    description.push(policy.clone());
    description.push(tr.confirm_paste_a11y_preview(&preview.spoken));
    let recommended = buttons
        .iter()
        .position(|button| {
            if controls {
                matches!(button, ConfirmButton::PasteWithoutControls { .. })
            } else {
                matches!(button, ConfirmButton::Confirm { .. })
            }
        })
        .unwrap_or(0);
    ConfirmDialogState {
        heading: tr.text(T::ConfirmPasteHeading).to_string(),
        body,
        buttons,
        focus_idx: 0,
        on_confirm: ConfirmAction::Paste {
            target,
            text: text.to_string(),
            broadcast,
            recipients,
            unbracketed: ask.unbracketed,
        },
        scroll: 0,
        armed_press: None,
        space_pressed: None,
        paste: Some(Box::new(PasteQuestion {
            detail: kettle_render::ConfirmDialogDetail {
                preview: preview.text,
                preview_tokens: preview.tokens,
                summary,
                findings,
                more,
                policy,
                link: tr.text(T::ConfirmPasteSettings).to_string(),
                link_access_key: key(T::ConfirmPasteKeySettings),
                description: description.join("\n"),
            },
            access_keys,
            recommended,
        })),
    }
}

pub(super) fn text_paste_modes<'a>(
    panes: impl IntoIterator<Item = (u64, &'a Pane)>,
) -> std::collections::HashMap<u64, bool> {
    panes
        .into_iter()
        .filter(|(_, pane)| !pane.read_only)
        .map(|(id, pane)| {
            let bracketed =
                pane.term.term.lock().ok().is_some_and(|term| {
                    term.mode().contains(kettle_core::TermMode::BRACKETED_PASTE)
                });
            (id, bracketed)
        })
        .collect()
}

/// What an answered paste question does with its recipients' modes as they
/// are when the answer arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PasteAnswer {
    Deliver,
    AskAgain,
}

/// The recipients of a paste as they are when its question is answered.
pub(super) struct PasteNow {
    pub broadcast: BroadcastScope,
    /// The broadcast recipients, sorted; empty when broadcast is off.
    pub recipients: Vec<u64>,
    /// Bracketed paste on or off, per writable recipient, sampled once.
    pub modes: std::collections::HashMap<u64, bool>,
    /// The question as Kettle would ask it for these modes.
    pub ask: PasteAsk,
}

/// What answering a paste question does.
#[derive(Debug)]
pub(super) enum PasteDispatch {
    /// The broadcast scope or its recipients changed since the question:
    /// the answer covered other panes, and nothing is sent.
    Drop,
    /// Send the text, encoded for these modes: the snapshot that judged it.
    Deliver(std::collections::HashMap<u64, bool>),
    /// A recipient left bracketed paste after the question was asked. Nothing
    /// is sent, and this question, asked for the modes of now, replaces it.
    AskAgain(Box<ConfirmDialogState>),
}

/// Decide what the answer `answered` (a paste action) does for the
/// recipients as they are `now`. The handler only carries the result out.
pub(super) fn text_paste_dispatch(
    answered: &ConfirmAction,
    now: PasteNow,
    tr: kettle_i18n::Translator,
) -> PasteDispatch {
    let ConfirmAction::Paste {
        target,
        text,
        broadcast,
        recipients,
        unbracketed,
    } = answered
    else {
        return PasteDispatch::Drop;
    };
    // Consent covers the original pane or the unchanged broadcast scope and
    // recipient set, nothing else.
    if now.broadcast != *broadcast || now.recipients != *recipients {
        return PasteDispatch::Drop;
    }
    match paste_answer(*unbracketed, now.ask.policy, text, &now.modes) {
        PasteAnswer::Deliver => PasteDispatch::Deliver(now.modes),
        PasteAnswer::AskAgain => PasteDispatch::AskAgain(Box::new(text_paste_confirmation(
            *target,
            text,
            now.broadcast,
            now.recipients,
            PasteAsk {
                reasked: true,
                ..now.ask
            },
            tr,
        ))),
    }
}

/// Whether the answer to a paste question still covers the paste.
///
/// A question asked while every recipient had bracketed paste on said that
/// Kettle removes ESC and the signal bytes, and its line breaks were not
/// going to run anything. A program can turn bracketed paste off while the
/// question is open. The same answer would then send those controls and run
/// those lines, so it is not carried over: Kettle asks again, for the modes
/// of now.
///
/// That holds for any text with a line break or a control in it, whichever
/// policy asked: it is the default policy's own rule for a program without
/// bracketed paste. Text with neither reads the same in both modes and goes
/// through, as does everything once the policy is `off`. A question asked
/// with bracketed paste already off described the worst case, and its
/// answer stands in any mode.
pub(super) fn paste_answer(
    asked_unbracketed: bool,
    protection: kettle_config::PasteProtection,
    text: &str,
    modes: &std::collections::HashMap<u64, bool>,
) -> PasteAnswer {
    use kettle_config::PasteProtection;
    let now_unbracketed = modes.values().any(|bracketed| !bracketed);
    if protection != PasteProtection::Off
        && !asked_unbracketed
        && now_unbracketed
        && PasteProtection::Auto.confirms(text, false)
    {
        PasteAnswer::AskAgain
    } else {
        PasteAnswer::Deliver
    }
}

pub(super) fn text_paste_needs_confirmation(
    protection: kettle_config::PasteProtection,
    text: &str,
    modes: &std::collections::HashMap<u64, bool>,
) -> bool {
    modes
        .values()
        .any(|&bracketed| protection.confirms(text, bracketed))
}
