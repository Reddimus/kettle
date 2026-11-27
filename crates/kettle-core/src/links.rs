//! Hyperlink discovery: explicit OSC 8 links carried on cells, plus
//! autodetected URLs and local file paths in the visible grid.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use regex::Regex;
use std::sync::OnceLock;

use crate::event::EventProxy;

/// Maximum byte length accepted for an opened URL.
pub const MAX_URL_BYTES: usize = 4096;

/// A clickable link in viewport (visible-row) coordinates.
#[derive(Debug, Clone)]
pub struct Link {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub uri: String,
    /// Shared by the segments of one link: a URL wrapped across rows is one
    /// link with a segment per row, and hovering any of them lights them all.
    pub group: usize,
    /// Explicit OSC 8 link rather than detected terminal text.
    pub explicit: bool,
}

/// The pattern for detected URLs, shared with quick select. A scheme is
/// case-insensitive (RFC 3986), as `www.` is to a reader. A scheme starts a
/// URL wherever it is, but a `www.` only where no name runs into it: not in
/// `docs/www.example.com.txt` or `me@www.example.com`. The pattern has no
/// look-behind, so it takes the character before such a `www.` into the
/// match, and [`url_match`] drops it; one pass stays linear.
pub(crate) const URL_PATTERN: &str =
    r#"(?i:https?://|ftp://|file://|(?:^|[^\w\p{N}/\\.\-@%+~])www\.)[^\s\x00-\x1f<>"]+"#;

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(URL_PATTERN).unwrap())
}

/// A [`URL_PATTERN`] match's start and text, without the character before
/// a `www.` that the pattern takes to check it. A URL starts with a letter,
/// and that character is never a letter or a number.
pub(crate) fn url_match<'a>(m: &regex::Match<'a>) -> (usize, &'a str) {
    let text = m.as_str();
    let taken = text
        .chars()
        .next()
        .filter(|c| !c.is_alphabetic())
        .map_or(0, char::len_utf8);
    (m.start() + taken, &text[taken..])
}

/// Whether a detected URL, trimmed of trailing punctuation, still names
/// something to open: a host after `scheme://` or `www.`, or a path after
/// `file://`. `see https://...` trims to a bare `https://`, and
/// `see www...` to `www`.
pub(crate) fn names_a_target(url: &str) -> bool {
    if let Some(rest) = url
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("www."))
        .map(|_| &url[4..])
    {
        // Opened as `https://` and the text, so its host is what follows
        // any user info (`www.example@/x` has none).
        return rest.starts_with(char::is_alphanumeric) && authority_has_host(url);
    }
    match url.split_once(':') {
        // A path before any query: not `file://?q=/x` or `file://localhost`.
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("file") => rest
            .strip_prefix("//")
            .is_some_and(|rest| rest[..rest.find(['?', '#']).unwrap_or(rest.len())].contains('/')),
        Some((_, rest)) => names_a_host(rest),
        None => false,
    }
}

/// Whether the text after a URL's `scheme:` is `//` and an authority with
/// a host, past any user info and port: not `//`, `///x`, `//?q`, `//#f`
/// or `//:8080/x`.
fn names_a_host(rest: &str) -> bool {
    rest.strip_prefix("//").is_some_and(authority_has_host)
}

/// Whether the authority that starts `rest` names a host.
fn authority_has_host(rest: &str) -> bool {
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.strip_prefix('[') {
        Some(literal) => literal.split_once(']').map_or("", |(address, _)| address),
        None => host.split_once(':').map_or(host, |(name, _)| name),
    };
    !host.is_empty()
}

/// Normalize a detected URL for opening, leaving its displayed text alone:
/// `www.x.test` and `WWW.x.test` open as `https://` and the text as shown.
pub fn normalize_url(text: &str) -> String {
    if text
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("www."))
    {
        format!("https://{text}")
    } else {
        text.to_string()
    }
}

fn path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
            (?:
                [A-Za-z]:[\\/][A-Za-z0-9._@+%()=$!,;~{}-]+(?:[\\/][A-Za-z0-9._@+%()=$!,;~{}-]+)*(?::[0-9]+(?::[0-9]+)?)?
              | (?:~|\.{1,2})/[A-Za-z0-9._@+%()=$!,;~{}-]+(?:/[A-Za-z0-9._@+%()=$!,;~{}-]+)*(?::[0-9]+(?::[0-9]+)?)?
              | /[A-Za-z0-9._@+%()=$!,;~{}-]+(?:/[A-Za-z0-9._@+%()=$!,;~{}-]+)*(?::[0-9]+(?::[0-9]+)?)?
              | (?:[A-Za-z0-9._@+%-]+/)+[A-Za-z0-9._@+%()=$!,;~{}-]+(?::[0-9]+(?::[0-9]+)?)?
            )
            "#,
        )
        .unwrap()
    })
}

fn diagnostic_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^:[ \t]+(?:fatal[ \t]+error|error|warning|note|message|info|remark)(?:[ \t:]|$)",
        )
        .unwrap()
    })
}

/// The path matches in one row's text that can become links, as (start byte,
/// end byte before trimming, text): trimmed of trailing punctuation, and not
/// starting inside a longer token. `/bar.png` in `foo(1)/bar.png` would name an unrelated file.
fn path_candidates(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let runs = crate::hints::quoted_runs(text).collect::<Vec<_>>();
    path_re().find_iter(text).filter_map(move |m| {
        let matched = crate::url_trim::trim_path_trailing(m.as_str());
        let before = text[..m.start()].chars().next_back();
        let after_host = text[..m.start()].strip_suffix(':').is_some_and(|prefix| {
            let previous = prefix
                .rsplit(|c: char| c.is_whitespace() || "\"'`*([{<=:,;&>|".contains(c))
                .next()
                .unwrap_or("");
            !previous.is_empty() && !previous.contains(['/', '\\'])
        });
        let filename = strip_location_suffix(matched);
        let path_end = m.start() + filename.len();
        let rest = &text[path_end..];
        // A numeric parenthesized suffix can end a filename in grep output,
        // too. Keep rejecting ambiguous colon-bearing names and quoted pieces.
        let grep_separator = strip_parenthesized_location(filename) != filename
            && rest.starts_with(':')
            && before.is_none_or(|c| !"\"'`*".contains(c));
        (!matched.is_empty()
            && crate::hints::has_link_path_context(matched)
            && !after_host
            && crate::hints::path_may_start_after(before)
            && (grep_separator || crate::hints::match_has_end(before, rest))
            && crate::hints::path_reaches_quote(text, &runs, m.start(), m.end()))
        .then_some((m.start(), m.end(), matched))
    })
}

/// All links visible in the current viewport. Explicit OSC 8 links take
/// precedence over autodetected URLs and file paths on the same cells.
pub fn links(term: &Term<EventProxy>) -> Vec<Link> {
    links_with_cwd(term, None)
}

/// All links visible in the current viewport, with `cwd` used to resolve
/// relative file paths such as `crates/kettle-core/src/links.rs:42`.
///
/// Autodetected URLs and paths are found per soft-wrapped logical line, so a
/// URL the terminal wrapped onto the next row is one link: it yields one
/// [`Link`] segment per row it covers, each with the whole URI. A hard line
/// break ends a line, so text a program wrapped itself is never joined.
pub fn links_with_cwd(term: &Term<EventProxy>, cwd: Option<&str>) -> Vec<Link> {
    let grid = term.grid();
    let cols = grid.columns();
    let rows = grid.screen_lines();
    // Scan the VISIBLE viewport, not the active screen. A visible viewport row
    // `row` maps to grid line `row - display_offset` (alacritty addresses history
    // with NEGATIVE lines). `grid[Line(row)]` always reads the active (bottom)
    // screen, so while scrolled back it would return the active screen's links
    // and the renderer would paint their underlines over the scrolled-back history.
    // This mirrors the grid-absolute-to-viewport conversion the renderer uses for
    // cell decorations and selection.
    let off = grid.display_offset() as i32;
    let lines: Vec<i32> = (0..rows).map(|row| row as i32 - off).collect();
    let mut out: Vec<Link> = Vec::new();

    // OSC 8 runs: consecutive cells sharing a hyperlink URI. Each row's runs
    // are kept so autodetection skips the cells they already cover.
    // Columns already linked on each row, half-open.
    let mut covered: Vec<crate::hints::Taken> = (0..rows).map(|_| Default::default()).collect();
    let mut group = 0usize;
    for (row, &gl) in lines.iter().enumerate() {
        let mut c = 0usize;
        while c < cols {
            let cell = &grid[Point::new(Line(gl), Column(c))];
            if let Some(h) = cell.hyperlink() {
                let start = c;
                while c < cols {
                    let cc = &grid[Point::new(Line(gl), Column(c))];
                    // The same link, or the same target linked again beside
                    // it. A target past the cap is never compared by its
                    // bytes, which would cost its length at every cell.
                    match cc.hyperlink() {
                        Some(h2)
                            if std::ptr::eq(h2.uri(), h.uri())
                                || (h.uri().len() <= MAX_URL_BYTES && h2.uri() == h.uri()) =>
                        {
                            c += 1
                        }
                        _ => break,
                    }
                }
                let end = c.saturating_sub(1);
                // An oversized target links nothing, but its cells stay
                // covered, so their text is not linked in its place.
                covered[row].insert(start, end + 1);
                if h.uri().len() > MAX_URL_BYTES {
                    continue;
                }
                let continuation = row > 0 && start == 0 && {
                    let previous = &grid[Point::new(Line(lines[row - 1]), Column(cols - 1))];
                    previous
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::WRAPLINE)
                        && previous.hyperlink().as_ref() == Some(&h)
                };
                let previous_group = continuation
                    .then(|| out.last())
                    .flatten()
                    .filter(|link| {
                        link.row + 1 == row && link.end_col + 1 == cols && link.uri == h.uri()
                    })
                    .map(|link| link.group);
                let link_group = previous_group.unwrap_or_else(|| {
                    let next = group;
                    group += 1;
                    next
                });
                out.push(Link {
                    row,
                    start_col: start,
                    end_col: end,
                    uri: h.uri().to_string(),
                    group: link_group,
                    explicit: true,
                });
            } else {
                c += 1;
            }
        }
    }

    // Reuse the scan buffers across every logical line instead of allocating
    // per line each time links are recomputed (every redraw).
    let mut text = String::with_capacity(cols);
    let mut pos_of_byte: Vec<(usize, usize)> = Vec::with_capacity(cols * 2);
    let mut first = 0;
    while first < rows {
        let read = crate::grid_text::logical_line_into(
            grid,
            &lines,
            first,
            cols,
            &mut text,
            &mut pos_of_byte,
        );
        // Any part of a token cut at the viewport edge may belong to a URL.
        let leading_end = text.find(char::is_whitespace).unwrap_or(text.len());
        let trailing_start = text
            .rfind(char::is_whitespace)
            .map_or(0, |at| at + text[at..].chars().next().unwrap().len_utf8());
        let fragment = move |start: usize, end: usize| {
            (read.cut_before && start < leading_end) || (read.cut_after && end > trailing_start)
        };
        let mut push =
            |start: usize, len: usize, uri: String, covered: &mut Vec<crate::hints::Taken>| {
                let mut segments = row_segments(&pos_of_byte, start, len);
                // A wide character's spacer cell is part of the link too.
                for (row, _, end) in &mut segments {
                    if *end + 1 < cols
                        && grid[Point::new(Line(lines[*row]), Column(*end))]
                            .flags
                            .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR)
                    {
                        *end += 1;
                    }
                }
                // Skip a match overlapping a link already found on any row it covers.
                if segments
                    .iter()
                    .any(|&(row, s, e)| covered[row].overlaps(s, e + 1))
                {
                    return;
                }
                for (row, s, e) in segments {
                    covered[row].insert(s, e + 1);
                    out.push(Link {
                        row,
                        start_col: s,
                        end_col: e,
                        uri: uri.clone(),
                        group,
                        explicit: false,
                    });
                }
                group += 1;
            };
        // Take a refused match's cells, so no piece of it links either.
        // An explicit link over some of its cells keeps them.
        let cover = |start: usize, len: usize, covered: &mut Vec<crate::hints::Taken>| {
            for (row, s, e) in row_segments(&pos_of_byte, start, len) {
                covered[row].fill(s, e + 1);
            }
        };
        for m in url_re().find_iter(&text) {
            let (start, raw) = url_match(&m);
            let matched = crate::url_trim::trim_trailing(raw);
            if fragment(start, m.end()) {
                continue;
            }
            // A scheme with nothing after it is no link, and its query is
            // not a local path (`https://?q=/etc/hosts`).
            if !names_a_target(matched) {
                cover(start, raw.len(), &mut covered);
                continue;
            }
            let uri = normalize_url(matched);
            push(start, matched.len(), uri, &mut covered);
        }
        // A quoted path goes before bare ones, so they cannot take a piece
        // of it; its link covers a location suffix as a bare path's does.
        for (start, _, close) in crate::hints::quoted_paths(&text) {
            if fragment(start - 1, close + 1) {
                continue;
            }
            match path_match_to_file_uri(&text[start..close], cwd) {
                Some(uri) => push(start, close - start, uri, &mut covered),
                // Refused (a climb, no directory to resolve against): no
                // piece of it may link either.
                None => cover(start, close - start, &mut covered),
            }
        }
        for (start, end, matched) in path_candidates(&text) {
            if fragment(start, end) {
                continue;
            }
            // A colon alone also occurs in file, grep, cat and ls output.
            // Only a compiler diagnostic keyword identifies a numeric location.
            let path = if diagnostic_prefix_re().is_match(&text[start + matched.len()..]) {
                strip_parenthesized_location(matched)
            } else {
                matched
            };
            if let Some(uri) = path_match_to_file_uri(path, cwd) {
                push(start, matched.len(), uri, &mut covered);
            }
        }
        first = read.next;
    }
    // Reading order, as before: rows top to bottom, then columns.
    out.sort_by_key(|link| (link.row, link.start_col));
    out
}

/// The per-row pieces of the byte range `start..start + len` of a logical
/// line, as (row, first column, last column).
fn row_segments(
    pos_of_byte: &[(usize, usize)],
    start: usize,
    len: usize,
) -> Vec<(usize, usize, usize)> {
    let mut segments: Vec<(usize, usize, usize)> = Vec::new();
    for &(row, col) in pos_of_byte.iter().skip(start).take(len) {
        match segments.last_mut() {
            Some((last_row, _, end)) if *last_row == row => *end = (*end).max(col),
            _ => segments.push((row, col, col)),
        }
    }
    segments
}

fn path_match_to_file_uri(raw: &str, cwd: Option<&str>) -> Option<String> {
    path_match_to_file_uri_with_home(raw, cwd, || {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
    })
}

fn path_match_to_file_uri_with_home(
    raw: &str,
    cwd: Option<&str>,
    home: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let path = strip_location_suffix(raw);
    // Only a quoted path holds a space (see `hints::quoted_paths`); the
    // URL encodes it. Encoded, a character that hides text would pass the
    // URL's own check, so it is refused here, as `is_safe_url` refuses it.
    if path.is_empty() || path.chars().any(|c| c != ' ' && hides_text(c)) {
        return None;
    }
    let normalized = path.replace('\\', "/");
    if normalized.starts_with("//")
        || has_parent_component(&normalized)
        || climbs_once_decoded(&normalized)
    {
        return None;
    }

    let absolute = if is_windows_drive_path(&normalized) {
        format!("/{normalized}")
    } else if normalized.starts_with('/') {
        normalized
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        let home = home()?;
        let mut home = home.replace('\\', "/");
        if home.is_empty() || home.starts_with("//") || has_parent_component(&home) {
            return None;
        }
        if is_windows_drive_path(&home) {
            home.insert(0, '/');
        }
        format!("{}/{}", home.trim_end_matches('/'), rest)
    } else {
        let cwd = cwd?;
        let mut cwd = cwd.replace('\\', "/");
        if cwd.is_empty() || cwd.starts_with("//") || has_parent_component(&cwd) {
            return None;
        }
        if is_windows_drive_path(&cwd) {
            cwd.insert(0, '/');
        }
        let rel = normalized.strip_prefix("./").unwrap_or(&normalized);
        if rel.starts_with('/') || rel.is_empty() {
            return None;
        }
        format!("{}/{}", cwd.trim_end_matches('/'), rel)
    };

    if !absolute.starts_with('/') || absolute.starts_with("//") || has_parent_component(&absolute) {
        return None;
    }
    let uri = format!("file://{}", encode_file_path(&absolute));
    is_safe_url(&uri).then_some(uri)
}

fn strip_location_suffix(path: &str) -> &str {
    let mut end = path.len();
    for _ in 0..2 {
        let Some(pos) = path[..end].rfind(':') else {
            break;
        };
        if is_windows_drive_colon(path, pos) {
            break;
        }
        if path[pos + 1..end].chars().all(|c| c.is_ascii_digit()) {
            end = pos;
        } else {
            break;
        }
    }
    &path[..end]
}

fn strip_parenthesized_location(path: &str) -> &str {
    if let Some(inner) = path.strip_suffix(')')
        && let Some(pos) = inner.rfind('(')
    {
        let mut parts = inner[pos + 1..].split(',');
        let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if parts.next().is_some_and(numeric)
            && parts.next().is_none_or(numeric)
            && parts.next().is_none()
        {
            return &path[..pos];
        }
    }
    path
}

fn is_windows_drive_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

fn is_windows_drive_colon(path: &str, pos: usize) -> bool {
    let b = path.as_bytes();
    pos == 1 && b.len() >= 3 && b[0].is_ascii_alphabetic() && (b[2] == b'/' || b[2] == b'\\')
}

/// Whether a detected path has a segment that names a parent only once its
/// escapes are decoded (`%2e%2e`, `.%2E`, `..%20`). A `%` in it is encoded in
/// its URL, so `100%2e.pdf` stays that name, but a handler that decodes
/// twice would climb, so such a path is not opened.
pub(crate) fn climbs_once_decoded(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| {
        let lowered = segment.to_ascii_lowercase();
        segment.contains('%') && climbs(&lowered.replace("%2e", ".").replace("%20", " "))
    })
}

fn has_parent_component(path: &str) -> bool {
    path.split('/').any(climbs)
}

/// Whether a path segment names the parent directory: `..`, or `..` and
/// trailing spaces and dots, which Windows drops from the end of a name
/// (`.. `). A segment of dots alone stays a name there, so `...` and
/// `a..b` do not climb.
fn climbs(segment: &str) -> bool {
    segment.starts_with("..")
        && segment.bytes().all(|b| b == b'.' || b == b' ')
        && (segment == ".." || segment.contains(' '))
}

fn encode_file_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Whether `c` hides or reorders text instead of showing as itself: a
/// control, Unicode whitespace (the no-break and ideographic spaces, line
/// and paragraph separators), a format character (zero-width spaces and
/// joiners, bidirectional controls, the byte order mark, tags), another
/// default-ignorable code point (variation selectors, Hangul fillers), or
/// the blank Braille pattern. Text holding one does not read as what it
/// names, so a link holding one does not open.
pub(crate) fn hides_text(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

/// Whether a URI from terminal output is safe to hand to the OS opener.
/// Terminal output is untrusted: an OSC 8 hyperlink (or a crafted line)
/// can carry an arbitrary URI, and custom schemes (`vscode:`, `ms-…`,
/// `javascript:`, app handlers that exec) are a known abuse/RCE vector.
/// We allow only a conservative scheme allowlist and reject controls,
/// whitespace, characters that hide or reorder text (see `hides_text`),
/// and absurd lengths, so the URI a user reads is the one that opens. Pure
/// — fully unit tested.
pub fn is_safe_url(uri: &str) -> bool {
    if uri.is_empty() || uri.len() > MAX_URL_BYTES {
        return false;
    }
    if uri.chars().any(hides_text) {
        return false;
    }
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false; // no scheme → not opened
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "ftp" | "ftps" => names_a_host(rest),
        "mailto" => !rest.is_empty(),
        // Allow LOCAL files only — never a remote authority.
        "file" => is_local_file_url(uri),
        _ => false,
    }
}

/// The local path a `file://` URI names, decoded: `file:///a%20b` is `/a b`.
/// The path ends at a `?` or `#`, as a URL parser reads it, so
/// `file:///x/Calculator.app#note` names `Calculator.app`. `None` for
/// anything [`is_safe_url`] would refuse, another scheme, an invalid percent
/// escape (never decoded to U+FFFD), an encoded separator (`%2F`, `%5C`,
/// which could join segments into `//host/share`), or a decoded path holding
/// a control character or a `..` segment.
pub fn local_file_path(uri: &str) -> Option<std::path::PathBuf> {
    if !is_safe_url(uri) {
        return None;
    }
    let decoded = decoded_file_url_path(uri)?;
    // `file:///C:/x` names `C:/x` on Windows.
    let decoded = match decoded.as_bytes() {
        [b'/', drive, b':', ..] if cfg!(windows) && drive.is_ascii_alphabetic() => {
            decoded[1..].to_string()
        }
        _ => decoded,
    };
    Some(std::path::PathBuf::from(decoded))
}

/// The path of a `file://` URI, decoded strictly: up to its `?` or `#`, as a
/// URL parser reads it, with every escape valid hex (never decoded to
/// U+FFFD), and checked after decoding: no encoded separator (`%2F`, `%5C`,
/// which could join segments into `//host/share`), no control character,
/// no `//` start and no `..` segment. `None` otherwise, or for another
/// scheme or a URI without a path.
fn decoded_file_url_path(uri: &str) -> Option<String> {
    let rest = uri.get(..7).filter(|p| p.eq_ignore_ascii_case("file://"))?;
    let rest = &uri[rest.len()..];
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(at) => &rest[at..], // `localhost` or a loopback authority
        None => return None,
    };
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let mut bytes = Vec::with_capacity(path.len());
    let mut iter = path.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let hex = [iter.next()?, iter.next()?];
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            let decoded = u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?;
            if matches!(decoded, b'/' | b'\\') {
                return None;
            }
            bytes.push(decoded);
        } else {
            bytes.push(byte);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    let clean = !decoded.chars().any(char::is_control)
        && !decoded.starts_with("//")
        && !decoded.split(['/', '\\']).any(climbs);
    clean.then_some(decoded)
}

/// A `file://` URL for an absolute local `path`, percent-encoded here so
/// whatever receives it reads the same path back: no query or fragment, and
/// a Windows drive path as `file:///C:/…`. `None` for a relative path or one
/// [`is_safe_url`] would refuse.
pub fn file_url_for_path(path: &std::path::Path) -> Option<String> {
    let text = path.to_str()?;
    // A backslash separates only on Windows; elsewhere it is part of a name,
    // which a local file URL cannot carry (`is_safe_url` refuses `%5C`).
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.to_string()
    };
    let absolute = if is_windows_drive_path(&text) {
        format!("/{text}")
    } else if text.starts_with('/') && !text.starts_with("//") {
        text
    } else {
        return None;
    };
    let url = format!("file://{}", encode_file_path(&absolute));
    is_safe_url(&url).then_some(url)
}

/// Extensions of programs and shortcuts that are folders on disk: macOS
/// applications, installers and plug-ins that run or install when opened.
const PROGRAM_BUNDLES: &[&str] = &[
    "app",
    "pkg",
    "mpkg",
    "workflow",
    "action",
    "scptd",
    "prefpane",
    "saver",
    "wdgt",
    "kext",
    "qlgenerator",
    "mdimporter",
    "appex",
    "xpc",
    "plugin",
];

/// Extensions of programs, scripts, installers and shortcuts that are files:
/// opening one runs something or follows a link elsewhere.
const PROGRAM_FILES: &[&str] = &[
    // Windows programs, scripts, installers and shortcuts
    "exe",
    "com",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "psd1",
    "ps1xml",
    "ps2",
    "ps2xml",
    "psc1",
    "psc2",
    "msh",
    "msh1",
    "msh2",
    "mshxml",
    "msh1xml",
    "msh2xml",
    "vb",
    "vbs",
    "vbe",
    "vbp",
    "js",
    "jse",
    "ws",
    "wsc",
    "wsf",
    "wsh",
    "sct",
    "shb",
    "shs",
    "msi",
    "msp",
    "mst",
    "msu",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appinstaller",
    "scr",
    "pif",
    "cpl",
    "hta",
    "chm",
    "hlp",
    "jar",
    "jnlp",
    "reg",
    "msc",
    "scf",
    "inf",
    "ins",
    "isp",
    "xll",
    "xbap",
    "ade",
    "adp",
    "mde",
    "grp",
    "diagcab",
    "application",
    "appref-ms",
    "gadget",
    "lnk",
    "url",
    "website",
    "rdp",
    "ica",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "searchconnector-ms",
    // macOS scripts, terminal settings and location files
    "command",
    "tool",
    "terminal",
    "term",
    "scpt",
    "applescript",
    "osax",
    "webloc",
    "inetloc",
    "fileloc",
    "ftploc",
    "afploc",
    "vncloc",
    "mailloc",
    "newsloc",
    "telnetloc",
    "mobileconfig",
    "shortcut",
    // Linux and Unix launchers and scripts
    "desktop",
    "sh",
    "bash",
    "zsh",
    "fish",
    "csh",
    "tcsh",
    "ksh",
    "run",
    "bin",
    "appimage",
    "flatpakref",
    "py",
    "pyw",
    "pyz",
    "pyzw",
    "pyc",
    "pyo",
    "pl",
    "rb",
];

/// Whether `path` names a program or a shortcut by its extension: opening it
/// would run something or follow a link elsewhere, not show a document. A
/// folder (`is_dir`) is one only with a bundle extension (`.app`): a folder
/// named `archive.sh` opens as a folder.
///
/// The name is read as Windows reads it, whatever the platform: trailing
/// dots and spaces are dropped (`payload.exe.` runs `payload.exe`), and an
/// alternate data stream, `file:stream:type`, is checked by its file name
/// and by its stream name (`a.exe::$DATA`, `a.txt:b.exe`,
/// `a.txt:b.exe::$DATA`).
pub fn names_program_or_shortcut(path: &std::path::Path, is_dir: bool) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    let known = |part: &str| {
        let Some((_, extension)) = part.trim_end_matches(['.', ' ']).rsplit_once('.') else {
            return false;
        };
        let listed = |list: &[&str]| {
            list.iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        };
        listed(PROGRAM_BUNDLES) || (!is_dir && listed(PROGRAM_FILES))
    };
    // Each part on its own: the last `.` of the whole name may belong to
    // neither, as in `a.txt:b.exe::$DATA`.
    name.split(':').any(known)
}

/// Whether a `file://` URI points at the local machine with no traversal.
///
/// Blocking `..` alone is not enough. On Windows a remote authority such as
/// `file://host/path` maps to the UNC `\\host\path`, so the OS opener transparently
/// connects to `host` over SMB/WebDAV and leaks the user's NTLMv2 hash (forced
/// authentication / pass-the-hash) plus SSRF to an arbitrary host, all reachable
/// from untrusted PTY output (autodetected link or OSC 8). Accept only an empty
/// authority (`file:///path`) or an explicit loopback host, and reject any
/// backslash / `file:////` / percent-encoded separator that could smuggle an
/// authority back in. Traversal is judged on the decoded path's segments, so a
/// name holding dots (`a..b`, `...`) or a query mentioning them stays local.
fn is_local_file_url(uri: &str) -> bool {
    // is_safe_url lowercases the scheme before the `file` arm, so accept
    // `FILE://` / `File://` here too. The authority and separator checks below
    // already use a lowercased copy.
    let rest = match uri.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("file://") => &uri[7..],
        _ => return false,
    };
    let lowered = uri.to_ascii_lowercase();
    if uri.contains('\\')
        || lowered.contains("%5c")
        // `file:////host/...` leaves an empty authority but a `//host` path that
        // some openers still treat as a UNC share — reject the double slash.
        || rest.starts_with("//")
    {
        return false;
    }
    // The authority is everything before the first '/'. Empty (`file:///...`)
    // or an explicit loopback host is local; anything else is remote.
    let authority = rest.split('/').next().unwrap_or("");
    matches!(
        authority.to_ascii_lowercase().as_str(),
        "" | "localhost" | "127.0.0.1" | "[::1]"
    )
        // What the escapes decode to is checked too: `.%2e`, `%2F%2Fhost`,
        // `%00` and a bad escape spell traversal or garbage only once decoded.
        && decoded_file_url_path(uri).is_some()
}

#[cfg(test)]
mod tests {
    use super::{
        is_safe_url, links, links_with_cwd, names_program_or_shortcut, path_candidates,
        path_match_to_file_uri,
    };
    use crate::event::EventProxy;
    use crate::term::TermSize;
    use alacritty_terminal::Term;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::cell::Flags;

    fn term_with(columns: usize, rows: &[&str], soft_wrapped: &[usize]) -> Term<EventProxy> {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut term = Term::new(
            Config::default(),
            &TermSize {
                columns,
                screen_lines: rows.len(),
            },
            EventProxy::new(tx, std::sync::Arc::new(|| {})),
        );
        for (line, text) in rows.iter().enumerate() {
            for (column, c) in text.chars().enumerate() {
                term.grid_mut()[Line(line as i32)][Column(column)].c = c;
            }
        }
        for &line in soft_wrapped {
            term.grid_mut()[Line(line as i32)][Column(columns - 1)]
                .flags
                .insert(Flags::WRAPLINE);
        }
        term
    }

    /// A URL the terminal wrapped onto the next row is one link: a segment on
    /// each row, both opening the whole URL.
    #[test]
    fn a_soft_wrapped_url_is_one_link() {
        let term = term_with(12, &["see https://", "x.test/a/b c", "next"], &[0]);
        let found = links(&term);
        let segments = found
            .iter()
            .map(|link| (link.row, link.start_col, link.end_col, link.uri.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(found[0].group, found[1].group, "one link, two segments");
        assert_eq!(
            segments,
            [
                (0, 4, 11, "https://x.test/a/b"),
                (1, 0, 9, "https://x.test/a/b"),
            ]
        );
    }

    /// A URL running past the last visible row is cut there: no link rather
    /// than one opening a fragment. One that ends before the cut stays.
    #[test]
    fn a_url_cut_at_the_viewport_edge_is_no_link() {
        let term = term_with(14, &["ok", "see https://a/"], &[1]);
        assert!(links(&term).is_empty(), "{:?}", links(&term));
        let term = term_with(14, &["ok", "https://a/b cd"], &[1]);
        let found = links(&term);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].uri, "https://a/b");
    }

    /// A terminal fed `bytes` as a program's output, so wraps, wide-character
    /// spacers and scrollback come from the parser itself.
    fn term_fed(columns: usize, lines: usize, bytes: &[u8]) -> Term<EventProxy> {
        let mut term = term_with(columns, &vec![""; lines], &[]);
        let mut processor: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        processor.advance(&mut term, bytes);
        term
    }

    #[test]
    fn clipped_url_query_is_not_a_local_link() {
        let term = term_fed(10, 4, b"https://x.test/a?redirect=/etc/hosts \r\nok");
        let found = links_with_cwd(&term, Some("/work"));
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn oversized_osc8_targets_do_not_fall_back_to_text() {
        for size in [4096, 4097] {
            let uri = format!("https://target.test/{}", "a".repeat(size - 20));
            assert_eq!(uri.len(), size);
            let output =
                format!("\x1b]8;;{uri}\x1b\\https://visible.test/a\r\n/etc/hosts\x1b]8;;\x1b\\");
            let term = term_fed(80, 2, output.as_bytes());
            let found = links_with_cwd(&term, Some("/work"));
            if size > 4096 {
                assert!(found.is_empty(), "{found:?}");
            } else {
                assert_eq!(found.len(), 2);
                assert!(found.iter().all(|link| link.uri == uri));
            }
        }
    }

    /// A URL wrapped in from above the first visible row, or running past the
    /// last, is cut: no link, not even a path link to its tail. Scrolled back
    /// to show all of it, it is one link.
    #[test]
    fn a_url_wrapped_across_the_viewport_edges_links_only_when_whole() {
        use alacritty_terminal::grid::Scroll;
        let url = "https://x.test/aaaa/bbbb";
        let mut term = term_fed(10, 3, format!("a\r\nb\r\nc\r\n{url}\r\nok").as_bytes());
        // Showing `test/aaaa/`, `bbbb`, `ok`: the URL began above.
        assert!(
            links_with_cwd(&term, Some("/p")).is_empty(),
            "{:?}",
            links_with_cwd(&term, Some("/p"))
        );
        term.scroll_display(Scroll::Delta(1));
        let found = links_with_cwd(&term, Some("/p"));
        assert_eq!(
            found
                .iter()
                .map(|link| (link.row, link.start_col, link.end_col))
                .collect::<Vec<_>>(),
            [(0, 0, 9), (1, 0, 9), (2, 0, 3)]
        );
        assert!(
            found
                .iter()
                .all(|link| link.uri == url && link.group == found[0].group)
        );
        // Showing `b`, `c`, `https://x.`: the URL runs on below.
        term.scroll_display(Scroll::Delta(2));
        assert!(links(&term).is_empty(), "{:?}", links(&term));
    }

    /// A wide character that did not fit on the last column wraps whole,
    /// leaving a spacer cell that is not part of the text: the URL is not cut
    /// at it.
    #[test]
    fn a_wide_character_wrapped_to_the_next_row_stays_in_the_url() {
        let term = term_fed(10, 2, "https://a\u{754c}/b".as_bytes());
        let found = links(&term);
        assert_eq!(
            found
                .iter()
                .map(|link| (link.row, link.start_col, link.end_col, link.uri.as_str()))
                .collect::<Vec<_>>(),
            [
                (0, 0, 8, "https://a\u{754c}/b"),
                (1, 0, 3, "https://a\u{754c}/b"),
            ]
        );
    }

    /// A hard line break ends the line: text a program wrapped itself is not
    /// joined into a different URL.
    #[test]
    fn a_hard_line_break_does_not_join_a_url() {
        let term = term_with(12, &["see https://", "x.test/a/b c"], &[]);
        assert!(
            links(&term)
                .iter()
                .all(|link| link.uri != "https://x.test/a/b"),
            "{:?}",
            links(&term)
        );
    }

    /// A link cannot begin inside a longer token; ordinary paths after a
    /// space, a delimiter, `=` or a list or redirect separator stay links.
    #[test]
    fn a_path_link_cannot_start_inside_a_token() {
        fn found(line: &str) -> Vec<&str> {
            path_candidates(line).map(|(_, _, text)| text).collect()
        }
        assert!(
            found("foo(1)/bar.png").is_empty(),
            "{:?}",
            found("foo(1)/bar.png")
        );
        assert!(found("x]/etc/hosts").is_empty());
        assert_eq!(found("open /etc/hosts"), ["/etc/hosts"]);
        assert_eq!(found("(/etc/hosts)."), ["/etc/hosts"]);
        assert_eq!(found("--out=/tmp/x.png"), ["/tmp/x.png"]);
        assert_eq!(found("cmd >/tmp/out.log"), ["/tmp/out.log"]);
        assert_eq!(found("see src/main.rs:12"), ["src/main.rs:12"]);
    }

    /// Dots ending a detected URL end a sentence or trail off, as Ghostty,
    /// WezTerm and GNOME Terminal read them, so they are not part of the
    /// link; dots inside it are, before a query or fragment too.
    #[test]
    fn trailing_dots_after_a_url_are_prose() {
        for (line, uri) in [
            (
                "see https://example.com/docs...",
                "https://example.com/docs",
            ),
            ("see https://example.com/a.", "https://example.com/a"),
            (
                "curl https://example.com/a.. -o out",
                "https://example.com/a",
            ),
            (
                "get https://example.com/files/report..csv",
                "https://example.com/files/report..csv",
            ),
            (
                "get https://example.com/a..?q=1",
                "https://example.com/a..?q=1",
            ),
            (
                "get https://example.com/a..#frag",
                "https://example.com/a..#frag",
            ),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links(&term);
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
    }

    /// A closing bracket that pairs with an open before it ends the link,
    /// whatever stray close came earlier in the query.
    #[test]
    fn a_paired_closing_bracket_stays_in_the_link() {
        let term = term_with(30, &["see https://x.test/a)b(c) end"], &[]);
        let found = links(&term);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].uri, "https://x.test/a)b(c)");
    }

    #[test]
    fn parent_directory_path_does_not_become_a_link_to_its_prefix() {
        let term = term_with(80, &["/tmp/.."], &[]);
        assert!(links(&term).is_empty());
    }

    #[test]
    fn tilde_links_expand_windows_home_directories() {
        assert_eq!(
            super::path_match_to_file_uri_with_home("~/Documents/report.pdf", None, || {
                Some(r"C:\Users\me".into())
            }),
            Some("file:///C:/Users/me/Documents/report.pdf".into())
        );
    }

    #[test]
    fn numeric_parentheses_in_filenames_are_literal() {
        for (text, uri) in [
            ("/tmp/report(2024)", "file:///tmp/report%282024%29"),
            ("/tmp/report(1,2)", "file:///tmp/report%281%2C2%29"),
            ("\"/tmp/report (1)\"", "file:///tmp/report%20%281%29"),
            (
                "\"/tmp/report(2024)\": error",
                "file:///tmp/report%282024%29",
            ),
            ("'/tmp/report (1,2)'", "file:///tmp/report%20%281%2C2%29"),
        ] {
            let term = term_with(100, &[text], &[]);
            let found = links(&term);
            assert_eq!(found.len(), 1, "{text}");
            assert_eq!(found[0].uri, uri, "{text}");
        }
    }

    #[test]
    fn numeric_parentheses_before_non_diagnostic_colons_are_literal() {
        for (text, uri) in [
            (
                "cat: /tmp/report(2024): No such file or directory",
                "file:///tmp/report%282024%29",
            ),
            ("/tmp/report(1): ASCII text", "file:///tmp/report%281%29"),
            ("./build(2):", "file:///work/build%282%29"),
            ("/tmp/a(2024):text", "file:///tmp/a%282024%29"),
            ("/tmp/a(1,2): text", "file:///tmp/a%281%2C2%29"),
            ("/tmp/a(1): errorless", "file:///tmp/a%281%29"),
            ("/tmp/a(1): fatal errors", "file:///tmp/a%281%29"),
            ("/tmp/a(1),: error", "file:///tmp/a%281%29"),
            ("/tmp/a(1):error", "file:///tmp/a%281%29"),
            ("\"/tmp/a(1,2)\": warning C2065", "file:///tmp/a%281%2C2%29"),
        ] {
            let term = term_with(100, &[text], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{text}");
            assert_eq!(found[0].uri, uri, "{text}");
        }
    }

    #[test]
    fn parenthesized_diagnostic_locations_are_not_filename_components() {
        for (suffix, diagnostic) in [
            ("(12)", "error C2143"),
            ("(12,5)", "warning C2065: unknown identifier"),
            ("(12)", "NOTE: declared here"),
            ("(12,5)", "FaTaL ErRoR C1083: cannot open file"),
            ("(12)", "message: see declaration"),
            ("(12,5)", "INFO: building"),
            ("(12)", "remark #123: optimized"),
        ] {
            let text = format!("src/main.cpp{suffix}: {diagnostic}");
            let term = term_with(80, &[&text], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].uri, "file:///work/src/main.cpp");
        }
        for name in [
            "src/main(backup).cpp",
            "src/main.cpp(backup)",
            "src/main.cpp(12,x)",
        ] {
            assert_eq!(
                super::strip_location_suffix(name),
                name,
                "ordinary parentheses remain in filenames"
            );
        }
    }

    #[test]
    fn host_prefixed_paths_are_not_local_links() {
        for line in [
            "user@host:/etc/hosts",
            "host:/etc/hosts",
            "user@host:dir/file.txt",
        ] {
            let term = term_with(80, &[line], &[]);
            assert!(links_with_cwd(&term, Some("/work")).is_empty(), "{line}");
        }
        let term = term_with(80, &["PATH=/usr/bin:/bin"], &[]);
        assert!(
            links_with_cwd(&term, Some("/work"))
                .iter()
                .any(|link| link.uri == "file:///bin")
        );
        let term = term_with(80, &["C:/Users/me/file.txt"], &[]);
        assert_eq!(links(&term)[0].uri, "file:///C:/Users/me/file.txt");
    }

    #[test]
    fn bare_paths_must_reach_their_enclosing_quote() {
        for line in [
            "'My Files/a b.txt'",
            "\"cannot open src/main.rs now\"",
            "\" docs/a b.pdf\"",
        ] {
            let term = term_with(80, &[line], &[]);
            assert!(links_with_cwd(&term, Some("/work")).is_empty(), "{line}");
        }
        let term = term_with(80, &["\"see src/main.rs\""], &[]);
        let found = links_with_cwd(&term, Some("/work"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].uri, "file:///work/src/main.rs");
    }

    #[test]
    fn bare_path_links_require_a_complete_filename() {
        for line in [
            "out/report#1.pdf",
            "out/report?raw=1",
            "out/report:final.pdf",
            "out/report:12,final.pdf",
            "\"docs/annual report.pdf",
        ] {
            let term = term_with(80, &[line], &[]);
            assert!(links_with_cwd(&term, Some("/work")).is_empty(), "{line}");
        }
        for (line, uri) in [
            ("src/main.rs:12:5", "file:///work/src/main.rs"),
            ("src/main.rs:12:let x = 1;", "file:///work/src/main.rs"),
            ("cmd >out/report.pdf", "file:///work/out/report.pdf"),
            ("see (out/report.pdf).", "file:///work/out/report.pdf"),
            ("\"out/report#1.pdf\"", "file:///work/out/report%231.pdf"),
            (
                "open \"docs/annual report.pdf\" now",
                "file:///work/docs/annual%20report.pdf",
            ),
        ] {
            let term = term_with(80, &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_uri_decodes_to_its_local_path() {
        use super::local_file_path;
        use std::path::PathBuf;
        assert_eq!(
            local_file_path("file:///tmp/a%20b.png"),
            Some(PathBuf::from("/tmp/a b.png"))
        );
        assert_eq!(
            local_file_path("file://localhost/etc/hosts"),
            Some(PathBuf::from("/etc/hosts"))
        );
        assert_eq!(
            local_file_path("file:///caf%C3%A9"),
            Some(PathBuf::from("/café"))
        );
        // Refused: another scheme, a remote authority, traversal, a bad escape.
        assert_eq!(local_file_path("https://x.test/a"), None);
        assert_eq!(local_file_path("file://evil.example/share/x"), None);
        assert_eq!(local_file_path("file:///a/../etc/passwd"), None);
        assert_eq!(local_file_path("file:///a%zz"), None);
        assert_eq!(local_file_path("file:///a%2"), None);
    }

    #[test]
    fn programs_and_shortcuts_are_known_by_extension() {
        use std::path::Path;
        let program = |name: &str| names_program_or_shortcut(Path::new(name), false);
        for name in [
            "setup.exe",
            "run.BAT",
            "x.ps1",
            "Calculator.app",
            "go.command",
            "a.lnk",
            "site.url",
            "page.webloc",
            "kettle.desktop",
            "build.sh",
            "tool.AppImage",
            "x.settingcontent-ms",
            "help.chm",
            "profile.mobileconfig",
            "office.rdp",
            "screen.vncloc",
            // Read as Windows reads a name.
            "payload.exe.",
            "payload.exe ",
            "payload.exe. . ",
            "payload.exe::$DATA",
            "notes.txt:payload.exe",
            ".exe",
            // A program stream, typed or not.
            "notes.txt:payload.exe::$DATA",
            "notes.txt:payload.bat::$DATA",
            "report.pdf:setup.msi::$DATA",
            "notes.txt:PAYLOAD.EXE::$DATA",
            "notes.txt:tool.ps1::$DATA",
            "a.txt:b.exe::$DATA",
            "notes.txt:payload.exe.::$DATA",
            "notes.txt:payload.exe::$DATA ",
            "notes.txt:payload.lnk:$DATA",
        ] {
            assert!(program(name), "{name}");
        }
        for name in [
            "diagram.png",
            "notes.md",
            "report.pdf",
            "movie.mp4",
            "README",
            "a.svg",
            "archive.tar.gz",
            "exe",
            "notes:2.txt",
            "notes.txt:Zone.Identifier",
            "notes.txt:Zone.Identifier:$DATA",
            "photo.jpg:payload.txt::$DATA",
            "notes.txt::$DATA",
        ] {
            assert!(!program(name), "{name}");
        }
        // A folder is a program only as a bundle.
        let folder = |name: &str| names_program_or_shortcut(Path::new(name), true);
        assert!(folder("Calculator.app"));
        assert!(folder("Installer.pkg"));
        assert!(folder("Calculator.app/"));
        assert!(!folder("archive.sh"));
        assert!(!folder("tools.exe"));
        assert!(!folder("photos"));
    }

    /// A file URL decodes to the path a URL parser reads: up to a `?` or `#`,
    /// with no control character or decoded `..` segment.
    #[cfg(unix)]
    #[test]
    fn a_file_uri_path_ends_at_its_query_or_fragment() {
        use super::local_file_path;
        use std::path::PathBuf;
        assert_eq!(
            local_file_path("file:///Applications/Calculator.app#note"),
            Some(PathBuf::from("/Applications/Calculator.app"))
        );
        assert_eq!(
            local_file_path("file:///tmp/a.pdf?page=2"),
            Some(PathBuf::from("/tmp/a.pdf"))
        );
        assert_eq!(
            local_file_path("file:///tmp/50%25%23.pdf"),
            Some(PathBuf::from("/tmp/50%#.pdf"))
        );
        assert_eq!(local_file_path("file:///tmp/a%00.pdf"), None);
        assert_eq!(local_file_path("file:///tmp/a%0A.pdf"), None);
        assert_eq!(local_file_path("file:///home/me/.%2e/etc/passwd"), None);
        assert_eq!(local_file_path("file:///home/me/%2e./etc/passwd"), None);
        // An encoded separator could join segments into a share's path.
        assert_eq!(
            local_file_path("file:///%2Fattacker.example/share/x.pdf"),
            None
        );
        assert_eq!(local_file_path("file:///tmp/a%2Fb.pdf"), None);
        assert_eq!(local_file_path("file:///tmp/a%5cb.pdf"), None);
        assert_eq!(local_file_path("file://localhost//host/share/x.pdf"), None);
    }

    /// The URL a custom handler gets for a checked file reads back as that
    /// file: encoded here, with nothing after the path.
    #[test]
    fn a_checked_path_becomes_its_own_file_url() {
        use super::file_url_for_path;
        use std::path::Path;
        assert_eq!(
            file_url_for_path(Path::new("/tmp/a b#1?.pdf")).as_deref(),
            Some("file:///tmp/a%20b%231%3F.pdf")
        );
        assert_eq!(
            file_url_for_path(Path::new("C:/Users/me/a b.pdf")).as_deref(),
            Some("file:///C:/Users/me/a%20b.pdf")
        );
        // A backslash separates on Windows; elsewhere it is part of a name a
        // file URL cannot carry, so no URL rather than a different path.
        #[cfg(windows)]
        assert_eq!(
            file_url_for_path(Path::new(r"C:\Users\me\a b.pdf")).as_deref(),
            Some("file:///C:/Users/me/a%20b.pdf")
        );
        #[cfg(not(windows))]
        assert_eq!(file_url_for_path(Path::new(r"/tmp/docs\run")), None);
        assert_eq!(file_url_for_path(Path::new("relative/a.pdf")), None);
        assert_eq!(file_url_for_path(Path::new("//host/share/a.pdf")), None);
        assert_eq!(file_url_for_path(Path::new("/a/../b.pdf")), None);
    }

    /// A quoted path with spaces is one link to the whole file, its spaces
    /// encoded; quoted prose links only the path inside it.
    #[test]
    fn a_quoted_path_is_one_link() {
        let found = |line: &str| {
            let term = term_with(line.chars().count(), &[line], &[]);
            links_with_cwd(&term, Some("/home/me/proj"))
                .into_iter()
                .map(|link| (link.start_col, link.end_col, link.uri))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found("open \"docs/annual report.pdf\" now"),
            [(
                6,
                27,
                "file:///home/me/proj/docs/annual%20report.pdf".to_string()
            )]
        );
        assert_eq!(
            found("File \"/my app/x.py:3\", line 3"),
            [(6, 19, "file:///my%20app/x.py".to_string())]
        );
        #[cfg(windows)]
        assert_eq!(
            found("`C:\\Program Files\\K\\a.toml`"),
            [(1, 25, "file:///C:/Program%20Files/K/a.toml".to_string())]
        );
        // Off Windows a one-letter prefix is a remote host, not a drive.
        #[cfg(not(windows))]
        assert!(
            found("scp \"h:/srv/my report.pdf\" .")
                .iter()
                .all(|(_, _, uri)| !uri.contains("%20")),
            "{:?}",
            found("scp \"h:/srv/my report.pdf\" .")
        );
        assert_eq!(
            found("\"see src/main.rs\""),
            [(5, 15, "file:///home/me/proj/src/main.rs".to_string())]
        );
        // A climb stays refused inside quotes too, and no piece of the
        // refused path links on its own.
        assert!(found("\"../up here/a.txt\"").is_empty());
        assert!(found("\"../my dir/etc/hosts.txt\"").is_empty());
    }

    /// A dotfile's name is its extension: a quoted path ending in one links
    /// whole, and a two-part relative path to one links like `src/main.rs`.
    #[test]
    fn a_path_ending_in_a_dotfile_is_a_link() {
        for (line, uri) in [
            (
                "open \"docs/my dir/.env\" now",
                "file:///work/docs/my%20dir/.env",
            ),
            (
                "open \"docs/annual report/.gitignore\" now",
                "file:///work/docs/annual%20report/.gitignore",
            ),
            ("see docs/.env ok", "file:///work/docs/.env"),
            ("see x/.a ok", "file:///work/x/.a"),
            ("see a/b/.env ok", "file:///work/a/b/.env"),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
        for line in ["see docs/. ok", "see docs/.. ok", "see and/.5 ok"] {
            assert_eq!(path_candidates(line).count(), 0, "{line}");
        }
    }

    /// A segment naming a parent once decoded in another word of quoted
    /// text leaves a later path whole; joined to the path, or with a hidden
    /// character anywhere in the quoted text, no piece links.
    #[test]
    fn quoted_text_decides_whether_a_later_path_links() {
        let found = |line: &str| {
            let term = term_with(line.chars().count(), &[line], &[]);
            links_with_cwd(&term, Some("/work"))
                .into_iter()
                .map(|link| link.uri)
                .collect::<Vec<_>>()
        };
        let line = "\"Compare docs/%2e%2e/old.txt with /tmp/report.pdf\"";
        assert_eq!(found(line), ["file:///tmp/report.pdf"], "{line}");
        for line in [
            "\"docs/%2e%2e/a?=/tmp/report.pdf\"",
            "\"docs/old\u{a0}dir name/x.txt\"",
            "\"Compare docs/a\u{200b}b.txt with /tmp/report.pdf\"",
        ] {
            assert!(found(line).is_empty(), "{line}: {:?}", found(line));
        }
    }

    /// A quoted relative path with a space links whole whatever the length
    /// of its extension.
    #[test]
    fn a_quoted_path_with_a_long_extension_is_one_link() {
        for (path, uri) in [
            (
                "docs/annual report.swiftinterface",
                "file:///work/docs/annual%20report.swiftinterface",
            ),
            (
                "a/b/annual report.documentation",
                "file:///work/a/b/annual%20report.documentation",
            ),
            (
                "docs/my app.code-workspace",
                "file:///work/docs/my%20app.code-workspace",
            ),
        ] {
            let line = format!("open \"{path}\" now");
            let term = term_with(line.len(), &[&line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
    }

    /// A link ending in a wide character covers both of its cells, so the
    /// whole glyph underlines and clicks.
    #[test]
    fn a_link_ending_in_a_wide_character_covers_it() {
        let term = term_fed(
            40,
            1,
            "\"./my dir/\u{754c}\" https://x.test/\u{754c}".as_bytes(),
        );
        let found = links_with_cwd(&term, Some("/p"))
            .into_iter()
            .map(|link| (link.start_col, link.end_col))
            .collect::<Vec<_>>();
        assert_eq!(found, [(1, 11), (14, 30)]);
    }

    /// A quoted path the terminal wrapped is one link.
    #[test]
    fn a_soft_wrapped_quoted_path_is_one_link() {
        let term = term_with(12, &["see \"docs/an", "nual a.pdf\""], &[0]);
        let found = links_with_cwd(&term, Some("/p"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].group, found[1].group);
        assert!(
            found
                .iter()
                .all(|link| link.uri == "file:///p/docs/annual%20a.pdf")
        );
    }

    #[test]
    fn allows_web_and_mail_rejects_custom_schemes() {
        assert!(is_safe_url("https://example.com/a?b=c#d"));
        assert!(is_safe_url("http://10.0.0.1:8080/x"));
        assert!(is_safe_url("ftp://host/file"));
        assert!(is_safe_url("mailto:a@b.com"));
        assert!(is_safe_url("file:///etc/hostname"));

        // Custom / dangerous schemes are refused.
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("vscode://x"));
        assert!(!is_safe_url("ms-cxh://x"));
        assert!(!is_safe_url("data:text/html,<script>"));
        // No scheme, empty, control chars, traversal, oversized.
        assert!(!is_safe_url("example.com"));
        assert!(!is_safe_url(""));
        assert!(!is_safe_url("https://e\nvil/x"));
        assert!(!is_safe_url("https://has space/x"));
        assert!(!is_safe_url("file://../../etc/passwd"));
        assert!(!is_safe_url(&format!("https://{}", "a".repeat(5000))));
        assert!(!is_safe_url("https:"), "scheme with empty target");
    }

    /// A URI holding a character that hides or reorders text does not read
    /// as what it opens, so it is refused: every Unicode space, the format
    /// characters (zero-width, bidirectional, byte order mark, tags) and
    /// the other characters drawn as nothing. Visible non-ASCII stays.
    #[test]
    fn uris_holding_invisible_characters_are_refused() {
        let invisible = [
            '\u{a0}',
            '\u{1680}',
            '\u{2000}',
            '\u{2001}',
            '\u{2002}',
            '\u{2003}',
            '\u{2004}',
            '\u{2005}',
            '\u{2006}',
            '\u{2007}',
            '\u{2008}',
            '\u{2009}',
            '\u{200a}',
            '\u{2028}',
            '\u{2029}',
            '\u{202f}',
            '\u{205f}',
            '\u{3000}',
            '\u{200b}',
            '\u{200c}',
            '\u{200d}',
            '\u{2060}',
            '\u{feff}',
            '\u{202e}',
            '\u{202a}',
            '\u{202b}',
            '\u{202c}',
            '\u{202d}',
            '\u{2066}',
            '\u{2067}',
            '\u{2068}',
            '\u{2069}',
            '\u{180e}',
            '\u{200e}',
            '\u{200f}',
            '\u{061c}',
            '\u{ad}',
            '\u{34f}',
            '\u{fe0f}',
            '\u{e0041}',
            '\u{3164}',
            '\u{115f}',
            '\u{2800}',
            '\u{85}',
        ];
        for c in invisible {
            for uri in [
                format!("https://x.test/a{c}b"),
                format!("https://x{c}.test/"),
                format!("mailto:a{c}@b.test"),
                format!("file:///tmp/a{c}b.pdf"),
            ] {
                assert!(!is_safe_url(&uri), "U+{:04X} in {uri:?}", u32::from(c));
            }
            // Encoded, a detected path would slip past the URL's check.
            let path = format!("/tmp/a{c}b.pdf");
            assert_eq!(
                path_match_to_file_uri(&path, None),
                None,
                "U+{:04X}",
                u32::from(c)
            );
            // Quoted, no piece of the name links: not `dir/x.txt` either.
            for line in [
                format!("open \"docs/my a{c}b.pdf\" now"),
                format!("open \"docs/old{c}dir/x.txt\" now"),
            ] {
                let term = term_with(line.chars().count(), &[&line], &[]);
                let found = links_with_cwd(&term, Some("/work"));
                assert!(found.is_empty(), "U+{:04X}: {found:?}", u32::from(c));
            }
        }
        for uri in [
            "https://x.test/café",
            "https://例.test/路径",
            "https://x.test/a%E2%80%8Bb",
            "file:///tmp/caf%C3%A9",
        ] {
            assert!(is_safe_url(uri), "{uri}");
        }
        assert_eq!(
            path_match_to_file_uri("/tmp/my café.pdf", None).as_deref(),
            Some("file:///tmp/my%20caf%C3%A9.pdf")
        );
    }

    /// A scheme is case-insensitive: an uppercase or mixed-case URL links
    /// as printed, and `WWW.` gains `https://` as `www.` does.
    #[test]
    fn uppercase_schemes_are_links() {
        for (line, uri) in [
            ("see HTTPS://example.com/a", "HTTPS://example.com/a"),
            ("see Http://example.com/a", "Http://example.com/a"),
            ("see hTTpS://example.com/a.", "hTTpS://example.com/a"),
            ("see FTP://host/file", "FTP://host/file"),
            ("see FILE:///etc/hosts", "FILE:///etc/hosts"),
            ("see WWW.example.com/a", "https://WWW.example.com/a"),
            ("see Www.example.com/a", "https://Www.example.com/a"),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
            assert!(is_safe_url(uri), "{uri}");
        }
        // A `www.` inside a longer name is part of it, in any case.
        for (line, uri) in [
            (
                "cat docs/WWW.example.com.txt",
                "file:///work/docs/WWW.example.com.txt",
            ),
            (
                "cat docs/www.example.com.txt",
                "file:///work/docs/www.example.com.txt",
            ),
            ("see (WWW.x.test)", "https://WWW.x.test"),
            ("--url=www.x.test", "https://www.x.test"),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
        for line in ["mail me@WWW.x.test", "docs/\u{b2}www.example.com.txt"] {
            let term = term_with(line.chars().count(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert!(
                found.iter().all(|link| !link.uri.starts_with("https")),
                "{line}: {found:?}"
            );
        }
        // A name ending in `www` is a path; a scheme after a refused `www.`
        // still starts a URL.
        for (line, uri) in [
            ("cat /tmp/www...", "file:///tmp/www"),
            (
                "docs/www.cfg=HTTPS://example.com/a",
                "HTTPS://example.com/a",
            ),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"))
                .into_iter()
                .map(|link| link.uri)
                .collect::<Vec<_>>();
            assert_eq!(found, [uri], "{line}");
        }
        assert_eq!(super::normalize_url("WWW.x.test"), "https://WWW.x.test");
        assert_eq!(super::normalize_url("wwwx.test"), "wwwx.test");
        assert_eq!(super::normalize_url("w\u{e9}"), "w\u{e9}");
        #[cfg(unix)]
        assert_eq!(
            super::local_file_path("FILE:///etc/hosts"),
            Some(std::path::PathBuf::from("/etc/hosts"))
        );
    }

    /// A web or FTP URL names a host: a scheme with nothing after it, or
    /// with only a path, query, fragment or port, opens nothing.
    #[test]
    fn web_urls_without_a_host_are_refused() {
        for uri in [
            "https://",
            "https:///",
            "https:///etc/passwd",
            "https://?q=1",
            "https://#frag",
            "https://:8080/x",
            "https://user@/x",
            "https://[]/x",
            "https:example.com",
            "http:/example.com",
            "ftp://",
        ] {
            assert!(!is_safe_url(uri), "{uri}");
        }
        for uri in [
            "https://a",
            "https://me@x.test:8080/a",
            "http://[::1]:8080/x",
            "ftp://host/file",
            "https://www./",
        ] {
            assert!(is_safe_url(uri), "{uri}");
        }
    }

    /// Trailing prose can leave a bare scheme or `www` once trimmed: no
    /// link, and no piece of it links as a path.
    #[test]
    fn a_bare_scheme_is_no_link() {
        for line in [
            "see www...",
            "see www.,",
            "see www./",
            "see https://...",
            "see https:///",
            "see https://?q=1",
            "see https://#frag",
            "see https://:8080/x",
            "see ftp://...",
            "see file://...",
            "see https://?q=/etc/hosts",
            "see https:///etc/hosts",
            "see file://?q=/etc/hosts",
            "see file://localhost",
            "see file://localhost?q=/etc/hosts",
            "see www.example@/etc/hosts",
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert!(found.is_empty(), "{line}: {found:?}");
        }
        for (line, uri) in [
            ("see https://a", "https://a"),
            ("see www.example.com...", "https://www.example.com"),
            (
                "see www.example.com@x.test/",
                "https://www.example.com@x.test/",
            ),
            ("see file:///etc/hosts.", "file:///etc/hosts"),
            (
                "see file://localhost/etc/hosts",
                "file://localhost/etc/hosts",
            ),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
        // An explicit link over part of a refused URL keeps its cells, and
        // the rest of the URL is not a path either.
        let term = term_fed(
            40,
            1,
            b"\x1b]8;;https://e.test/\x1b\\https\x1b]8;;\x1b\\://?q=docs/.env end",
        );
        let found = links_with_cwd(&term, Some("/work"))
            .into_iter()
            .map(|link| link.uri)
            .collect::<Vec<_>>();
        assert_eq!(found, ["https://e.test/"]);
    }

    /// Drift guard: `file://` must be local-only — a remote
    /// authority is a Windows NTLM-hash leak / SSRF vector from PTY output.
    #[test]
    fn file_url_is_local_only() {
        // Local forms stay allowed.
        assert!(is_safe_url("file:///etc/hostname"));
        assert!(is_safe_url("file:///C:/Users/me/notes.txt"));
        assert!(is_safe_url("file://localhost/etc/hostname"));
        assert!(is_safe_url("file://127.0.0.1/x"));
        // Remote authority → rejected (the NTLM/SSRF case).
        assert!(!is_safe_url("file://evil.example.com/share/x"));
        assert!(!is_safe_url("file://host/share"));
        assert!(!is_safe_url("file://10.0.0.5/x"));
        // Authority smuggling / UNC re-entry → rejected.
        assert!(!is_safe_url("file:////evil/share"));
        assert!(!is_safe_url("file://localhost:8080/x"));
        assert!(!is_safe_url("file://user@host/x"));
        // Percent-encoded / backslash traversal → rejected.
        assert!(!is_safe_url("file:///x/%2e%2e/etc/passwd"));
        assert!(!is_safe_url("file://\\\\evil\\share"));
    }

    /// A file URL is judged by what its escapes decode to: traversal,
    /// separators that join into a share, controls and bad escapes are
    /// refused however they are spelled, ordinary escapes are fine.
    #[test]
    fn file_urls_are_checked_after_decoding() {
        for uri in [
            "file:///home/me/.%2e/etc/passwd",
            "file:///home/me/%2E./etc/passwd",
            "file:///%2F%2Fattacker.example/share/x",
            "file:///home/me%2F..%2Fetc",
            "file:///home/me%2Fnotes.txt",
            "file:///tmp/a%00b",
            "file:///tmp/a%0Ab",
            "file:///tmp/a%zz",
            "file:///tmp/a%+5",
            "file:///tmp/a%2",
            "file://localhost//host/share",
        ] {
            assert!(!is_safe_url(uri), "{uri}");
        }
        for uri in [
            "file:///tmp/a%20b.pdf",
            "file:///tmp/caf%C3%A9",
            "file:///tmp/50%25.txt",
            "file:///tmp/a.b/c",
            "file:///tmp/x.pdf#page=2",
        ] {
            assert!(is_safe_url(uri), "{uri}");
        }
    }

    /// Only a segment that names the parent climbs: dots inside a name, a
    /// name of three or more dots, and dots in a query or fragment are
    /// local file URLs like any other.
    #[test]
    fn dots_inside_a_file_url_name_are_not_traversal() {
        use super::file_url_for_path;
        use std::path::Path;
        for uri in [
            "file:///foo..bar/x",
            "file:///.../x",
            "file:///tmp/...",
            "file:///..hidden/x",
            "file:///tmp/a..b.pdf",
            "file:///tmp/a%2e%2eb.pdf",
            "file:///tmp/x.pdf?note=a..b",
            "file:///tmp/x#frag..ment",
            "file://localhost/tmp/v1..v2/report.pdf",
        ] {
            assert!(is_safe_url(uri), "{uri}");
        }
        for uri in [
            "file:///tmp/../etc/passwd",
            "file:///tmp/..",
            "file:///tmp/.%2e/x",
            "file:///tmp/%2e%2e/x",
            "file:///tmp/%2E./x",
            "file://../../etc/passwd",
            "file://localhost/../etc/passwd",
            // Windows drops trailing spaces and dots from a name.
            "file:///C:/Users/me/..%20/secret.txt",
            "file:///C:/Users/me/..%20./secret.txt",
        ] {
            assert!(!is_safe_url(uri), "{uri}");
        }
        assert_eq!(
            file_url_for_path(Path::new("/tmp/foo..bar/baz.rs")).as_deref(),
            Some("file:///tmp/foo..bar/baz.rs")
        );
        assert_eq!(
            file_url_for_path(Path::new("/tmp/...")).as_deref(),
            Some("file:///tmp/...")
        );
        assert_eq!(file_url_for_path(Path::new("/tmp/../etc")), None);
        for (line, uri) in [
            ("see /tmp/a..b.pdf here", "file:///tmp/a..b.pdf"),
            ("ls /tmp/.../x.txt now", "file:///tmp/.../x.txt"),
        ] {
            let term = term_with(line.len(), &[line], &[]);
            let found = links_with_cwd(&term, Some("/work"));
            assert_eq!(found.len(), 1, "{line}: {found:?}");
            assert_eq!(found[0].uri, uri, "{line}");
        }
    }

    #[test]
    fn osc8_and_detected_links_keep_their_source_and_counters_stay_plain() {
        let term = term_fed(100, 1, b"\x1b]8;;https://explicit.test\x1b\\caption\x1b]8;;\x1b\\ https://detected.test 158/158 N/N 1/2 a/b");
        let links = links_with_cwd(&term, Some("/tmp"));
        assert_eq!(links.len(), 2);
        assert!(links[0].explicit);
        assert_eq!(links[0].uri, "https://explicit.test");
        assert!(!links[1].explicit);
        assert_eq!(links[1].uri, "https://detected.test");
    }

    #[test]
    fn wrapped_osc8_segments_share_a_hover_group_but_hard_breaks_do_not() {
        let term = term_fed(
            8,
            3,
            b"\x1b]8;;https://x.test\x1b\\abcdefghijklmnop\x1b]8;;\x1b\\",
        );
        let links = links(&term);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].group, links[1].group);
        let term = term_fed(
            8,
            3,
            b"\x1b]8;;https://x.test\x1b\\abcd\r\nefgh\x1b]8;;\x1b\\",
        );
        let links = super::links(&term);
        assert_eq!(links.len(), 2);
        assert_ne!(links[0].group, links[1].group);
    }

    #[test]
    fn path_candidates_need_path_context() {
        for text in [
            "158/158",
            "1/2",
            "N/N",
            "a/b",
            "1.2/3.4",
            "read/write",
            "and/or",
            "Save/restore",
            "ICH/DCH",
        ] {
            assert_eq!(path_candidates(text).count(), 0, "{text}");
        }
        for text in [
            "src/main.rs",
            "./build.sh",
            "~/project",
            "/usr/bin/env",
            "crates/a/b.rs:12:4",
        ] {
            assert_eq!(path_candidates(text).count(), 1, "{text}");
        }
    }

    #[test]
    fn path_matches_become_local_file_urls() {
        assert_eq!(
            path_match_to_file_uri(
                "crates/kettle-core/src/links.rs:12:3",
                Some("/home/me/kettle")
            )
            .as_deref(),
            Some("file:///home/me/kettle/crates/kettle-core/src/links.rs")
        );
        assert_eq!(
            path_match_to_file_uri("/home/me/kettle/src/main.rs:7", None).as_deref(),
            Some("file:///home/me/kettle/src/main.rs")
        );
        assert_eq!(
            path_match_to_file_uri(r"C:\Users\me\src\main.rs:9", None).as_deref(),
            Some("file:///C:/Users/me/src/main.rs")
        );
        assert_eq!(
            path_match_to_file_uri("C:/Users/me/src/main.rs:9:2", None).as_deref(),
            Some("file:///C:/Users/me/src/main.rs")
        );
        assert_eq!(
            path_match_to_file_uri(
                "crates/kettle-core/src/links.rs:12:3",
                Some(r"C:\Users\me\kettle")
            )
            .as_deref(),
            Some("file:///C:/Users/me/kettle/crates/kettle-core/src/links.rs")
        );
    }

    #[test]
    fn path_matches_reject_remote_and_traversal_shapes() {
        assert!(path_match_to_file_uri("crates/kettle/src/main.rs", None).is_none());
        assert!(path_match_to_file_uri("../secret/file.txt", Some("/home/me/kettle")).is_none());
        assert!(path_match_to_file_uri("//evil/share/file.txt", None).is_none());
        assert!(path_match_to_file_uri("src/%2e%2e/secret.txt", Some("/home/me/kettle")).is_none());
        assert!(path_match_to_file_uri("src/.%2E/secret.txt", Some("/home/me/kettle")).is_none());
        assert!(path_match_to_file_uri("src/%2E./secret.txt", Some("/home/me/kettle")).is_none());
        assert!(path_match_to_file_uri("src/..%20/secret.txt", Some("/home/me/kettle")).is_none());
        assert!(path_match_to_file_uri("src/main.rs", Some("//evil/share")).is_none());
    }

    /// A detected name holding a literal `%2e` links to that name, encoded
    /// so it reads back the same: the converter agrees with
    /// `file_url_for_path` about the file.
    #[test]
    fn a_literal_percent_2e_in_a_name_is_text() {
        use super::file_url_for_path;
        use std::path::Path;
        for (path, uri) in [
            ("/tmp/100%2e.pdf", "file:///tmp/100%252e.pdf"),
            ("/tmp/a%2E/b.txt", "file:///tmp/a%252E/b.txt"),
            ("/tmp/%2e/x.txt", "file:///tmp/%252e/x.txt"),
        ] {
            assert_eq!(
                path_match_to_file_uri(path, Some("/work")).as_deref(),
                Some(uri),
                "{path}"
            );
            assert_eq!(
                file_url_for_path(Path::new(path)).as_deref(),
                Some(uri),
                "{path}"
            );
            #[cfg(unix)]
            assert_eq!(
                super::local_file_path(uri),
                Some(std::path::PathBuf::from(path))
            );
        }
        let term = term_with(24, &["see /tmp/100%2e.pdf here"], &[]);
        let found = links_with_cwd(&term, Some("/work"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].uri, "file:///tmp/100%252e.pdf");
    }
}
