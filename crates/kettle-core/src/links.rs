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
    let mut covered: Vec<Vec<(usize, usize)>> = vec![Vec::new(); rows];
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
                covered[row].push((start, end));
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
            |start: usize, len: usize, uri: String, covered: &mut Vec<Vec<(usize, usize)>>| {
                let segments = row_segments(&pos_of_byte, start, len);
                // Skip a match overlapping a link already found on any row it covers.
                if segments
                    .iter()
                    .any(|&(row, s, e)| covered[row].iter().any(|&(cs, ce)| !(e < cs || s > ce)))
                {
                    return;
                }
                for (row, s, e) in segments {
                    covered[row].push((s, e));
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
    if path.is_empty() || path.chars().any(|c| c.is_control() || c.is_whitespace()) {
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
}

#[cfg(test)]
mod tests {
    use super::{is_safe_url, links, links_with_cwd, path_candidates, path_match_to_file_uri};
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
