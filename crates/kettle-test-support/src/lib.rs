//! Shared filesystem fixtures for Kettle's workspace tests.

use std::path::Path;

/// Returns `src` with every test-only item removed, so a guard searching it
/// cannot match its own assertions.
///
/// An item is removed when its `cfg` predicate cannot hold in a non-test
/// build — `test`, `all(test, ...)` and so on. An item that merely *mentions*
/// `test` is kept: `any(unix, test)` compiles on every Unix build and
/// `not(test)` is production-only.
///
/// # Known limitation
///
/// An item's body is taken to start at its first top-level `{`. A braced macro
/// invocation in return-type position — `fn f() -> ty! { () } { .. }` — would
/// therefore end the item early and leave its real body in the slice. That form
/// is legal Rust but appears nowhere in this workspace (checked), and handling
/// it properly needs macro-aware parsing. Recorded rather than implemented; if
/// such an item is ever added under `cfg(test)`, the wrapper postcondition
/// asserting the slice contains no `#[test]` is the thing most likely to catch
/// it.
///
/// # Panics
///
/// Panics when the source cannot be lexed, **or** when a test-only attribute is
/// attached to something this helper cannot delimit — a `#[cfg(test)]` struct
/// field or match arm, say, rather than a whole item. The second case lexes
/// perfectly well; it is the item-extent step that has no answer, and the
/// contract covers both.
///
/// Failing closed is deliberate. Several callers read another file's source
/// directly and do not re-assert the slice postconditions, so returning the
/// input unchanged would hand them the complete file — test module included —
/// and every guard built on it would quietly go back to satisfying itself. A
/// panicking test helper is the loud failure; a permissive one is the bug this
/// whole helper exists to prevent.
pub fn production_source(src: &str) -> String {
    let normalized = src.replace("\r\n", "\n");
    strip_test_items(&normalized).unwrap_or_else(|()| {
        panic!(
            "production_source could not lex this source, so it cannot prove the \
             slice excludes test items. Returning the input unchanged would let \
             every guard built on it satisfy itself again, so this fails closed."
        )
    })
}

fn strip_test_items(src: &str) -> Result<String, ()> {
    let bytes = src.as_bytes();
    let mut production = String::with_capacity(src.len());
    let mut copied_through = 0;
    let mut cursor = 0;

    // Start of the doc-comment run immediately preceding `cursor`, tracked as
    // the forward lex proceeds.
    //
    // This used to be recovered afterwards by searching BACKWARDS for `/*`,
    // which is unsound: the search happily paired a `// */` line with an
    // already-closed doc comment further up and deleted every line between
    // them. On one three-line input the entire production half disappeared,
    // which a negative guard reads as a pass. `skip_lexeme` already walks
    // comments correctly going forward, so record the position there instead of
    // reconstructing it later.
    let mut doc_start: Option<usize> = None;

    while cursor < bytes.len() {
        if let Some(end) = skip_lexeme(bytes, cursor)? {
            if is_line_leading_doc_comment(bytes, cursor) {
                // Record the LINE start, not the `///` token, so an indented
                // doc comment is removed along with its indentation.
                let line_start = src[..cursor].rfind('\n').map_or(0, |newline| newline + 1);
                doc_start.get_or_insert(line_start);
            } else {
                // A string literal, or a comment that is not documentation.
                // Either way the run of docs attached to whatever follows has
                // been broken.
                doc_start = None;
            }
            cursor = end;
            continue;
        }

        if bytes[cursor] != b'#' {
            if !bytes[cursor].is_ascii_whitespace() {
                doc_start = None;
            }
            cursor += 1;
            continue;
        }

        let Some(first_attribute) = parse_attribute(bytes, cursor)? else {
            doc_start = None;
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
            doc_start = None;
            cursor = attributes_end;
            continue;
        }

        // Remove the attached doc comments along with the item: prose is
        // searchable text, so a needle quoted in a test item's documentation
        // would outlive the item and satisfy a guard on its own. Only a run
        // recorded by the forward lex counts, and only when nothing but
        // whitespace separates it from the attribute.
        let line_start = src[..cursor].rfind('\n').map_or(0, |newline| newline + 1);
        let attribute_starts_its_line = bytes[line_start..cursor]
            .iter()
            .all(u8::is_ascii_whitespace);
        let removal_start = match doc_start {
            Some(start) if attribute_starts_its_line => start,
            _ => {
                if attribute_starts_its_line {
                    line_start
                } else {
                    cursor
                }
            }
        };
        let item_start = skip_trivia(bytes, attributes_end)?;
        let item_end = find_item_end(bytes, item_start)?;

        production.push_str(&src[copied_through..removal_start]);
        copied_through = item_end;
        cursor = item_end;
        doc_start = None;
    }

    production.push_str(&src[copied_through..]);
    Ok(production)
}

/// True when a comment beginning at `start` is documentation that leads its
/// line — `///` or `/** … */`, with only whitespace before it.
///
/// A plain `//` or `/* … */` is deliberately excluded: it may document the
/// PRECEDING production item, and removing production text is the worse
/// failure, since a negative guard (`!src.contains(...)`) then passes while
/// protecting nothing.
fn is_line_leading_doc_comment(bytes: &[u8], start: usize) -> bool {
    let is_doc = match bytes.get(start..start + 3) {
        Some(b"///") => bytes.get(start + 3) != Some(&b'/'),
        Some(b"/**") => bytes.get(start + 3) != Some(&b'*') && bytes.get(start + 3) != Some(&b'/'),
        _ => false,
    };
    if !is_doc {
        return false;
    }
    bytes[..start]
        .iter()
        .rev()
        .take_while(|byte| **byte != b'\n')
        .all(u8::is_ascii_whitespace)
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
                        has_test_cfg: cfg_is_test_only(&bytes[contents_start..cursor])?,
                    }));
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Err(())
}

/// A `cfg` predicate evaluated with `test = false` and every other atom
/// unknown, because we only know one of the configuration's values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    False,
    True,
    Unknown,
}

impl Tri {
    fn not(self) -> Self {
        match self {
            Tri::False => Tri::True,
            Tri::True => Tri::False,
            Tri::Unknown => Tri::Unknown,
        }
    }
}

/// True when the item is compiled **only** under `cfg(test)`.
///
/// Presence of the `test` identifier is not the question, and treating it as
/// one silently deletes production code. `#[cfg(any(unix, test))]` compiles on
/// every Unix build and `#[cfg(not(test))]` is production-*only*, yet both
/// mention `test`. Stripping either removes real code from the slice, which
/// makes a negative guard (`!src.contains(...)`) pass while protecting nothing.
///
/// So evaluate the predicate three-valued with `test = false`: strip only when
/// the result is definitely `False`, i.e. the item cannot exist in a non-test
/// build no matter what the other atoms are.
fn cfg_is_test_only(contents: &[u8]) -> Result<bool, ()> {
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
    let (value, _) = eval_predicate(contents, cursor + 1)?;
    Ok(value == Tri::False)
}

/// Evaluate one predicate starting at `cursor`, returning its value and the
/// offset just past it. `cursor` sits immediately after the opening `(` of the
/// enclosing list, or at the start of a bare predicate.
fn eval_predicate(contents: &[u8], cursor: usize) -> Result<(Tri, usize), ()> {
    let mut cursor = skip_trivia(contents, cursor)?;
    let Some((name, end)) = identifier(contents, cursor) else {
        return Err(());
    };
    cursor = skip_trivia(contents, end)?;

    // `key = "value"` and bare identifiers other than `test` are unknown to us.
    if contents.get(cursor) != Some(&b'(') {
        if contents.get(cursor) == Some(&b'=') {
            cursor = skip_trivia(contents, cursor + 1)?;
            cursor = match skip_lexeme(contents, cursor)? {
                Some(end) => end,
                None => return Err(()),
            };
            return Ok((Tri::Unknown, cursor));
        }
        // `cfg(true)` / `cfg(false)` are boolean literals, stable since 1.79 and
        // usable at this workspace's 1.89 MSRV. `false` means the item exists in
        // no build at all, so it is safe to drop from a production slice.
        let value = match name {
            b"test" | b"false" => Tri::False,
            b"true" => Tri::True,
            _ => Tri::Unknown,
        };
        return Ok((value, cursor));
    }

    // A list form: not(..), all(..), any(..).
    cursor += 1;
    let mut values = Vec::new();
    loop {
        cursor = skip_trivia(contents, cursor)?;
        match contents.get(cursor) {
            Some(b')') => {
                cursor += 1;
                break;
            }
            Some(b',') => {
                cursor += 1;
                continue;
            }
            Some(_) => {
                let (value, next) = eval_predicate(contents, cursor)?;
                values.push(value);
                cursor = next;
            }
            None => return Err(()),
        }
    }

    let combined = match name {
        b"not" => {
            if values.len() != 1 {
                return Err(());
            }
            values[0].not()
        }
        b"all" => {
            if values.contains(&Tri::False) {
                Tri::False
            } else if values.iter().all(|v| *v == Tri::True) {
                Tri::True
            } else {
                Tri::Unknown
            }
        }
        b"any" => {
            if values.contains(&Tri::True) {
                Tri::True
            } else if values.iter().all(|v| *v == Tri::False) {
                // An empty `any()` is false in Rust, which matches this arm.
                Tri::False
            } else {
                Tri::Unknown
            }
        }
        // An unrecognised list form (a proc-macro cfg extension, say) tells us
        // nothing; assume it can appear in production rather than delete it.
        _ => Tri::Unknown,
    };
    Ok((combined, cursor))
}

/// Parse one identifier, accepting the raw form.
///
/// `r#test` is exactly equivalent to `test` in a `cfg` predicate. Parsing only
/// the leading `r` leaves the rest of the token unconsumed, the predicate
/// evaluates to Unknown, and the test-only item survives into the "production"
/// slice — which is the self-satisfying state this helper exists to prevent.
fn identifier(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
    // A raw identifier: `r#ident`. Skip the sigil and return the bare name, so
    // callers compare against `test` without knowing which form was written.
    let start = if bytes.get(start) == Some(&b'r') && bytes.get(start + 1) == Some(&b'#') {
        start + 2
    } else {
        start
    };
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

/// How old a scratch directory must be before a later run will remove it.
///
/// Long enough that a concurrently running test process is never in range —
/// the whole suite finishes in minutes — and short enough that the leftovers
/// never accumulate across a working session.
const STALE_SCRATCH_AFTER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Remove scratch directories that *earlier processes* left behind.
///
/// A guard cannot always win. `spawn_server` hands the activation `Primary` —
/// and with it an open `activation.lock` — to a thread that owns it for the
/// process lifetime, by design, so the directory is still pinned when the guard
/// drops at the end of the test. Unix unlinks a file someone still holds and the
/// directory goes; Windows refuses, and `TempDir::drop` discards the error. That
/// is how 148 `kettle*` entries accumulated in a real `%TEMP%`, and switching to
/// a guard alone would only have moved them to `%LOCALAPPDATA%` — which is
/// exactly what the Windows job reported when the claim was finally asserted
/// rather than assumed.
///
/// Nothing can clear those during the run that pinned them, so each run clears
/// what previous runs left. Matching on the caller's own prefix keeps this to
/// directories this helper made: a real `kettle-<uid>` temp directory shares no
/// prefix with `kettle-activation-…`.
fn sweep_stale_scratch(base: &Path, prefix: &str, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= max_age);
        if stale {
            // Still best-effort: the process that pinned it may yet be alive.
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[doc(hidden)]
pub fn sweep_stale_scratch_for_test(base: &Path, prefix: &str, max_age: std::time::Duration) {
    sweep_stale_scratch(base, prefix, max_age);
}

/// Create an automatically cleaned-up scratch directory suitable for private
/// Kettle state.
///
/// Unix requests owner-only permissions explicitly instead of inheriting a
/// permissive ambient umask. Windows stages beneath the user profile because
/// the shared temporary directory can grant deletion rights to other users.
///
/// Removal is belt and braces: the guard clears it when the test ends, and this
/// clears anything a previous run could not. See [`sweep_stale_scratch`] for why
/// the guard is not sufficient on its own.
pub fn private_tempdir(prefix: &str) -> PrivateTempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(windows)]
    let base = std::path::PathBuf::from(
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE"),
    );
    #[cfg(not(windows))]
    let base = std::env::temp_dir();

    sweep_stale_scratch(&base, prefix, STALE_SCRATCH_AFTER);

    let dir = builder
        .tempdir_in(&base)
        .expect("create private test directory");
    PrivateTempDir(dir)
}

#[cfg(test)]
mod sweep_tests {
    /// The sweep has to remove what an earlier run pinned, and leave everything
    /// else alone — including the live directories of a run happening right now,
    /// which is the failure mode that would turn this cleanup into flakiness.
    ///
    /// Age is the only thing separating the two, so it is passed in here rather
    /// than waiting an hour for the real threshold.
    #[test]
    fn stale_scratch_goes_and_everything_else_stays() {
        let base = super::private_tempdir("kettle-sweep-base-");

        let stale = base.path().join("kettle-swept-aaaa");
        let fresh = base.path().join("kettle-swept-bbbb");
        let foreign = base.path().join("kettle-1000");
        for dir in [&stale, &fresh, &foreign] {
            std::fs::create_dir(dir).expect("fixture directory");
        }

        // Zero age: everything matching the prefix is old enough to go.
        super::sweep_stale_scratch_for_test(
            base.path(),
            "kettle-swept-aaaa",
            std::time::Duration::ZERO,
        );
        assert!(!stale.exists(), "a stale scratch directory must be removed");
        assert!(
            fresh.exists(),
            "a directory outside the prefix must survive"
        );

        // The real call shape: same prefix as the live directories, but an age
        // no directory created moments ago can meet.
        super::sweep_stale_scratch_for_test(
            base.path(),
            "kettle-swept-",
            std::time::Duration::from_secs(3600),
        );
        assert!(
            fresh.exists(),
            "a concurrent run's live directory must not be swept"
        );
        assert!(
            foreign.exists(),
            "a real kettle-<uid> directory shares no prefix and must be untouched"
        );
    }
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
    fn keeps_an_item_whose_cfg_excludes_test_rather_than_requiring_it() {
        // `not(any(feature = "fastest", test))` compiles when the feature is
        // OFF and test is OFF — production-only code, the exact opposite of a
        // test item, despite the predicate mentioning `test`. This test
        // previously asserted the item was removed, which encoded the bug: any
        // appearance of the `test` identifier was treated as "test-only".
        let src = "#[cfg(not(any(feature = \"fastest\", test)))]\nconst PRODUCTION_ONLY: bool = true;\nconst KEEP: bool = true;\n";
        assert_eq!(production_source(src), src);
    }

    #[test]
    fn removes_only_items_that_cannot_exist_in_a_non_test_build() {
        // Strip: the predicate is false whenever `test` is off.
        for predicate in [
            "test",
            "all(test, unix)",
            "all(unix, test)",
            "any(all(test, unix), all(test, windows))",
        ] {
            let src = format!("#[cfg({predicate})]\nfn gone() {{}}\nfn keep() {{}}\n");
            assert!(
                !production_source(&src).contains("fn gone()"),
                "{predicate} is test-only and must be stripped"
            );
        }
        // Keep: the predicate can still hold with `test` off.
        for predicate in [
            "not(test)",
            "any(unix, test)",
            "any(windows, test)",
            "any(feature = \"asciicast\", test)",
            "any(target_os = \"linux\", test)",
            "feature = \"fastest\"",
        ] {
            let src = format!("#[cfg({predicate})]\nfn stays() {{}}\nfn keep() {{}}\n");
            assert!(
                production_source(&src).contains("fn stays()"),
                "{predicate} can hold without test and must survive"
            );
        }
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
    fn malformed_input_panics_rather_than_passing_the_file_through() {
        // This previously returned the input UNCHANGED, on the theory that
        // callers' postconditions would catch it. Several callers read another
        // file's source directly and assert nothing, so a pass-through handed
        // them the whole file — test module included — and every guard built on
        // it silently went back to satisfying itself. Fail closed instead.
        for malformed in [
            "#[cfg(test)]\nmod tests {\n",
            "#[cfg(test)]\nfn test_only() { let _ = \"unterminated; }\n",
            "#[cfg(test)]\nfn test_only() { /* unterminated\n",
        ] {
            let outcome = std::panic::catch_unwind(|| production_source(malformed));
            assert!(
                outcome.is_err(),
                "malformed input {malformed:?} must panic, not pass the file through"
            );
        }
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
