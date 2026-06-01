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
    let mut out: Vec<Link> = Vec::new();
    // Cycle 762: reuse the URL-scan scratch buffers across every viewport row
    // instead of allocating a String + Vec per row each time links are
    // recomputed (every redraw). `.clear()` keeps the capacity.
    let mut text = String::with_capacity(cols);
    let mut col_of_byte: Vec<usize> = Vec::with_capacity(cols * 2);

    for row in 0..rows {
        // OSC 8 runs: consecutive cells sharing a hyperlink URI.
        let mut c = 0usize;
        while c < cols {
            let cell = &grid[Point::new(Line(row as i32), Column(c))];
            if let Some(h) = cell.hyperlink() {
                let uri = h.uri().to_string();
                let start = c;
                while c < cols {
                    let cc = &grid[Point::new(Line(row as i32), Column(c))];
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

        // Autodetected URLs (skip cells already covered by an OSC 8 link).
        text.clear();
        col_of_byte.clear();
        for col in 0..cols {
            let ch = grid[Point::new(Line(row as i32), Column(col))].c;
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
            if out
                .iter()
                .any(|l| l.row == row && !(e < l.start_col || s > l.end_col))
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
        // Allow local files but never a traversal payload.
        "file" => uri.starts_with("file://") && !uri.contains(".."),
        _ => false,
    }
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
}
