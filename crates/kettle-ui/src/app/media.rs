//! The media shelf: `show` requests, renders, crops, pulls and copies, and agent preview setup.

use super::*;

impl App {
    /// The Agent previews row's footer note for the current state.
    pub(super) fn agent_previews_note(&self) -> kettle_i18n::Text {
        let turned_on = self.cfg.ctl_policy().with_display();
        agent_previews_note(
            self.cfg.agent_display,
            self.ctl_policy,
            ctl_overrides(&self.startup).resolve(turned_on).display(),
            self.ctl.is_some(),
            self.ctl_start == CtlStart::Attempted,
        )
    }

    /// Whether agent previews are on now and stay on: granted to this
    /// process and still asked for. Turning them off reaches the server at
    /// the next launch, but new panes stop getting the plugin at once.
    pub(super) fn agent_previews_live(&self) -> bool {
        self.ctl_policy.display()
            && ctl_overrides(&self.startup)
                .resolve(self.cfg.ctl_policy())
                .display()
    }

    /// Offer new panes Kettle's Claude Code plugin while agent previews and
    /// the Claude Code integration are both on, writing it if needed; panes
    /// already open keep what they started with.
    pub(super) fn reconcile_claude_plugin(&mut self) {
        if !(self.agent_previews_live() && self.cfg.agent_display_claude_code) {
            crate::agent_plugin::offer(None, None);
            return;
        }
        let keep_older = self.other_builds.running();
        let prepared = self
            .executable
            .as_deref()
            .ok_or(crate::agent_plugin::PluginRefusal::UnnamedExecutable)
            .and_then(|executable| {
                crate::agent_plugin::PluginFiles::new(executable, env!("CARGO_PKG_VERSION"))
            })
            .and_then(|files| {
                let root = crate::agent_plugin::plugins_root()
                    .ok_or(crate::agent_plugin::PluginRefusal::Unverified)?;
                crate::agent_plugin::install(&root, &files, keep_older)
                    .map(|directory| (directory, files))
            })
            .and_then(|(directory, files)| {
                crate::agent_plugin::plugin_dir_entry(&directory)
                    .ok_or(crate::agent_plugin::PluginRefusal::UnnamedDirectory)?;
                Ok((directory, files))
            });
        match prepared {
            Ok(plugin) => {
                crate::agent_plugin::offer(Some(plugin), None);
                // Check now, as for a new pane with the configured `env`, so
                // Settings names a policy that forbids the plugin before the
                // next pane opens.
                let _ = crate::agent_plugin::for_new_pane(|| {
                    crate::mux::claude_config_for(&self.cfg.env)
                });
            }
            Err(refusal) => {
                log::warn!("agent previews: the Claude Code plugin is unavailable ({refusal:?})");
                crate::agent_plugin::offer(None, Some(refusal));
            }
        }
    }

    /// Prepare enabled agent functions for new panes. Existing panes keep
    /// their startup, and user-defined functions always win.
    pub(super) fn reconcile_codex_shell(&mut self) {
        let agents = crate::codex_shell::Agents {
            codex: self.cfg.agent_display_codex,
            claude: cfg!(target_os = "linux") && self.cfg.agent_display_claude_code,
        };
        if !self.agent_previews_live() || !(agents.codex || agents.claude) {
            crate::codex_shell::offer(None, None);
            return;
        }
        if cfg!(not(unix)) {
            crate::codex_shell::offer(None, Some(crate::codex_shell::ShellRefusal::Unsupported));
            return;
        }
        let keep_older = self.other_builds.running();
        let prepared = self
            .executable
            .as_deref()
            .ok_or(crate::codex_shell::ShellRefusal::Unavailable)
            .and_then(|executable| {
                let root = crate::codex_shell::startup_root()
                    .ok_or(crate::codex_shell::ShellRefusal::Unavailable)?;
                crate::codex_shell::prepare(
                    &root,
                    executable,
                    env!("CARGO_PKG_VERSION"),
                    agents,
                    keep_older,
                )
            });
        match prepared {
            Ok(offered) => crate::codex_shell::offer(Some(offered), None),
            Err(refusal) => {
                log::warn!("agent previews: shell integration is unavailable ({refusal:?})");
                crate::codex_shell::offer(None, Some(refusal));
            }
        }
    }

    /// Apply a reloaded `agent-display`. Turning display on takes effect at
    /// once, for open connections too, and starts the server if none runs;
    /// turning it off waits for the next launch. Launch flags keep their
    /// precedence, and `agent-server` changes wait for the next launch.
    pub(super) fn reconcile_agent_display(&mut self) {
        let requested = ctl_overrides(&self.startup).resolve(self.cfg.ctl_policy());
        let Some(widened) = self.ctl_policy.latch_display(requested.display()) else {
            return;
        };
        self.ctl_policy = widened;
        log::info!("agent-server: agent previews turned on");
        match &self.ctl {
            Some(ctl) => ctl.enable_display(),
            None => {
                // Display is a new grant, so it earns one more start attempt
                // even if an earlier one failed. The latch makes this happen
                // at most once per process.
                if self.ctl_start == CtlStart::Attempted {
                    self.ctl_start = CtlStart::Ready;
                }
                self.ensure_ctl_started();
            }
        }
    }

    /// `show`: route the push to its caller's pane, queue its render, and
    /// answer once its item is on that pane's shelf (see [`crate::media`]).
    pub(super) fn ctl_show(
        &mut self,
        ws: &mut WindowState,
        conn_id: u64,
        request: &mut crate::ctl_server::AdmittedRequest,
        reply: crate::ctl_server::ReplyTx,
    ) {
        use kettle_media::FailureCode;
        let id = request.request().id;
        let refuse = |failure: FailureCode| {
            let _ = reply.send(kettle_ctl::show::show_failure(id, failure));
        };
        let Some(crate::ctl_server::ShowAdmission {
            request: show,
            sender,
            harness,
            terminal,
        }) = request.take_show()
        else {
            refuse(FailureCode::BadParams);
            return;
        };
        if let Some(failure) = media_unavailable(self.startup.media.as_deref()) {
            refuse(failure);
            return;
        }
        let full_control = self.ctl.as_ref().is_some_and(|ctl| {
            ctl.policy()
                .allows(kettle_ctl::protocol::Capability::Mutate)
        });
        let (roots, _) = self.caller_roots(ws);
        let live: Vec<(u64, u64)> = self
            .all_windows(ws)
            .flat_map(|window| {
                window
                    .mux
                    .panes
                    .iter()
                    .filter(|(_, pane)| !pane.closed && !pane.held)
                    .map(move |(&pane, _)| (pane, window.seq))
            })
            .collect();
        let route = match crate::media::route(
            request.caller(),
            show.pane,
            full_control,
            &roots,
            &live,
            std::process::id(),
        ) {
            Ok(route) => route,
            Err(failure) => {
                refuse(failure);
                return;
            }
        };
        #[cfg(target_os = "linux")]
        let _ = harness;
        #[cfg(target_os = "linux")]
        let native_grant = show
            .inline
            .filter(|&target| target != kettle_ctl::show::InlineTarget::Cli)
            .zip(sender.as_ref())
            .and_then(|(target, sender)| {
                let caller = self.scoped_card_caller(ws, request.caller(), sender, target)?;
                self.agent_launches.grant(caller, std::time::Instant::now())
            });
        #[cfg(target_os = "linux")]
        let harness = native_grant.as_ref().map(|grant| grant.owner());
        // The command that would print a command-line card, checked again
        // once the render is done.
        let printer = sender.as_ref().map(|sender| sender.process);
        // An unverified push counts against its sending process; one whose
        // sender the kernel could not name has no one to count against or
        // to show as its sender.
        let (sender, provenance) = match (route.verified, sender) {
            (true, _) => (
                crate::media::Sender::Pane(route.pane),
                crate::media::Provenance::Verified,
            ),
            // A script that runs `kettle show` many times is one sender: the
            // program that asked, not each command line it ran.
            (false, Some(sender)) => {
                let asker = sender.asker;
                (
                    crate::media::Sender::Process(asker.process),
                    crate::media::Provenance::Unverified(crate::media::UnverifiedSender {
                        executable: asker
                            .executable
                            .map(|path| crate::media::display_title(&path.to_string_lossy())),
                        pid: asker.process.pid(),
                        signer: asker.signature.as_ref().map(|signature| {
                            crate::media::display_title(&crate::media::signer_name(signature))
                        }),
                    }),
                )
            }
            (false, None) => {
                refuse(FailureCode::NotInKettlePane);
                return;
            }
        };
        let Some((theme, target)) = self.media_surface(ws, route.pane) else {
            refuse(FailureCode::NotInKettlePane);
            return;
        };
        let kettle_ctl::show::ShowRequest {
            source,
            title,
            key,
            inline,
            ..
        } = show;
        let kind = source.job_kind();
        let (source, file_name) = match source {
            kettle_ctl::show::ShowSource::Svg(text) => {
                (kettle_media::Source::Bytes(text.into_bytes()), None)
            }
            kettle_ctl::show::ShowSource::Image(bytes) => {
                (kettle_media::Source::Bytes(bytes), None)
            }
            kettle_ctl::show::ShowSource::Mermaid(text) => {
                (kettle_media::Source::Bytes(text.into_bytes()), None)
            }
            kettle_ctl::show::ShowSource::File {
                path, attestation, ..
            } => {
                let name = path.file_name_lossy();
                (
                    kettle_media::Source::Path {
                        path,
                        authorization: kettle_media::Authorization::ExternalAttested(attestation),
                    },
                    name,
                )
            }
        };
        // A file's key defaults to the file, so showing it again replaces it.
        let key = key
            .map(crate::media::ShelfKey::Explicit)
            .or_else(|| match &source {
                kettle_media::Source::Path { path, .. } => {
                    Some(crate::media::ShelfKey::Path(path.clone()))
                }
                kettle_media::Source::Bytes(_) => None,
            });
        let card_name = file_name.clone();
        let title = title
            .or(file_name)
            .map(|title| crate::media::display_title(&title))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| {
                self.ui_text
                    .text(kettle_i18n::Text::MediaShelfUntitled)
                    .into()
            });
        // A card only for the harness Kettle checked, which runs in this very
        // pane, or for `kettle show --inline` writing to the pane's own
        // terminal from its foreground; everyone else's push goes to the
        // shelf alone.
        let (inline, inline_refused) = match (inline, harness) {
            (Some(kettle_ctl::show::InlineTarget::Cli), _) => {
                let checked = self
                    .all_windows(ws)
                    .find_map(|window| window.mux.panes.get(&route.pane))
                    .ok_or(kettle_ctl::show::InlineRefusal::Unverified)
                    .and_then(|pane| cli_card_owner(pane, route.verified, terminal));
                match checked {
                    Ok(owner) => (
                        Some(crate::media::InlineDraft {
                            #[cfg(target_os = "linux")]
                            launch: None,
                            owner,
                            harness: kettle_render::CardHarness::Cli,
                            name: card_name,
                            printer,
                        }),
                        None,
                    ),
                    Err(refusal) => (None, Some(refusal)),
                }
            }
            (Some(target), Some(owner)) if route.verified => (
                Some(crate::media::InlineDraft {
                    #[cfg(target_os = "linux")]
                    launch: native_grant,
                    owner,
                    harness: printed_by(target),
                    name: card_name,
                    printer: None,
                }),
                None,
            ),
            _ => (None, None),
        };
        // The bytes a push carried are kept, charged, for the item to be
        // rendered again: no room for them is no room for the push.
        let Some(spec) = crate::media::JobSpec::from_job(kettle_media::Job {
            kind,
            source,
            theme,
            canvas: kettle_media::Canvas::Theme,
            target,
            fallback_fonts: Vec::new(),
        }) else {
            refuse(FailureCode::OverBudget);
            return;
        };
        let deadline = request.admitted_at() + kettle_ctl::show::SHOW_SERVER_DEADLINE;
        let push = crate::media::Push::new(
            crate::media::Requester::Ctl(crate::media::Origin {
                conn_id,
                request_id: id,
                reply,
            }),
            route,
            crate::media::Draft {
                key,
                title,
                provenance,
                inline,
                inline_refused,
                gallery: None,
            },
            spec,
            kettle_media::client::RenderControl::with_deadline(deadline),
        );
        self.media.admit(sender, deadline, push);
        self.media_tick(ws);
    }

    /// The theme and render target for media shown in `pane`: the pane's
    /// effective colors, and its window's size in device pixels.
    pub(super) fn media_surface(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Option<(kettle_media::Theme, kettle_media::Target)> {
        let window = self
            .all_windows(ws)
            .find(|window| window.mux.panes.contains_key(&pane))?;
        let pane = window.mux.panes.get(&pane)?;
        let colors = pane.term.term.lock().ok().map(|term| *term.colors());
        let theme = &self.cfg.theme;
        let pick = |index: usize| {
            colors
                .as_ref()
                .and_then(|colors| kettle_render::resolve_query(index, theme, colors))
                .unwrap_or(match index {
                    256 => theme.foreground,
                    257 => theme.background,
                    _ => theme.palette[index.min(15)],
                })
        };
        let rgba = |color: kettle_config::Rgb| [color.r, color.g, color.b, 255];
        let background = pick(257);
        let media_theme = kettle_media::Theme {
            background: rgba(background),
            foreground: rgba(pick(256)),
            palette: std::array::from_fn(|index| rgba(pick(index))),
            accent: rgba(theme.accent),
            is_dark: kettle_render::relative_luminance(background) < 0.5,
        };
        let (width, height, scale) = window
            .window
            .as_ref()
            .map(|window| {
                let size = window.inner_size();
                (size.width, size.height, window.scale_factor())
            })
            .unwrap_or((1024, 768, 1.0));
        let edge = |value: u32| value.clamp(64, kettle_media::MAX_SVG_RENDERED_EDGE);
        let target = kettle_media::Target {
            width: edge(width),
            height: edge(height),
            scale: if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                1.0
            },
            crop: None,
        };
        Some((media_theme, target))
    }

    /// Advance `show`: drop pushes whose pane is gone, publish finished
    /// renders, and start the next one.
    pub(super) fn media_tick(&mut self, ws: &mut WindowState) {
        self.retire_cards(ws);
        self.tell_preview_failures(ws);
        if self.media.is_idle() {
            return;
        }
        let live: std::collections::HashSet<u64> = self
            .all_windows(ws)
            .flat_map(|window| {
                window
                    .mux
                    .panes
                    .iter()
                    .filter(|(_, pane)| !pane.closed && !pane.held)
                    .map(|(&pane, _)| pane)
            })
            .collect();
        self.media.retain_panes(|pane| live.contains(&pane));
        while let Some(finished) = self.media.take_finished() {
            self.finish_show(ws, finished);
        }
        let proxy = self.proxy.clone();
        self.media.pump(self.startup.media.as_ref(), move || {
            std::sync::Arc::new(move || {
                let _ = proxy.send_event(UserEvent::MediaRendered);
            })
        });
        self.tell_preview_failures(ws);
    }

    /// Tell the user about each preview they asked for that ended before
    /// it rendered: busy, unavailable, out of time.
    pub(super) fn tell_preview_failures(&mut self, ws: &mut WindowState) {
        use crate::window_state::LaneNotice;
        let tr = self.ui_text;
        for (_, failure) in self.media.take_user_failures() {
            notify_preview_failure(tr, Some(failure));
        }
        // A lane's render that could not run, and each copy that ended, are
        // said in the lane that asked.
        for render in self.media.take_lane_failures() {
            // A preview that could not even start says so; the poster stays.
            if let Some(ticket) = render.preview {
                let failed = self
                    .window_by_seq(ws, render.window)
                    .and_then(|window| {
                        window
                            .lane_previews
                            .remove(&render.pane)
                            .filter(|preview| preview.waits_on() == Some(ticket))
                    })
                    .is_some();
                if failed {
                    self.tell_lane(
                        ws,
                        render.window,
                        render.pane,
                        render.item,
                        LaneNotice::PreviewFailed,
                    );
                }
                continue;
            }
            // Sharper pixels that could not be asked for leave the lane
            // showing its own, without a word.
            if let Some(ticket) = render.tile {
                if let Some(crop) = self
                    .window_by_seq(ws, render.window)
                    .and_then(|window| window.lane_crops.get_mut(&render.pane))
                    .filter(|crop| crop.asked == Some(ticket))
                {
                    crop.asked = None;
                }
                continue;
            }
            // A gallery page the lane no longer waits on goes unsaid.
            if !self.lane_wants(ws, &render) {
                continue;
            }
            self.forget_lane_page(ws, &render);
            self.tell_lane(
                ws,
                render.window,
                render.pane,
                render.item,
                LaneNotice::RenderFailed,
            );
        }
        let copies = self
            .preview_copy
            .as_ref()
            .map(crate::media::CopyService::take_done)
            .unwrap_or_default();
        for copy in copies {
            let current = self
                .window_by_seq(ws, copy.window)
                .and_then(|window| preview_item(window, copy.pane))
                .map(|item| (item.id, item.generation));
            if let Some(notice) = copy.notice_for(current) {
                self.tell_lane(ws, copy.window, copy.pane, copy.item, notice);
            }
        }
    }

    /// Say `notice` in the lane of `pane` in window `window`, while it still
    /// shows `item`.
    pub(super) fn tell_lane(
        &mut self,
        ws: &mut WindowState,
        window: u64,
        pane: u64,
        item: u64,
        notice: crate::window_state::LaneNotice,
    ) {
        let Some(window) = self.window_by_seq(ws, window) else {
            return;
        };
        if let Some(panel) = window
            .preview_panels
            .get_mut(&pane)
            .filter(|panel| panel.item == item)
        {
            panel.notice = Some(notice);
            window.accessibility_pending = true;
            if let Some(handle) = &window.window {
                handle.request_redraw();
            }
        }
    }

    /// Open, in this window, the previews the user asked for that are now
    /// on their pane's shelf, and forget those for windows that closed.
    pub(super) fn open_ready_previews(&mut self, ws: &mut WindowState) {
        let seq = ws.seq;
        let ready: Vec<(u64, u64)> = self
            .previews_ready
            .iter()
            .filter(|(window, _, _)| *window == seq)
            .map(|(_, pane, item)| (*pane, *item))
            .collect();
        let windows = &self.windows;
        self.previews_ready
            .retain(|(window, _, _)| *window != seq && windows.contains_key(window));
        for (pane, item) in ready {
            if ws.mux.panes.get(&pane).is_some_and(|state| !state.closed) {
                self.show_media_item(ws, pane, item);
            }
        }
    }

    /// Preview the local file a link from pane `pane`'s output names in that
    /// pane's lane. A link from a pane that may be remote, or has gone, is
    /// refused as opening it is. The pane's origin is read now, when the user
    /// acts.
    pub(super) fn preview_pane_link(&mut self, ws: &mut WindowState, pane: u64, uri: &str) {
        let tr = self.ui_text;
        let origin = ws.mux.panes.get(&pane).map(pane_path_origin);
        let Some(path) = kettle_core::links::local_file_path(uri) else {
            return notify_preview_failure(tr, Some(kettle_media::FailureCode::UnsupportedMedia));
        };
        match link_gate(uri, origin) {
            LinkGate::Open => self.pull_preview(ws, pane, &path, None),
            LinkGate::Refuse => fire_notify(
                tr.text(kettle_i18n::Text::NotifyTitleRemoteFileNotOpened),
                &tr.notify_body_remote_file_not_opened(&link_display_name(uri, 48)),
            ),
        }
    }

    /// What the clipboard names for `pane`'s lane, read now.
    pub(super) fn copied_preview(&mut self, ws: &WindowState, pane: u64) -> Option<CopiedPreview> {
        let files = self.clipboard_file_paste_paths().unwrap_or_default();
        let text = files
            .is_empty()
            .then(|| self.clipboard.as_mut().and_then(|c| c.get_text().ok()))
            .flatten();
        let cwd = ws
            .mux
            .panes
            .get(&pane)
            .and_then(|state| state.term.current_dir_or_native());
        copied_preview(
            &files,
            text.as_deref(),
            cwd.as_deref(),
            crate::mux::home_dir_string().as_deref(),
        )
    }

    /// Preview, in the focused pane's lane, the Mermaid diagram the user
    /// copied (`/copy` in Claude Code or Codex) or, with `selection`,
    /// selected in a pane: the source taken back from what the agent's UI
    /// added and wrapped, rendered as the user's own pull. The first of
    /// several copied diagrams shows. A control client cannot have the
    /// clipboard or a selection read.
    pub(super) fn preview_copied_diagram(&mut self, ws: &mut WindowState, selection: bool) {
        use kettle_core::DiagramCopyError as E;
        use kettle_i18n::Text as T;
        if self.ctl_driving {
            log::info!("preview: a control client cannot preview copied or selected text");
            return;
        }
        let tr = self.ui_text;
        let pane = if selection {
            ws.selecting_pane.or_else(|| ws.mux.active_focus())
        } else {
            ws.mux.active_focus()
        };
        let Some(pane) = pane else {
            return;
        };
        let sources = if selection {
            self.selected_diagram(ws, pane)
        } else {
            self.copied_diagram()
        };
        let sources = match sources {
            Ok(sources) => sources,
            Err(E::TooLarge) => {
                return notify_preview_failure(tr, Some(kettle_media::FailureCode::TooLarge));
            }
            Err(error) => {
                let body = match error {
                    E::Wrapped => T::NotifyBodyPreviewDiagramWrapped,
                    E::Unbounded => T::NotifyBodyPreviewDiagramUnbounded,
                    E::TooMany => {
                        return fire_notify(
                            tr.text(T::NotifyTitlePreviewNothing),
                            &tr.notify_body_preview_diagram_too_many(
                                kettle_core::MAX_COPIED_DIAGRAMS as u64,
                            ),
                        );
                    }
                    _ => T::NotifyBodyPreviewNoDiagram,
                };
                return fire_notify(tr.text(T::NotifyTitlePreviewNothing), tr.text(body));
            }
        };
        let count = sources.len();
        let title = match (selection, count > 1) {
            (true, false) => tr.text(T::MediaShelfSelectedDiagram).to_owned(),
            (true, true) => tr.media_shelf_selected_diagrams(count as u64),
            (false, false) => tr.text(T::MediaShelfCopiedDiagram).to_owned(),
            (false, true) => tr.media_shelf_copied_diagrams(count as u64),
        };
        let Some(first) = sources.first().cloned() else {
            return;
        };
        if let Some(failure) = media_unavailable(self.startup.media.as_deref()) {
            return notify_preview_failure(tr, Some(failure));
        }
        // Several are one gallery, each charged on its own, whose first is
        // rendered; the item keeps them all.
        let gallery = match count {
            1 => None,
            _ => match crate::media::Gallery::new(sources) {
                Some(gallery) => Some(Arc::new(gallery)),
                None => {
                    return notify_preview_failure(tr, Some(kettle_media::FailureCode::OverBudget));
                }
            },
        };
        self.admit_user_pull_of(
            ws,
            pane,
            kettle_media::JobKind::Mermaid,
            kettle_media::Source::Bytes(first.into_bytes()),
            None,
            title,
            kettle_media::Canvas::Theme,
            gallery,
        );
    }

    /// Put on the clipboard the `codex` function `kettle agent-setup --print`
    /// prints, for the shell new panes start (the configured `shell`, else
    /// the one the PTY picks for a pane with no command, from the configured
    /// `env`'s `SHELL`, Kettle's own or the user's passwd shell), and say
    /// so; or say why there is none to copy.
    pub(super) fn copy_agent_setup(&mut self) {
        use kettle_i18n::Text as T;
        let tr = self.ui_text;
        let not_copied =
            |body: T| fire_notify(tr.text(T::NotifyTitleAgentSetupNotCopied), tr.text(body));
        let program = match self.cfg.shell.clone() {
            Some(shell) => Some(std::ffi::OsString::from(shell)),
            #[cfg(unix)]
            None => {
                let configured = self
                    .cfg
                    .env
                    .iter()
                    .rev()
                    .find(|(name, _)| name == "SHELL")
                    .map(|(_, value)| std::ffi::OsStr::new(value.as_str()));
                Some(kettle_core::default_pane_shell(configured).into())
            }
            #[cfg(not(unix))]
            None => None,
        };
        let shell = program
            .as_deref()
            .map(std::path::Path::new)
            .and_then(crate::codex_shell::CodexShell::from_program)
            .filter(|_| cfg!(unix));
        let Some(shell) = shell else {
            return not_copied(T::NotifyBodyAgentSetupShell);
        };
        let Ok(kettle) = std::env::current_exe() else {
            return not_copied(T::NotifyBodyAgentSetupUnnamed);
        };
        let function = match crate::codex_shell::codex_function(&kettle, shell) {
            Ok(function) => function,
            Err(crate::codex_shell::FunctionError::Translocated) => {
                return not_copied(T::NotifyBodyAgentSetupTranslocated);
            }
            Err(crate::codex_shell::FunctionError::UnnamedKettle) => {
                return not_copied(T::NotifyBodyAgentSetupUnnamed);
            }
        };
        let copied =
            self.clipboard
                .as_mut()
                .is_some_and(|clipboard| match clipboard.set_text(function) {
                    Ok(()) => true,
                    Err(error) => {
                        log::warn!("clipboard set_text failed (agent setup): {error}");
                        false
                    }
                });
        if !copied {
            return not_copied(T::NotifyBodyAgentSetupClipboard);
        }
        fire_notify(
            tr.text(T::NotifyTitleAgentSetupCopied),
            &tr.notify_body_agent_setup_copied(shell.name()),
        );
    }

    /// The Mermaid sources on the clipboard.
    pub(super) fn copied_diagram(&mut self) -> Result<Vec<String>, kettle_core::DiagramCopyError> {
        let text = self
            .clipboard
            .as_mut()
            .and_then(|clipboard| clipboard.get_text().ok());
        kettle_core::diagram_sources(&text.unwrap_or_default(), None)
    }

    /// The Mermaid sources in `pane`'s selection.
    pub(super) fn selected_diagram(
        &self,
        ws: &WindowState,
        pane: u64,
    ) -> Result<Vec<String>, kettle_core::DiagramCopyError> {
        let selected = ws
            .mux
            .panes
            .get(&pane)
            .and_then(|state| state.term.term.lock().ok())
            .map(|term| kettle_core::selected_diagram_sources(&term));
        match selected {
            Some(Ok(Some(sources))) => Ok(sources),
            Some(Err(error)) => Err(error),
            _ => Err(kettle_core::DiagramCopyError::Empty),
        }
    }

    /// Preview, in the focused pane's lane, the file the clipboard names. A
    /// copied path or link meets the pane's gate, as a link in its output
    /// would; a file copied in a file manager is on this computer.
    pub(super) fn preview_copied_file(&mut self, ws: &mut WindowState) {
        let Some(pane) = ws.mux.active_focus() else {
            return;
        };
        match self.copied_preview(ws, pane) {
            Some(CopiedPreview::File(path)) => self.pull_preview(ws, pane, &path, None),
            Some(CopiedPreview::Link(url)) => self.preview_pane_link(ws, pane, &url),
            None => {
                let tr = self.ui_text;
                fire_notify(
                    tr.text(kettle_i18n::Text::NotifyTitlePreviewNothing),
                    tr.text(kettle_i18n::Text::NotifyBodyPreviewNothingCopied),
                );
            }
        }
    }

    /// Read `pane`'s item from its file again, on the user's gesture: the
    /// result replaces the item in place, under its key and title, on the
    /// canvas chosen for its lane, as the user's own. The worker read the file here
    /// before, so the pane's gate for files it names has nothing to add; a
    /// control client still cannot start the read (see `pull_preview`).
    pub(super) fn reload_preview(&mut self, ws: &mut WindowState, pane: u64) {
        let Some((native, reload)) = preview_item(ws, pane)
            .zip(ws.preview_panels.get(&pane))
            .and_then(|(item, panel)| reload_of(item, panel))
        else {
            return;
        };
        if let Some(panel) = ws.preview_panels.get_mut(&pane) {
            panel.notice = None;
        }
        self.pull_preview(ws, pane, &native, Some(reload));
    }

    /// Ask the media worker to render the local file at `path` for `pane`'s
    /// lane: the user's own pull, which waits in the queue's slot for the
    /// user, opens the lane when it is ready, and says why when it cannot.
    pub(super) fn pull_preview(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        path: &std::path::Path,
        reload: Option<ReloadOf>,
    ) {
        // A control client may drive Kettle's UI, but its input is never the
        // user's gesture, so it cannot have a file read on the GUI's word.
        if self.ctl_driving {
            log::info!("preview: a control client cannot start a preview read");
            return;
        }
        let tr = self.ui_text;
        if let Some(failure) = media_unavailable(self.startup.media.as_deref()) {
            return notify_preview_failure(tr, Some(failure));
        }
        let Ok(native) = kettle_media::NativePath::from_path(path) else {
            return notify_preview_failure(tr, Some(kettle_media::FailureCode::FileNotFound));
        };
        // A reload replaces its item in place, under the item's key and
        // title, on the canvas it shows, a gallery at the page it shows; a
        // new pull is keyed by its file.
        let (key, title, canvas, page) = match reload {
            Some(item) => (item.key, item.title, item.canvas, item.page),
            None => (
                crate::media::ShelfKey::Path(native.clone()),
                native
                    .file_name_lossy()
                    .map(|name| crate::media::display_title(&name))
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| tr.text(kettle_i18n::Text::MediaShelfUntitled).into()),
                kettle_media::Canvas::Theme,
                None,
            ),
        };
        let kind = match page {
            Some(index) => kettle_media::JobKind::MarkdownDiagrams { index },
            None => kettle_media::JobKind::Auto,
        };
        let source = kettle_media::Source::user_pull(
            native,
            kettle_media::GuiActionWitness::from_explicit_gui_action(),
        );
        self.admit_user_pull(ws, pane, kind, source, Some(key), title, canvas);
    }

    /// Queue the user's own pull for `pane`'s lane: `source` as `kind`,
    /// under `key` and `title`, on `canvas`. It waits in the queue's slot
    /// for the user, opens the lane when it is ready, and says why when it
    /// cannot. Its caller has refused a control client already.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_user_pull(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        kind: kettle_media::JobKind,
        source: kettle_media::Source,
        key: Option<crate::media::ShelfKey>,
        title: String,
        canvas: kettle_media::Canvas,
    ) {
        self.admit_user_pull_of(ws, pane, kind, source, key, title, canvas, None);
    }

    /// [`Self::admit_user_pull`], for the first of `gallery`'s diagrams when
    /// there is one: its item keeps them all.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_user_pull_of(
        &mut self,
        ws: &mut WindowState,
        pane: u64,
        kind: kettle_media::JobKind,
        source: kettle_media::Source,
        key: Option<crate::media::ShelfKey>,
        title: String,
        canvas: kettle_media::Canvas,
        gallery: Option<Arc<crate::media::Gallery>>,
    ) {
        let tr = self.ui_text;
        let Some((theme, target)) = self.media_surface(ws, pane) else {
            return;
        };
        let Some(spec) = crate::media::JobSpec::from_job(kettle_media::Job {
            kind,
            source,
            theme,
            canvas,
            target,
            fallback_fonts: Vec::new(),
        }) else {
            return notify_preview_failure(tr, Some(kettle_media::FailureCode::OverBudget));
        };
        let deadline = std::time::Instant::now() + kettle_ctl::show::SHOW_SERVER_DEADLINE;
        let push = crate::media::Push::new(
            crate::media::Requester::User,
            crate::media::Route {
                pane,
                window: ws.seq,
                verified: true,
            },
            crate::media::Draft {
                key,
                title,
                provenance: crate::media::Provenance::User,
                inline: None,
                inline_refused: None,
                gallery,
            },
            spec,
            kettle_media::client::RenderControl::with_deadline(deadline),
        );
        self.media.admit(crate::media::Sender::User, deadline, push);
        self.media_tick(ws);
    }

    /// Retire the cards that no longer stand for anything: their harness
    /// exited, their pane closed, or their item left the shelf or was
    /// replaced. Their text stays in the transcript and paints as unknown.
    pub(super) fn retire_cards(&mut self, ws: &mut WindowState) {
        if self.media.cards.is_empty() {
            return;
        }
        let owners = self.media.cards.owners();
        let gone: Vec<kettle_ctl::process::ProcessIdentity> = owners
            .iter()
            .copied()
            .filter(|owner| kettle_ctl::process::identity(owner.pid()) != Ok(*owner))
            .collect();
        // A launched harness's cards also go with its launch's scope; a
        // command-line card has none.
        #[cfg(target_os = "linux")]
        let lapsed: Vec<kettle_ctl::process::ProcessIdentity> = owners
            .iter()
            .copied()
            .filter(|owner| !self.agent_launches.owner_live(*owner))
            .collect();
        #[cfg(target_os = "linux")]
        if !gone.is_empty() || !lapsed.is_empty() {
            self.agent_launches.prune(std::time::Instant::now());
        }
        #[cfg(not(target_os = "linux"))]
        let lapsed: Vec<kettle_ctl::process::ProcessIdentity> = Vec::new();
        // Each live pane's items, by id and generation.
        let shelves: std::collections::HashMap<u64, Vec<(u64, u64)>> = self
            .all_windows(ws)
            .flat_map(|window| window.mux.panes.iter())
            .filter(|(_, pane)| !pane.closed)
            .map(|(&id, pane)| {
                let items = pane
                    .media_shelf
                    .items()
                    .iter()
                    .map(|item| (item.id, item.generation))
                    .collect();
                (id, items)
            })
            .collect();
        let retired = self.media.cards.retire(|card| {
            card.owner_retires(&gone, &lapsed)
                || !shelves
                    .get(&card.pane)
                    .is_some_and(|items| items.contains(&card.item))
        });
        for (pane_id, nonce) in retired {
            if let Some(window) = window_with_pane(ws, &mut self.windows, pane_id)
                && let Some(pane) = window.mux.panes.get_mut(&pane_id)
            {
                pane.inline_cards.remove(nonce);
                if let Some(handle) = &window.window {
                    handle.request_redraw();
                }
            }
        }
    }

    /// Charge rendered pixels to the preview account, releasing the pixels of
    /// the least recently viewed items not on screen while there is no room.
    pub(super) fn admit_preview_pixels(
        &mut self,
        ws: &mut WindowState,
        width: u32,
        height: u32,
        mut pixels: Vec<u8>,
    ) -> Result<kettle_core::ImageData, kettle_media::FailureCode> {
        let visible = media_visible_items(self.all_windows(ws), &self.media.cards);
        loop {
            match kettle_core::ImageData::try_with_budget(
                width,
                height,
                pixels,
                &kettle_core::GraphicsBudget::previews(),
            ) {
                Ok(image) => return Ok(image),
                Err(kettle_core::ImageRefusal::Invalid) => {
                    return Err(kettle_media::FailureCode::RenderResource);
                }
                Err(kettle_core::ImageRefusal::NoRoom(back)) => {
                    if !self.evict_media_pixels(ws, &visible) {
                        return Err(kettle_media::FailureCode::OverBudget);
                    }
                    pixels = back;
                }
            }
        }
    }

    /// Take a lane's render of its item on another canvas. It replaces the
    /// item's pixels only while the item is the one asked about, unreplaced,
    /// and what was read is the source the item came from: a file changed
    /// since says so in the lane instead. The item keeps its place, id and
    /// generation; its cards show the new pixels.
    pub(super) fn finish_lane_render(
        &mut self,
        ws: &mut WindowState,
        render: crate::media::LaneRender,
        spec: crate::media::JobSpec,
        result: Result<kettle_media::RenderOutput, kettle_media::client::RenderError>,
    ) {
        use crate::window_state::LaneNotice;
        use kettle_media::FailureCode;
        use kettle_media::client::RenderError;
        if let Some(ticket) = render.preview {
            return self.finish_lane_preview(ws, render, ticket, spec, result);
        }
        if let Some(ticket) = render.tile {
            return self.finish_lane_crop(ws, render, ticket, spec, result);
        }
        // A gallery page the lane no longer waits on is dropped unsaid.
        if !self.lane_wants(ws, &render) {
            return;
        }
        let control = self.ctl_driving;
        let tell = |app: &mut Self, ws: &mut WindowState, notice| {
            app.forget_lane_page(ws, &render);
            app.tell_lane(ws, render.window, render.pane, render.item, notice);
        };
        let output = match result {
            Ok(output) => output,
            Err(RenderError::Cancelled) => return self.forget_lane_page(ws, &render),
            Err(RenderError::Failure(FailureCode::Changed)) => {
                self.mark_preview_changed(ws, &render);
                return tell(self, ws, LaneNotice::Changed);
            }
            Err(RenderError::Failure(_)) => return tell(self, ws, LaneNotice::RenderFailed),
        };
        let rendered = output.rendered;
        let verdict = self
            .window_by_seq(ws, render.window)
            .and_then(|window| window.mux.panes.get(&render.pane))
            .map_or(LaneRenderVerdict::Gone, |pane| {
                lane_render_verdict(pane.media_shelf.items(), &render, &rendered.digest)
            });
        match verdict {
            // Replaced or gone: nothing to say.
            LaneRenderVerdict::Gone => return self.forget_lane_page(ws, &render),
            LaneRenderVerdict::Changed => {
                self.mark_preview_changed(ws, &render);
                return tell(self, ws, LaneNotice::Changed);
            }
            LaneRenderVerdict::Same => {}
        }
        let (width, height, layout) = (rendered.width, rendered.height, rendered.layout);
        let warnings = rendered.warnings;
        let video = rendered.video.as_ref().map(|video| video.info);
        let image = match self.admit_preview_pixels(ws, width, height, rendered.rgba) {
            Ok(image) => image,
            Err(_) => return tell(self, ws, LaneNotice::RenderFailed),
        };
        // The item's cards show its new pixels: their posters hold the old
        // ones only weakly, and those go now.
        let cards = self
            .media
            .cards
            .cards_of(render.pane, (render.item, render.generation));
        let Some(window) = self.window_by_seq(ws, render.window) else {
            return;
        };
        let Some(pane) = window.mux.panes.get_mut(&render.pane) else {
            return;
        };
        for nonce in cards {
            pane.inline_cards.set_poster(nonce, Some(&image));
        }
        let Some(item) = pane
            .media_shelf
            .get_mut(render.item)
            .filter(|item| item.generation == render.generation)
        else {
            return;
        };
        // Another page of the gallery becomes the item's source; the page
        // shown, drawn again, stays it.
        let paged = render.page.is_some_and(|ask| {
            item.source.gallery_page().map(|(shown, _)| shown) != Some(ask.page)
        });
        if let Some(ask) = render.page
            && !item.source.select(ask.page)
        {
            return;
        }
        item.pixels = crate::media::ItemPixels::Ready(image);
        item.size = (width, height);
        item.layout = Some(layout);
        item.warnings = warnings;
        if video.is_some() {
            item.video = video;
        }
        item.source.spec.canvas = spec.canvas;
        if let Some(panel) = window
            .preview_panels
            .get_mut(&render.pane)
            .filter(|panel| panel.item == render.item)
        {
            panel.notice = None;
            if render.page.is_some() && page_waited_on(panel, render.generation) == render.page {
                panel.page = None;
            }
            if paged {
                // Another page starts fitted, its source from the top, with
                // no sharper pixels of the last.
                panel.view = kettle_render::MediaViewport::FIT;
                panel.scroll = (0, 0);
                window.lane_crops.remove(&render.pane);
            } else if !panel.view.is_fit() {
                // Sharper pixels for the old canvas no longer serve: a zoomed
                // lane asks again.
                window
                    .lane_crops
                    .entry(render.pane)
                    .or_default()
                    .again(std::time::Instant::now(), control);
            }
        }
        window.accessibility_pending = true;
        if let Some(handle) = &window.window {
            handle.request_redraw();
        }
    }

    /// Take a zoomed lane's sharper pixels: only those its lane asked for
    /// last, for the item it still shows unreplaced, read from the same
    /// source. A file changed since is said in the lane, and nothing more is
    /// read for that item until a reload makes another generation; any
    /// other failure leaves the lane showing its own pixels.
    pub(super) fn finish_lane_crop(
        &mut self,
        ws: &mut WindowState,
        render: crate::media::LaneRender,
        ticket: u64,
        spec: crate::media::JobSpec,
        result: Result<kettle_media::RenderOutput, kettle_media::client::RenderError>,
    ) {
        use kettle_media::FailureCode;
        use kettle_media::client::RenderError;
        let verdict = match &result {
            Ok(output) => self
                .window_by_seq(ws, render.window)
                .and_then(|window| window.mux.panes.get(&render.pane))
                .map_or(LaneRenderVerdict::Gone, |pane| {
                    lane_render_verdict(pane.media_shelf.items(), &render, &output.rendered.digest)
                }),
            Err(RenderError::Failure(FailureCode::Changed)) => LaneRenderVerdict::Changed,
            Err(_) => LaneRenderVerdict::Gone,
        };
        // A changed file is the item's to remember, whether or not its lane
        // still wants these pixels.
        if verdict == LaneRenderVerdict::Changed {
            self.mark_preview_changed(ws, &render);
        }
        let Some(window) = self.window_by_seq(ws, render.window) else {
            return;
        };
        let Some(crop) = window
            .lane_crops
            .get_mut(&render.pane)
            .filter(|crop| crop.asked == Some(ticket))
        else {
            return;
        };
        crop.asked = None;
        match verdict {
            LaneRenderVerdict::Gone => return,
            LaneRenderVerdict::Changed => {
                crop.tile = None;
                return self.tell_lane(
                    ws,
                    render.window,
                    render.pane,
                    render.item,
                    crate::window_state::LaneNotice::Changed,
                );
            }
            LaneRenderVerdict::Same => {}
        }
        let Ok(output) = result else {
            return;
        };
        let rendered = output.rendered;
        let Some(coverage) = crate::media::crop_coverage(&rendered.layout) else {
            return;
        };
        let image_width = rendered.layout.image_in_target.width;
        // Sharper pixels are a nicety: with no room they are not kept, and
        // nothing on a shelf gives way for them.
        let Ok(image) = kettle_core::ImageData::try_with_budget(
            rendered.width,
            rendered.height,
            rendered.rgba,
            &kettle_core::GraphicsBudget::previews(),
        ) else {
            return;
        };
        crop.tile = Some(crate::media::LaneTile {
            item: render.item,
            generation: render.generation,
            canvas: spec.canvas,
            image,
            coverage,
            image_width,
        });
        if let Some(handle) = &window.window {
            handle.request_redraw();
        }
    }

    /// Note that the file `render`'s item came from changed since it was
    /// read: nothing more is read for that item unasked, whatever becomes
    /// of its lane, until a reload replaces it.
    pub(super) fn mark_preview_changed(
        &mut self,
        ws: &mut WindowState,
        render: &crate::media::LaneRender,
    ) {
        // What the lane asked for it and is still waiting reads nothing.
        self.media.withdraw_lane_renders(render.pane, render.item);
        if let Some(item) = self
            .window_by_seq(ws, render.window)
            .and_then(|window| window.mux.panes.get_mut(&render.pane))
            .and_then(|pane| pane.media_shelf.get_mut(render.item))
            .filter(|item| item.generation == render.generation)
        {
            item.source.changed = true;
        }
    }

    /// Publish a finished render to its pane's shelf and answer its push.
    pub(super) fn finish_show(&mut self, ws: &mut WindowState, finished: crate::media::Finished) {
        use kettle_media::FailureCode;
        use kettle_media::client::RenderError;
        let crate::media::Finished { push, result } = finished;
        let tr = self.ui_text;
        let (origin, route, draft, spec) = push.into_parts();
        if let crate::media::Requester::Lane(render) = origin {
            return self.finish_lane_render(ws, render, spec, result);
        }
        let output = match result {
            Ok(output) => output,
            // Cancelled because its pane went, or its client left; the user
            // who asked for a pane that went needs no word.
            Err(RenderError::Cancelled) => {
                let _ = origin.refuse(FailureCode::NotInKettlePane);
                return;
            }
            Err(RenderError::Failure(failure)) => {
                return notify_preview_failure(tr, origin.refuse(failure));
            }
        };
        let kettle_media::RenderOutput { kind, rendered } = output;
        let (width, height) = (rendered.width, rendered.height);
        let layout = rendered.layout;
        let warnings = rendered.warnings;
        let video = rendered.video.as_ref().map(|video| video.info);
        let image = match self.admit_preview_pixels(ws, width, height, rendered.rgba) {
            Ok(image) => image,
            Err(failure) => return notify_preview_failure(tr, origin.refuse(failure)),
        };
        let visible = media_visible_items(self.all_windows(ws), &self.media.cards);
        let item_id = self.media.next_item();
        let crate::media::Draft {
            key,
            title,
            provenance,
            inline,
            mut inline_refused,
            gallery,
        } = draft;
        let textual = matches!(
            kind,
            kettle_media::MediaKind::Svg | kettle_media::MediaKind::Mermaid
        );
        // A gallery's item is the page rendered, with every page kept to show
        // the others from: a Markdown file's is Markdown, as its card and the
        // reply say, and the first of diagrams copied together is Mermaid.
        let (item_kind, source) = if let Some(gallery) = gallery {
            let Some(page) = crate::media::ItemSource::page(&spec, gallery, 0) else {
                return notify_preview_failure(tr, origin.refuse(FailureCode::OverBudget));
            };
            (kettle_media::MediaKind::Mermaid, page)
        } else if kind == kettle_media::MediaKind::Markdown {
            let page = rendered
                .fence_index
                .zip(crate::media::Gallery::new(rendered.fence_sources))
                .and_then(|(index, gallery)| {
                    crate::media::ItemSource::page(&spec, Arc::new(gallery), index)
                });
            let Some(page) = page else {
                return notify_preview_failure(tr, origin.refuse(FailureCode::OverBudget));
            };
            (kettle_media::MediaKind::Markdown, page)
        } else {
            let source = crate::media::ItemSource::new(
                spec,
                rendered.digest,
                textual,
                rendered.exact_source,
            );
            (kind, source)
        };
        let mut item = crate::media::ShelfItem::new(
            item_id,
            key,
            title,
            provenance,
            item_kind,
            warnings.clone(),
            image,
            source,
        );
        item.layout = Some(layout);
        item.video = video;
        let Some(window) = window_with_pane(ws, &mut self.windows, route.pane) else {
            let _ = origin.refuse(FailureCode::NotInKettlePane);
            return;
        };
        let cell = window
            .renderer
            .as_ref()
            .map(|renderer| (renderer.cell_w, renderer.cell_h));
        let Some(pane) = window
            .mux
            .panes
            .get_mut(&route.pane)
            .filter(|pane| !pane.closed && !pane.held)
        else {
            let _ = origin.refuse(FailureCode::NotInKettlePane);
            return;
        };
        #[cfg(target_os = "linux")]
        let inline = inline.filter(|draft| {
            draft.printer.is_some()
                || draft
                    .launch
                    .as_ref()
                    .zip(pane.caller_root)
                    .is_some_and(|(grant, root)| grant.valid(route.pane, root))
        });
        // `kettle show --inline` is checked again now the render is done: the
        // command may have left the foreground, or a program taken the screen.
        let inline = inline.and_then(|draft| {
            let Some(printer) = draft.printer else {
                return Some(draft);
            };
            match crate::ctl_server::CliTerminal::of(printer)
                .ok_or(kettle_ctl::show::InlineRefusal::NotTerminal)
                .and_then(|terminal| cli_card_owner(pane, route.verified, Some(terminal)))
            {
                Ok(_) => Some(draft),
                Err(refusal) => {
                    inline_refused = Some(refusal);
                    None
                }
            }
        });
        let cli = inline.as_ref().is_some_and(|draft| draft.printer.is_some());
        #[cfg(target_os = "linux")]
        let native_pickup = inline
            .as_ref()
            .and_then(|draft| draft.launch.clone())
            .zip(pane.caller_root);
        let shown = window
            .preview_panels
            .get(&route.pane)
            .map(|panel| panel.item);
        // A full shelf never drops an item on screen: the one its pane's lane
        // shows, or one whose card the last frame painted.
        let published = pane.media_shelf.publish(item, shown, &visible);
        // Cards of the item this one replaced, or of the item a full shelf
        // dropped, show what is gone; they never show the new pixels.
        let current = pane
            .media_shelf
            .items()
            .iter()
            .find(|item| item.id == published.id)
            .map(|item| (item.generation, item.image().cloned()));
        let cards = &mut self.media.cards;
        for (pane_id, nonce) in cards.retire(|card| {
            card.pane == route.pane
                && (Some(card.item.0) == published.dropped
                    || (card.item.0 == published.id
                        && current
                            .as_ref()
                            .is_none_or(|(generation, _)| card.item.1 != *generation)))
        }) {
            debug_assert_eq!(pane_id, route.pane);
            pane.inline_cards.remove(nonce);
        }
        // The pane's size, and the column its cursor stands at, where the
        // command line's card will start printing.
        let grid = pane.term.term.lock().ok().map(|term| {
            let grid = term.grid();
            (
                (grid.columns(), grid.screen_lines()),
                grid.cursor.point.column.0,
            )
        });
        let delivery = match (inline, current, cell, grid) {
            (Some(inline), Some((generation, Some(image))), Some(cell), Some((grid, column))) => {
                match register_card(
                    cards,
                    (route.pane, &mut pane.inline_cards, grid, column),
                    cell,
                    (published.id, generation),
                    (kind, video.as_ref()),
                    &image,
                    inline,
                    std::time::Instant::now(),
                ) {
                    Ok(delivery) => Some(delivery),
                    Err(refusal) => {
                        inline_refused = cli.then_some(refusal).or(inline_refused);
                        None
                    }
                }
            }
            _ => None,
        };
        #[cfg(target_os = "linux")]
        let delivery = delivery.and_then(|mut delivery| {
            // The command prints its own card: there is nothing to pick up.
            if cli {
                return Some(delivery);
            }
            let (grant, root) = native_pickup?;
            delivery.permit = Some(self.agent_launches.issue(
                grant,
                root,
                std::time::Instant::now(),
                |bytes| getrandom::fill(bytes).is_ok(),
            )?);
            Some(delivery)
        });
        let seq = window.seq;
        if let Some(handle) = &window.window {
            handle.request_redraw();
        }
        let origin = match origin {
            crate::media::Requester::Ctl(origin) => origin,
            // What the user asked to preview opens in its pane's lane, from
            // that pane's window's next pass of the event loop.
            crate::media::Requester::User => {
                if self.previews_ready.len() < MAX_PREVIEWS_READY {
                    self.previews_ready.push((seq, route.pane, published.id));
                }
                return;
            }
            // Taken by `finish_lane_render` before publishing.
            crate::media::Requester::Lane(_) => return,
        };
        let mut result = kettle_ctl::show::ShowResult::new(
            (route.pane, route.verified, seq),
            published.id,
            kind,
            (width, height),
            &warnings,
        );
        result.inline = delivery;
        result.inline_refused = inline_refused;
        result.video = video.map(kettle_ctl::show::ShowVideo::from);
        let id = origin.request_id;
        origin.answer(kettle_ctl::protocol::Response::ok(
            id,
            serde_json::to_value(result).unwrap_or_default(),
        ));
    }

    /// Release the pixels of the least recently viewed shelf item in any
    /// window that is not on screen. False when there is none.
    pub(super) fn evict_media_pixels(&mut self, ws: &mut WindowState, visible: &[u64]) -> bool {
        let oldest = self
            .all_windows(ws)
            .flat_map(|window| window.mux.panes.iter())
            .filter_map(|(&pane, state)| {
                state
                    .media_shelf
                    .eviction_candidate(visible)
                    .map(|(viewed, item)| (viewed, pane, item))
            })
            .min();
        let Some((_, pane, item)) = oldest else {
            return false;
        };
        window_with_pane(ws, &mut self.windows, pane)
            .and_then(|window| window.mux.panes.get_mut(&pane))
            .is_some_and(|pane| pane.media_shelf.evict_pixels(item))
    }
}

/// The card kind a harness's adapter, or `kettle show --inline`, asks for.
fn printed_by(target: kettle_ctl::show::InlineTarget) -> kettle_render::CardHarness {
    match target {
        kettle_ctl::show::InlineTarget::ClaudeHook => kettle_render::CardHarness::ClaudeHook,
        kettle_ctl::show::InlineTarget::CodexHook => kettle_render::CardHarness::CodexHook,
        kettle_ctl::show::InlineTarget::Cli => kettle_render::CardHarness::Cli,
    }
}

/// Whether `kettle show --inline` may print a card in `pane`, and whose the
/// card is. Its sender must run in the pane (`verified`) and, as `terminal`
/// says, write to the pane's own terminal from its foreground while no
/// full-screen program owns the screen: then the card text reaches the screen
/// and nothing that captures output, and lands under the command. The card
/// is the pane's own child's, so a script's cards outlive the script.
#[cfg(unix)]
pub(super) fn cli_card_owner(
    pane: &crate::mux::Pane,
    verified: bool,
    terminal: Option<crate::ctl_server::CliTerminal>,
) -> Result<kettle_ctl::process::ProcessIdentity, kettle_ctl::show::InlineRefusal> {
    use kettle_ctl::show::InlineRefusal;
    let owner = pane
        .caller_root
        .filter(|_| verified)
        .ok_or(InlineRefusal::Unverified)?;
    let terminal = terminal.ok_or(InlineRefusal::NotTerminal)?;
    let device = pane
        .term
        .tty_path()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| kettle_ctl::process::device_number(&metadata));
    if !device.is_some_and(|device| {
        terminal.output == kettle_ctl::process::StandardOutput::Device(device)
    }) {
        return Err(InlineRefusal::NotTerminal);
    }
    if pane.term.foreground_process_group().ok() != Some(terminal.group) {
        return Err(InlineRefusal::Background);
    }
    let full_screen = pane.term.term.lock().map_or(true, |term| {
        term.mode().contains(kettle_core::TermMode::ALT_SCREEN)
    });
    if full_screen {
        return Err(InlineRefusal::AlternateScreen);
    }
    Ok(owner)
}

/// No command-line cards where Kettle cannot tell a pane's terminal apart.
#[cfg(not(unix))]
pub(super) fn cli_card_owner(
    _: &crate::mux::Pane,
    _: bool,
    _: Option<crate::ctl_server::CliTerminal>,
) -> Result<kettle_ctl::process::ProcessIdentity, kettle_ctl::show::InlineRefusal> {
    Err(kettle_ctl::show::InlineRefusal::Unsupported)
}
