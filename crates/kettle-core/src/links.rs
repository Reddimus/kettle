//! Hyperlink discovery: explicit OSC 8 links carried on cells, plus
//! autodetected URLs and local file paths in the visible grid.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use regex::Regex;
use std::sync::OnceLock;

use crate::event::EventProxy;

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
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(https?://|ftp://|file://|www\.)[^\s\x00-\x1f<>"]+"#).unwrap())
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

use crate::url_trim::trim_trailing;

/// The path matches in one row's text that can become links, as (start byte,
/// end byte before trimming, text): trimmed of trailing punctuation, and not
/// starting inside a longer token. `/bar.png` in `foo(1)/bar.png` would name an unrelated file.
fn path_candidates(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    path_re().find_iter(text).filter_map(move |m| {
        let matched = trim_trailing(m.as_str());
        let before = text[..m.start()].chars().next_back();
        (!matched.is_empty() && crate::hints::path_may_start_after(before)).then_some((
            m.start(),
            m.end(),
            matched,
        ))
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
                let uri = h.uri().to_string();
                let start = c;
                while c < cols {
                    let cc = &grid[Point::new(Line(gl), Column(c))];
                    match cc.hyperlink() {
                        Some(h2) if h2.uri() == uri => c += 1,
                        _ => break,
                    }
                }
                let end = c.saturating_sub(1);
                covered[row].insert(start, end + 1);
                out.push(Link {
                    row,
                    start_col: start,
                    end_col: end,
                    uri,
                    group,
                });
                group += 1;
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
        // A match touching an edge where the line was cut may be a fragment
        // of a longer URL or path: no link rather than a wrong one.
        let text_len = text.len();
        let fragment = move |start: usize, end: usize| {
            (read.cut_before && start == 0) || (read.cut_after && end == text_len)
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
                    });
                }
                group += 1;
            };
        for m in url_re().find_iter(&text) {
            let matched = trim_trailing(m.as_str());
            if matched.is_empty() || fragment(m.start(), m.end()) {
                continue;
            }
            let uri = if matched.starts_with("www.") {
                format!("https://{matched}")
            } else {
                matched.to_string()
            };
            push(m.start(), matched.len(), uri, &mut covered);
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
                None => {
                    let segments = row_segments(&pos_of_byte, start, close - start);
                    if segments
                        .iter()
                        .all(|&(row, s, e)| !covered[row].overlaps(s, e + 1))
                    {
                        for (row, s, e) in segments {
                            covered[row].insert(s, e + 1);
                        }
                    }
                }
            }
        }
        for (start, end, matched) in path_candidates(&text) {
            if fragment(start, end) {
                continue;
            }
            if let Some(uri) = path_match_to_file_uri(matched, cwd) {
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
    let path = strip_location_suffix(raw);
    // Only a quoted path holds a space (see `hints::quoted_paths`); the
    // URL encodes it.
    if path.is_empty()
        || path
            .chars()
            .any(|c| c.is_control() || (c.is_whitespace() && c != ' '))
    {
        return None;
    }
    let normalized = path.replace('\\', "/");
    if normalized.starts_with("//")
        || has_parent_component(&normalized)
        || normalized.to_ascii_lowercase().contains("%2e")
    {
        return None;
    }

    let absolute = if is_windows_drive_path(&normalized) {
        format!("/{normalized}")
    } else if normalized.starts_with('/') {
        normalized
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let home = home.replace('\\', "/");
        if home.is_empty() || home.starts_with("//") || has_parent_component(&home) {
            return None;
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

fn is_windows_drive_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

fn is_windows_drive_colon(path: &str, pos: usize) -> bool {
    let b = path.as_bytes();
    pos == 1 && b.len() >= 3 && b[0].is_ascii_alphabetic() && (b[2] == b'/' || b[2] == b'\\')
}

fn has_parent_component(path: &str) -> bool {
    path.split('/').any(|part| part == "..")
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

/// Whether a URI from terminal output is safe to hand to the OS opener.
/// Terminal output is untrusted: an OSC 8 hyperlink (or a crafted line)
/// can carry an arbitrary URI, and custom schemes (`vscode:`, `ms-…`,
/// `javascript:`, app handlers that exec) are a known abuse/RCE vector.
/// We allow only a conservative scheme allowlist and reject controls,
/// whitespace, and absurd lengths. Pure — fully unit tested.
pub fn is_safe_url(uri: &str) -> bool {
    if uri.is_empty() || uri.len() > 4096 {
        return false;
    }
    if uri.chars().any(|c| c.is_control() || c == ' ') {
        return false;
    }
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false; // no scheme → not opened
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "ftp" | "ftps" | "mailto" => !rest.is_empty(),
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
        && !decoded.split(['/', '\\']).any(|segment| segment == "..");
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
/// alternate data stream (`a.exe::$DATA`, `a.txt:b.exe`) is checked both
/// before and after its colon.
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
    known(&name)
        || name
            .split_once(':')
            .is_some_and(|(stream_of, _)| known(stream_of))
}

/// Whether a `file://` URI points at the local machine with no traversal.
///
/// Blocking `..` alone is not enough. On Windows a remote authority such as
/// `file://host/path` maps to the UNC `\\host\path`, so the OS opener transparently
/// connects to `host` over SMB/WebDAV and leaks the user's NTLMv2 hash (forced
/// authentication / pass-the-hash) plus SSRF to an arbitrary host, all reachable
/// from untrusted PTY output (autodetected link or OSC 8). Accept only an empty
/// authority (`file:///path`) or an explicit loopback host, and reject any
/// backslash / `file:////` / percent-encoded traversal that could smuggle an
/// authority back in.
fn is_local_file_url(uri: &str) -> bool {
    // is_safe_url lowercases the scheme before the `file` arm, so accept
    // `FILE://` / `File://` here too. The authority and traversal checks below
    // already use a lowercased copy.
    let rest = match uri.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("file://") => &uri[7..],
        _ => return false,
    };
    let lowered = uri.to_ascii_lowercase();
    if uri.contains("..")
        || lowered.contains("%2e%2e")
        || uri.contains('\\')
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
        assert!(path_match_to_file_uri("src/main.rs", Some("//evil/share")).is_none());
    }
}
