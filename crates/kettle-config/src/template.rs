//! Tiny `{name}` placeholder templater for user-configurable strings
//! (e.g. `window-title-format`). Pure and dependency-free.
//!
//! - `{name}` looks `name` up in the `vars` slice; unknown placeholders
//!   are left as the literal text (so a typo is visible, not lost).
//! - `{{` and `}}` escape a literal brace.
//! - Names are `[A-Za-z0-9_]`; anything else aborts the placeholder and
//!   emits the original `{…}` text.

/// Substitute `{name}` placeholders in `template` from `vars`. Order in
/// `vars` is preserved (first match wins). Output is limited to 4096 bytes,
/// ending at a UTF-8 boundary.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    const MAX_OUTPUT_BYTES: usize = 4096;
    fn append(out: &mut String, value: &str) -> bool {
        let len = value.floor_char_boundary(MAX_OUTPUT_BYTES - out.len());
        out.push_str(&value[..len]);
        len == value.len()
    }

    let mut out = String::with_capacity(template.len().min(MAX_OUTPUT_BYTES));
    let mut it = template.chars().peekable();
    while let Some(c) = it.next() {
        if c.len_utf8() > MAX_OUTPUT_BYTES - out.len() {
            break;
        }
        match c {
            '{' => {
                if it.peek() == Some(&'{') {
                    it.next();
                    out.push('{');
                    continue;
                }
                // Collect a name; bail out if we hit non-name char before `}`.
                let mut name = String::new();
                let mut ok = false;
                while let Some(&nc) = it.peek() {
                    if nc == '}' {
                        it.next();
                        ok = true;
                        break;
                    }
                    if !nc.is_ascii_alphanumeric() && nc != '_' {
                        break;
                    }
                    name.push(nc);
                    it.next();
                }
                if ok
                    && !name.is_empty()
                    && let Some(&(_, v)) = vars.iter().find(|(k, _)| *k == name)
                {
                    if !append(&mut out, v) {
                        break;
                    }
                } else {
                    // Unknown / malformed → echo the original `{…}`.
                    out.push('{');
                    if !append(&mut out, &name) {
                        break;
                    }
                    if ok && out.len() < MAX_OUTPUT_BYTES {
                        out.push('}');
                    }
                }
            }
            '}' => {
                if it.peek() == Some(&'}') {
                    it.next();
                }
                out.push('}');
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn substitutes_known_names_and_keeps_literals() {
        let v = [
            ("title", "vim"),
            ("cwd", "/home/k/Repos/kettle"),
            ("tab", "1"),
        ];
        assert_eq!(fill("{title} — kettle", &v), "vim — kettle");
        assert_eq!(
            fill("[{tab}] {title} ({cwd})", &v),
            "[1] vim (/home/k/Repos/kettle)"
        );
        // No placeholders → identity.
        assert_eq!(fill("just text", &v), "just text");
        // Empty input → empty output.
        assert_eq!(fill("", &v), "");
    }

    #[test]
    fn unknown_or_malformed_placeholders_pass_through() {
        let v = [("title", "vim")];
        // Unknown name → echoed verbatim (typo visible, not silently lost).
        assert_eq!(fill("{missing} - x", &v), "{missing} - x");
        // No closing brace → echoed.
        assert_eq!(fill("a {oops", &v), "a {oops");
        // Empty {} → echoed.
        assert_eq!(fill("a {} b", &v), "a {} b");
    }

    #[test]
    fn brace_escapes_work() {
        let v = [("title", "vim")];
        assert_eq!(fill("{{not a var}}", &v), "{not a var}");
        assert_eq!(fill("{{{title}}}", &v), "{vim}");
    }

    #[test]
    fn expansion_is_bounded_for_repeated_maximum_length_titles() {
        let template = "{title}".repeat(100_000);
        let title = "\u{1f600}".repeat(512);
        let out = fill(&template, &[("title", &title)]);
        assert_eq!(out.len(), 4096, "expanded title exceeded its byte budget");
        assert_eq!(out, "\u{1f600}".repeat(1024));
        assert_eq!(fill(&"x".repeat(5000), &[]), "x".repeat(4096));
        assert_eq!(
            fill(
                &format!("{}{{title}}tail", "x".repeat(4095)),
                &[("title", &title)]
            ),
            "x".repeat(4095)
        );
        assert_eq!(fill(&format!("{{{}", "x".repeat(5000)), &[]).len(), 4096);
    }
}
