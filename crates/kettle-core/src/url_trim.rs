//! Trim trailing punctuation that follows URLs in prose but isn't actually
//! part of them (`.`, `,`, `;`, `:`, `'`, `"`, and Markdown's `` ` `` code
//! and `*` emphasis marks), plus *bracket-balance-aware*
//! handling of `)`, `]`, `}`. Shared by the OSC 8 / autodetect link path
//! (`links.rs`) and the quick-select hint mode (`hints.rs`).
//!
//! Why this needs to be careful: many legitimate URLs end in a closing
//! bracket — `https://en.wikipedia.org/wiki/Foo_(bar)` is the canonical
//! example, but the same shape shows up in Apple docs, MDN reference URLs,
//! some markdown link targets, and any forum post that ends a URL with
//! `(blah)` for disambiguation. Always stripping the trailing `)` turns
//! those URLs into 404s (the `(bar` half is a real path component).
//!
//! At the same time, the autodetect regex starts at `https?://` / `www.` /
//! `ftp://` / `file://` — so `(https://rust-lang.org).` *as a regex match*
//! is `https://rust-lang.org).` (the opening `(` lives outside the match).
//! Naively keeping the trailing `)` here would leave the `)` glued to the
//! URL — also wrong.
//!
//! The rule that works for both: strip a `)` (or `]` / `}`) only when no
//! open of its pair before it is still unclosed, pairing each close with the
//! nearest unclosed open before it, left to right.
//! - `https://en.wikipedia.org/wiki/Foo_(bar)` → the `)` closes `(bar` →
//!   keep it. ✓
//! - `https://rust-lang.org)` (from `(https://rust-lang.org).`) → no open
//!   before the `)` → strip. ✓
//! - `https://x.test/?q=a)b(c)` → the first `)` has no open, but the last
//!   closes `(c` → keep it. Counting opens and closes over the whole URL
//!   would blame the last one for the stray first one.

/// Strip URL-tail prose punctuation, bracket-balance-aware. Pure; byte-
/// level (the punctuation chars and the pairs we count are all
/// single-byte ASCII, and UTF-8 continuation bytes never collide with
/// any of them, so a multi-byte char inside a URL is left untouched).
pub fn trim_trailing(s: &str) -> &str {
    trim_trailing_inner(s, false)
}

pub(crate) fn trim_path_trailing(s: &str) -> &str {
    trim_trailing_inner(s, true)
}

fn trim_trailing_inner(s: &str, path: bool) -> &str {
    let bytes = s.as_bytes();
    // Whether a close pairs with an open depends only on what precedes it,
    // so one pass finds the end: just after the last byte that is neither
    // trailing punctuation nor a close with no open to pair with.
    let mut unclosed = [0usize; 3];
    let mut kept = 0;
    for (at, &byte) in bytes.iter().enumerate() {
        let keep = match byte {
            b'(' | b'[' | b'{' => {
                unclosed[pair(byte)] += 1;
                true
            }
            b')' | b']' | b'}' => {
                let open = &mut unclosed[pair(byte)];
                let paired = *open > 0;
                *open = open.saturating_sub(1);
                paired
            }
            // A backtick is never part of a URL (it must be percent-encoded),
            // and a trailing `*` closes Markdown emphasis far more often than
            // it ends a URL: `` `https://x.test/a` `` and `**https://x.test**`.
            b'.' | b',' | b';' | b':' | b'\'' | b'"' | b'`' | b'*' => false,
            _ => true,
        };
        if keep {
            kept = at + 1;
        }
    }
    if path {
        // A last component of `.` or `..` names a directory, so it stays.
        // Any longer run of dots goes whole, after a name (`notes...`) or a
        // separator (`/usr/lib/...`), as Ghostty reads a path's end, never
        // just enough of it to leave `..`, which would name the parent.
        // Callers do not open a path trimmed that way (see
        // `trimmed_dot_run`). Each run is scanned once, so this stays linear.
        let mut end = bytes.len();
        while end > kept {
            if bytes[end - 1] != b'.' {
                end -= 1;
                continue;
            }
            let dots = bytes[..end]
                .iter()
                .rev()
                .take_while(|&&b| b == b'.')
                .count();
            let start = end - dots;
            if dots <= 2 && (start == 0 || matches!(bytes[start - 1], b'/' | b'\\')) {
                return &s[..end];
            }
            end = start;
        }
    }
    &s[..kept]
}

/// The length of `raw` up to the end of the dots trimming took off it, when
/// it took two or more (`/tmp/a..` trimmed to `/tmp/a`, `/tmp/...` to
/// `/tmp/`). The name may end in those dots, or a sentence may trail off
/// after it, and the shorter path names another file, so a caller opens
/// the exact name only where a closing quote marks it and nothing
/// otherwise. `None` when trimming took at most one dot: a single dot ends
/// a sentence.
pub(crate) fn trimmed_dot_run(raw: &str, trimmed: &str) -> Option<usize> {
    let dots = raw[trimmed.len()..]
        .bytes()
        .take_while(|&b| b == b'.')
        .count();
    (dots >= 2).then_some(trimmed.len() + dots)
}

/// The index of a bracket's pair: `()`, `[]` or `{}`.
fn pair(byte: u8) -> usize {
    match byte {
        b'(' | b')' => 0,
        b'[' | b']' => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::trim_trailing;

    #[test]
    fn long_mixed_bracket_tail_is_trimmed_promptly() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let expected = "https://x.test/a(b)[c]{d}";
            let candidate = format!("{expected}{}", ")]}".repeat(1_000_000));
            tx.send(trim_trailing(&candidate) == expected).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("trimming a bracket tail exceeded the linear-work budget")
        );
        worker.join().unwrap();
    }

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

    /// Markdown wraps URLs in backticks and emphasis; neither belongs to the
    /// URL, but the same characters inside it stay.
    #[test]
    fn strips_markdown_code_and_emphasis_marks() {
        assert_eq!(trim_trailing("https://x.test/a`"), "https://x.test/a");
        assert_eq!(trim_trailing("https://x.test/a**"), "https://x.test/a");
        assert_eq!(trim_trailing("https://x.test/a*"), "https://x.test/a");
        assert_eq!(trim_trailing("https://x.test/a`."), "https://x.test/a");
        assert_eq!(trim_trailing("https://x.test/a**)."), "https://x.test/a");
        assert_eq!(
            trim_trailing("https://x.test/a*b`c/d"),
            "https://x.test/a*b`c/d",
            "inside a URL the marks are kept"
        );
    }

    #[test]
    fn keeps_balanced_closing_brackets() {
        // The Wikipedia case. Stripping the `)` would turn the URL into a 404
        // (`Foo_(bar` is a real, different path).
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

    /// A trailing close that pairs with an open before it stays, even after
    /// a stray close earlier in the URL (legal in a query): counting the
    /// whole URL would strip the wrong one.
    #[test]
    fn a_stray_close_earlier_does_not_strip_a_paired_one() {
        for url in [
            "https://example.com/search?q=a)b(c)",
            "https://example.com/a?x=1]y[z]",
            "https://example.com/a?x=1}y{z}",
            "https://例.test/路)b(径)",
        ] {
            assert_eq!(trim_trailing(url), url);
            assert_eq!(trim_trailing(&format!("{url}.")), url, "{url}.");
        }
        // Only the close with no open before it goes.
        assert_eq!(
            trim_trailing("https://x.test/a)b(c))"),
            "https://x.test/a)b(c)"
        );
        // `(bar)` pairs its own close, so the last `)` has none left.
        assert_eq!(
            trim_trailing("https://en.wikipedia.org/wiki/Foo_(bar)?x=)"),
            "https://en.wikipedia.org/wiki/Foo_(bar)?x="
        );
        assert_eq!(super::trim_path_trailing("./a)b(c)"), "./a)b(c)");
        assert_eq!(super::trim_path_trailing("./a)b(c))."), "./a)b(c)");
    }

    /// A last path component of `.` or `..` names a directory and stays. A
    /// longer run of dots is an ellipsis, as after a name, and goes whole:
    /// trimming `...` only down to `..` would name the parent directory.
    #[test]
    fn a_dot_run_ending_a_path_never_becomes_its_parent() {
        use super::trim_path_trailing;
        for (path, trimmed) in [
            (".", "."),
            ("..", ".."),
            ("/tmp/.", "/tmp/."),
            ("/tmp/..", "/tmp/.."),
            (r"C:\x\..", r"C:\x\.."),
            ("...", ""),
            ("/tmp/...", "/tmp/"),
            ("foo/....", "foo/"),
            (r"C:\x\...", r"C:\x\"),
            ("/usr/lib/...", "/usr/lib/"),
            ("dir/notes...", "dir/notes"),
            ("/tmp/a..", "/tmp/a"),
            ("/tmp/a.", "/tmp/a"),
        ] {
            assert_eq!(trim_path_trailing(path), trimmed, "{path}");
            for tail in [",", ").", "\".", ";:"] {
                let candidate = format!("{path}{tail}");
                assert_eq!(trim_path_trailing(&candidate), trimmed, "{candidate}");
            }
        }
        // Two or more dots trimmed off leave the name uncertain.
        use super::trimmed_dot_run;
        for (raw, exact) in [
            ("/tmp/a..", Some(8)),
            ("/tmp/a...)", Some(9)),
            ("/tmp/...", Some(8)),
            ("/tmp/a.", None),
            ("/tmp/a.)", None),
            ("/tmp/a.,.", None),
            ("/tmp/..", None),
            ("/tmp/a", None),
        ] {
            assert_eq!(
                trimmed_dot_run(raw, trim_path_trailing(raw)),
                exact,
                "{raw}"
            );
        }
        // The run takes linear work however long it is.
        let long = format!("a/{}", ".".repeat(1_000_000));
        assert_eq!(trim_path_trailing(&long), "a/");
        let long = format!("a/b{}", ".".repeat(1_000_000));
        assert_eq!(trim_path_trailing(&long), "a/b");
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
        // Non-ASCII chars (an IRI-ish URL with a CJK path) are never
        // stripped. The byte-level filter and pair counts match only ASCII
        // punctuation, so multi-byte chars pass through verbatim.
        let url = "https://例.test/路径";
        assert_eq!(trim_trailing(url), url);
        // Trailing prose `.` after a multi-byte char still strips.
        let with_dot = "https://例.test/路径.";
        assert_eq!(trim_trailing(with_dot), url);
    }
}
