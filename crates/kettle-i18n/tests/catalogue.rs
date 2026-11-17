use kettle_i18n::{Language, Text, Translator};

#[test]
fn every_static_message_has_clean_text_in_both_languages() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    for key in Text::ALL {
        for text in [en.text(*key), es.text(*key)] {
            assert!(!text.is_empty(), "{key:?}");
            assert_eq!(text.trim(), text, "{key:?} has edge whitespace");
            assert!(!text.contains(['{', '}']), "{key:?} has a placeholder");
            assert!(!text.contains('_'), "{key:?} looks like a raw key: {text}");
        }
        // Model-drafted Spanish is a draft, but it is never left as English,
        // except for words Spanish shares.
        let cognate = [
            Text::MacosMenuApp,
            Text::MacosMenuShell,
            // macOS's own Spanish Window menu says Zoom.
            Text::MacosMenuZoomWindow,
            Text::SettingsGpuKindVirtual,
            Text::SettingsGpuKindSoftware,
            Text::MediaViewerKindVideo,
            // Key names Spanish keyboards and screen readers share.
            Text::A11yKeyControl,
            Text::A11yKeyAlt,
            Text::A11yKeyEscape,
            Text::PaletteGroupTerminal,
            // One access-key letter; the Spanish labels share it.
            Text::ConfirmPasteKeyCancel,
            Text::ConfirmPasteKeyPaste,
        ];
        if !cognate.contains(key) {
            assert_ne!(en.text(*key), es.text(*key), "{key:?} is untranslated");
        }
    }
    assert_eq!(en.text(Text::SettingsCategoryAppearance), "Appearance");
    assert_eq!(es.text(Text::SettingsCategoryAppearance), "Apariencia");
    assert_eq!(en.text(Text::SettingsValueOn), "On");
    assert_eq!(es.text(Text::SettingsValueOn), "Activado");
}

#[test]
fn formatted_messages_keep_their_arguments_verbatim() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    assert_eq!(en.settings_heading("Tabs"), "Settings — Tabs");
    assert_eq!(es.settings_heading("Pestañas"), "Configuración — Pestañas");
    assert_eq!(
        en.settings_active_gpu("Apple M5 Max", "IntegratedGpu", "Metal"),
        "Active GPU: Apple M5 Max (IntegratedGpu, Metal)"
    );
    assert_eq!(
        es.settings_active_gpu("Apple M5 Max", "IntegratedGpu", "Metal"),
        "GPU activa: Apple M5 Max (IntegratedGpu, Metal)"
    );
    // Arguments are data: braces, quotes and escapes pass through untouched.
    assert_eq!(
        es.settings_value_not_detected("{name} \"\\\n"),
        "{name} \"\\\n (no detectada)"
    );
    assert_eq!(
        en.settings_value_not_detected("{name} \"\\\n"),
        "{name} \"\\\n (not detected)"
    );
    assert_eq!(
        en.confirm_close_running("a{b}"),
        "The program a{b} is still running. Closing stops it, and unsaved work may be lost."
    );
    // The Spanish sentence opens with the word for "program", which the
    // spelling check reads as an English typo, so it is matched from the name.
    assert!(es.confirm_close_running("a{b}").ends_with(
        " a{b} sigue en ejecución. Al cerrar se detendrá y el trabajo sin guardar puede perderse."
    ));
}

/// Only 1 selects the singular; 0 and every larger count read as plural.
#[test]
fn integer_plural_boundaries() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    for count in [0, 1, 2, 1000, 1_000_000, u64::MAX] {
        let one = count == 1;
        assert_eq!(
            en.confirm_close_tab(count),
            format!(
                "Close tab with {count} {}?",
                if one { "pane" } else { "panes" }
            )
        );
        assert_eq!(
            es.confirm_close_tab(count),
            format!(
                "¿Cerrar la pestaña con {count} {}?",
                if one { "panel" } else { "paneles" }
            )
        );
    }
    for count in [0, 1, 2, 1000, 1_000_000, u64::MAX] {
        let one = count == 1;
        assert_eq!(
            en.confirm_close_window(count),
            format!(
                "Close window with {count} {}?",
                if one { "pane" } else { "panes" }
            )
        );
        assert_eq!(
            es.confirm_close_window(count),
            format!(
                "¿Cerrar la ventana con {count} {}?",
                if one { "panel" } else { "paneles" }
            )
        );
    }
}

/// Config lines, paths and line breaks pass through every language unchanged.
#[test]
fn notifications_keep_config_lines_paths_and_line_breaks() {
    for tr in [Translator::new(Language::En), Translator::new(Language::Es)] {
        assert!(
            tr.notify_body_update_staged_first("v5.0.0")
                .contains("`update-policy = off`")
        );
        assert!(
            tr.notify_body_update_installed_first("v5.0.0")
                .starts_with("v5.0.0 ")
        );
        let ignored = tr.notify_body_config_ignored("/tmp/k {x}.config", "denied");
        assert!(ignored.contains("/tmp/k {x}.config") && ignored.ends_with("\ndenied"));
        let lines = tr.notify_body_config_values_ignored("a = 1\nb = 2");
        assert!(lines.ends_with(":\na = 1\nb = 2"));
        assert!(tr.notify_body_command_result(3, 12, "✓ ok").contains("12"));
    }
}

#[cfg(not(debug_assertions))]
#[test]
fn release_translator_contains_only_shipping_language() {
    assert_eq!(
        std::mem::size_of::<Translator>(),
        std::mem::size_of::<Language>()
    );
}

/// Every Spanish message has a review status, and only a known one; every
/// confirmation and screen-reader message is listed for the second pass.
#[test]
fn every_spanish_message_has_a_review_status() {
    let review: toml::Table = include_str!("../locales/es.review.toml")
        .parse()
        .expect("es.review.toml parses");
    let spanish: toml::Table = include_str!("../locales/es.toml")
        .parse()
        .expect("es.toml parses");
    let status = review["status"].as_table().expect("a [status] table");
    let keys = |table: &toml::Table| table.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        keys(status),
        keys(&spanish),
        "es.review.toml must list every message once"
    );
    for (key, value) in status {
        assert!(
            matches!(value.as_str(), Some("draft" | "accepted")),
            "{key}: status must be \"draft\" or \"accepted\""
        );
    }
    // The second-pass list is exactly the confirmations and screen-reader
    // messages, each once.
    let listed = review["second-pass"]["keys"]
        .as_array()
        .expect("a [second-pass] key list")
        .iter()
        .map(|key| key.as_str().expect("a key name").to_string())
        .collect::<Vec<_>>();
    let expected = spanish
        .keys()
        .filter(|key| {
            key.starts_with("confirm_")
                || key.contains("a11y")
                || key.starts_with("config_notice")
                // These visible notice labels also name live regions and buttons.
                || matches!(
                    key.as_str(),
                    "hint_legend"
                        | "hint_legend_plain"
                        | "link_open_click"
                        | "link_open_command_click"
                        | "link_open_control_click"
                        | "picker_theme_for_light_appearance"
                        | "picker_theme_for_dark_appearance"
                        | "process_exited"
                        | "process_exit_pending"
                        | "process_restart"
                        | "process_close"
                        | "notice_dismiss"
                        | "osc52_copied"
                        | "osc52_copied_by"
                        | "osc52_blocked_read"
                        | "osc52_blocked_write"
                        | "osc52_blocked_read_by"
                        | "osc52_blocked_write_by"
                        | "mouse_override_hint"
                        | "font_size_chip"
                        // The notification toast's Show button is named by
                        // it when the tab has no title, and its overflow line
                        // describes the live status.
                        | "notification_toast_show"
                        | "notification_toast_more"
                        // Palette group headings are also read after the
                        // first command under each.
                        | "palette_group_tabs"
                        | "palette_group_splits"
                        | "palette_group_panes"
                        | "palette_group_windows"
                        | "palette_group_search_and_selection"
                        | "palette_group_scrolling"
                        | "palette_group_text_size"
                        | "palette_group_appearance"
                        | "palette_group_terminal"
                        | "palette_group_broadcast_input"
                        | "palette_group_previews"
                        | "palette_group_layouts_profiles_ssh"
                        | "palette_group_settings"
                        | "palette_group_help"
                        // Palette descriptions are read after their command,
                        // and the relabelled entries are its name.
                        | "palette_desc_split_right"
                        | "palette_desc_split_down"
                        | "palette_desc_split_auto"
                        | "palette_desc_new_tab"
                        | "palette_desc_undo_close_tab"
                        | "palette_desc_duplicate_tab"
                        | "palette_desc_duplicate_pane"
                        | "palette_desc_needs_shell_integration"
                        | "palette_desc_prompt_needs_shell_integration"
                        | "palette_desc_show_help"
                        | "palette_about"
                        | "palette_show_help"
                        | "palette_split_right"
                        | "palette_split_down"
                        | "palette_split_left"
                        | "palette_split_up"
                        | "palette_equalize_splits"
                        // The SSH launcher's status line is also a polite
                        // live region.
                        | "ssh_status_saved"
                        | "ssh_status_removed"
                        | "ssh_status_not_saved"
                        | "ssh_status_not_removed"
                        // The kept-history label is read by screen readers
                        // as the text it shows.
                        | "scrollback_kept"
                        | "scrollback_kept_lines"
                        | "selection_gesture_hint"
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut sorted = listed.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        listed.len(),
        "a second-pass key is listed twice"
    );
    assert_eq!(
        sorted, expected,
        "second-pass keys must be exactly the sensitive ones"
    );
}

/// The Shift hint shows over whatever program has the mouse, so it speaks
/// of that program and the key, never of the product.
#[test]
fn the_mouse_override_hint_does_not_name_the_product() {
    for catalogue in [
        include_str!("../locales/en.toml"),
        include_str!("../locales/es.toml"),
    ] {
        let table: toml::Table = catalogue.parse().unwrap();
        let text = table["mouse_override_hint"]
            .as_str()
            .expect("mouse_override_hint is a plain message");
        assert!(text.contains("Shift"), "{text}");
        assert!(!text.to_lowercase().contains("kettle"), "{text}");
    }
}

/// Prose names the product Kettle: the About row, and a sentence or clause
/// that starts with it. Lowercase `kettle` stays for the command (`kettle
/// update`, "Add kettle to PATH") and the `kettle:` prefix of notification
/// titles.
#[test]
fn prose_names_the_product_with_a_capital() {
    for catalogue in [
        include_str!("../locales/en.toml"),
        include_str!("../locales/es.toml"),
    ] {
        let table: toml::Table = catalogue.parse().unwrap();
        for (key, value) in &table {
            let Some(text) = value.as_str() else {
                continue;
            };
            // Code in backticks (`true; kettle update`) is a command, not prose.
            let prose: String = text.split('`').step_by(2).collect::<Vec<_>>().join(" ");
            for lead in ["About kettle", "Acerca de kettle", ". kettle ", "; kettle "] {
                assert!(!prose.contains(lead), "{key}: {text}");
            }
        }
    }
}

/// No Spanish message is left in English, formatted and plural ones included,
/// except words Spanish shares and a message with no words.
#[test]
fn no_spanish_message_is_left_in_english() {
    let english: toml::Table = include_str!("../locales/en.toml").parse().unwrap();
    let spanish: toml::Table = include_str!("../locales/es.toml").parse().unwrap();
    let cognates = [
        "macos_menu_app",
        "macos_menu_shell",
        "macos_menu_zoom_window",
        "settings_gpu_kind_software",
        "settings_gpu_kind_virtual",
        "media_lane_video_time",
        "media_viewer_kind_video",
        // Key names Spanish keyboards and screen readers share.
        "a11y_key_control",
        "a11y_key_alt",
        "a11y_key_escape",
        "palette_group_terminal",
        // Punctuation and a counter, no words.
        "notification_toast_sourced",
        "notification_toast_repeats",
        // One access-key letter; the Spanish labels share it.
        "confirm_paste_key_cancel",
        "confirm_paste_key_paste",
    ];
    for (key, text) in &english {
        if !cognates.contains(&key.as_str()) {
            assert_ne!(Some(text), spanish.get(key), "{key} is untranslated");
        }
    }
}

/// The Linux desktop entry names Kettle in Spanish too: a launcher in a
/// Spanish session describes and finds it in Spanish, each key beside its
/// English one.
#[test]
fn the_desktop_entry_speaks_spanish() {
    let entry = include_str!("../../../packaging/linux/kettle.desktop");
    let value = |key: &str| {
        entry
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("{key} is missing"))
    };
    for key in ["GenericName", "Comment", "Keywords"] {
        let spanish = value(&format!("{key}[es]"));
        assert!(!spanish.trim().is_empty(), "{key}[es] is empty");
        // "Terminal" is the same word in Spanish.
        if key != "GenericName" {
            assert_ne!(spanish, value(key), "{key}[es] is untranslated");
        }
    }
    assert!(value("Keywords[es]").ends_with(';'));
}

#[test]
fn structured_confirmation_copy_is_localized_and_pluralized() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    assert_eq!(en.text(Text::ConfirmTitleClosePane), "Close this pane?");
    assert_eq!(es.text(Text::ConfirmTitleClosePane), "¿Cerrar este panel?");
    assert_eq!(en.text(Text::ConfirmTitleCloseTab), "Close this tab?");
    assert_eq!(
        es.text(Text::ConfirmTitleCloseWindow),
        "¿Cerrar esta ventana?"
    );
    assert_eq!(en.text(Text::ConfirmButtonCloseWindow), "Close window");
    assert_eq!(es.text(Text::ConfirmButtonCloseTab), "Cerrar pestaña");
    for count in [0, 1, 2, 1000, u64::MAX] {
        assert_eq!(
            en.confirm_close_scope(count),
            if count == 1 {
                "The pane will close.".to_string()
            } else {
                format!("All {count} panes will close.")
            }
        );
        assert_eq!(
            es.confirm_close_scope(count),
            if count == 1 {
                "Se cerrará el panel.".to_string()
            } else {
                format!("Se cerrarán los {count} paneles.")
            }
        );
    }
    assert_eq!(en.confirm_rebind_title("Ctrl+T"), "Reassign Ctrl+T?");
    assert_eq!(es.confirm_rebind_title("Ctrl+T"), "¿Reasignar Ctrl+T?");
    assert_eq!(
        en.confirm_rebind_body("Copy", "Paste"),
        "Assign this shortcut to \"Paste\". \"Copy\" will lose this shortcut."
    );
    assert_eq!(
        es.confirm_rebind_body("Copiar", "Pegar"),
        "Asignar este atajo de teclado a \"Pegar\". \"Copiar\" perderá este atajo."
    );
}

#[test]
fn spanish_pane_group_names_multiple_panes() {
    assert_eq!(
        Translator::new(Language::Es).text(Text::EditTitleGroup),
        "Editar grupo de paneles:"
    );
}

#[test]
fn builtin_theme_names_are_display_only_translations() {
    let es = Translator::new(Language::Es);
    let en = Translator::new(Language::En);
    assert_eq!(es.theme_name("Builtin Dark"), "Oscuro integrado");
    assert_eq!(es.theme_name("Builtin Light"), "Claro integrado");
    for name in [
        "Builtin Dark",
        "Builtin Light",
        "TokyoNight Night",
        "custom",
    ] {
        assert_eq!(en.theme_name(name), name);
    }
    assert_eq!(es.theme_name("custom"), "custom");
}

#[test]
fn settings_descriptions_are_short_and_translated() {
    let en: toml::Table = include_str!("../locales/en.toml").parse().unwrap();
    let es: toml::Table = include_str!("../locales/es.toml").parse().unwrap();
    let mut descriptions = 0;
    for (key, value) in &en {
        if key.starts_with("settings_description_") {
            descriptions += 1;
            assert!(value.as_str().unwrap().chars().count() <= 70, "{key}");
            assert_ne!(value, &es[key], "{key}");
        }
    }
    assert_eq!(
        descriptions, 67,
        "every curated field and keybind has its own description"
    );
}

/// Each Confirm paste access key is one letter of the label it activates and
/// belongs to one control only, in each language. The dialog matches the
/// letter the keyboard layout produces, so a letter missing from its label
/// could not be shown, and a shared one would pick a button by position.
#[test]
fn paste_access_keys_occur_in_their_labels_and_are_unique_per_language() {
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let pairs = [
            (Text::ConfirmPasteKeyCancel, Text::ConfirmButtonCancel),
            (
                Text::ConfirmPasteKeyWithoutControls,
                Text::ConfirmPasteWithoutControls,
            ),
            (Text::ConfirmPasteKeyOneLine, Text::ConfirmPasteOneLine),
            (Text::ConfirmPasteKeyPaste, Text::MenuPaste),
            (Text::ConfirmPasteKeySettings, Text::ConfirmPasteSettings),
        ];
        let mut seen = Vec::new();
        for (key, label) in pairs {
            let key = tr.text(key);
            let label = tr.text(label);
            let mut letters = key.chars();
            let letter = letters.next().expect("an access key");
            assert!(letters.next().is_none(), "{key:?} is not one character");
            assert!(letter.is_alphabetic() && letter.is_uppercase(), "{key:?}");
            assert!(
                label.to_lowercase().contains(&key.to_lowercase()),
                "{language:?}: {key:?} does not occur in {label:?}"
            );
            assert!(
                !seen.contains(&key.to_lowercase()),
                "{language:?}: {key:?} is used twice"
            );
            seen.push(key.to_lowercase());
        }
    }
}

/// The paste reasons name what was found and what Kettle does about it, in
/// both languages, and every findings meaning is a short lower-case phrase.
#[test]
fn paste_reasons_and_findings_are_translated() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    for marker in ["ESC[201~", "CSI 201~"] {
        let reason = en.confirm_paste_reason_marker(marker);
        assert!(reason.starts_with(&format!("This text contains {marker},")));
        assert!(reason.contains("Kettle removes it"));
        let reason = es.confirm_paste_reason_marker(marker);
        assert!(reason.contains(marker) && reason.contains("Kettle lo quita"));
    }
    assert!(
        en.text(Text::ConfirmPasteReasonLines)
            .contains("each line break is sent as Enter")
    );
    assert!(
        es.text(Text::ConfirmPasteReasonLines)
            .contains("cada salto de línea se envía como Enter")
    );
    // A question asked again says first that the answer was not used.
    assert!(
        en.text(Text::ConfirmPasteReasonReasked)
            .contains("after you answered")
    );
    assert!(
        en.text(Text::ConfirmPasteReasonReasked)
            .ends_with("nothing was pasted.")
    );
    assert!(
        es.text(Text::ConfirmPasteReasonReasked)
            .contains("después de que respondiste")
    );
    assert!(
        es.text(Text::ConfirmPasteReasonReasked)
            .ends_with("no se pegó nada.")
    );
    assert_eq!(en.text(Text::ConfirmPasteFindingQuit), "quit (Ctrl+\\)");
    assert_eq!(es.text(Text::ConfirmPasteFindingQuit), "salir (Ctrl+\\)");
    for finding in [
        Text::ConfirmPasteFindingEndMarker,
        Text::ConfirmPasteFindingEndMarkerC1,
        Text::ConfirmPasteFindingInterrupt,
        Text::ConfirmPasteFindingSuspend,
        Text::ConfirmPasteFindingQuit,
        Text::ConfirmPasteFindingEndOfInput,
        Text::ConfirmPasteFindingEscape,
        Text::ConfirmPasteFindingBidi,
        Text::ConfirmPasteFindingBell,
        Text::ConfirmPasteFindingBackspace,
        Text::ConfirmPasteFindingDelete,
        Text::ConfirmPasteFindingControl,
        Text::ConfirmPasteFindingC1,
    ] {
        for text in [en.text(finding), es.text(finding)] {
            assert!(!text.ends_with('.') && !text.contains('`'), "{text}");
            assert!(
                text.chars().next().is_some_and(|c| !c.is_uppercase()),
                "{text}"
            );
        }
    }
    assert_eq!(en.confirm_paste_more_kinds(1), "and 1\u{a0}more kind");
    assert_eq!(es.confirm_paste_more_kinds(3), "y 3\u{a0}tipos más");
    assert_eq!(
        en.confirm_paste_trailing_break("1\u{a0}line"),
        "1\u{a0}line, ends with a line break"
    );
    assert_eq!(
        es.confirm_paste_policy("Siempre"),
        "Protección al pegar: Siempre"
    );
    assert_eq!(
        en.confirm_paste_a11y_finding("^C", "interrupt (Ctrl+C)", 1),
        "^C: interrupt (Ctrl+C), once"
    );
    assert_eq!(
        es.confirm_paste_a11y_finding("^C", "interrupción (Ctrl+C)", 2),
        "^C: interrupción (Ctrl+C), 2 veces"
    );
    assert_eq!(
        en.confirm_paste_a11y_control("Ctrl+C"),
        "control character Ctrl+C"
    );
    assert_eq!(
        es.text(Text::ConfirmPasteA11yEscape),
        "carácter de control escape"
    );
    // A notice names the foreground program without saying it sent the
    // request: Kettle cannot tell which program in the pane did.
    assert_eq!(
        en.osc52_copied_by("vim", 1),
        "A program copied 1 character to the clipboard while vim was in the foreground"
    );
    assert_eq!(
        en.osc52_copied_by("vim", 42),
        "A program copied 42 characters to the clipboard while vim was in the foreground"
    );
    for (count, noun) in [(1, "1 carácter"), (42, "42 caracteres")] {
        assert!(
            es.osc52_copied_by("vim", count).ends_with(&format!(
                "{noun} al portapapeles mientras vim estaba en primer plano"
            )),
            "{count}"
        );
    }
    // A refusal points at the Settings row, by its translated names, and
    // keeps the config key for those who edit the file.
    let (setting, category) = ("Clipboard access by programs", "Behavior");
    for (read, text) in [
        (true, en.osc52_blocked_read(setting, category)),
        (false, en.osc52_blocked_write(setting, category)),
        (true, en.osc52_blocked_read_by("vim", setting, category)),
        (false, en.osc52_blocked_write_by("vim", setting, category)),
    ] {
        assert!(text.starts_with("A program was blocked from "), "{text}");
        assert!(
            text.contains(if read { "reading" } else { "copying" }),
            "{text}"
        );
        assert!(
            text.contains("change Clipboard access by programs in Settings > Behavior"),
            "{text}"
        );
        assert!(text.contains("config key osc52"), "{text}");
        assert!(!text.contains("osc52 ="), "{text}");
    }
    assert!(
        en.osc52_blocked_read_by("vim", "x", "y")
            .contains("while vim was in the foreground")
    );
    assert!(
        es.osc52_blocked_write_by("vim", "x", "y")
            .contains("mientras vim estaba en primer plano")
    );
    assert_eq!(
        en.confirm_paste_finding_removed("interrupt (Ctrl+C)"),
        "interrupt (Ctrl+C); Kettle removes it"
    );
    assert_eq!(
        es.confirm_paste_finding_removed("campana"),
        "campana; Kettle lo quita"
    );
}
