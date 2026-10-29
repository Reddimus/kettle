/// A row hint adds dependencies or timing, rather than repeating the control.
/// Whether `saved` would show Kettle in another language than `running`.
/// `os_locale` runs only for `auto`.
pub(super) fn language_change_pending(
    saved: kettle_config::LanguagePreference,
    running: kettle_i18n::Language,
    os_locale: impl FnOnce() -> Option<String>,
) -> bool {
    saved.resolve(os_locale) != running
}

pub(super) fn settings_footer_note(
    tr: &kettle_i18n::Translator,
    active_gpu: Option<&str>,
    focused_key: Option<&str>,
    restart_pending: bool,
    language_pending: bool,
    agent_previews: Option<kettle_i18n::Text>,
) -> Option<String> {
    use kettle_i18n::Text;
    let mut notes = Vec::new();
    if let Some(gpu) = active_gpu {
        notes.push(gpu);
    }
    match focused_key {
        Some("agent-display" | "agent-display-claude-code" | "agent-display-codex") => {
            notes.extend(agent_previews.map(|note| tr.text(note)));
        }
        Some("window-blur" | "background-opacity") => {
            notes.push(tr.text(Text::SettingsNoteBlur));
        }
        Some("completion-overlay") => notes.push(tr.text(Text::SettingsNoteNewShells)),
        // A new window keeps the process's language, unlike the changes the
        // general restart note covers.
        Some("language") if !language_pending => {
            notes.push(tr.text(Text::SettingsNoteLanguage));
        }
        _ => {}
    }
    if language_pending {
        notes.push(tr.text(Text::SettingsNoteLanguagePending));
    }
    if restart_pending {
        // The pending flag records no cause, even when Graphics is selected.
        notes.push(tr.text(Text::SettingsNoteRestart));
    }
    (!notes.is_empty()).then(|| notes.join("\n"))
}

/// What the Claude Code row's footer says: why new panes get no plugin, or
/// else when the setting applies.
pub(super) fn claude_code_note(
    display_on: bool,
    refusal: Option<crate::agent_plugin::PluginRefusal>,
) -> kettle_i18n::Text {
    use crate::agent_plugin::PluginRefusal;
    use kettle_i18n::Text;
    match refusal {
        Some(PluginRefusal::SideloadDisabled) => Text::SettingsNoteClaudeCodeForbidden,
        Some(PluginRefusal::Translocated) => Text::SettingsNoteClaudeCodeTranslocated,
        Some(
            PluginRefusal::UnnamedExecutable
            | PluginRefusal::UnnamedDirectory
            | PluginRefusal::Unverified,
        ) => Text::SettingsNoteClaudeCodeUnavailable,
        Some(PluginRefusal::Unsupported) => Text::SettingsNoteClaudeCodeUnsupported,
        None if !display_on => Text::SettingsNoteClaudeCodeNeedsPreviews,
        None => Text::SettingsNoteClaudeCodeNewPanes,
    }
}

/// What the Codex row's footer says: why new panes' shells get no `codex`,
/// or else where and when the setting applies.
pub(super) fn codex_note(
    display_on: bool,
    refusal: Option<crate::codex_shell::ShellRefusal>,
) -> kettle_i18n::Text {
    use crate::codex_shell::ShellRefusal;
    use kettle_i18n::Text;
    match refusal {
        Some(ShellRefusal::Translocated) => Text::SettingsNoteCodexTranslocated,
        Some(ShellRefusal::Unavailable) => Text::SettingsNoteCodexUnavailable,
        Some(ShellRefusal::Unsupported) => Text::SettingsNoteCodexUnsupported,
        None if !display_on => Text::SettingsNoteCodexNeedsPreviews,
        None => Text::SettingsNoteCodexNewPanes,
    }
}

/// What the Agent previews row's footer says: the one fact that explains
/// the row's effect right now, most surprising first. `launch_allows` is
/// whether this launch's flags let the row turn display on at all.
pub(super) fn agent_previews_note(
    saved: bool,
    policy: kettle_ctl::CtlPolicy,
    launch_allows: bool,
    server_running: bool,
    start_attempted: bool,
) -> kettle_i18n::Text {
    use kettle_i18n::Text;
    if policy.runs_server() && !server_running && start_attempted {
        Text::SettingsNoteAgentPreviewsUnavailable
    } else if policy.server() == kettle_config::AgentServer::Full {
        Text::SettingsNoteAgentPreviewsFull
    } else if !launch_allows && !policy.display() {
        Text::SettingsNoteAgentPreviewsLaunchOff
    } else if !saved && policy.display() {
        Text::SettingsNoteAgentPreviewsPendingOff
    } else {
        Text::SettingsNoteAgentPreviews
    }
}

/// The Settings footer: each key followed by what it does. The keys are the
/// same in every language; the vim keys show when `vim-menu-nav` is on.
pub(super) fn settings_hints(tr: &kettle_i18n::Translator, vim_nav: bool) -> String {
    use kettle_i18n::Text;
    let hints: &[(&str, Text)] = if vim_nav {
        &[
            ("↑↓/jk", Text::SettingsHintField),
            ("←→/hl", Text::SettingsHintChange),
            ("g/G", Text::SettingsHintEnds),
            ("Tab", Text::SettingsHintCategory),
            ("Esc", Text::SettingsHintClose),
        ]
    } else {
        &[
            ("↑↓", Text::SettingsHintField),
            ("←→", Text::SettingsHintChange),
            ("Tab", Text::SettingsHintCategory),
            ("Esc", Text::SettingsHintClose),
        ]
    };
    hints
        .iter()
        .map(|(keys, what)| format!("{keys} {}", tr.text(*what)))
        .collect::<Vec<_>>()
        .join("    ")
}

/// A GPU kind from the renderer's adapter info, in the UI's language. An
/// unknown kind shows as the renderer gave it.
pub(super) fn gpu_kind_text<'a>(tr: &kettle_i18n::Translator, kind: &'a str) -> &'a str {
    use kettle_i18n::Text;
    match kind {
        "Discrete" => tr.text(Text::SettingsGpuKindDiscrete),
        "Integrated" => tr.text(Text::SettingsGpuKindIntegrated),
        "Virtual" => tr.text(Text::SettingsGpuKindVirtual),
        "Software" => tr.text(Text::SettingsGpuKindSoftware),
        "Other" => tr.text(Text::SettingsGpuKindOther),
        kind => kind,
    }
}
