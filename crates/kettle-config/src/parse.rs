//! Ghostty-compatible `key = value` config tokenizer.
//!
//! Rules (matching Ghostty): one entry per line, first `=` splits key/value,
//! surrounding whitespace trimmed, full-line `#` comments only (a `#` inside a
//! value, e.g. a hex color, is part of the value), empty value resets the key.
//! Keys may repeat (e.g. `font-family`, `keybind`, `palette`).

#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub value: String,
}

pub fn parse(input: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    // Strip the UTF-8 byte-order mark if Notepad / certain Windows
    // editors saved the file with one. Without this, the BOM bytes
    // were prepended to the first key — so `\u{feff}theme` showed up
    // as "unknown key: ﻿theme" in --check-config and the theme silently
    // didn't apply. Cycle 155.
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    for raw in input.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key = line[..eq].trim().to_ascii_lowercase();
        let value = line[eq + 1..].trim().to_string();
        if key.is_empty() {
            continue;
        }
        out.push(Entry { key, value });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_utf8_bom() {
        // Cycle 155: Notepad and a few Windows editors save UTF-8
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
