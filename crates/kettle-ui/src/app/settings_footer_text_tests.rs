use super::{gpu_kind_text, modal_accessibility_projection, settings_footer_note, settings_hints};
use kettle_i18n::{Language, Translator};

const EN: Translator = Translator::new(Language::En);
const ES: Translator = Translator::new(Language::Es);

#[test]
fn hints_name_the_keys_and_translate_only_their_actions() {
    assert_eq!(
        settings_hints(&EN, true),
        "↑↓/jk field · ←→/hl change · g/G ends · Del reset · Tab category · Esc close"
    );
    assert_eq!(
        settings_hints(&EN, false),
        "↑↓ field · ←→ change · Del reset · Tab category · Esc close"
    );
    assert_eq!(
        settings_hints(&ES, false),
        "↑↓ campo · ←→ cambiar · Del restablecer · Tab categoría · Esc cerrar"
    );
}

/// Screen readers name the Settings dialog and its field list in the UI's
/// language.
#[test]
fn settings_accessibility_names_follow_the_ui_language() {
    let overlay = |categories: Vec<String>| kettle_render::Overlay {
        settings: Some(kettle_render::SettingsOverlay {
            search_query: String::new(),
            search_focused: false,
            search_placeholder: String::new(),
            no_matches: String::new(),
            categories,
            label_cols: 10,
            value_cols: 10,
            active_category: 0,
            background_page: false,
            rows: Vec::new(),
            focused_row: 0,
            title: String::new(),
            hints: String::new(),
            footer_note: None,
        }),
        ..kettle_render::Overlay::default()
    };
    let labels = |overlay: &kettle_render::Overlay, tr: &Translator| {
        let projection =
            modal_accessibility_projection(overlay, (800.0, 600.0), (8.0, 16.0), 1.0, tr);
        let mut labels: Vec<String> = projection
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().map(str::to_string))
            .collect();
        labels.sort();
        labels
    };
    assert_eq!(
        labels(&overlay(vec!["Tabs".into()]), &EN),
        ["Search settings", "Settings fields", "Settings, Tabs"]
    );
    assert_eq!(
        labels(&overlay(vec!["Pestañas".into()]), &ES),
        [
            "Buscar configuración",
            "Campos de configuración",
            "Configuración, Pestañas"
        ]
    );
    assert_eq!(
        labels(&overlay(Vec::new()), &ES),
        [
            "Buscar configuración",
            "Campos de configuración",
            "Configuración"
        ]
    );
}

#[test]
fn gpu_kinds_translate_and_unknown_kinds_pass_through() {
    assert_eq!(gpu_kind_text(&EN, "Integrated"), "Integrated");
    assert_eq!(gpu_kind_text(&ES, "Integrated"), "Integrada");
    assert_eq!(gpu_kind_text(&ES, "Discrete"), "Dedicada");
    assert_eq!(gpu_kind_text(&ES, "Quantum"), "Quantum");
}

#[test]
fn notes_follow_the_ui_language() {
    assert_eq!(
        settings_footer_note(
            &ES,
            None,
            Some("completion-overlay"),
            false,
            false,
            false,
            None
        )
        .as_deref(),
        Some("Se aplica a las shells nuevas.")
    );
}

/// The Agent previews footer names the one fact that explains the row
/// right now.
#[test]
fn agent_previews_note_explains_the_effective_policy() {
    use kettle_config::AgentServer::{Full, Off, ReadOnly};
    use kettle_ctl::CtlPolicy;
    use kettle_i18n::Text;
    let note = super::agent_previews_note;
    // Off, saved off: what toggling does.
    assert_eq!(
        note(false, CtlPolicy::new(Off, false), true, false, false),
        Text::SettingsNoteAgentPreviews
    );
    // On and running: still what toggling does.
    assert_eq!(
        note(true, CtlPolicy::new(ReadOnly, true), true, true, true),
        Text::SettingsNoteAgentPreviews
    );
    // Saved off while this run keeps showing previews.
    assert_eq!(
        note(false, CtlPolicy::new(Off, true), true, true, true),
        Text::SettingsNoteAgentPreviewsPendingOff
    );
    // A launch flag keeps display off, whether or not the row is saved
    // on: turning the row on would change nothing until a restart.
    for saved in [false, true] {
        assert_eq!(
            note(saved, CtlPolicy::new(Off, false), false, false, false),
            Text::SettingsNoteAgentPreviewsLaunchOff
        );
        assert_eq!(
            note(saved, CtlPolicy::new(ReadOnly, false), false, true, true),
            Text::SettingsNoteAgentPreviewsLaunchOff
        );
    }
    // Full control already shows previews, whatever the row says.
    for saved in [false, true] {
        assert_eq!(
            note(saved, CtlPolicy::new(Full, saved), true, true, true),
            Text::SettingsNoteAgentPreviewsFull
        );
    }
    // A server the policy wants but could not start explains itself first.
    assert_eq!(
        note(true, CtlPolicy::new(Full, true), true, false, true),
        Text::SettingsNoteAgentPreviewsUnavailable
    );
    assert_eq!(
        note(true, CtlPolicy::new(Off, true), true, false, true),
        Text::SettingsNoteAgentPreviewsUnavailable
    );
    // Before the first start attempt nothing has failed yet.
    assert_eq!(
        note(true, CtlPolicy::new(Off, true), true, false, false),
        Text::SettingsNoteAgentPreviews
    );
    assert_eq!(
        settings_footer_note(
            &EN,
            None,
            Some("agent-display"),
            false,
            false,
            false,
            Some(Text::SettingsNoteAgentPreviews)
        )
        .as_deref(),
        Some("Turning this on applies now. Turning it off applies when Kettle restarts.")
    );
    // Other rows never show it.
    assert_eq!(
        settings_footer_note(
            &EN,
            None,
            Some("font-size"),
            false,
            false,
            false,
            Some(Text::SettingsNoteAgentPreviews)
        ),
        None
    );
}

/// The Claude Code row's footer names why new panes get no plugin, or
/// else when the setting applies.
#[test]
fn claude_code_note_names_why_new_panes_get_no_plugin() {
    use crate::agent_plugin::PluginRefusal;
    use kettle_i18n::Text;
    let note = super::claude_code_note;
    assert_eq!(note(true, None), Text::SettingsNoteClaudeCodeNewPanes);
    assert_eq!(note(false, None), Text::SettingsNoteClaudeCodeNeedsPreviews);
    for display_on in [true, false] {
        assert_eq!(
            note(display_on, Some(PluginRefusal::SideloadDisabled)),
            Text::SettingsNoteClaudeCodeForbidden
        );
        assert_eq!(
            note(display_on, Some(PluginRefusal::Translocated)),
            Text::SettingsNoteClaudeCodeTranslocated
        );
        assert_eq!(
            note(display_on, Some(PluginRefusal::UnnamedExecutable)),
            Text::SettingsNoteClaudeCodeUnavailable
        );
        assert_eq!(
            note(display_on, Some(PluginRefusal::UnnamedDirectory)),
            Text::SettingsNoteClaudeCodeUnavailable
        );
        assert_eq!(
            note(display_on, Some(PluginRefusal::Unverified)),
            Text::SettingsNoteClaudeCodeUnavailable
        );
        assert_eq!(
            note(display_on, Some(PluginRefusal::Unsupported)),
            Text::SettingsNoteClaudeCodeUnsupported
        );
    }
    assert_eq!(
        settings_footer_note(
            &EN,
            None,
            Some("agent-display-claude-code"),
            false,
            false,
            false,
            Some(Text::SettingsNoteClaudeCodeForbidden)
        )
        .as_deref(),
        Some("Claude Code's managed settings forbid plugins from the environment.")
    );
    // The row with focus picks its own note.
    let src = super::production_source();
    assert!(src.contains(
        "Some(\"agent-display-claude-code\") => Some(claude_code_note(\n                            self.agent_previews_live(),\n                            crate::agent_plugin::refusal(),"
    ));
}

/// The Codex row's footer names why new panes' shells get no `codex`, or
/// else where the setting applies; the row with focus picks it.
#[test]
fn codex_note_names_why_new_panes_get_no_codex() {
    use crate::codex_shell::ShellRefusal;
    use kettle_i18n::Text;
    let note = super::codex_note;
    assert_eq!(note(true, None), Text::SettingsNoteCodexNewPanes);
    assert_eq!(note(false, None), Text::SettingsNoteCodexNeedsPreviews);
    for display_on in [true, false] {
        assert_eq!(
            note(display_on, Some(ShellRefusal::Translocated)),
            Text::SettingsNoteCodexTranslocated
        );
        assert_eq!(
            note(display_on, Some(ShellRefusal::Unavailable)),
            Text::SettingsNoteCodexUnavailable
        );
        assert_eq!(
            note(display_on, Some(ShellRefusal::Unsupported)),
            Text::SettingsNoteCodexUnsupported
        );
    }
    assert!(
        settings_footer_note(
            &EN,
            None,
            Some("agent-display-codex"),
            false,
            false,
            false,
            Some(Text::SettingsNoteCodexNewPanes)
        )
        .is_some_and(|note| note.starts_with("Applies to zsh and fish"))
    );
    let src = super::production_source();
    assert!(src.contains(
        "Some(\"agent-display-codex\") => Some(codex_note(\n                            self.agent_previews_live(),\n                            crate::codex_shell::refusal(),"
    ));
}

/// New panes' shells get `codex` from the first pane on, and a reload
/// that turns the integration or agent previews on or off changes what
/// the next pane gets.
#[test]
fn the_codex_startup_files_are_offered_before_the_first_pane_and_on_reload() {
    let src = super::production_source();
    let run = src
        .split_once("pub fn run_with(mut startup: crate::Options) -> Result<()> {")
        .expect("run_with")
        .1;
    let built = run.find("let mut app = App {").expect("app built");
    let offered = run
        .find("app.reconcile_codex_shell();")
        .expect("startup offers the startup files");
    let first = run
        .find("app.start_first_pane_before_launch()")
        .expect("first pane");
    assert!(built < offered && offered < first);
    let load = src
        .split_once("fn load_reloaded_config(&mut self) -> bool {")
        .expect("load_reloaded_config")
        .1
        .split_once("\n    fn apply_reloaded_config(")
        .expect("end of load_reloaded_config")
        .0;
    let display = load
        .find("self.reconcile_agent_display();")
        .expect("reload reconciles agent display");
    let shell = load
        .find("self.reconcile_codex_shell();")
        .expect("reload reconciles the startup files");
    assert!(display < shell, "the startup files follow the live policy");
    let reconcile = src
        .split_once("fn reconcile_codex_shell(&mut self) {")
        .expect("reconcile_codex_shell")
        .1
        .split_once("\n    }\n")
        .expect("end of reconcile_codex_shell")
        .0;
    let gate = reconcile
        .find("if !self.agent_previews_live() || !(agents.codex || agents.claude) {")
        .expect("gated on both settings");
    let withdrawn = reconcile
        .find("crate::codex_shell::offer(None, None);")
        .expect("off withdraws the startup files");
    let installed = reconcile
        .find("crate::codex_shell::prepare(")
        .expect("on installs them");
    assert!(gate < withdrawn && withdrawn < installed);
}

/// New panes get the plugin from the first one on, and a reload that
/// turns the integration or agent previews on or off changes what the
/// next pane gets.
#[test]
fn the_claude_code_plugin_is_offered_before_the_first_pane_and_on_reload() {
    let src = super::production_source();
    let run = src
        .split_once("pub fn run_with(mut startup: crate::Options) -> Result<()> {")
        .expect("run_with")
        .1;
    let built = run.find("let mut app = App {").expect("app built");
    let offered = run
        .find("app.reconcile_claude_plugin();")
        .expect("startup offers the plugin");
    let first = run
        .find("app.start_first_pane_before_launch()")
        .expect("first pane");
    assert!(built < offered && offered < first);
    let load = src
        .split_once("fn load_reloaded_config(&mut self) -> bool {")
        .expect("load_reloaded_config")
        .1
        .split_once("\n    fn apply_reloaded_config(")
        .expect("end of load_reloaded_config")
        .0;
    let display = load
        .find("self.reconcile_agent_display();")
        .expect("reload reconciles agent display");
    let plugin = load
        .find("self.reconcile_claude_plugin();")
        .expect("reload reconciles the plugin");
    assert!(display < plugin, "the plugin follows the live policy");
    let reconcile = src
        .split_once("fn reconcile_claude_plugin(&mut self) {")
        .expect("reconcile_claude_plugin")
        .1
        .split_once("\n    }\n")
        .expect("end of reconcile_claude_plugin")
        .0;
    let gate = reconcile
        .find("if !(self.agent_previews_live() && self.cfg.agent_display_claude_code) {")
        .expect("gated on both settings");
    let withdrawn = reconcile
        .find("crate::agent_plugin::offer(None, None);")
        .expect("off withdraws the plugin");
    let installed = reconcile
        .find("crate::agent_plugin::install(")
        .expect("on installs it");
    let named = reconcile
        .find("crate::agent_plugin::plugin_dir_entry(&directory)")
        .expect("a directory the variable cannot hold is refused");
    let offered = reconcile
        .find("crate::agent_plugin::offer(Some(plugin), None);")
        .expect("offered");
    let rechecked = reconcile
        .find("crate::mux::claude_config_for(&self.cfg.env)")
        .expect("policy checked again for Settings, as a new pane would be");
    assert!(gate < withdrawn && withdrawn < installed && installed < named);
    assert!(named < offered && offered < rechecked);
    let live = src
        .split_once("fn agent_previews_live(&self) -> bool {")
        .expect("agent_previews_live")
        .1
        .split_once("\n    }\n")
        .expect("end of agent_previews_live")
        .0;
    assert!(live.contains("self.ctl_policy.display()"));
    assert!(live.contains(".resolve(self.cfg.ctl_policy())"));
}

/// Display is turned on by a successful reload only, launch flags keep
/// precedence, and the server starts in exactly one place.
#[test]
fn agent_display_reloads_widen_the_live_policy_once() {
    let src = super::production_source();
    let load = src
        .split_once("fn load_reloaded_config(&mut self) -> bool {")
        .expect("load_reloaded_config")
        .1
        .split_once("\n    fn apply_reloaded_config(")
        .expect("end of load_reloaded_config")
        .0;
    let assigned = load.find("self.cfg = new;").expect("config assigned");
    let reconciled = load
        .find("self.reconcile_agent_display();")
        .expect("reload reconciles agent display");
    assert!(assigned < reconciled, "reconcile the config just loaded");
    let reconcile = src
        .split_once("fn reconcile_agent_display(&mut self) {")
        .expect("reconcile_agent_display")
        .1
        .split_once("\n    }\n")
        .expect("end of reconcile_agent_display")
        .0;
    assert!(reconcile.contains("ctl_overrides(&self.startup).resolve(self.cfg.ctl_policy())"));
    assert!(reconcile.contains("self.ctl_policy.latch_display(requested.display())"));
    assert!(reconcile.contains("Some(ctl) => ctl.enable_display(),"));
    assert_eq!(
        src.matches("crate::ctl_server::CtlServer::start(").count(),
        1,
        "only ensure_ctl_started starts the control server"
    );
    let ensure = src
        .split_once("fn ensure_ctl_started(&mut self) {")
        .expect("ensure_ctl_started")
        .1
        .split_once("\n    }\n")
        .expect("end of ensure_ctl_started")
        .0;
    let marked = ensure
        .find("self.ctl_start = CtlStart::Attempted;")
        .expect("a start is recorded");
    let started = ensure
        .find("crate::ctl_server::CtlServer::start(")
        .expect("ensure starts the server");
    assert!(
        marked < started,
        "a failed start must not be retried by every reload"
    );
    let resumed = src
        .split_once("fn resumed_inner(")
        .expect("resumed_inner")
        .1;
    assert!(resumed.contains("self.ensure_ctl_started();"));
    // The footer asks the launch flags what turning the row on would do.
    let note = src
        .split_once("fn agent_previews_note(&self) -> kettle_i18n::Text {")
        .expect("agent_previews_note")
        .1;
    assert!(
        note[..note.find("\n    }\n").expect("end of agent_previews_note")]
            .contains("ctl_overrides(&self.startup).resolve(turned_on).display()")
    );
}

#[test]
fn ordinary_rows_have_no_note_and_dependencies_are_contextual() {
    assert_eq!(
        settings_footer_note(&EN, None, Some("font-size"), false, false, false, None),
        None
    );
    assert_eq!(
        settings_footer_note(
            &EN,
            None,
            Some("completion-overlay"),
            false,
            false,
            false,
            None
        )
        .as_deref(),
        Some("Applies to new shells.")
    );
    assert_eq!(
        settings_footer_note(&EN, None, Some("window-blur"), false, false, false, None).as_deref(),
        Some("Blur requires background opacity below 100%. Applies to new windows.")
    );
}

#[test]
fn pending_notice_does_not_infer_gpu_changes_from_the_current_category() {
    let pending = "Restart Kettle or open a new window to apply pending changes.";
    for key in ["window-blur", "background-opacity", "gpu", "gpu-backend"] {
        let note = settings_footer_note(&EN, None, Some(key), false, true, false, None).unwrap();
        assert_eq!(note.lines().last(), Some(pending));
        // A blur/opacity edit followed by a switch to Graphics has the
        // same pending flag; an active adapter does not identify its cause.
        let gpu_note = settings_footer_note(
            &EN,
            Some("Active GPU: Test (Integrated, Vulkan)"),
            Some(key),
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert_eq!(gpu_note.lines().last(), Some(pending));
        assert!(!gpu_note.contains("GPU changes"));
    }
    assert_eq!(
        settings_footer_note(
            &EN,
            Some("Active GPU: Test"),
            Some("gpu"),
            false,
            false,
            false,
            None
        )
        .as_deref(),
        Some("Active GPU: Test")
    );
}

#[test]
fn pending_changes_are_never_hidden_by_an_appearance_hint() {
    let note =
        settings_footer_note(&EN, None, Some("window-blur"), false, true, false, None).unwrap();
    assert!(note.contains("below 100%"));
    assert!(note.contains("pending changes"));
    let note = settings_footer_note(
        &EN,
        Some("Active GPU: Test (Integrated, Vulkan)"),
        Some("gpu"),
        false,
        true,
        false,
        None,
    )
    .unwrap();
    assert!(note.contains("Restart Kettle or open a new window to apply pending changes."));
}

/// On screen, each key stays beside what it does and the groups stand apart:
/// the laid-out footer puts more space between one group and the next than
/// between a key and its action, and wraps only between groups.
#[test]
fn the_laid_out_settings_footer_separates_key_groups() {
    use kettle_i18n::Text;
    use unicode_width::UnicodeWidthStr as _;
    let cw = 8.0;
    let mut wrapped = false;
    for tr in [EN, ES] {
        for vim_nav in [false, true] {
            let mut groups = vec![
                (
                    if vim_nav { "↑↓/jk" } else { "↑↓" },
                    Text::SettingsHintField,
                ),
                (
                    if vim_nav { "←→/hl" } else { "←→" },
                    Text::SettingsHintChange,
                ),
            ];
            if vim_nav {
                groups.push(("g/G", Text::SettingsHintEnds));
            }
            groups.extend([
                ("Del", Text::SettingsHintReset),
                ("Tab", Text::SettingsHintCategory),
                ("Esc", Text::SettingsHintClose),
            ]);
            // The release-comparison window, then two narrower ones; the
            // longer hint sets wrap in both.
            for surface in [(1600.0, 1000.0), (800.0, 600.0), (560.0, 600.0)] {
                let set = kettle_render::SettingsOverlay {
                    hints: settings_hints(&tr, vim_nav),
                    ..Default::default()
                };
                let layout = kettle_render::settings_layout(&set, (cw, 16.0), surface, cw);
                let lines: Vec<&str> = layout.footer.iter().map(|(t, _)| t.as_str()).collect();
                // Each group's line and its key, action and end columns.
                let mut placed = Vec::new();
                let mut from = (0, 0);
                for (key, what) in &groups {
                    let action = tr.text(*what);
                    let group = format!("{key} {action}");
                    let (line, at) = lines
                        .iter()
                        .enumerate()
                        .skip(from.0)
                        .find_map(|(i, line)| {
                            let start = if i == from.0 { from.1 } else { 0 };
                            line[start..].find(&group).map(|at| (i, start + at))
                        })
                        .unwrap_or_else(|| panic!("{group:?} is not whole in {lines:?}"));
                    let text = lines[line];
                    let key_end = text[..at + key.len()].width();
                    let action_at = at + key.len() + text[at + key.len()..].find(action).unwrap();
                    placed.push((
                        line,
                        text[..at].width(),
                        key_end,
                        text[..action_at].width(),
                        text[..at + group.len()].width(),
                    ));
                    from = (line, at + group.len());
                }
                let mut pairs = 0;
                for pair in placed.windows(2) {
                    let (line, _, key_end, action_start, end) = pair[0];
                    let (next_line, next_start, ..) = pair[1];
                    let within = (action_start - key_end) as f32 * cw;
                    if line == next_line {
                        let between = (next_start - end) as f32 * cw;
                        assert!(
                            between > within,
                            "{surface:?}: {between} px between groups, {within} px within, in {lines:?}"
                        );
                        pairs += 1;
                    }
                }
                assert!(pairs > 0, "{lines:?}");
                wrapped |= lines.len() > 1;
                for line in &lines {
                    assert!(
                        !line.starts_with('·') && !line.trim_end().ends_with('·'),
                        "{lines:?}"
                    );
                }
            }
        }
    }
    assert!(wrapped, "no surface wrapped the footer");
}

/// Pseudo-locale layout check: the Settings key hints, the widest footer
/// line, fit a default window's 144 columns in every language.
#[cfg(debug_assertions)]
#[test]
fn the_settings_hints_fit_a_default_window_in_every_language() {
    use unicode_width::UnicodeWidthStr as _;
    for tr in [EN, ES, Translator::pseudo()] {
        for vim_nav in [false, true] {
            let hints = settings_hints(&tr, vim_nav);
            assert!(hints.width() <= 144, "{hints:?}");
        }
    }
}

/// The Language row says a change waits for a restart; once the saved
/// language differs from the running one, every row says so, ahead of the
/// general restart note, which a new window would satisfy.
#[test]
fn a_language_change_waits_for_a_restart() {
    assert_eq!(
        settings_footer_note(&EN, None, Some("language"), false, false, false, None).as_deref(),
        Some("Applies when Kettle restarts.")
    );
    assert_eq!(
        settings_footer_note(&ES, None, Some("language"), false, false, true, None).as_deref(),
        Some("Reiniciar Kettle para cambiar el idioma.")
    );
    let note = settings_footer_note(&EN, None, Some("font-size"), false, true, true, None).unwrap();
    assert_eq!(
        note.lines().collect::<Vec<_>>(),
        [
            "Restart Kettle to change the language.",
            "Restart Kettle or open a new window to apply pending changes."
        ]
    );
}

#[test]
fn a_saved_language_is_pending_only_when_it_would_change_the_language() {
    use super::language_change_pending;
    use kettle_config::LanguagePreference as Pref;
    use kettle_i18n::Language;
    let spanish_os = || Some("es_MX.UTF-8".to_string());
    // Started on auto in a Spanish OS: picking Spanish changes nothing.
    assert!(!language_change_pending(
        Pref::Auto,
        Language::Es,
        spanish_os
    ));
    assert!(!language_change_pending(
        Pref::Spanish,
        Language::Es,
        spanish_os
    ));
    assert!(language_change_pending(
        Pref::English,
        Language::Es,
        spanish_os
    ));
    // Started in English with an explicit choice: auto follows the OS.
    assert!(language_change_pending(
        Pref::Auto,
        Language::En,
        spanish_os
    ));
    assert!(!language_change_pending(Pref::Auto, Language::En, || None));
}

/// Where the session cannot blur, the Window blur row says so; the footer
/// on that row and on Background opacity drops the hint about what blur
/// needs, which would contradict it. Pending changes and other rows' notes
/// still show.
#[test]
fn unavailable_blur_drops_the_blur_hint() {
    use kettle_i18n::Text;
    for tr in [EN, ES] {
        for key in ["window-blur", "background-opacity"] {
            assert_eq!(
                settings_footer_note(&tr, None, Some(key), false, false, false, None).as_deref(),
                Some(tr.text(Text::SettingsNoteBlur)),
                "{key}"
            );
            assert_eq!(
                settings_footer_note(&tr, None, Some(key), true, false, false, None),
                None,
                "{key}"
            );
            assert_eq!(
                settings_footer_note(&tr, None, Some(key), true, true, false, None).as_deref(),
                Some(tr.text(Text::SettingsNoteRestart)),
                "{key}"
            );
        }
        assert_eq!(
            settings_footer_note(
                &tr,
                None,
                Some("completion-overlay"),
                true,
                false,
                false,
                None
            )
            .as_deref(),
            Some(tr.text(Text::SettingsNoteNewShells))
        );
    }
}

/// The Settings projection, which paint, pointer hits and screen readers
/// all read, takes the Window blur row's description and the footer from
/// the window's own blur state. No App can be built in a test, so this
/// guards the wiring in the source.
#[test]
fn the_settings_projection_follows_the_windows_blur_state() {
    let src = super::production_source();
    let projection = src
        .split_once("fn settings_overlay_projection(")
        .expect("settings_overlay_projection")
        .1
        .split_once("fn settings_mouse(")
        .expect("end of settings_overlay_projection")
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for wiring in [
        "let blur_unavailable = ws .native_material .as_ref() \
         .is_some_and(|material| material.blur_unavailable());",
        "description: tr .text(crate::settings::row_description(f, blur_unavailable)) \
         .to_string(),",
        "settings_footer_note( tr, active_gpu.as_deref(), \
         active.fields.get(fld).map(|f| f.key), blur_unavailable,",
    ] {
        assert!(projection.contains(wiring), "{wiring}");
    }
    assert!(
        !projection.contains("crate::settings::description("),
        "rows describe themselves through row_description"
    );
}
