//! Quick-select "hint mode" core (kitty `kitten hints` / WezTerm
//! `QuickSelect` style): scan the visible rows for interesting tokens —
//! URLs, filesystem paths, git hashes, IPv4 addresses — and assign each a
//! short, easy-to-type label. Pure and fully unit-tested; the overlay +
//! keypress handling (a follow-up) consume [`detect`] and [`labels`].

use regex::Regex;
use std::sync::OnceLock;

/// What a detected hint points at (drives the copy-vs-open action later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Url,
    Path,
    Hash,
    Ip,
}

/// A detected token in row/column coordinates (`end` is inclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintSpan {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
    pub text: String,
}

fn res() -> &'static [(Kind, Regex)] {
    static RE: OnceLock<Vec<(Kind, Regex)>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            (
                Kind::Url,
                Regex::new(r#"(?:https?://|ftp://|file://|www\.)[^\s\x00-\x1f<>"]+"#).unwrap(),
            ),
            (
                Kind::Path,
                // Absolute, ~-relative, or ./ ../ relative paths with at
                // least one separator.
                Regex::new(r"(?:~|\.{0,2})?/[\w.\-/@+]+").unwrap(),
            ),
            (
                Kind::Ip,
                // Clamp each octet to 0..=255 (was
                // `\d{1,3}`, which surfaced 999.999.999.999 and other out-of-range
                // dotted numbers as IP quick-select targets). The `regex` crate
                // has no lookaround, so a 5-group `1.2.3.4.5` can still match its
                // first four octets — acceptable since the only Ip action is a
                // clipboard copy (no network/open).
                Regex::new(
                    r"\b(?:25[0-5]|2[0-4]\d|1?\d?\d)(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3}\b",
                )
                .unwrap(),
            ),
            (
                Kind::Hash,
                // Git-style hex blob (7–40), not part of a longer word.
                Regex::new(r"\b[0-9a-f]{7,40}\b").unwrap(),
            ),
        ]
    })
}

use crate::url_trim::trim_trailing;

/// Detect hint targets across `rows` (one string per visible line). Earlier
/// kinds win on overlap (a URL is not also matched as a path), and matches
/// are returned in reading order (row, then column).
pub fn detect(rows: &[&str]) -> Vec<HintSpan> {
    let mut out: Vec<HintSpan> = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        // Byte offset -> char column for this line. Push each char's
        // column exactly `len_utf8()` times (matching
        // links.rs/search.rs) so a multi-byte char's continuation bytes map to
        // ITS column, not the next one — the old `while v.len() <= b` attributed a
        // trailing non-ASCII char's bytes to the following column, so
        // double-clicking a token ending in e.g. `é` over-selected by one cell.
        let col_of_byte: Vec<usize> = {
            let mut v = Vec::with_capacity(line.len() + 1);
            for (col, ch) in line.chars().enumerate() {
                for _ in 0..ch.len_utf8() {
                    v.push(col);
                }
            }
            v.push(line.chars().count()); // sentinel for the end-exclusive byte
            v
        };
        let mut taken: Vec<(usize, usize)> = Vec::new(); // byte ranges claimed
        for (kind, re) in res() {
            for m in re.find_iter(line) {
                let raw = m.as_str();
                let trimmed = trim_trailing(raw);
                if trimmed.is_empty() {
                    continue;
                }
                let (bs, be) = (m.start(), m.start() + trimmed.len());
                if taken.iter().any(|&(s, e)| bs < e && be > s) {
                    continue; // overlaps a higher-priority match
                }
                taken.push((bs, be));
                let start = col_of_byte[bs];
                let end = col_of_byte[be - 1];
                out.push(HintSpan {
                    row,
                    start,
                    end,
                    kind: *kind,
                    text: trimmed.to_string(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.row.cmp(&b.row).then(a.start.cmp(&b.start)));
    out
}

/// Default label alphabet — home-row first so the common (few) targets are
/// one comfortable keystroke.
pub const ALPHABET: &str = "asdfghjklqwertyuiopzxcvbnm";

/// `n` distinct fixed-width labels over `alphabet`, shortest width that
/// fits, in lexicographic order (`a, b, …` then `aa, ab, …`). Stable, so a
/// target's label doesn't jump around as you type to filter.
pub fn labels(n: usize, alphabet: &str) -> Vec<String> {
    let a: Vec<char> = alphabet.chars().collect();
    if n == 0 || a.is_empty() {
        return Vec::new();
    }
    let base = a.len();
    // A single-character alphabet can't make `n` distinct FIXED-width labels —
    // `base^width` is always 1, so the width search below would loop forever.
    // Fall back to distinct INCREASING-length labels (`a`, `aa`, `aaa`, …).
    if base == 1 {
        return (0..n).map(|i| a[0].to_string().repeat(i + 1)).collect();
    }
    let mut width = 1usize;
    while base.checked_pow(width as u32).is_none_or(|cap| cap < n) {
        width += 1;
    }
    (0..n)
        .map(|mut i| {
            let mut digits = vec![0usize; width];
            for d in digits.iter_mut().rev() {
                *d = i % base;
                i /= base;
            }
            digits.into_iter().map(|d| a[d]).collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_kind_with_columns() {
        let rows = ["see https://example.com/x and /etc/hosts now"];
        let h = detect(&rows);
        assert_eq!(h[0].kind, Kind::Url);
        assert_eq!(h[0].text, "https://example.com/x");
        assert_eq!(h[0].row, 0);
        assert_eq!(h[0].start, 4, "column of the 'h' in https");
        assert!(
            h.iter()
                .any(|s| s.kind == Kind::Path && s.text == "/etc/hosts")
        );

        let rows2 = ["commit a1b2c3d4e5 at 10.0.0.2:8080"];
        let h2 = detect(&rows2);
        assert!(
            h2.iter()
                .any(|s| s.kind == Kind::Hash && s.text == "a1b2c3d4e5")
        );
        assert!(
            h2.iter()
                .any(|s| s.kind == Kind::Ip && s.text == "10.0.0.2")
        );
    }

    #[test]
    fn trailing_punctuation_trimmed_and_no_overlap() {
        let h = detect(&["go to (https://rust-lang.org)."]);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].kind, Kind::Url);
        assert_eq!(h[0].text, "https://rust-lang.org", "no trailing ).");
        // The URL's path-looking substring is not also a separate hint.
        assert!(h.iter().all(|s| s.kind == Kind::Url));
    }

    #[test]
    fn reading_order_and_empty() {
        let h = detect(&["/a/b", "x", "/c/d /e/f"]);
        let coords: Vec<(usize, usize)> = h.iter().map(|s| (s.row, s.start)).collect();
        assert_eq!(coords, vec![(0, 0), (2, 0), (2, 5)]);
        assert!(detect(&["nothing here", ""]).is_empty());
    }

    #[test]
    fn labels_are_short_unique_and_deterministic() {
        // Simple a,b,c… alphabet makes the ordering easy to assert.
        let abc = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(labels(0, abc), Vec::<String>::new());
        assert_eq!(labels(1, abc), vec!["a"]);
        assert_eq!(labels(3, abc), vec!["a", "b", "c"], "1-char while they fit");
        // With the real home-row alphabet the first labels are a, s, d…
        assert_eq!(labels(3, ALPHABET), vec!["a", "s", "d"]);

        // Just past the alphabet → all uniform 2-char, distinct.
        let n = abc.len() + 2;
        let many = labels(n, abc);
        assert!(many.iter().all(|l| l.len() == 2), "uniform width");
        assert_eq!(many[0], "aa");
        assert_eq!(many[1], "ab");
        let uniq: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(uniq.len(), n, "all labels distinct");
        // Deterministic for a given n (labels are assigned once per scan).
        assert_eq!(labels(n, abc), labels(n, abc));
        // Degenerate alphabet → no labels (caller falls back).
        assert!(labels(5, "").is_empty());
        // A single-character alphabet must NOT hang (the
        // fixed-width search can't represent n>1) — fall back to distinct
        // increasing-length labels.
        assert_eq!(labels(1, "x"), vec!["x"]);
        assert_eq!(labels(3, "x"), vec!["x", "xx", "xxx"]);
        let single = labels(50, "x");
        assert_eq!(single.len(), 50);
        assert_eq!(
            single
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            50,
            "all distinct, no hang"
        );
    }
}
