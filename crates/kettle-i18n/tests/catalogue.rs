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

#[cfg(not(debug_assertions))]
#[test]
fn release_translator_contains_only_shipping_language() {
    assert_eq!(
        std::mem::size_of::<Translator>(),
        std::mem::size_of::<Language>()
    );
}
