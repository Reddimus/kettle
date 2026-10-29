use super::*;

/// The title editor's label for `scope`. The overlay paints it and the
/// input-method placement measures it, so both use this.
pub(super) fn title_edit_label(
    scope: TitleEditScope,
    tr: &kettle_i18n::Translator,
) -> &'static str {
    use kettle_i18n::Text;
    tr.text(match scope {
        TitleEditScope::Window => Text::EditTitleWindow,
        TitleEditScope::Tab => Text::EditTitleTab,
        TitleEditScope::Pane => Text::EditTitlePane,
        TitleEditScope::Group => Text::EditTitleGroup,
    })
}

/// The column of the title editor's caret: the painted prefix in the UI's
/// language, the input, then any composition before the focus point.
#[cfg(test)]
pub(super) fn title_edit_caret_cols(
    scope: TitleEditScope,
    input: &str,
    preedit_before_focus: &str,
    tr: &kettle_i18n::Translator,
) -> usize {
    use unicode_width::UnicodeWidthStr as _;
    kettle_render::title_edit_prefix(title_edit_label(scope, tr)).width()
        + input.width()
        + preedit_before_focus.width()
}

/// Where the title-edit overlay is painted.
///
/// Pure so the geometry can be tested.
///
/// Under a VERTICAL tab bar this must span the full window width, not the tab
/// strip. The renderer composes
/// `"  ✎ {label} {input}_   (Enter apply · Esc cancel)"` into
/// `overlay_label_cols(rect.2, cw)`; a strip is `tab-bar-width` wide (default
/// 180px, ~22 columns) and the trailing hint alone is 30 columns, so the budget
/// cannot hold even the prefix. The input would render zero columns (no text,
/// no caret, no horizontal scroll) while Enter still commits the invisible
/// buffer. For `EditPaneGroup` that buffer names the broadcast group, so it
/// decides which panes receive subsequent keystrokes.
pub(super) fn title_edit_rect_for(
    vertical: bool,
    bar_y: f32,
    bar_height: f32,
    content_y: f32,
    sw: f32,
    sh: f32,
    row_h: f32,
) -> kettle_render::Rect4 {
    if bar_height > 0.0 {
        if vertical {
            let height = row_h.max(0.0).min(sh);
            let y = (content_y - height).clamp(0.0, (sh - height).max(0.0));
            (0.0, y, sw, height)
        } else {
            (0.0, bar_y, sw, bar_height)
        }
    } else {
        (0.0, 0.0, sw, row_h)
    }
}

/// Neutralize an OSC-set window/tab title before it reaches
/// the OS titlebar, the tab label, and the status bar. Replaces control
/// characters AND Unicode bidirectional-override format characters (the U+202E
/// titlebar / Alt-Tab spoofing vector) with a space, and caps the length so a
/// pathological program can't set a multi-kilobyte title. Mirrors the
/// notification path's control-stripping + length-cap policy.
pub(super) fn sanitize_title(t: &str) -> String {
    const MAX_TITLE_CHARS: usize = 512;
    t.chars()
        .map(|c| {
            if c.is_control() || is_bidi_format_char(c) {
                ' '
            } else {
                c
            }
        })
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Native window-manager titlebars use the desktop UI font, not kettle's
/// terminal font stack. Strip Unicode Private Use glyphs (Nerd Font icons,
/// Powerline variants, etc.) from the OS title only so GNOME/Ubuntu titlebars
/// do not show tofu boxes while in-app tabs/pane titlebars can still render
/// those glyphs through kettle's embedded font path.
pub(super) fn sanitize_native_window_title(t: &str) -> String {
    let out = t
        .chars()
        .filter(|&c| !is_private_use_char(c))
        .collect::<String>();
    let out = out.trim();
    if out.is_empty() {
        "kettle".to_string()
    } else {
        out.to_string()
    }
}

fn is_private_use_char(c: char) -> bool {
    matches!(
        c,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'
    )
}

/// Unicode bidirectional formatting / override code points. These are NOT
/// `char::is_control()` (they are general category Cf) but enable right-to-left
/// titlebar / Alt-Tab spoofing, so a sanitized title must neutralize them too.
pub(crate) fn is_bidi_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'      // LRE RLE PDF LRO RLO
            | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI
            | '\u{200E}' | '\u{200F}' // LRM RLM
    )
}

/// Render the OS window title from the user's `window-title-format`
/// template with the active pane's title / cwd / 1-based tab index. While
/// the pane title is still the initial placeholder ("kettle") and the cwd
/// is known, substitute the cwd's basename so `~/Repos/kettle` shows up
/// as `kettle — kettle` and `~/Documents` as `Documents — kettle`,
/// matching the cwd-basename tab-title fallback so the OS window title and
/// the in-app tab agree pre-OSC 2. Real shell-set titles still win the
/// moment they arrive.
pub(super) fn window_title(template: &str, pane_title: &str, cwd: &str, tab: usize) -> String {
    let home = crate::mux::home_dir_string();
    window_title_with_home(template, pane_title, cwd, home.as_deref(), tab)
}

pub(super) fn window_title_with_home(
    template: &str,
    pane_title: &str,
    cwd: &str,
    home: Option<&str>,
    tab: usize,
) -> String {
    let t_raw = pane_title.trim();
    let pane_placeholder = t_raw.is_empty() || t_raw == "kettle";
    let cwd_basename = std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty());
    let cwd_display = crate::mux::abbreviate_home(cwd, home);
    // Prefer the cwd basename until the shell names the pane. A directory
    // named "kettle" is real; only the pane title is a placeholder.
    if pane_placeholder {
        if cwd_basename.is_none() && template == kettle_config::DEFAULT_WINDOW_TITLE_FORMAT {
            return "kettle".to_string();
        }
        let tab = tab.to_string();
        return kettle_config::template::fill(
            template,
            &[
                ("title", cwd_basename.unwrap_or("kettle")),
                ("cwd", &cwd_display),
                ("tab", &tab),
            ],
        );
    }
    if !cwd.is_empty()
        && let Some(label) = crate::mux::cwd_label_for_shell_title(t_raw, cwd, home)
        && let Some(path) = label.path.as_deref()
    {
        let tab = tab.to_string();
        return kettle_config::template::fill(
            template,
            &[("title", path), ("cwd", &cwd_display), ("tab", &tab)],
        );
    }
    let tab = tab.to_string();
    kettle_config::template::fill(
        template,
        &[("title", t_raw), ("cwd", &cwd_display), ("tab", &tab)],
    )
}

/// Truncate a status-bar title to `max` chars, appending `…` so the elision
/// is visible. Counts chars, not bytes, so UTF-8 multibyte glyphs aren't split.
/// A title that already fits is returned unchanged.
///
/// Without this, a long pane title wraps past the status strip's 1-cell
/// height and the rest is hidden with no indication.
pub(super) fn cap_title_for_status_bar(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let mut out: String = title.chars().take(max).collect();
    out.push('…');
    out
}

/// Terminator parity ("Read only"):
/// the per-pane titlebar label, composed from the pane's state badges —
/// `[RO] ` while the pane is read-only (input dropped before the PTY), then
/// the `agent-badge` when an agent control connection has the pane attached.
/// Pure (unit-tested). No badges → the title unchanged (zero cost for the
/// common case).
#[cfg(test)]
pub(super) fn compose_pane_title(
    badge: &str,
    attached: bool,
    read_only: bool,
    title: &str,
) -> String {
    let prefix = pane_title_prefix(badge, attached, read_only);
    if prefix.is_empty() {
        title.to_string()
    } else {
        format!("{prefix}{title}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneTitleParts {
    pub(super) prefix: String,
    pub(super) title: String,
    pub(super) path: Option<String>,
}

/// The pane titlebar's mark for media on the pane's shelf that the user has
/// not looked at yet: `▣` and how many, or nothing.
pub(super) fn media_shelf_badge(unseen: usize) -> String {
    if unseen == 0 {
        String::new()
    } else {
        format!("▣{unseen} ")
    }
}

fn pane_title_prefix(badge: &str, attached: bool, read_only: bool) -> String {
    let agent = attached && !badge.is_empty();
    if !read_only && !agent {
        return String::new();
    }
    let mut out = String::new();
    if read_only {
        out.push_str("[RO] ");
    }
    if agent {
        out.push_str(badge);
    }
    out
}

pub(super) fn pane_title_parts(
    agent_badge: &str,
    attached: bool,
    read_only: bool,
    pane_title: &str,
    placeholder: bool,
    cwd: Option<&str>,
    home: Option<&str>,
) -> PaneTitleParts {
    let prefix = pane_title_prefix(agent_badge, attached, read_only);
    let raw = pane_title.trim();
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        if placeholder || raw.is_empty() {
            let path = crate::mux::abbreviate_home(cwd, home);
            let title = cwd
                .rsplit(['/', '\\'])
                .find(|s| !s.is_empty())
                .unwrap_or(raw)
                .to_string();
            return PaneTitleParts {
                prefix,
                title,
                path: Some(path),
            };
        }
        if let Some(label) = crate::mux::cwd_label_for_shell_title(raw, cwd, home) {
            return PaneTitleParts {
                prefix,
                title: label.text,
                path: label.path,
            };
        }
    }
    PaneTitleParts {
        prefix,
        title: if raw.is_empty() {
            "kettle".to_string()
        } else {
            raw.to_string()
        },
        path: None,
    }
}

/// Does this OSC 2 title look like the bogus full-exe-path that
/// conhost/ConPTY injects at startup for a native Windows shell, rather than a
/// title the program set deliberately? When a stock `pwsh`/`cmd` launches,
/// ConPTY seeds the window title with the launched executable's absolute path
/// (e.g. `C:\Program Files\…\pwsh.exe`); kettle would otherwise show that ugly
/// path and never fall back to the working directory.
///
/// Returns true when the title is that injected launch path:
///   - with an explicit launch `argv`, the title must equal argv0 (case-
///     insensitive, `/`+`\` normalized) OR be an absolute `X:\…\<leaf>.exe`
///     whose leaf matches argv0's leaf — tight, so a program that sets its own
///     title to some other path is never mistaken for the injection;
///   - for the default shell (empty `argv`, argv0 unknown), any absolute
///     `X:\…\*.exe` path qualifies — safe because the caller only consults this
///     while the pane title is still the placeholder (the first title seen), and
///     a real shell title is virtually never a bare absolute `.exe` path.
///
/// Pure (unit-tested). Never fires on Unix/macOS — those shells don't emit an
/// absolute-Windows-path title and conhost doesn't exist there.
pub(super) fn suppress_startup_title(title: &str, argv: &[String], windows: bool) -> bool {
    windows && is_conhost_startup_title(title, argv)
}

pub(super) fn is_conhost_startup_title(title: &str, argv: &[String]) -> bool {
    let t = title.trim().trim_matches('"');
    if t.is_empty() {
        return false;
    }
    let tl = t.replace('/', "\\").to_ascii_lowercase();
    let looks_abs_exe = {
        let b = tl.as_bytes();
        b.len() >= 3 && b[1] == b':' && b[2] == b'\\' && tl.ends_with(".exe")
    };
    let leaf = |s: &str| {
        s.rsplit(['/', '\\'])
            .find(|p| !p.is_empty())
            .unwrap_or(s)
            .to_ascii_lowercase()
    };
    match argv.first().map(String::as_str) {
        Some(arg0) => {
            let a_n = arg0.replace('/', "\\").to_ascii_lowercase();
            tl == a_n || (looks_abs_exe && leaf(t) == leaf(arg0))
        }
        None => looks_abs_exe,
    }
}

/// May a shell-emitted OSC 0/2 title replace the pane's current one?
///
/// Everything except a hand-set title, which the user typed and expects to
/// stay. Shells emit OSC 0/2 on every prompt, so without this rule a manual
/// name survived less than a second.
///
/// A REMOTE title still takes precedence over a manual one, deliberately:
/// `apply_remote_title_transition` saves the previous title and restores it
/// when the remote context ends, so the manual name comes back on exit rather
/// than being lost, and while connected the host is worth showing.
pub(super) fn osc_title_may_replace(origin: PaneTitleOrigin) -> bool {
    origin != PaneTitleOrigin::Manual
}

/// Apply a shell-driven title change: `Some(t)` for an OSC 0/2 set, `None`
/// for a reset.
///
/// Both doors need the same two rules, which is why they share one function:
///
/// * A `Manual` title is the user's, and the shell may not touch it by either
///   door. Shells also emit reset sequences at prompts, so gating only the set
///   would keep a hand-set name alive only until the next reset.
/// * Inside a remote context the change IS applied (the remote shell's own
///   title, `user@host:~/work`, beats kettle's synthetic label), but the origin
///   stays `Remote` and the saved pre-remote title is left alone. Overwriting
///   the saved title would lose the title to restore on disconnect. Demoting
///   the origin would stop `apply_remote_title_transition` from recognising the
///   pane as leaving a remote context, leaving the remote host's name on a
///   local pane after ssh exits.
pub(super) fn apply_shell_title(
    title: &mut String,
    title_is_placeholder: &mut bool,
    origin: &mut PaneTitleOrigin,
    title_before_remote: &mut Option<(String, PaneTitleOrigin)>,
    new: Option<&str>,
) {
    if !osc_title_may_replace(*origin) {
        return;
    }
    let in_remote = *origin == PaneTitleOrigin::Remote;
    match new {
        // Sanitize before storing; this string flows to
        // set_title(), the tab label, and the status bar.
        Some(t) => {
            *title = sanitize_title(t);
            *title_is_placeholder = false;
        }
        None => {
            *title = "kettle".into();
            *title_is_placeholder = true;
        }
    }
    if in_remote {
        return;
    }
    *origin = match new {
        Some(_) => PaneTitleOrigin::Osc,
        None => PaneTitleOrigin::Placeholder,
    };
    *title_before_remote = None;
}

pub(super) fn apply_remote_title_transition(
    title: &mut String,
    title_is_placeholder: &mut bool,
    origin: &mut PaneTitleOrigin,
    title_before_remote: &mut Option<(String, PaneTitleOrigin)>,
    remote: Option<&kettle_remote::RemoteContext>,
) {
    if let Some(context) = remote {
        if *origin != PaneTitleOrigin::Remote {
            *title_before_remote = Some((title.clone(), *origin));
        }
        *title = sanitize_title(&kettle_remote::format_remote_title(context));
        *title_is_placeholder = false;
        *origin = PaneTitleOrigin::Remote;
        return;
    }

    if *origin == PaneTitleOrigin::Remote {
        let (restored_title, restored_origin) = title_before_remote
            .take()
            .unwrap_or_else(|| ("kettle".to_string(), PaneTitleOrigin::Placeholder));
        *title = restored_title;
        *title_is_placeholder = restored_origin == PaneTitleOrigin::Placeholder;
        *origin = restored_origin;
    } else {
        *title_before_remote = None;
    }
}

/// What the title-edit overlay edits (Terminator parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleEditScope {
    /// Edit the OS window title (winit Window::set_title).
    Window,
    /// Edit the tab active when the overlay opens (overrides what the
    /// tab-bar shows independent of any OSC 1/2 from a pane).
    Tab,
    /// Edit the pane focused when the overlay opens (shown in its per-pane
    /// titlebar; the OSC-1 equivalent).
    Pane,
    /// Edit the opening pane's broadcast-group name (Terminator parity).
    /// Writes to pane.group_name. Empty input clears the group.
    Group,
}

#[derive(Debug, Clone)]
pub struct TitleEditState {
    pub scope: TitleEditScope,
    /// Stable identity captured when the edit opens. Pane ids are never reused;
    /// a tab is named by every pane it held so it survives index shifts and one
    /// of its panes exiting.
    pub target: TitleEditTarget,
    /// Current text the user has typed. Pre-filled with the existing
    /// title so the user can edit in place vs starting blank.
    pub(crate) input: crate::text_editor::TextEditor,
    /// When `scope == Group`, which panes Apply writes
    /// to. Single = opening pane only (existing behavior); Tab/Window
    /// = bulk-assign via `Action::GroupTab`/`GroupWindow`.
    pub bulk: GroupBulkScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleEditTarget {
    /// The owning `WindowState`. Window-title edits and window-bulk group edits
    /// deliberately keep live window membership: the state itself cannot move
    /// to another window, and `GroupWindow` still means every pane present in
    /// this window when the name is applied.
    Window,
    /// All pane ids held by the target tab when the edit opened. Resolving any
    /// surviving id finds the same tab after reordering or sibling exits.
    Tab(Vec<u64>),
    /// One pane, used by pane-title and single-pane group edits.
    Pane(u64),
}

pub(super) fn begin_title_edit(
    mux: &Mux,
    scope: TitleEditScope,
    bulk: GroupBulkScope,
    window_title: &str,
) -> Option<TitleEditState> {
    let target = match (scope, bulk) {
        (TitleEditScope::Window, _) | (TitleEditScope::Group, GroupBulkScope::Window) => {
            TitleEditTarget::Window
        }
        (TitleEditScope::Tab, _) | (TitleEditScope::Group, GroupBulkScope::Tab) => {
            let panes = mux.tab_anchor_panes(mux.active);
            if panes.is_empty() {
                return None;
            }
            TitleEditTarget::Tab(panes)
        }
        (TitleEditScope::Pane, _) | (TitleEditScope::Group, GroupBulkScope::Single) => {
            TitleEditTarget::Pane(mux.active_focus()?)
        }
    };
    let input = match (scope, bulk, &target) {
        (TitleEditScope::Window, _, TitleEditTarget::Window) => window_title,
        (TitleEditScope::Tab, _, TitleEditTarget::Tab(panes)) => {
            let idx = mux.tab_index_of_any_pane(panes)?;
            mux.tabs[idx].title_override.as_deref().unwrap_or_default()
        }
        (TitleEditScope::Pane, _, TitleEditTarget::Pane(id)) => mux.panes.get(id)?.title.as_str(),
        (TitleEditScope::Group, GroupBulkScope::Single, TitleEditTarget::Pane(id)) => {
            mux.panes.get(id)?.group_name.as_deref().unwrap_or_default()
        }
        (TitleEditScope::Group, GroupBulkScope::Tab, TitleEditTarget::Tab(_))
        | (TitleEditScope::Group, GroupBulkScope::Window, TitleEditTarget::Window) => "",
        _ => return None,
    };
    // Restored titles can exceed the editor cap. Refuse to open rather than
    // turn a rejected initialization into an empty, committable override.
    if input.len() > crate::modal_input::MAX_MODAL_INPUT_BYTES {
        log::warn!("title exceeds the modal editor byte limit; leaving the title unchanged");
        return None;
    }
    Some(TitleEditState {
        scope,
        target,
        input: input.into(),
        bulk,
    })
}

fn title_edit_target_is_live(mux: &Mux, state: &TitleEditState) -> bool {
    match (state.scope, state.bulk, &state.target) {
        (TitleEditScope::Window, _, TitleEditTarget::Window)
        | (TitleEditScope::Group, GroupBulkScope::Window, TitleEditTarget::Window) => true,
        (TitleEditScope::Tab, _, TitleEditTarget::Tab(panes))
        | (TitleEditScope::Group, GroupBulkScope::Tab, TitleEditTarget::Tab(panes)) => {
            mux.tab_index_of_any_pane(panes).is_some()
        }
        (TitleEditScope::Pane, _, TitleEditTarget::Pane(id))
        | (TitleEditScope::Group, GroupBulkScope::Single, TitleEditTarget::Pane(id)) => {
            mux.panes.contains_key(id)
        }
        _ => false,
    }
}

pub(super) fn clear_stale_title_edit(mux: &Mux, editing: &mut Option<TitleEditState>) -> bool {
    if editing
        .as_ref()
        .is_some_and(|state| !title_edit_target_is_live(mux, state))
    {
        *editing = None;
        true
    } else {
        false
    }
}

pub(super) fn apply_mux_title_edit(mux: &mut Mux, state: &TitleEditState) -> bool {
    let value = state.input.text();
    match (state.scope, state.bulk, &state.target) {
        (TitleEditScope::Window, _, _) => false,
        (TitleEditScope::Tab, _, TitleEditTarget::Tab(panes)) => {
            let Some(idx) = mux.tab_index_of_any_pane(panes) else {
                return false;
            };
            let tab = &mut mux.tabs[idx];
            tab.title_override = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            true
        }
        (TitleEditScope::Pane, _, TitleEditTarget::Pane(id)) => {
            let Some(pane) = mux.panes.get_mut(id) else {
                return false;
            };
            // Empty restores shell-driven titles; anything else pins the name
            // against the next OSC 0/2.
            if value.trim().is_empty() {
                pane.title = "kettle".into();
                pane.title_is_placeholder = true;
                pane.title_origin = PaneTitleOrigin::Placeholder;
            } else {
                pane.title = value.to_string();
                pane.title_is_placeholder = false;
                pane.title_origin = PaneTitleOrigin::Manual;
            }
            true
        }
        (TitleEditScope::Group, bulk, target) => {
            let next = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            match (bulk, target) {
                (GroupBulkScope::Single, TitleEditTarget::Pane(id)) => {
                    let Some(pane) = mux.panes.get_mut(id) else {
                        return false;
                    };
                    pane.group_name = next;
                    true
                }
                (GroupBulkScope::Tab, TitleEditTarget::Tab(panes)) => {
                    let Some(idx) = mux.tab_index_of_any_pane(panes) else {
                        return false;
                    };
                    let ids = mux
                        .tabs
                        .get(idx)
                        .map(|tab| tab.root.leaf_ids())
                        .unwrap_or_default();
                    let applied = !ids.is_empty();
                    for id in ids {
                        if let Some(pane) = mux.panes.get_mut(&id) {
                            pane.group_name = next.clone();
                        }
                    }
                    applied
                }
                (GroupBulkScope::Window, TitleEditTarget::Window) => {
                    for pane in mux.panes.values_mut() {
                        pane.group_name = next.clone();
                    }
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

impl App {
    /// Keep the OS window title in sync with the *active* pane's title —
    /// including after tab/focus switches, not only on OSC title events.
    /// Deduped so it isn't a syscall every frame.
    pub(super) fn sync_window_title(&mut self, ws: &mut WindowState) {
        let want = self.desired_window_title(ws);
        if want != ws.last_title {
            if let Some(w) = &ws.window {
                w.set_title(&want);
            }
            ws.last_title = want;
        }
    }

    pub(super) fn desired_window_title(&self, ws: &WindowState) -> String {
        let pane = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id));
        let title = pane.map(|p| p.title.as_str()).unwrap_or("kettle");
        // OSC 7/9;9 cwd if the shell reported one, else the natively
        // polled cwd — so the OS window title tracks the directory for a stock
        // pwsh/cmd just like the tab label does.
        let cwd = pane
            .and_then(|p| p.term.current_dir_or_native())
            .unwrap_or_default();
        let tab = ws.mux.active + 1;
        let want = match &ws.window_title_override {
            Some(forced) => sanitize_title(forced),
            None => window_title(&self.cfg.window_title_format, title, &cwd, tab),
        };
        // An always-visible recording indicator in the title bar so the
        // recorder is never silently capturing (and `[REC RAW]` when raw
        // keystroke capture is on). Keep it ASCII because Linux window-manager
        // title fonts do not reliably carry symbol glyphs.
        let raw = self.recorder.as_ref().is_some_and(|rec| rec.raw_input());
        let want = recording_window_title(want, self.recording_status(), raw);
        // New media on the focused pane's shelf, in ASCII like the
        // recording mark, until the user opens it.
        let unseen = pane.map_or(0, |pane| pane.media_shelf.unseen());
        let want = if unseen > 0 {
            format!(
                "{} {want}",
                self.ui_text.window_title_new_media(unseen as u64)
            )
        } else {
            want
        };
        let want = sanitize_native_window_title(&want);
        if self.gpu_software_fallback {
            format!(
                "{want} - {}",
                self.ui_text
                    .text(kettle_i18n::Text::WindowTitleSoftwareFallback)
            )
        } else {
            want
        }
    }

    /// Apply the in-progress title edit + clear the
    /// overlay. The scope decides which setter is invoked.
    pub(super) fn apply_title_edit(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.editing_title.take() {
            ws.pending_resize = true;
            match state.scope {
                TitleEditScope::Window => {
                    let value = state.input.text().to_string();
                    // Empty clears the override and restores the
                    // `window-title-format` behaviour; anything else pins it
                    // as the base for `sync_window_title`.
                    ws.window_title_override = if value.trim().is_empty() {
                        None
                    } else {
                        Some(value)
                    };
                    self.sync_window_title(ws);
                }
                _ => {
                    apply_mux_title_edit(&mut ws.mux, &state);
                }
            }
        }
        self.hoover_groups(ws);
    }

    /// Terminator plugin parity (`LuaEvent::TitleChanged`). Walk live panes,
    /// diff each title against `ws.last_emitted_titles`, emit on any
    /// boundary cross. One pass site, regardless of how many
    /// title-mutating sites exist in App.
    ///
    /// O(n_panes) per redraw. Even 100 panes is trivial — a
    /// hash lookup + string compare per entry. A follow-up
    /// can add a "dirty-title" bitset on Mux if pane counts
    /// grow into the thousands.
    pub(super) fn poll_title_event(&mut self, ws: &mut WindowState) {
        // Also run when a ctl subscriber is present, so title changes reach
        // agents even without a Lua engine.
        let has_subscribers = self
            .ctl
            .as_ref()
            .map(|c| c.has_subscribers())
            .unwrap_or(false);
        if self.lua_engine.is_none() && !has_subscribers {
            return;
        }
        let mut changes: Vec<(u64, String)> = Vec::new();
        for (id, p) in ws.mux.panes.iter() {
            let last = ws.last_emitted_titles.get(id);
            if last.map(|s| s.as_str()) != Some(p.title.as_str()) {
                changes.push((*id, p.title.clone()));
            }
        }
        for (id, title) in changes {
            ws.last_emitted_titles.insert(id, title.clone());
            self.fire_lua_event(
                ws,
                crate::LuaEvent::TitleChanged(id, title.clone()),
                "title_changed hook",
            );
            self.ctl_broadcast("title", Some(id), serde_json::json!({"title": title}));
        }
        // Drop title state for panes that have closed so this map
        // can't grow unbounded over a long session of opening/closing panes
        // (it's only ever read for live panes). Covers every close path —
        // keybind, confirm dialog, tab close, reap. The O(1) length guard means
        // the O(n) retain runs only when there are actually stale entries.
        if ws.last_emitted_titles.len() > ws.mux.panes.len() {
            let panes = &ws.mux.panes;
            ws.last_emitted_titles
                .retain(|id, _| panes.contains_key(id));
        }
    }
}
