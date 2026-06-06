//! Regex search across the whole buffer (scrollback + viewport), powering the
//! Ctrl+Shift+F overlay.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use regex::Regex;

use crate::event::EventProxy;

/// A match expressed in absolute grid coordinates (line can be negative for
/// scrollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: i32,
    pub start_col: usize,
    pub end_col: usize,
}

/// Cycle 617 (Terminator parity, terminatorlib/config.py:117
/// `case_sensitive`): override the default smart-case policy
/// at search time. `Smart` keeps ripgrep/vim's smart-case
/// (insensitive until the pattern has any uppercase),
/// `Always` forces case-sensitive even for lowercase patterns,
/// `Never` forces case-insensitive even for mixed-case patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseSensitivity {
    #[default]
    Smart,
    Always,
    Never,
}

/// Compile a search pattern with **smart-case** semantics: case-insensitive
/// unless the pattern contains an uppercase letter (ripgrep/vim behavior).
/// The pattern is a real regex; if it doesn't compile it falls back to a
/// literal (escaped) search so a stray `(` or `*` never breaks search.
///
/// Preserved as the smart-case shorthand; new callers that want to
/// honor a user-configured override should call `build_regex_with`.
pub fn build_regex(pattern: &str) -> Option<Regex> {
    build_regex_with(pattern, CaseSensitivity::Smart)
}

/// Cycle 617: same as `build_regex` but with an explicit override.
/// Pure — no terminal state involved.
pub fn build_regex_with(pattern: &str, mode: CaseSensitivity) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    let ci = match mode {
        CaseSensitivity::Smart => !pattern.chars().any(|c| c.is_uppercase()),
        CaseSensitivity::Always => false,
        CaseSensitivity::Never => true,
    };
    let flag = if ci { "(?i)" } else { "" };
    Regex::new(&format!("{flag}{pattern}"))
        .or_else(|_| Regex::new(&format!("{flag}{}", regex::escape(pattern))))
        .ok()
}

pub fn search(term: &Term<EventProxy>, pattern: &str) -> Vec<Match> {
    search_with(term, pattern, CaseSensitivity::Smart)
}

/// Cycle 617: search with an explicit case-sensitivity override.
pub fn search_with(term: &Term<EventProxy>, pattern: &str, mode: CaseSensitivity) -> Vec<Match> {
    let Some(re) = build_regex_with(pattern, mode) else {
        return Vec::new();
    };

    let grid = term.grid();
    let cols = grid.columns();
    let top = grid.topmost_line().0;
    let bottom = grid.bottommost_line().0;

    let mut matches = Vec::new();
    // Cycle 762: reuse the line-text + byte→column scratch buffers across every
    // scrollback line instead of allocating a fresh String + Vec per line. On a
    // 10k-line scrollback that's 2 allocations total rather than ~20k; `.clear()`
    // keeps the capacity.
    let mut text = String::with_capacity(cols);
    let mut col_of_byte: Vec<usize> = Vec::with_capacity(cols * 2);
    for line in top..=bottom {
        // Reconstruct the line text, tracking the byte->column mapping.
        text.clear();
        col_of_byte.clear();
        for c in 0..cols {
            let cell = &grid[Point::new(Line(line), Column(c))];
            let ch = cell.c;
            for b in 0..ch.len_utf8() {
                let _ = b;
                col_of_byte.push(c);
            }
            text.push(ch);
        }
        for m in re.find_iter(&text) {
            // Cycle 865 (audit): skip zero-width matches. A user pattern that can
            // match empty (`a*`, `^`, `\b`) yields a zero-length match at every
            // position; without this each produces a spurious one-cell highlight
            // the match doesn't really cover.
            if m.start() == m.end() {
                continue;
            }
            let start_col = col_of_byte.get(m.start()).copied().unwrap_or(0);
            let end_col = col_of_byte
                .get(m.end().saturating_sub(1))
                .copied()
                .unwrap_or(start_col);
            matches.push(Match {
                line,
                start_col,
                end_col,
            });
        }
    }
    matches
}

/// The `display_offset` that brings a match on grid line `match_line`
/// (negative = scrollback) into view, or keeps the current one if the
/// match is already visible (no jitter while typing/cycling). When a
/// scroll is needed the match is placed ~1/3 from the top for context.
/// Pure — `hist` = scrollback lines, `screen_lines` = visible rows.
pub fn reveal_offset(match_line: i32, cur_off: usize, hist: usize, screen_lines: usize) -> usize {
    let h = hist as i64;
    let off = cur_off as i64;
    let sl = screen_lines.max(1) as i64;
    // Absolute line (0 = oldest scrollback … h+rows = newest).
    let target = h + match_line as i64;
    let top = h - off; // absolute line at the viewport's top row
    if target >= top && target < top + sl {
        return cur_off; // already on screen
    }
    let want_top = (target - sl / 3).max(0);
    (h - want_top).clamp(0, h) as usize
}

#[cfg(test)]
mod tests {
    use super::build_regex;

    #[test]
    fn smart_case_is_insensitive_until_an_uppercase() {
        // All-lowercase pattern → case-insensitive.
        let re = build_regex("error").unwrap();
        assert!(re.is_match("ERROR"));
        assert!(re.is_match("Error"));
        assert!(re.is_match("error"));
        // Any uppercase → case-sensitive.
        let re = build_regex("Error").unwrap();
        assert!(re.is_match("Error"));
        assert!(!re.is_match("error"));
        assert!(!re.is_match("ERROR"));
    }

    #[test]
    fn pattern_is_a_real_regex() {
        let re = build_regex(r"warn|fail").unwrap();
        assert!(re.is_match("a fail here"));
        assert!(re.is_match("WARN: x"), "alternation + smart-case");
        let re = build_regex(r"\bfoo\b").unwrap();
        assert!(re.is_match("a foo b"));
        assert!(!re.is_match("foobar"));
    }

    #[test]
    fn reveal_offset_keeps_visible_else_scrolls() {
        use super::reveal_offset;
        // hist=100, screen=40, at the bottom (off=0): viewport abs 100..139.
        // A viewport match (line 10 → abs 110) is already visible → no move.
        assert_eq!(reveal_offset(10, 0, 100, 40), 0);
        // A scrollback match (line -50 → abs 50) isn't visible → scroll so
        // it sits ~1/3 down: want_top = 50 - 13 = 37 → off = 100-37 = 63.
        assert_eq!(reveal_offset(-50, 0, 100, 40), 63);
        // Already-scrolled and the match is within that window → unchanged.
        // off=63 → top abs = 100-63 = 37, window 37..76; abs 50 is inside.
        assert_eq!(reveal_offset(-50, 63, 100, 40), 63);
        // Clamped to [0, hist]; never panics on extremes.
        assert!(reveal_offset(-9999, 0, 100, 40) <= 100);
        assert_eq!(reveal_offset(9999, 0, 100, 40), 0);
    }

    #[test]
    fn invalid_regex_falls_back_to_literal() {
        // Unbalanced paren is not a valid regex → literal search instead.
        let re = build_regex("a(b").unwrap();
        assert!(re.is_match("xx a(b yy"));
        assert!(!re.is_match("ab"));
        // Empty pattern yields nothing.
        assert!(build_regex("").is_none());
    }

    /// Cycle 617 drift guard. The three CaseSensitivity modes drive
    /// the (?i) flag override on `build_regex_with`:
    ///   - Smart: case-insensitive unless any uppercase in pattern
    ///   - Always: case-sensitive even for all-lowercase pattern
    ///   - Never: case-insensitive even for mixed-case pattern
    #[test]
    fn build_regex_with_honors_explicit_case_sensitivity() {
        use super::{CaseSensitivity, build_regex_with};
        // Smart: lowercase → insensitive (matches ERROR).
        let re = build_regex_with("error", CaseSensitivity::Smart).unwrap();
        assert!(re.is_match("ERROR"));
        // Always: even lowercase pattern is sensitive (no match on ERROR).
        let re = build_regex_with("error", CaseSensitivity::Always).unwrap();
        assert!(!re.is_match("ERROR"));
        assert!(re.is_match("error"));
        // Never: even mixed-case pattern is insensitive (matches ERROR).
        let re = build_regex_with("Error", CaseSensitivity::Never).unwrap();
        assert!(re.is_match("ERROR"));
        assert!(re.is_match("error"));
        // Empty pattern still None across all modes.
        for mode in [
            CaseSensitivity::Smart,
            CaseSensitivity::Always,
            CaseSensitivity::Never,
        ] {
            assert!(build_regex_with("", mode).is_none());
        }
    }
}
