//! Trim trailing punctuation that follows URLs in prose but isn't actually
//! part of them (`.`, `,`, `;`, `:`, `'`, `"`), plus *bracket-balance-aware*
//! handling of `)`, `]`, `}`. Shared by the OSC 8 / autodetect link path
//! (`links.rs`) and the quick-select hint mode (`hints.rs`) — both used to
//! have their own private `trim_trailing` doing the same too-aggressive
//! strip.
//!
//! Why this needs to be careful: many legitimate URLs end in a closing
//! bracket — `https://en.wikipedia.org/wiki/Foo_(bar)` is the canonical
//! example, but the same shape shows up in Apple docs, MDN reference URLs,
//! some markdown link targets, and any forum post that ends a URL with
//! `(blah)` for disambiguation. Always stripping the trailing `)` broke
//! those URLs into 404s (the `(bar` half is a real path component).
//!
//! At the same time, the autodetect regex starts at `https?://` / `www.` /
//! `ftp://` / `file://` — so `(https://rust-lang.org).` *as a regex match*
//! is `https://rust-lang.org).` (the opening `(` lives outside the match).
//! Naively keeping the trailing `)` here would leave the `)` glued to the
//! URL — also wrong.
//!
//! The rule that works for both: strip a `)` (or `]` / `}`) only when the
//! candidate substring has *more closes than opens* of the matching pair.
//! - `https://en.wikipedia.org/wiki/Foo_(bar)` → 1 open, 1 close, balanced
//!   → keep the `)`. ✓
//! - `https://rust-lang.org)` (from `(https://rust-lang.org).`) → 0 opens,
//!   1 close, unbalanced → strip. ✓

/// Strip URL-tail prose punctuation, bracket-balance-aware. Pure; byte-
/// level (the punctuation chars and the pairs we count are all
/// single-byte ASCII, and UTF-8 continuation bytes never collide with
/// any of them, so a multi-byte char inside a URL is left untouched).
pub fn trim_trailing(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
        let last = bytes[end - 1];
        let strip = match last {
            b'.' | b',' | b';' | b':' | b'\'' | b'"' => true,
            b')' | b']' | b'}' => {
                let open = match last {
                    b')' => b'(',
                    b']' => b'[',
                    b'}' => b'{',
                    _ => unreachable!(),
                };
                let prefix = &bytes[..end];
                let closes = prefix.iter().filter(|&&b| b == last).count();
                let opens = prefix.iter().filter(|&&b| b == open).count();
                closes > opens
            }
            _ => false,
        };
        if !strip {
            break;
        }
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::trim_trailing;

    #[test]
    fn strips_sentence_punctuation() {
        // The unambiguous cases — these are always prose noise.
        assert_eq!(trim_trailing("https://example.com."), "https://example.com");
        assert_eq!(trim_trailing("https://example.com,"), "https://example.com");
        assert_eq!(trim_trailing("https://example.com;"), "https://example.com");
        assert_eq!(trim_trailing("https://example.com:"), "https://example.com");
        assert_eq!(trim_trailing("https://example.com'"), "https://example.com");
        assert_eq!(
            trim_trailing("https://example.com\""),
            "https://example.com"
        );
        // Run of trailing punctuation strips the whole run.
        assert_eq!(trim_trailing("https://e.com.,.,"), "https://e.com");
    }

    #[test]
    fn keeps_balanced_closing_brackets() {
        // The Wikipedia case. Pre-fix, the trailing `)` was
        // always stripped, turning the URL into a 404 (`Foo_(bar` is a
        // real, different path).
        assert_eq!(
            trim_trailing("https://en.wikipedia.org/wiki/Foo_(bar)"),
            "https://en.wikipedia.org/wiki/Foo_(bar)"
        );
        // Same for `[...]` and `{...}` URLs (rarer but legal).
        assert_eq!(
            trim_trailing("https://example.com/a[b]"),
            "https://example.com/a[b]"
        );
        assert_eq!(
            trim_trailing("https://example.com/a{b}"),
            "https://example.com/a{b}"
        );
        // Balanced bracket followed by a strip-always char: strip `.`,
        // then the bracket is balanced → keep.
        assert_eq!(
            trim_trailing("https://en.wikipedia.org/wiki/Foo_(bar)."),
            "https://en.wikipedia.org/wiki/Foo_(bar)"
        );
    }

    #[test]
    fn strips_unbalanced_closing_brackets() {
        // The other direction: an excerpt from `(https://rust-lang.org).`
        // — the regex starts matching at `h`, so the match's substring is
        // `https://rust-lang.org).`. Strip `.`, then the `)` has 0 opens
        // and 1 close (unbalanced) → strip. Result: clean URL.
        assert_eq!(
            trim_trailing("https://rust-lang.org)."),
            "https://rust-lang.org"
        );
        assert_eq!(trim_trailing("https://example.com]"), "https://example.com");
        assert_eq!(trim_trailing("https://example.com}"), "https://example.com");
        // Several unbalanced closes get peeled together.
        assert_eq!(
            trim_trailing("https://example.com)))"),
            "https://example.com"
        );
    }

    #[test]
    fn preserves_non_punct_tail() {
        // Letters / digits / standard URL chars never strip.
        assert_eq!(
            trim_trailing("https://example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            trim_trailing("https://example.com/x?q=1"),
            "https://example.com/x?q=1"
        );
        // Empty string is a no-op.
        assert_eq!(trim_trailing(""), "");
    }

    #[test]
    fn multibyte_in_url_is_untouched() {
        // Non-ASCII chars (an IRI-ish URL with a Japanese path) are never
        // stripped — the byte-level filter only matches ASCII punctuation
        // and the pair-counting only counts ASCII parens, so multi-byte
        // chars are passed through verbatim.
        let url = "https://例.test/路径";
        assert_eq!(trim_trailing(url), url);
        // Trailing prose `.` after a multi-byte char still strips.
        let with_dot = "https://例.test/路径.";
        assert_eq!(trim_trailing(with_dot), url);
    }
}
