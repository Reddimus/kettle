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
    fn keeps_hash_in_value_and_repeats() {
        let e = parse("# comment\nbackground = #1a1b26\nfont-family = A\nfont-family = B\n");
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].key, "background");
        assert_eq!(e[0].value, "#1a1b26");
        assert_eq!(e[2].value, "B");
    }
}
