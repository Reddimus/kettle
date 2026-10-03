//! Quick-select "hint mode" core (kitty `kitten hints` / WezTerm
//! `QuickSelect` style): scan the visible rows for interesting tokens —
//! URLs, filesystem paths, git hashes, IPv4 addresses — and assign each a
//! short, easy-to-type label. Pure and fully unit-tested; the UI hint overlay
//! consumes [`detect_rows`] and [`labels`], and double-click selection uses
//! [`detect`].

use regex::Regex;
use std::sync::OnceLock;

use alacritty_terminal::grid::Grid;
use alacritty_terminal::term::cell::Cell;

/// What a detected hint points at (drives the copy-vs-open action later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Url,
    Path,
    Hash,
    Ip,
}

/// A detected token in row/column coordinates (`end` is inclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintSpan {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
    pub text: String,
    /// The match meets a boundary on both sides: whitespace, the line's
    /// edge, or delimiting punctuation. A path that ran into a character the
    /// pattern does not take (`out/report#1.pdf` matched as `out/report`, or
    /// `foo(1)/bar.png` as `/bar.png`) is only part of a name, so it may be
    /// copied but must not be opened.
    pub bounded: bool,
}

fn res() -> &'static [(Kind, Regex)] {
    static RE: OnceLock<Vec<(Kind, Regex)>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            (
                Kind::Url,
                Regex::new(r#"(?:https?://|ftp://|file://|www\.)[^\s\x00-\x1f<>"]+"#).unwrap(),
            ),
            (
                Kind::Path,
                // A path with at least one separator, anchored at its first
                // segment so a relative path keeps its directory:
                // `out/diagram.png`, not `/diagram.png`. Absolute, `~/`,
                // `./`, `../` and drive-letter (`C:/`) paths match too;
                // segments are never empty, and a trailing `/` names a
                // directory. `\w` is Unicode, so `café/menú.png` is one path.
                Regex::new(r"(?:\b[A-Za-z]:)?(?:~|[\w.\-@+]+)?(?:/[\w.\-@+]+)+/?").unwrap(),
            ),
            (
                Kind::Ip,
                // Clamp each octet to 0..=255 so out-of-range dotted numbers such
                // as 999.999.999.999 are not IP quick-select targets. The `regex`
                // crate has no lookaround, so a 5-group `1.2.3.4.5` can still match
                // its first four octets. That is acceptable because the only Ip
                // action is a clipboard copy (no network/open).
                Regex::new(
                    r"\b(?:25[0-5]|2[0-4]\d|1?\d?\d)(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3}\b",
                )
                .unwrap(),
            ),
            (
                Kind::Hash,
                // Git-style hex blob (7–40), not part of a longer word.
                Regex::new(r"\b[0-9a-f]{7,40}\b").unwrap(),
            ),
        ]
    })
}

use crate::url_trim::trim_trailing;

/// Whether `text` is an IPv4 network in CIDR form (`10.0.0.1/24`), which is
/// an address with a prefix length, not a path; the address stays its own
/// hint.
fn is_cidr(text: &str) -> bool {
    // The path pattern takes a trailing `/` (`10.0.0.1/24/` in subnet lists).
    let text = text.strip_suffix('/').unwrap_or(text);
    let Some((address, prefix)) = text.split_once('/') else {
        return false;
    };
    let octets = address.split('.').collect::<Vec<_>>();
    octets.len() == 4
        && octets.iter().all(|octet| {
            (1..=3).contains(&octet.len()) && octet.bytes().all(|b| b.is_ascii_digit())
        })
        && (1..=2).contains(&prefix.len())
        && prefix.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a path may start right after `before`: the line's start, a space,
/// an opening delimiter, `=` (`--out=dir/x`), or the separators that list,
/// chain or redirect paths (`:` in `PATH=/a:/b`, `,`, `;`, `&`, `<`, `>`,
/// `|`). Anything
/// else means the match begins inside a longer token, as `/bar.png` does in
/// `foo(1)/bar.png`, and is not a path at all. Opening a path asks more of
/// it (see [`HintSpan::bounded`]).
pub(crate) fn path_may_start_after(before: Option<char>) -> bool {
    before.is_none_or(|c| c.is_whitespace() || "\"'`*([{<=:,;&>|".contains(c))
}

/// Whether a match starting after `before` and followed by `rest` meets a
/// boundary on both sides (see [`HintSpan::bounded`]).
///
/// Before it: the line's start, a space, an opening delimiter or `=`
/// (`--out=dir/x`). A `:` may join a remote `host:path` and a `,` or `;` may
/// sit inside a name, so they do not count there. A quote or Markdown `*`
/// before it must close right after it: `"docs/annual report.pdf"` is cut at
/// the space, and the `s/report.pdf` in `./user's/report.pdf` starts mid-name.
///
/// After it, past a location (`:12`, `:12:5`, `(12)`, `(12,5)`): see
/// [`ends_a_match`]. grep's `path:12:text` counts as whole, so a rare file
/// name containing `:12:` reads the same way.
fn is_bounded(before: Option<char>, rest: &str) -> bool {
    let left = before.is_none_or(|c| c.is_whitespace() || "\"'`*([{<=".contains(c));
    if !left {
        return false;
    }
    let (tail, grep) = skip_location(rest);
    match before.filter(|c| "\"'`*".contains(*c)) {
        Some(quote) => tail
            .trim_start_matches(|c| ",;:?!.".contains(c))
            .strip_prefix(quote)
            .is_some_and(ends_a_match),
        None => grep || ends_a_match(tail),
    }
}

/// The text after a location suffix (`:12`, `:12:5`, `(12)` or `(12,5)`),
/// and whether it is grep's `:12:` before a match's text.
fn skip_location(rest: &str) -> (&str, bool) {
    let digits =
        |text: &str| text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if let Some(inner) = rest.strip_prefix('(') {
        let line = digits(inner);
        let mut after = &inner[line..];
        if line > 0 {
            if let Some(column) = after.strip_prefix(',') {
                let width = digits(column);
                if width > 0 {
                    after = &column[width..];
                }
            }
            if let Some(closed) = after.strip_prefix(')') {
                return (closed, false);
            }
        }
        return (rest, false);
    }
    let mut tail = rest;
    let mut numbered = false;
    while let Some(after) = tail.strip_prefix(':') {
        let width = digits(after);
        if width == 0 {
            break;
        }
        tail = &after[width..];
        numbered = true;
    }
    let grep = numbered && tail.starts_with(':') && !tail[1..].starts_with(char::is_whitespace);
    (tail, grep)
}

/// Whether `rest` can follow the end of a whole name: the line ends or a
/// space follows, possibly after trailing punctuation and closing delimiters
/// (`out/a.png,"` or `(out/a.png).`). A closing delimiter followed by more
/// name (`./docs/it's.md`) or punctuation followed by more name
/// (`out/report,1.pdf`) does not end one.
fn ends_a_match(rest: &str) -> bool {
    let mut rest = rest;
    loop {
        rest = rest.trim_start_matches(|c| ",;:?!.".contains(c));
        match rest.chars().next() {
            None => return true,
            Some(c) if c.is_whitespace() => return true,
            Some(c) if "\"'`)]}>*".contains(c) => rest = &rest[c.len_utf8()..],
            Some(_) => return false,
        }
    }
}

/// Detect hint targets in one row's `line`, using `col_of_byte` to map byte
/// offsets to columns. Earlier kinds win on overlap (a URL is not also matched
/// as a path). Callers sort `out` into reading order (row, then column).
///
/// `cut` says whether `line` was cut before its start and after its end (see
/// `grid_text::LogicalLine`); a match touching a cut edge may be a fragment
/// and is skipped.
fn detect_line(
    row: usize,
    line: &str,
    col_of_byte: &[usize],
    cut: (bool, bool),
    out: &mut Vec<HintSpan>,
) {
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (kind, re) in res() {
        for m in re.find_iter(line) {
            let raw = m.as_str();
            let trimmed = trim_trailing(raw);
            let before = line[..m.start()].chars().next_back();
            let fragment = (cut.0 && m.start() == 0) || (cut.1 && m.end() == line.len());
            if trimmed.is_empty()
                || fragment
                || (*kind == Kind::Path && (is_cidr(trimmed) || !path_may_start_after(before)))
            {
                continue;
            }
            let bounded = is_bounded(before, &line[m.end()..]);
            let (bs, be) = (m.start(), m.start() + trimmed.len());
            if taken.iter().any(|&(s, e)| bs < e && be > s) {
                continue;
            }
            taken.push((bs, be));
            let start = col_of_byte[bs];
            let end = col_of_byte[be - 1];
            out.push(HintSpan {
                row,
                start,
                end,
                kind: *kind,
                text: trimmed.to_string(),
                bounded,
            });
        }
    }
}

pub fn detect(rows: &[&str]) -> Vec<HintSpan> {
    let mut out = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        // Byte offset -> char column for this line. Push each char's column
        // exactly `len_utf8()` times (matching links.rs/search.rs) so a
        // multi-byte char's continuation bytes map to ITS column, not the next
        // one. Otherwise double-clicking a token ending in e.g. `é` would
        // over-select by one cell.
        let col_of_byte: Vec<usize> = {
            let mut v = Vec::with_capacity(line.len() + 1);
            for (col, ch) in line.chars().enumerate() {
                for _ in 0..ch.len_utf8() {
                    v.push(col);
                }
            }
            v.push(line.chars().count()); // sentinel for the end-exclusive byte
            v
        };
        detect_line(row, line, &col_of_byte, (false, false), &mut out);
    }
    out.sort_by(|a, b| a.row.cmp(&b.row).then(a.start.cmp(&b.start)));
    out
}

/// Detect quick-select targets from terminal grid rows while preserving the
/// original grid columns of wide glyphs and combining marks. Targets are found
/// per soft-wrapped logical line, so a URL the terminal wrapped onto the next
/// row is one target with its whole text, labelled where it starts; its
/// `end` is the last column on that first row. A hard line break ends a line.
pub fn detect_rows(grid: &Grid<Cell>, lines: &[i32], cols: usize) -> Vec<HintSpan> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut pos_of_byte = Vec::new();
    let mut col_of_byte = Vec::new();
    let mut spans = Vec::new();
    let mut first = 0;
    while first < lines.len() {
        let read = crate::grid_text::logical_line_into(
            grid,
            lines,
            first,
            cols,
            &mut text,
            &mut pos_of_byte,
        );
        // Detect over the joined text with byte offsets standing in for
        // columns, then map each span back to its first row and columns.
        col_of_byte.clear();
        col_of_byte.extend(0..=text.len());
        spans.clear();
        detect_line(
            first,
            &text,
            &col_of_byte,
            (read.cut_before, read.cut_after),
            &mut spans,
        );
        for mut span in spans.drain(..) {
            let (row, start) = pos_of_byte[span.start];
            let end = pos_of_byte[..=span.end]
                .iter()
                .rev()
                .find(|(r, _)| *r == row)
                .map_or(start, |&(_, col)| col);
            span.row = row;
            span.start = start;
            span.end = end;
            out.push(span);
        }
        first = read.next;
    }
    out.sort_by(|a, b| a.row.cmp(&b.row).then(a.start.cmp(&b.start)));
    out
}

/// Default label alphabet — home-row first so the common (few) targets are
/// one comfortable keystroke.
pub const ALPHABET: &str = "asdfghjklqwertyuiopzxcvbnm";

/// `n` distinct fixed-width labels over `alphabet`, shortest width that
/// fits, in lexicographic order (`a, b, …` then `aa, ab, …`). Stable, so a
/// target's label doesn't jump around as you type to filter.
pub fn labels(n: usize, alphabet: &str) -> Vec<String> {
    let a: Vec<char> = alphabet.chars().collect();
    if n == 0 || a.is_empty() {
        return Vec::new();
    }
    let base = a.len();
    // A single-character alphabet can't make `n` distinct FIXED-width labels —
    // `base^width` is always 1, so the width search below would loop forever.
    // Fall back to distinct INCREASING-length labels (`a`, `aa`, `aaa`, …).
    if base == 1 {
        return (0..n).map(|i| a[0].to_string().repeat(i + 1)).collect();
    }
    let mut width = 1usize;
    while base.checked_pow(width as u32).is_none_or(|cap| cap < n) {
        width += 1;
    }
    (0..n)
        .map(|mut i| {
            let mut digits = vec![0usize; width];
            for d in digits.iter_mut().rev() {
                *d = i % base;
                i /= base;
            }
            digits.into_iter().map(|d| a[d]).collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_kind_with_columns() {
        let rows = ["see https://example.com/x and /etc/hosts now"];
        let h = detect(&rows);
        assert_eq!(h[0].kind, Kind::Url);
        assert_eq!(h[0].text, "https://example.com/x");
        assert_eq!(h[0].row, 0);
        assert_eq!(h[0].start, 4, "column of the 'h' in https");
        assert!(
            h.iter()
                .any(|s| s.kind == Kind::Path && s.text == "/etc/hosts")
        );

        let rows2 = ["commit a1b2c3d4e5 at 10.0.0.2:8080"];
        let h2 = detect(&rows2);
        assert!(
            h2.iter()
                .any(|s| s.kind == Kind::Hash && s.text == "a1b2c3d4e5")
        );
        assert!(
            h2.iter()
                .any(|s| s.kind == Kind::Ip && s.text == "10.0.0.2")
        );
    }

    #[test]
    fn grid_detection_skips_wide_character_spacers() {
        use alacritty_terminal::grid::Grid;
        use alacritty_terminal::index::{Column, Line, Point};
        use alacritty_terminal::term::cell::{Cell, Flags};

        let mut grid: Grid<Cell> = Grid::new(1, 10, 0);
        for (column, ch) in [
            (0, '/'),
            (1, '用'),
            (3, '户'),
            (5, '/'),
            (6, 'f'),
            (7, 'i'),
            (8, 'l'),
            (9, 'e'),
        ] {
            grid[Point::new(Line(0), Column(column))].c = ch;
        }
        for column in [1, 3] {
            grid[Point::new(Line(0), Column(column))]
                .flags
                .insert(Flags::WIDE_CHAR);
            grid[Point::new(Line(0), Column(column + 1))]
                .flags
                .insert(Flags::WIDE_CHAR_SPACER);
        }

        let hints = detect_rows(&grid, &[0], 10);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].text, "/用户/file");
        assert_eq!((hints[0].start, hints[0].end), (0, 9));
    }

    /// A relative path keeps its directory; before, `out/diagram.png` was
    /// detected as `/diagram.png`, an absolute path that names another file.
    #[test]
    fn a_relative_path_is_detected_whole() {
        let paths = |line: &str| {
            detect(&[line])
                .into_iter()
                .filter(|span| span.kind == Kind::Path)
                .map(|span| (span.start, span.text))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths("wrote out/diagram.png"),
            [(6, "out/diagram.png".to_string())]
        );
        assert_eq!(paths("./out/a.png"), [(0, "./out/a.png".to_string())]);
        assert_eq!(paths("../x/y.svg"), [(0, "../x/y.svg".to_string())]);
        assert_eq!(
            paths("~/notes/today.md"),
            [(0, "~/notes/today.md".to_string())]
        );
        assert_eq!(paths("/etc/hosts"), [(0, "/etc/hosts".to_string())]);
        assert!(
            paths("ls src/ now").is_empty(),
            "a bare `src/` has no segment after its separator, as before"
        );
        assert_eq!(
            paths("see docs/a/b.md."),
            [(4, "docs/a/b.md".to_string())],
            "trailing period trimmed"
        );
        // Unicode segments, and a column count that follows characters.
        assert_eq!(
            paths("abrir café/menú.png"),
            [(6, "café/menú.png".to_string())]
        );
        // A URL still wins, and its path is not a second target.
        let spans = detect(&["https://example.com/out/diagram.png"]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, Kind::Url);
        // A drive letter belongs to the path, but not a word ending in a colon.
        assert_eq!(
            paths("C:/out/diagram.png"),
            [(0, "C:/out/diagram.png".to_string())]
        );
        assert_eq!(paths("note:/srv/x"), [(5, "/srv/x".to_string())]);
    }

    /// A path cannot begin inside a longer token: `foo(1)/bar.png` holds no
    /// `/bar.png`, which would name an unrelated absolute file. Ordinary paths
    /// after a space, a delimiter, `=` or a list or redirect separator stay.
    #[test]
    fn a_path_cannot_start_inside_a_token() {
        let paths = |line: &str| {
            detect(&[line])
                .into_iter()
                .filter(|span| span.kind == Kind::Path)
                .map(|span| span.text)
                .collect::<Vec<_>>()
        };
        for line in ["foo(1)/bar.png", "x]/etc/hosts", "50%/tmp/x", "a#/b/c"] {
            assert!(paths(line).is_empty(), "{line}: {:?}", paths(line));
        }
        for (line, path) in [
            ("open /etc/hosts", "/etc/hosts"),
            ("(/etc/hosts)", "/etc/hosts"),
            ("--out=/tmp/x.png", "/tmp/x.png"),
            ("cmd >/tmp/out.log", "/tmp/out.log"),
            ("cmd </tmp/in.txt", "/tmp/in.txt"),
            ("a|/usr/bin/sort", "/usr/bin/sort"),
            ("true&&/usr/bin/printf", "/usr/bin/printf"),
            ("sleep 1&/usr/bin/true", "/usr/bin/true"),
            ("x,/srv/a", "/srv/a"),
        ] {
            assert_eq!(paths(line), [path], "{line}");
        }
        assert_eq!(paths("PATH=/usr/bin:/bin"), ["/usr/bin", "/bin"]);
    }

    /// An address with a prefix length stays an address: before this pattern
    /// anchored at the first segment, `10.0.0.1/24` left the IP selectable.
    #[test]
    fn a_cidr_network_is_not_a_path() {
        for line in ["route 10.0.0.1/24/ via eth0", "10.0.0.0/8/"] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| span.kind != Kind::Path),
                "{line}: {spans:?}"
            );
        }
        let spans = detect(&["route 10.0.0.1/24 via eth0"]);
        assert!(
            spans.iter().all(|span| span.kind != Kind::Path),
            "{spans:?}"
        );
        assert!(
            spans
                .iter()
                .any(|span| span.kind == Kind::Ip && span.text == "10.0.0.1")
        );
        // Names joined by punctuation, or split by an apostrophe, give only
        // partial spans, wherever they start.
        for line in ["out/report,dir/file.txt", "./user's/report.pdf"] {
            let spans = detect(&[line]);
            assert!(
                !spans.is_empty() && spans.iter().all(|span| !span.bounded),
                "{spans:?}"
            );
        }
        // A path whose segment merely looks numeric is still a path.
        assert!(
            detect(&["/srv/10.0.0.1/logs"])
                .iter()
                .any(|span| span.kind == Kind::Path)
        );
    }

    /// Only a path that meets a boundary on both sides is whole; one that ran
    /// into a character the pattern does not take is part of a name.
    #[test]
    fn a_path_cut_short_is_not_bounded() {
        let span = |line: &str| {
            detect(&[line])
                .into_iter()
                .find(|span| span.kind == Kind::Path)
                .map(|span| (span.text, span.bounded))
        };
        for (line, text) in [
            ("wrote out/diagram.png", "out/diagram.png"),
            ("see (out/a.png).", "out/a.png"),
            ("\"out/a.png\"", "out/a.png"),
            ("--out=out/a.png", "out/a.png"),
            ("src/main.rs:12:5 error", "src/main.rs"),
            ("src/main.rs:12", "src/main.rs"),
            ("is it out/a.png? yes", "out/a.png"),
            ("out/a.png, out/b.png", "out/a.png"),
            ("see out/a.png: done", "out/a.png"),
            ("\"Please inspect out/a.png,\" she said.", "out/a.png"),
            ("\"src/main.rs:12\"", "src/main.rs"),
            ("[src/main.rs:12]", "src/main.rs"),
            ("See src/main.rs:12.", "src/main.rs"),
            ("x, dir/file.txt", "dir/file.txt"),
            ("'out/a.png'", "out/a.png"),
            ("src/main.cpp(12,5): error C2143", "src/main.cpp"),
            ("src/main.cpp(12): warning", "src/main.cpp"),
            ("src/main.rs:12:let x = 1;", "src/main.rs"),
            ("src/main.rs:12:5: error[E0308]", "src/main.rs"),
            ("**docs/README.md**", "docs/README.md"),
            ("modified:   src/app.rs", "src/app.rs"),
        ] {
            assert_eq!(span(line), Some((text.to_string(), true)), "{line:?}");
        }
        for (line, text) in [
            ("open out/report#1.pdf", "out/report"),
            ("open out/50%/a.png", "out/50"),
            ("user@host:dir/file.txt", "dir/file.txt"),
            ("out/a.png?raw=1", "out/a.png"),
            ("out/report,1.pdf", "out/report"),
            ("out/report;v2.pdf", "out/report"),
            ("out/report:v2.pdf", "out/report"),
            ("out/report!.pdf", "out/report"),
            ("out/report:12,final.pdf", "out/report"),
            ("?? \"docs/annual report.pdf\"", "docs/annual"),
            ("./docs/it's.md", "./docs/it"),
            ("out/report(1).pdf", "out/report"),
        ] {
            assert_eq!(span(line), Some((text.to_string(), false)), "{line:?}");
        }
    }

    /// A URL the terminal wrapped onto the next row is one hint, labelled
    /// where it starts, with its whole text; a hard break does not join.
    #[test]
    fn a_soft_wrapped_url_is_one_hint() {
        use alacritty_terminal::grid::Grid;
        use alacritty_terminal::index::{Column, Line, Point};
        use alacritty_terminal::term::cell::{Cell, Flags};

        let grid_with = |wrapped: bool| {
            let mut grid: Grid<Cell> = Grid::new(2, 12, 0);
            for (line, text) in ["see https://", "x.test/a/b c"].iter().enumerate() {
                for (column, c) in text.chars().enumerate() {
                    grid[Point::new(Line(line as i32), Column(column))].c = c;
                }
            }
            if wrapped {
                grid[Point::new(Line(0), Column(11))]
                    .flags
                    .insert(Flags::WRAPLINE);
            }
            grid
        };
        let hints = detect_rows(&grid_with(true), &[0, 1], 12);
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].text, "https://x.test/a/b");
        assert_eq!((hints[0].row, hints[0].start, hints[0].end), (0, 4, 11));
        assert!(
            detect_rows(&grid_with(false), &[0, 1], 12)
                .iter()
                .all(|span| span.text != "https://x.test/a/b")
        );
        // Reading from row 1 alone, the line was cut before it: the
        // `x.test/a/b` that starts there is the tail of a longer URL.
        assert!(
            detect_rows(&grid_with(true), &[1], 12)
                .iter()
                .all(|span| span.start != 0),
            "{:?}",
            detect_rows(&grid_with(true), &[1], 12)
        );
    }

    #[test]
    fn trailing_punctuation_trimmed_and_no_overlap() {
        let h = detect(&["go to (https://rust-lang.org)."]);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].kind, Kind::Url);
        assert_eq!(h[0].text, "https://rust-lang.org", "no trailing ).");
        // The URL's path-looking substring is not also a separate hint.
        assert!(h.iter().all(|s| s.kind == Kind::Url));
    }

    #[test]
    fn reading_order_and_empty() {
        let h = detect(&["/a/b", "x", "/c/d /e/f"]);
        let coords: Vec<(usize, usize)> = h.iter().map(|s| (s.row, s.start)).collect();
        assert_eq!(coords, vec![(0, 0), (2, 0), (2, 5)]);
        assert!(detect(&["nothing here", ""]).is_empty());
    }

    #[test]
    fn labels_are_short_unique_and_deterministic() {
        // Simple a,b,c… alphabet makes the ordering easy to assert.
        let abc = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(labels(0, abc), Vec::<String>::new());
        assert_eq!(labels(1, abc), vec!["a"]);
        assert_eq!(labels(3, abc), vec!["a", "b", "c"], "1-char while they fit");
        // With the real home-row alphabet the first labels are a, s, d…
        assert_eq!(labels(3, ALPHABET), vec!["a", "s", "d"]);

        // Just past the alphabet → all uniform 2-char, distinct.
        let n = abc.len() + 2;
        let many = labels(n, abc);
        assert!(many.iter().all(|l| l.len() == 2), "uniform width");
        assert_eq!(many[0], "aa");
        assert_eq!(many[1], "ab");
        let uniq: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(uniq.len(), n, "all labels distinct");
        // Deterministic for a given n (labels are assigned once per scan).
        assert_eq!(labels(n, abc), labels(n, abc));
        // Degenerate alphabet → no labels (caller falls back).
        assert!(labels(5, "").is_empty());
        // A single-character alphabet must NOT hang (the
        // fixed-width search can't represent n>1) — fall back to distinct
        // increasing-length labels.
        assert_eq!(labels(1, "x"), vec!["x"]);
        assert_eq!(labels(3, "x"), vec!["x", "xx", "xxx"]);
        let single = labels(50, "x");
        assert_eq!(single.len(), 50);
        assert_eq!(
            single
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            50,
            "all distinct, no hang"
        );
    }
}
