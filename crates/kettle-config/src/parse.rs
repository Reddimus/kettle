//! Ghostty-compatible `key = value` config tokenizer.
//!
//! Rules (matching Ghostty): one entry per line, first `=` splits key/value,
//! surrounding whitespace trimmed, full-line `#` comments only (a `#` inside a
//! value, e.g. a hex color, is part of the value), empty value resets the key.
//! Keys may repeat (e.g. `font-family`, `keybind`, `palette`).

#[derive(Debug, Clone)]
pub struct Entry {
    /// The key folded to kettle's canonical spelling: lowercased, with `_`
    /// rewritten to `-`.
    ///
    /// Terminator writes every key with underscores (`scroll_on_keystroke`),
    /// kettle's arms are hyphenated, and the parser matched the raw string. It
    /// compensated by hand-listing ~60 underscore aliases — and missed several,
    /// so `scroll_on_keystroke`, `scroll_on_output`, `scrollback_lines` and
    /// friends were reported as unknown keys and silently did nothing. Folding
    /// once here closes the whole class instead of one alias at a time.
    ///
    /// Safe because no config key exists in underscore form only: every
    /// underscore spelling the parser matches has a hyphen sibling, which
    /// `every_underscore_key_has_a_hyphen_sibling` pins.
    pub key: String,
    /// The key exactly as the user wrote it, for diagnostics. An
    /// unknown-key warning must echo the spelling that is actually in their
    /// file, or it sends them grepping for a line that is not there.
    pub raw_key: String,
    pub value: String,
}

/// Strip ONE matched pair of surrounding quotes from a value.
///
/// Terminator's own manual writes quoted values — `background_color =
/// "#000000"`, `scrollback_lines = '500'` — and kettle's value parsers see the
/// quote as part of the text. `Rgb::parse` rejects a leading `"` at its
/// hex-digit gate and `usize::parse` rejects it too, so the key was recognised,
/// the value discarded, and the default silently used. Handling it once here
/// fixes every key at the same time rather than teaching each parser about
/// quotes.
///
/// Deliberately conservative:
///   * both ends must be the SAME quote character, so `"a'` is left alone;
///   * only the outermost pair is removed, so `""` (an intentionally empty
///     value) survives as `""` → `` and inner quotes are preserved verbatim
///     for values that legitimately contain them, such as a shell command.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        if (first == b'"' || first == b'\'') && bytes[bytes.len() - 1] == first {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// An INI-style section header: `[global_config]`, `[[default]]`,
/// `[[[child1]]]`. Returns `(depth, name)`, where depth is the bracket nesting
/// Terminator uses to express hierarchy.
///
/// kettle's own config format has no sections, so a file without any behaves
/// exactly as it did before — every line applies.
pub(crate) fn section_header(line: &str) -> Option<(usize, &str)> {
    if !line.starts_with('[') || !line.ends_with(']') {
        return None;
    }
    let depth = line.len() - line.trim_start_matches('[').len();
    if depth == 0 || depth != line.len() - line.trim_end_matches(']').len() {
        return None;
    }
    let name = line[depth..line.len() - depth].trim();
    (!name.is_empty()).then_some((depth, name))
}

/// Which part of a sectioned Terminator config we are currently reading.
///
/// Terminator's file is INI with nesting expressed by bracket count:
///
/// ```text
/// [global_config]
///   focus = system
/// [keybindings]
///   new_tab = <Control><Shift>t
/// [profiles]
///   [[default]]
///     background_color = "#1a1b26"
///   [[work]]
///     background_color = "#222222"
/// [layouts]
///   [[default]]
///     [[[child1]]]
/// ```
///
/// Reading every line regardless of section meant the LAST profile in the file
/// won: a user's `[[default]]` colours were silently replaced by whichever
/// other profile happened to be written last, and layout internals leaked in
/// as config keys. kettle applies the global config, the keybindings, and the
/// DEFAULT profile. Other profiles and `[layouts]` are Terminator structures
/// with no kettle equivalent, and reading them would mean applying settings
/// the user did not select.
#[derive(Default)]
struct Section {
    /// `None` until the first header — a file with no sections at all is
    /// kettle's own format and applies wholesale.
    path: Option<Vec<String>>,
}

impl Section {
    fn enter(&mut self, (depth, name): (usize, &str)) {
        let path = self.path.get_or_insert_with(Vec::new);
        path.truncate(depth.saturating_sub(1));
        path.push(name.to_ascii_lowercase());
    }

    fn applies(&self) -> bool {
        let Some(path) = self.path.as_deref() else {
            // No section header seen yet: kettle's own flat format.
            return true;
        };
        match path.first().map(String::as_str) {
            Some("global_config" | "keybindings") => true,
            // Only the default profile. Any other is one the user did not
            // select, and kettle has `--profile` for choosing between configs.
            Some("profiles") => path.get(1).is_none_or(|p| p == "default"),
            // `[layouts]` describes Terminator's saved window trees; kettle
            // has its own layout files, and none of these keys mean anything
            // here.
            _ => false,
        }
    }
}

pub fn parse(input: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    // Strip the UTF-8 byte-order mark if Notepad / certain Windows
    // editors saved the file with one. Without this, the BOM bytes
    // were prepended to the first key — so `\u{feff}theme` showed up
    // as "unknown key: ﻿theme" in --check-config and the theme silently
    // didn't apply.
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut section = Section::default();
    for raw in input.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = section_header(line) {
            section.enter(header);
            continue;
        }
        if !section.applies() {
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let raw_key = line[..eq].trim().to_string();
        let key = raw_key.to_ascii_lowercase().replace('_', "-");
        let value = unquote(line[eq + 1..].trim()).to_string();
        if key.is_empty() {
            continue;
        }
        out.push(Entry {
            key,
            raw_key,
            value,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_utf8_bom() {
        // Notepad and a few Windows editors save UTF-8
        // text with a leading byte-order mark (\u{feff}, 0xEF 0xBB
        // 0xBF). Without this strip, the first key would parse as
        // `\u{feff}theme` and surface as an unknown key. The BOM
        // can only legitimately be at byte 0 of the input.
        let e = parse("\u{feff}theme = TokyoNight Night\nfont-size = 14\n");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].key, "theme", "BOM must not be prepended to the key");
        assert_eq!(e[0].value, "TokyoNight Night");
        // A `\u{feff}` mid-file is NOT a BOM and stays in the value.
        let e = parse("font-family = Hack\u{feff}\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].value, "Hack\u{feff}");
    }

    #[test]
    fn keeps_hash_in_value_and_repeats() {
        let e = parse("# comment\nbackground = #1a1b26\nfont-family = A\nfont-family = B\n");
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].key, "background");
        assert_eq!(e[0].value, "#1a1b26");
        assert_eq!(e[2].value, "B");
    }
}

#[cfg(test)]
mod fold_tests {
    /// The parser folds `_` → `-` once, at tokenize time, instead of
    /// hand-listing an underscore alias per key. That is only sound while every
    /// underscore spelling the parser matches also has a hyphen sibling — if a
    /// key ever existed in underscore form ONLY, folding would make it
    /// permanently unreachable.
    ///
    /// This reads `lib.rs`'s own match arms rather than a maintained list, so a
    /// future underscore-only key fails here instead of silently going dead.
    #[test]
    fn every_underscore_key_has_a_hyphen_sibling() {
        let src = include_str!("lib.rs").replace("\r\n", "\n");
        let start = src.find("fn parse_collect").expect("parse_collect present");
        let region = &src[start..];

        // Match-arm string literals: `"key"` followed by `|` or `=>`.
        let mut literals: Vec<String> = Vec::new();
        let bytes: Vec<char> = region.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '"' {
                let mut j = i + 1;
                let mut lit = String::new();
                while j < bytes.len() && bytes[j] != '"' {
                    lit.push(bytes[j]);
                    j += 1;
                }
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_whitespace() {
                    k += 1;
                }
                let is_arm = k < bytes.len()
                    && (bytes[k] == '|' || (bytes[k] == '=' && bytes.get(k + 1) == Some(&'>')));
                if is_arm
                    && !lit.is_empty()
                    && lit.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
                    })
                {
                    literals.push(lit);
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }

        let hyphenated: std::collections::HashSet<&String> =
            literals.iter().filter(|l| l.contains('-')).collect();
        let orphans: Vec<&String> = literals
            .iter()
            .filter(|l| l.contains('_'))
            .filter(|l| !hyphenated.contains(&l.replace('_', "-")))
            .collect();

        assert!(
            orphans.is_empty(),
            "these keys exist only in underscore form, so the tokenizer's \
             `_`→`-` fold would make them unreachable: {orphans:?}"
        );
        assert!(
            literals.len() > 100,
            "sanity: expected to scrape many arms, got {}",
            literals.len()
        );
    }
}
