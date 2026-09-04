//! Tiny `{name}` placeholder templater for user-configurable strings
//! (e.g. `window-title-format`). Pure and dependency-free.
//!
//! - `{name}` looks `name` up in the `vars` slice; unknown placeholders
//!   are left as the literal text (so a typo is visible, not lost).
//! - `{{` and `}}` escape a literal brace.
//! - Names are `[A-Za-z0-9_]`; anything else aborts the placeholder and
//!   emits the original `{…}` text.

/// Substitute `{name}` placeholders in `template` from `vars`. Order in
/// `vars` is preserved (first match wins).
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut it = template.chars().peekable();
    while let Some(c) = it.next() {
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
                    out.push_str(v);
                } else {
                    // Unknown / malformed → echo the original `{…}`.
                    out.push('{');
                    out.push_str(&name);
                    if ok {
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
}
