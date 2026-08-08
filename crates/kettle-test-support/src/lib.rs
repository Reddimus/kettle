//! Shared filesystem fixtures for Kettle's workspace tests.

use std::path::Path;

/// Returns `src` with every `#[cfg(test)]` / `#[cfg(all(test, ...))]` item
/// removed, so a guard searching it cannot match its own assertions.
///
/// More generally, an item is removed when its `cfg` predicate contains
/// `test` as an identifier. If the source is malformed, the original input is
/// returned unchanged so callers' source-slice postconditions fail loudly.
pub fn production_source(src: &str) -> String {
    let normalized = src.replace("\r\n", "\n");
    strip_test_items(&normalized).unwrap_or_else(|()| src.to_owned())
}

fn strip_test_items(src: &str) -> Result<String, ()> {
    let bytes = src.as_bytes();
    let mut production = String::with_capacity(src.len());
    let mut copied_through = 0;
    let mut cursor = 0;

    while cursor < bytes.len() {
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            cursor = end;
            continue;
        }

        if bytes[cursor] != b'#' {
            cursor += 1;
            continue;
        }

        let Some(first_attribute) = parse_attribute(bytes, cursor)? else {
            cursor += 1;
            continue;
        };
        let mut has_test_cfg = first_attribute.has_test_cfg;
        let mut attributes_end = first_attribute.end;

        loop {
            let next = skip_trivia(bytes, attributes_end)?;
            let Some(attribute) = parse_attribute(bytes, next)? else {
                break;
            };
            has_test_cfg |= attribute.has_test_cfg;
            attributes_end = attribute.end;
        }

        if !has_test_cfg {
            cursor = attributes_end;
            continue;
        }

        let line_start = src[..cursor].rfind('\n').map_or(0, |newline| newline + 1);
        let removal_start = if bytes[line_start..cursor]
            .iter()
            .all(u8::is_ascii_whitespace)
        {
            preceding_doc_comments_start(src, line_start)
        } else {
            cursor
        };
        let item_start = skip_trivia(bytes, attributes_end)?;
        let item_end = find_item_end(bytes, item_start)?;

        production.push_str(&src[copied_through..removal_start]);
        copied_through = item_end;
        cursor = item_end;
    }

    production.push_str(&src[copied_through..]);
    Ok(production)
}

fn preceding_doc_comments_start(src: &str, mut start: usize) -> usize {
    while start > 0 {
        let line_end = start - 1;
        let line_start = src[..line_end].rfind('\n').map_or(0, |newline| newline + 1);
        let line = src[line_start..line_end].trim_start();
        if !line.starts_with("///") || line.starts_with("////") {
            break;
        }
        start = line_start;
    }
    start
}

#[derive(Clone, Copy)]
struct Attribute {
    end: usize,
    has_test_cfg: bool,
}

fn parse_attribute(bytes: &[u8], start: usize) -> Result<Option<Attribute>, ()> {
    if bytes.get(start) != Some(&b'#') {
        return Ok(None);
    }

    let mut cursor = start + 1;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'!') {
        return Ok(None);
    }
    if bytes.get(cursor) != Some(&b'[') {
        return Ok(None);
    }

    let contents_start = cursor + 1;
    let mut depth = 1usize;
    cursor = contents_start;
    while cursor < bytes.len() {
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            cursor = end;
            continue;
        }
        match bytes[cursor] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(Attribute {
                        end: cursor + 1,
                        has_test_cfg: cfg_contains_test(&bytes[contents_start..cursor])?,
                    }));
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Err(())
}

fn cfg_contains_test(contents: &[u8]) -> Result<bool, ()> {
    let mut cursor = skip_trivia(contents, 0)?;
    let Some((name, end)) = identifier(contents, cursor) else {
        return Ok(false);
    };
    if name != b"cfg" {
        return Ok(false);
    }

    cursor = skip_trivia(contents, end)?;
    if contents.get(cursor) != Some(&b'(') {
        return Ok(false);
    }
    let mut depth = 1usize;
    let mut found_test = false;
    cursor += 1;

    while cursor < contents.len() {
        if let Some(end) = skip_lexeme(contents, cursor)? {
            cursor = end;
            continue;
        }
        if let Some((name, end)) = identifier(contents, cursor) {
            if name == b"test" {
                found_test = true;
            }
            cursor = end;
            continue;
        }
        match contents[cursor] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(found_test);
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Err(())
}

fn identifier(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
    let first = *bytes.get(start)?;
    if !(first == b'_' || first.is_ascii_alphabetic() || first >= 0x80) {
        return None;
    }
    let mut end = start + 1;
    while bytes
        .get(end)
        .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphanumeric() || *byte >= 0x80)
    {
        end += 1;
    }
    Some((&bytes[start..end], end))
}

fn skip_trivia(bytes: &[u8], mut cursor: usize) -> Result<usize, ()> {
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"//")
            || bytes.get(cursor..cursor + 2) == Some(b"/*")
        {
            cursor = skip_lexeme(bytes, cursor)?.ok_or(())?;
        } else {
            return Ok(cursor);
        }
    }
}

fn find_item_end(bytes: &[u8], mut cursor: usize) -> Result<usize, ()> {
    if cursor >= bytes.len() {
        return Err(());
    }

    let requires_semicolon = item_requires_semicolon(bytes, cursor)?;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut angle_depth = 0usize;
    while cursor < bytes.len() {
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            cursor = end;
            continue;
        }
        if !requires_semicolon
            && paren_depth == 0
            && bracket_depth == 0
            && angle_depth > 0
            && bytes[cursor] == b'{'
        {
            cursor = matching_brace_end(bytes, cursor)?;
            continue;
        }
        match bytes[cursor] {
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.checked_sub(1).ok_or(())?,
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.checked_sub(1).ok_or(())?,
            b'<' if !requires_semicolon && paren_depth == 0 && bracket_depth == 0 => {
                angle_depth += 1;
            }
            b'>' if !requires_semicolon
                && paren_depth == 0
                && bracket_depth == 0
                && angle_depth > 0 =>
            {
                angle_depth -= 1;
            }
            b'{' if requires_semicolon => brace_depth += 1,
            b'}' if requires_semicolon && brace_depth > 0 => brace_depth -= 1,
            b'{' if paren_depth == 0 && bracket_depth == 0 && angle_depth == 0 => {
                let mut end = matching_brace_end(bytes, cursor)?;
                while matches!(bytes.get(end), Some(b' ' | b'\t' | b'\r')) {
                    end += 1;
                }
                if bytes.get(end) == Some(&b';') {
                    end += 1;
                }
                return Ok(end);
            }
            b';' if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && angle_depth == 0 =>
            {
                return Ok(cursor + 1);
            }
            b'}' if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && angle_depth == 0 =>
            {
                return Err(());
            }
            _ => {}
        }
        cursor += 1;
    }
    Err(())
}

fn item_requires_semicolon(bytes: &[u8], mut cursor: usize) -> Result<bool, ()> {
    cursor = skip_trivia(bytes, cursor)?;
    let Some((mut word, end)) = identifier(bytes, cursor) else {
        return Ok(false);
    };
    cursor = end;

    if word == b"pub" {
        cursor = skip_trivia(bytes, cursor)?;
        if bytes.get(cursor) == Some(&b'(') {
            cursor = matching_delimiter_end(bytes, cursor, b'(', b')')?;
            cursor = skip_trivia(bytes, cursor)?;
        }
        let Some((next, end)) = identifier(bytes, cursor) else {
            return Ok(false);
        };
        word = next;
        cursor = end;
    }

    if matches!(word, b"use" | b"type" | b"static") {
        return Ok(true);
    }
    if word == b"extern" {
        cursor = skip_trivia(bytes, cursor)?;
        return Ok(identifier(bytes, cursor).is_some_and(|(next, _)| next == b"crate"));
    }
    if word != b"const" {
        return Ok(false);
    }

    loop {
        cursor = skip_trivia(bytes, cursor)?;
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            cursor = end;
            continue;
        }
        let Some((next, end)) = identifier(bytes, cursor) else {
            return Ok(true);
        };
        match next {
            b"fn" => return Ok(false),
            b"async" | b"unsafe" | b"extern" => cursor = end,
            _ => return Ok(true),
        }
    }
}

fn matching_delimiter_end(
    bytes: &[u8],
    mut cursor: usize,
    open: u8,
    close: u8,
) -> Result<usize, ()> {
    let mut depth = 0usize;
    while cursor < bytes.len() {
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            cursor = end;
            continue;
        }
        if bytes[cursor] == open {
            depth += 1;
        } else if bytes[cursor] == close {
            depth = depth.checked_sub(1).ok_or(())?;
            if depth == 0 {
                return Ok(cursor + 1);
            }
        }
        cursor += 1;
    }
    Err(())
}

fn matching_brace_end(bytes: &[u8], mut cursor: usize) -> Result<usize, ()> {
    let mut depth = 0usize;
    while cursor < bytes.len() {
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            cursor = end;
            continue;
        }
        match bytes[cursor] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1).ok_or(())?;
                if depth == 0 {
                    return Ok(cursor + 1);
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Err(())
}

fn skip_lexeme(bytes: &[u8], start: usize) -> Result<Option<usize>, ()> {
    if bytes.get(start..start + 2) == Some(b"//") {
        return Ok(Some(
            bytes[start + 2..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| start + 2 + offset),
        ));
    }
    if bytes.get(start..start + 2) == Some(b"/*") {
        let mut depth = 1usize;
        let mut cursor = start + 2;
        while cursor < bytes.len() {
            if bytes.get(cursor..cursor + 2) == Some(b"/*") {
                depth += 1;
                cursor += 2;
            } else if bytes.get(cursor..cursor + 2) == Some(b"*/") {
                depth -= 1;
                cursor += 2;
                if depth == 0 {
                    return Ok(Some(cursor));
                }
            } else {
                cursor += 1;
            }
        }
        return Err(());
    }
    if let Some(end) = skip_raw_string(bytes, start)? {
        return Ok(Some(end));
    }
    if bytes.get(start) == Some(&b'"')
        || bytes.get(start..start + 2) == Some(b"b\"")
        || bytes.get(start..start + 2) == Some(b"c\"")
    {
        let quote = if bytes[start] == b'"' {
            start
        } else {
            start + 1
        };
        return Ok(Some(skip_quoted(bytes, quote, b'"')?));
    }
    if bytes.get(start..start + 2) == Some(b"b'") {
        return Ok(Some(skip_quoted(bytes, start + 1, b'\'')?));
    }
    if bytes.get(start) == Some(&b'\'') && looks_like_char_literal(bytes, start) {
        return Ok(Some(skip_quoted(bytes, start, b'\'')?));
    }
    Ok(None)
}

fn skip_raw_string(bytes: &[u8], start: usize) -> Result<Option<usize>, ()> {
    let mut cursor = match bytes.get(start..) {
        Some([b'r', ..]) => start + 1,
        Some([b'b' | b'c', b'r', ..]) => start + 2,
        _ => return Ok(None),
    };
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return Ok(None);
    }
    let hashes = cursor - hashes_start;
    cursor += 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes)
                == Some(&bytes[hashes_start..hashes_start + hashes])
        {
            return Ok(Some(cursor + 1 + hashes));
        }
        cursor += 1;
    }
    Err(())
}

fn skip_quoted(bytes: &[u8], quote: usize, delimiter: u8) -> Result<usize, ()> {
    let mut cursor = quote + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor += 2;
        } else if bytes[cursor] == delimiter {
            return Ok(cursor + 1);
        } else {
            cursor += 1;
        }
    }
    Err(())
}

fn looks_like_char_literal(bytes: &[u8], quote: usize) -> bool {
    let Some(first) = bytes.get(quote + 1) else {
        return false;
    };
    if *first == b'\\' {
        return true;
    }
    let width = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return false,
    };
    bytes.get(quote + 1 + width) == Some(&b'\'')
}

/// An automatically removed private scratch directory.
#[derive(Debug)]
pub struct PrivateTempDir(tempfile::TempDir);

impl PrivateTempDir {
    /// Return the scratch directory path.
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for PrivateTempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl std::ops::Deref for PrivateTempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

/// Create an automatically cleaned-up scratch directory suitable for private
/// Kettle state.
///
/// Unix requests owner-only permissions explicitly instead of inheriting a
/// permissive ambient umask. Windows stages beneath the user profile because
/// the shared temporary directory can grant deletion rights to other users.
pub fn private_tempdir(prefix: &str) -> PrivateTempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(windows)]
    let dir = {
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
        builder
            .tempdir_in(base)
            .expect("create private test directory in the user profile")
    };
    #[cfg(not(windows))]
    let dir = builder.tempdir().expect("create private test directory");
    PrivateTempDir(dir)
}

#[cfg(test)]
mod tests {
    use super::production_source;

    const BEFORE: &str = "const BEFORE: usize = 1;\n";
    const AFTER: &str = "\nconst AFTER: usize = 2;\n";

    fn braced_item(body: &str) -> String {
        format!("{BEFORE}#[cfg(test)]\nmod tests {{\n{body}\n}}{AFTER}")
    }

    fn without_braced_item() -> String {
        format!("{BEFORE}{AFTER}")
    }

    #[test]
    fn ignores_braces_in_ordinary_strings() {
        let src = braced_item(
            "    const TEXT: &str = \"escaped quote: \\\" and brace {\n}\nclosing }\";",
        );
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn ignores_braces_in_hashed_raw_strings() {
        let src = braced_item(
            "    const RAW: &str = r\"}\";\n    const ONE: &str = r#\"}\"#;\n    const TWO: &str = r##\"opening {\n}\nclosing }\"##;",
        );
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn ignores_braces_in_byte_strings() {
        let src = braced_item("    const BYTES: &[u8] = b\"{}\\\"}\";");
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn ignores_braces_in_line_comments() {
        let src = braced_item("    // } must not close the module\n    fn test_only() {}");
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn ignores_braces_in_block_comments() {
        let src = braced_item("    /* } must not close the module */\n    fn test_only() {}");
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn ignores_braces_in_nested_block_comments() {
        let src =
            braced_item("    /* outer { /* inner } */ still outer } */\n    fn test_only() {}");
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn ignores_braces_in_char_literals() {
        let src = braced_item("    const OPEN: char = '{';\n    const QUOTE: char = '\\'';");
        assert_eq!(production_source(&src), without_braced_item());
    }

    #[test]
    fn removes_indented_test_items() {
        let src = "trait Example {\n    /// Test-only method.\n    #[cfg(test)]\n    fn test_only() {\n        let _ = '}';\n    }\n    fn keep();\n}\n";
        assert_eq!(
            production_source(src),
            "trait Example {\n\n    fn keep();\n}\n"
        );
    }

    #[test]
    fn removes_cfg_all_test_items() {
        let src = "const BEFORE: u8 = 1;\n#[cfg(all(unix, test, feature = \"extra\"))]\nfn test_only() {}\nconst AFTER: u8 = 2;\n";
        assert_eq!(
            production_source(src),
            "const BEFORE: u8 = 1;\n\nconst AFTER: u8 = 2;\n"
        );
    }

    #[test]
    fn matches_the_item_body_after_const_generic_braces() {
        let src = "#[cfg(test)]\nfn test_only() -> Wrapper<{ if true { 1 } else { 2 } }> {\n    panic!();\n}\nconst KEEP: bool = true;\n";
        assert_eq!(production_source(src), "\nconst KEEP: bool = true;\n");
    }

    #[test]
    fn removes_any_cfg_predicate_with_bare_test_term() {
        let src = "#[cfg(not(any(feature = \"fastest\", test)))]\nconst TEST_ONLY: bool = true;\nconst KEEP: bool = true;\n";
        assert_eq!(production_source(src), "\nconst KEEP: bool = true;\n");
    }

    #[test]
    fn removes_semicolon_terminated_items() {
        let src = "#[cfg(test)] use crate::{fixture, helpers};\n#[cfg(all(test, unix))] mod platform_tests;\n#[cfg(test)] const TEST_ONLY: bool = if true { true } else { false };\nconst KEEP: bool = true;\n";
        assert_eq!(production_source(src), "\n\n\nconst KEEP: bool = true;\n");
    }

    #[test]
    fn preserves_test_substrings_that_are_not_bare_terms() {
        let src = "#[cfg(feature = \"fastest\")]\nfn feature_code() {}\n#[cfg(testing)]\nfn testing_code() {}\n";
        assert_eq!(production_source(src), src);
    }

    #[test]
    fn normalizes_crlf_before_stripping() {
        let src = "const KEEP: u8 = 1;\r\n#[cfg(test)]\r\nmod tests {\r\n}\r\n";
        assert_eq!(production_source(src), "const KEEP: u8 = 1;\n\n");
    }

    #[test]
    fn malformed_input_is_returned_unchanged() {
        let unclosed_item = "#[cfg(test)]\nmod tests {\n";
        let unclosed_string = "#[cfg(test)]\nfn test_only() { let _ = \"unterminated; }\n";
        let unclosed_comment = "#[cfg(test)]\nfn test_only() { /* unterminated\n";
        assert_eq!(production_source(unclosed_item), unclosed_item);
        assert_eq!(production_source(unclosed_string), unclosed_string);
        assert_eq!(production_source(unclosed_comment), unclosed_comment);
    }

    #[cfg(unix)]
    #[test]
    fn private_tempdir_has_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = super::private_tempdir("kettle-test-support-");
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
