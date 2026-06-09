//! Hyperlink discovery: explicit OSC 8 links carried on cells, plus
//! autodetected URLs in the visible grid.

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
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(https?://|ftp://|file://|www\.)[^\s\x00-\x1f<>"]+"#).unwrap())
}

use crate::url_trim::trim_trailing;

/// All links visible in the current viewport. Explicit OSC 8 links take
/// precedence over autodetected URLs on the same cells.
pub fn links(term: &Term<EventProxy>) -> Vec<Link> {
    let grid = term.grid();
    let cols = grid.columns();
    let rows = grid.screen_lines();
    // Cycle 917 (#3, user-reported on native Ubuntu): scan the VISIBLE viewport,
    // not the active screen. A visible viewport row `row` maps to grid line
    // `row - display_offset` (alacritty addresses history with NEGATIVE lines);
    // `grid[Line(row)]` always read the active (bottom) screen, so scrolling
    // Claude Code up returned the active screen's links and the renderer painted
    // their underlines over the scrolled-back history ("leftover/ghost
    // underlines"). Mirrors the cycle-912 decoration/selection conversion.
    let off = grid.display_offset() as i32;
    let mut out: Vec<Link> = Vec::new();
    // Cycle 762: reuse the URL-scan scratch buffers across every viewport row
    // instead of allocating a String + Vec per row each time links are
    // recomputed (every redraw). `.clear()` keeps the capacity.
    let mut text = String::with_capacity(cols);
    let mut col_of_byte: Vec<usize> = Vec::with_capacity(cols * 2);

    for row in 0..rows {
        let gl = row as i32 - off; // visible viewport row -> grid-absolute line
        // Cycle 781: this row's OSC 8 links occupy `out[osc8_start..osc8_end]`.
        // The autodetect overlap check below scans only that slice instead of
        // all-rows `out`, turning an O(total_links)-per-match scan (→ O(n²) on a
        // link-dense viewport, e.g. a log full of URLs) into one bounded by this
        // row's OSC 8 count.
        let osc8_start = out.len();
        // OSC 8 runs: consecutive cells sharing a hyperlink URI.
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
                out.push(Link {
                    row,
                    start_col: start,
                    end_col: c.saturating_sub(1),
                    uri,
                });
            } else {
                c += 1;
            }
        }
        let osc8_end = out.len();

        // Autodetected URLs (skip cells already covered by an OSC 8 link on
        // THIS row — regex `find_iter` yields non-overlapping matches, so only
        // OSC 8 links can collide with an autodetected one).
        text.clear();
        col_of_byte.clear();
        for col in 0..cols {
            let ch = grid[Point::new(Line(gl), Column(col))].c;
            for _ in 0..ch.len_utf8() {
                col_of_byte.push(col);
            }
            text.push(ch);
        }
        for m in url_re().find_iter(&text) {
            let matched = trim_trailing(m.as_str());
            if matched.is_empty() {
                continue;
            }
            let s = col_of_byte.get(m.start()).copied().unwrap_or(0);
            let e = col_of_byte
                .get(m.start() + matched.len().saturating_sub(1))
                .copied()
                .unwrap_or(s);
            if out[osc8_start..osc8_end]
                .iter()
                .any(|l| !(e < l.start_col || s > l.end_col))
            {
                continue;
            }
            let uri = if matched.starts_with("www.") {
                format!("https://{matched}")
            } else {
                matched.to_string()
            };
            out.push(Link {
                row,
                start_col: s,
                end_col: e,
                uri,
            });
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
        // Allow LOCAL files only (cycle 815) — never a remote authority.
        "file" => is_local_file_url(uri),
        _ => false,
    }
}

/// Whether a `file://` URI points at the local machine with no traversal.
///
/// Cycle 815 (audit): the old check was `starts_with("file://") && !contains("..")`
/// — it blocked traversal but not a remote authority. `file://evil.example.com/share`
/// passed, and on Windows `file://host/path` maps to the UNC `\\host\path`, so the
/// OS opener transparently connects to `host` over SMB/WebDAV and leaks the user's
/// NTLMv2 hash (forced authentication / pass-the-hash) plus SSRF to an arbitrary
/// host — all reachable from untrusted PTY output (autodetected link or OSC 8).
/// Accept only an empty authority (`file:///path`) or an explicit loopback host,
/// and reject any backslash / `file:////` / percent-encoded traversal that could
/// smuggle an authority back in.
fn is_local_file_url(uri: &str) -> bool {
    // Cycle 916 (file-by-file audit): the scheme is matched case-insensitively
    // upstream (is_safe_url lowercases it before the `file` arm), so accept
    // `FILE://` / `File://` here too — a case-sensitive strip rejected valid
    // uppercase-scheme local files while the authority/traversal checks below
    // (which already use a lowercased copy) stayed intact.
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
    use super::is_safe_url;

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

    /// Cycle 815 (audit) drift guard: `file://` must be local-only — a remote
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
}
