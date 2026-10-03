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
        let cognate = [Text::SettingsGpuKindVirtual, Text::SettingsGpuKindSoftware];
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
    assert_eq!(
        en.confirm_paste(1),
        "Paste 1 line into a shell-like target?"
    );
    assert_eq!(es.confirm_close_window(1), "¿Cerrar 1 panel?");
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
