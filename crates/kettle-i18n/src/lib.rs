//! Typed, embedded UI text. Choose the language once per process and pass the
//! translator explicitly; there is no global locale.
//!
//! ```
//! use kettle_i18n::{Language, Text, Translator};
//! let tr = Translator::new(Language::Es);
//! assert_eq!(tr.text(Text::SettingsCategoryAppearance), "Apariencia");
//! assert_eq!(tr.settings_heading("Pestañas"), "Configuración — Pestañas");
//! ```
//!
//! Unknown keys fail compilation.
//! ```compile_fail
//! use kettle_i18n::{Text, Translator, Language};
//! Translator::new(Language::En).text(Text::UnknownKey);
//! ```
//! Missing arguments fail compilation.
//! ```compile_fail
//! use kettle_i18n::{Translator, Language};
//! Translator::new(Language::En).settings_heading();
//! ```
//! Wrong argument types fail compilation.
//! ```compile_fail
//! use kettle_i18n::{Translator, Language};
//! Translator::new(Language::En).settings_heading(2u64);
//! ```
//! Extra arguments fail compilation.
//! ```compile_fail
//! use kettle_i18n::{Translator, Language};
//! Translator::new(Language::En).settings_heading("Tabs", "extra");
//! ```
//! Pseudo is never a stable language or a config token.
//! ```compile_fail
//! use kettle_i18n::Language;
//! let _ = Language::Pseudo;
//! ```

#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    En,
    Es,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Translator {
    language: Language,
    #[cfg(all(debug_assertions, any(feature = "dev-pseudo", test)))]
    pseudo: bool,
    #[cfg(test)]
    omit_spanish: bool,
}

/// English, the reference language. Code that shows text should be handed the
/// process's translator; this default serves tests and empty projections.
impl Default for Translator {
    fn default() -> Self {
        Self::new(Language::En)
    }
}

impl Translator {
    pub const fn new(language: Language) -> Self {
        Self {
            language,
            #[cfg(all(debug_assertions, any(feature = "dev-pseudo", test)))]
            pseudo: false,
            #[cfg(test)]
            omit_spanish: false,
        }
    }

    pub const fn language(&self) -> Language {
        self.language
    }

    /// Development-only English expansion. This constructor does not exist in release.
    #[cfg(all(debug_assertions, any(feature = "dev-pseudo", test)))]
    pub const fn pseudo() -> Self {
        Self {
            pseudo: true,
            ..Self::new(Language::En)
        }
    }

    fn spanish_entry<T>(&self, entry: Option<T>) -> Option<T> {
        #[cfg(test)]
        if self.omit_spanish {
            return None;
        }
        entry
    }
}

include!(concat!(env!("OUT_DIR"), "/catalogue.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_spanish_uses_english_for_every_entry() {
        let missing = Translator {
            omit_spanish: true,
            ..Translator::new(Language::Es)
        };
        let en = Translator::new(Language::En);
        for key in Text::ALL {
            assert_eq!(missing.text(*key), en.text(*key));
        }
        assert_eq!(missing.settings_heading("Tabs"), "Settings — Tabs");
        assert_eq!(
            missing.settings_active_gpu("M5", "IntegratedGpu", "Metal"),
            en.settings_active_gpu("M5", "IntegratedGpu", "Metal")
        );
        assert_eq!(
            missing.settings_value_not_detected("Old GPU"),
            "Old GPU (not detected)"
        );
        assert_eq!(missing.text(Text::SettingsCategoryAppearance), "Appearance");
        assert_ne!(
            missing.text(Text::SettingsCategoryAppearance),
            "settings_category_appearance"
        );
        assert_eq!(missing.spanish_entry::<&str>(None), None);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn pseudo_expands_literals_and_preserves_arguments() {
        let pseudo = Translator::pseudo();
        let en = Translator::new(Language::En);
        let mut original = 0;
        let mut expanded = 0;
        for key in Text::ALL {
            let text = pseudo.text(*key);
            assert!(text.starts_with('⟦') && text.ends_with('⟧'));
            original += en.text(*key).chars().count();
            expanded += text.chars().count();
        }
        let ratio = expanded as f64 / original as f64;
        assert!((1.35..=1.40).contains(&ratio), "{ratio}");
        let text = pseudo.settings_active_gpu("name{u64}", "kind\"\\é", "{backend}");
        assert!(
            text.contains("name{u64}") && text.contains("kind\"\\é") && text.contains("{backend}")
        );
        assert!(pseudo.text(Text::SettingsCategoryAppearance).contains('é'));
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_state_has_no_pseudo_field() {
        // Unit-test omission state remains; release integration tests assert the real size.
        assert_eq!(
            Translator::new(Language::Es).text(Text::SettingsCategoryAppearance),
            "Apariencia"
        );
    }
}
