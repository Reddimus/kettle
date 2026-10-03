//! Matching the operating system's locale to a shipping language.

use crate::Language;

/// The operating system's preferred UI locale, such as `es-MX` or
/// `es_ES.UTF-8`, or `None` when it reports none. The native query runs at
/// most once per process, so the startup notice and the GUI agree; call this
/// only when the configured language is `auto`.
pub fn os_locale() -> Option<String> {
    static OS_LOCALE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    query_once(&OS_LOCALE, sys_locale::get_locale)
}

/// The value in `cell`, running `query` only if the cell is empty.
fn query_once(
    cell: &std::sync::OnceLock<Option<String>>,
    query: impl FnOnce() -> Option<String>,
) -> Option<String> {
    cell.get_or_init(query).clone()
}

/// The shipping language for a locale tag. A well-formed Spanish tag of any
/// region, in Latin script, selects Spanish; English, other languages, `C`,
/// `POSIX` and anything malformed select English. Parsing is bounded and
/// allocates nothing.
pub fn language_for_locale(locale: &str) -> Language {
    if locale.is_empty() || locale.len() > 96 || !locale.is_ascii() {
        return Language::En;
    }
    let tag_end = locale.find(['.', '@']).unwrap_or(locale.len());
    let tag = &locale[..tag_end];
    if !valid_suffix(&locale[tag_end..]) || !valid_tag(tag) {
        return Language::En;
    }
    let primary = tag.split(['-', '_']).next().unwrap_or("");
    if primary.eq_ignore_ascii_case("es") {
        Language::Es
    } else {
        Language::En
    }
}

/// A POSIX `.encoding` and `@modifier` suffix, each one word.
fn valid_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
        return true;
    }
    let mut rest = suffix;
    if let Some(encoding) = rest.strip_prefix('.') {
        let end = encoding.find('@').unwrap_or(encoding.len());
        if !suffix_word(&encoding[..end]) {
            return false;
        }
        rest = &encoding[end..];
    }
    if let Some(modifier) = rest.strip_prefix('@') {
        return suffix_word(modifier);
    }
    rest.is_empty()
}

fn suffix_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

/// A BCP 47 tag (with `-` or `_` separators) whose language is `es` or `en`:
/// an optional Latin script, region and variants, then extensions and private
/// use. At most sixteen subtags of up to eight characters each.
fn valid_tag(tag: &str) -> bool {
    let mut parts = [""; 16];
    let mut count = 0;
    for part in tag.split(['-', '_']) {
        if count == parts.len()
            || part.is_empty()
            || part.len() > 8
            || !part.bytes().all(|c| c.is_ascii_alphanumeric())
        {
            return false;
        }
        parts[count] = part;
        count += 1;
    }
    if count == 0 || !(parts[0].eq_ignore_ascii_case("es") || parts[0].eq_ignore_ascii_case("en")) {
        return false;
    }
    let parts = &parts[..count];
    let alpha = |s: &str| s.bytes().all(|c| c.is_ascii_alphabetic());
    let digit = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    let mut i = 1;
    if i < count && parts[i].len() == 4 && alpha(parts[i]) {
        // The catalogues and the UI are Latin script only, Spanish included.
        if !parts[i].eq_ignore_ascii_case("Latn") {
            return false;
        }
        i += 1;
    }
    if i < count
        && ((parts[i].len() == 2 && alpha(parts[i])) || (parts[i].len() == 3 && digit(parts[i])))
    {
        i += 1;
    }
    let variants_start = i;
    while i < count
        && ((5..=8).contains(&parts[i].len())
            || (parts[i].len() == 4 && parts[i].as_bytes()[0].is_ascii_digit()))
    {
        if parts[variants_start..i]
            .iter()
            .any(|p| p.eq_ignore_ascii_case(parts[i]))
        {
            return false;
        }
        i += 1;
    }
    let mut singletons = [false; 36];
    while i < count {
        let singleton = parts[i];
        if singleton.len() != 1 {
            return false;
        }
        let byte = singleton.as_bytes()[0].to_ascii_lowercase();
        i += 1;
        if byte == b'x' {
            // Everything after `x` is private use, already bounded above.
            return i < count;
        }
        let slot = if byte.is_ascii_digit() {
            (byte - b'0') as usize
        } else {
            (byte - b'a' + 10) as usize
        };
        if singletons[slot] {
            return false;
        }
        singletons[slot] = true;
        let start = i;
        while i < count && parts[i].len() >= 2 {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{language_for_locale, query_once};
    use crate::Language;

    #[test]
    fn the_os_is_asked_once() {
        let cell = std::sync::OnceLock::new();
        let asked = std::cell::Cell::new(0);
        let query = || {
            asked.set(asked.get() + 1);
            Some("es-MX".to_string())
        };
        assert_eq!(query_once(&cell, query).as_deref(), Some("es-MX"));
        assert_eq!(query_once(&cell, query).as_deref(), Some("es-MX"));
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn spanish_tags_of_any_region_select_spanish() {
        for locale in [
            "es",
            "ES",
            "es-MX",
            "es_ES",
            "es_419",
            "es-Latn-MX",
            "es_ES.UTF-8",
            "es_MX.UTF-8@latin",
            "es@latin",
            "es-419-u-nu-latn",
            "es-x-custom",
            "es-1996",
            "es-abcde",
        ] {
            assert_eq!(language_for_locale(locale), Language::Es, "{locale}");
        }
    }

    #[test]
    fn everything_else_selects_english() {
        for locale in [
            "en",
            "en-US",
            "en_Latn_GB",
            "C",
            "C.UTF-8",
            "POSIX",
            "",
            " ",
            " es",
            "es ",
            "espanol",
            "est",
            "esoteric",
            "es--MX",
            "es_",
            "es-1",
            "es-1234a6789",
            "es-USA",
            "es-abc",
            "es-MX-ES",
            "es-Latn-Latn",
            "es-Arab",
            "es-Hebr-MX",
            "es-Cyrl",
            "es-u",
            "es-u-a-test",
            "es-u-nu-u-ca",
            "es-x",
            "es-1996-1996",
            "es.UTF-8@",
            "es..UTF8",
            "es@foo@bar",
            "es.UTF 8",
            "es.UTF-8.MX",
            "es-💻",
            "ar",
            "he-IL",
            "fa_IR",
            "ur-PK",
            "fr-FR",
            "de-DE",
            "x-es",
            "i-es",
        ] {
            assert_eq!(language_for_locale(locale), Language::En, "{locale:?}");
        }
        // Parsing is bounded: an overlong tag or too many subtags is English.
        assert_eq!(
            language_for_locale(&format!("es-x-{}", "a".repeat(97))),
            Language::En
        );
        assert_eq!(
            language_for_locale(&format!("es-x{}", "-a".repeat(16))),
            Language::En
        );
    }
}
