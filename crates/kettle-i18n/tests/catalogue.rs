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
            Text::SettingsGpuKindVirtual,
            Text::SettingsGpuKindSoftware,
            Text::MediaViewerKindVideo,
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
        .filter(|key| key.starts_with("confirm_") || key.contains("a11y"))
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

/// No Spanish message is left in English, formatted and plural ones included,
/// except words Spanish shares and a message with no words.
#[test]
fn no_spanish_message_is_left_in_english() {
    let english: toml::Table = include_str!("../locales/en.toml").parse().unwrap();
    let spanish: toml::Table = include_str!("../locales/es.toml").parse().unwrap();
    let cognates = [
        "settings_gpu_kind_software",
        "settings_gpu_kind_virtual",
        "media_lane_video_time",
        "media_viewer_kind_video",
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
        "Asignar este atajo a \"Pegar\". \"Copiar\" perderá este atajo."
    );
}
