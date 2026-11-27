//! Quick-select "hint mode" core (kitty `kitten hints` / WezTerm
//! `QuickSelect` style): scan the visible rows for interesting tokens —
//! URLs, filesystem paths, git hashes, IPv4 addresses — and assign each a
//! short, easy-to-type label. Pure and fully unit-tested; the UI hint overlay
//! consumes [`detect_rows`] and [`labels`]; double-click selection uses the
//! same grid mapping through [`selection_at`].

use regex::Regex;
use std::sync::OnceLock;

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
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
    /// copied but must not be opened. A path holding a character that hides
    /// text (a zero-width joiner, a variation selector) is not bounded
    /// either: it does not read as the file it names.
    pub bounded: bool,
}

fn res() -> &'static [(Kind, Regex)] {
    static RE: OnceLock<Vec<(Kind, Regex)>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            (Kind::Url, Regex::new(crate::links::URL_PATTERN).unwrap()),
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

/// Require path context so progress counters and short ratios are ordinary text.
/// Explicit roots are unambiguous; relative names need a filename extension
/// (a dotfile's name, `.a`, is one), multiple directories, or a component
/// with at least two letters.
pub(crate) fn has_path_context(text: &str) -> bool {
    let bytes = text.as_bytes();
    if text.starts_with(['/', '\\'])
        || text.starts_with("~/")
        || text.starts_with("./")
        || text.starts_with("../")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
    {
        return true;
    }
    let text = text.split(':').next().unwrap_or(text);
    let parts: Vec<_> = text.split('/').collect();
    let letters = |part: &str| part.chars().filter(|c| c.is_alphabetic()).count();
    parts.iter().any(|part| letters(part) >= 2)
        || (parts.len() >= 3 && parts.iter().any(|part| letters(part) > 0))
        || parts.last().is_some_and(|name| {
            name.rsplit_once('.')
                .is_some_and(|(_, ext)| ext.chars().any(char::is_alphabetic))
        })
}

/// The stricter context for paths Kettle underlines on its own: besides
/// [`has_path_context`], a relative name without an explicit root needs an
/// extension on its last part (a dotfile's name, `.env`, is one) or at least
/// three parts, so slash-joined words in prose (`read/write`, `and/or`,
/// `Save/restore`) are not drawn as links. Hint mode, an explicit request,
/// keeps the looser rule.
pub(crate) fn has_link_path_context(text: &str) -> bool {
    if !has_path_context(text) {
        return false;
    }
    let bytes = text.as_bytes();
    if text.starts_with(['/', '\\', '~', '.'])
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return true;
    }
    let text = text.split(':').next().unwrap_or(text);
    let parts: Vec<_> = text.split('/').collect();
    // Dots after the extension leave it the name's (`"docs/report.pdf.."`,
    // which only a closing quote right after them lets through).
    parts.len() >= 3
        || parts.last().is_some_and(|name| {
            name.trim_end_matches('.')
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ext.chars().any(char::is_alphabetic))
        })
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

/// The longest quoted path [`Quotes::quoted_paths`] considers, in bytes.
const MAX_QUOTED_PATH: usize = 1024;

/// A line's quotes, read as a shell reads them, and the words they make.
/// Where a path sits among them decides whether it may open (see
/// [`Quotes::lets_open`]); nothing else about quotes does.
///
/// A `"`, `'` or `` ` `` opens a string wherever no backslash escapes it,
/// and the string closes at the next quote of its kind. Inside double quotes
/// a backslash escapes a `"` (`"a\"b"`). Before a `'` or `` ` `` it closes
/// the string where a shell token boundary follows (`'C:\work\';`), as a
/// shell's literal backslash does, and escapes the quote where more text is
/// glued on (`'docs/a\'b'`), as JavaScript and Python do. Outside a string
/// an escaped quote is a plain character. Whitespace and the shell's
/// separators `;`, `&`, `|`, `<` and `>` outside strings separate words,
/// unless a backslash escapes them, so a string may sit inside a word
/// (`x="a b"`, `{"k":"v"}`, `prefix"…"`). A `(` or `)` does not: glued to a
/// closed string it keeps the word quoted (`a"b"(/tmp/r.pdf`).
///
/// A `'` right after a letter or digit is an apostrophe (`can't`, `it's`,
/// `users'`) unless a later `'` that ends a word, one after a word's text
/// that the word does not run on from (see [`runs_on`]), closes it, as a
/// shell would read it (`prefix'a b'`,
/// `pre'a b':x`): then the text up to that `'` may be quoted or not, and is
/// unsure, so a contraction and a later possessive (`Bob's … users'`) leave
/// the text between them unsure. Two contractions (`don't … it's`) do not.
///
/// A quote that never closes makes the whole line uncertain, since every
/// string on it may be read out of step, before it as after it (`x 'tis
/// A='/a b/c d.txt'`). So does an apostrophe that could close a string,
/// one a letter or digit does not follow (`users'`, unlike `can't`), unless
/// a later `'` that ends a word closes it. And so does a string
/// that runs on into more of its word after it closes (`"a"b`,
/// `"$src"/a.txt`): two stray quotes, or one and an apostrophe, pair with
/// each other and with the strings between them and leave no quote open
/// (`the "quoted x="a b/c" the "quoted`), but the string a stray quote
/// opens ends where the next string was to begin.
pub(crate) struct Quotes {
    /// Closed strings, as the byte offsets of their opening and closing
    /// quotes, in order.
    strings: Vec<(usize, usize)>,
    /// Whether each of `strings` holds a character that hides text, a space
    /// aside, found once here rather than for every path in it.
    hiding: Vec<bool>,
    /// Unsure text, as the byte offsets of the apostrophe that starts it and
    /// of the `'` that ends it, in order.
    unsure: Vec<(usize, usize)>,
    /// The quote characters outside strings and unsure text, and those that
    /// delimit them, in order; an escaped one outside strings is text and
    /// not among them.
    marks: Vec<usize>,
    /// The words, as byte ranges, in order. In unsure text, and on an
    /// uncertain line, whitespace alone separates them.
    words: Vec<(usize, usize)>,
    /// The line holds a quote that never closes.
    uncertain: bool,
}

impl Quotes {
    /// Read `line`'s quotes, once and in linear time.
    pub(crate) fn read(line: &str) -> Self {
        let mut quotes = Self {
            strings: Vec::new(),
            hiding: Vec::new(),
            unsure: Vec::new(),
            marks: Vec::new(),
            words: Vec::new(),
            uncertain: false,
        };
        let mut word: Option<usize> = None;
        let split = |from: usize, to: usize, word: &mut Option<usize>, words: &mut Vec<_>| {
            for (offset, c) in line[from..to].char_indices() {
                if !c.is_whitespace() {
                    word.get_or_insert(from + offset);
                } else if let Some(start) = word.take() {
                    words.push((start, from + offset));
                }
            }
        };
        let mut backslashes = 0usize;
        let mut previous: Option<char> = None;
        let mut at = 0;
        while let Some(c) = line[at..].chars().next() {
            let escaped = backslashes % 2 == 1;
            backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            let before = previous.replace(c);
            let next = at + c.len_utf8();
            if (c.is_whitespace() || ";&|<>".contains(c)) && !escaped {
                if let Some(start) = word.take() {
                    quotes.words.push((start, at));
                }
                at = next;
                continue;
            }
            word.get_or_insert(at);
            if !matches!(c, '"' | '\'' | '`') || escaped {
                at = next;
                continue;
            }
            let apostrophe = c == '\'' && before.is_some_and(char::is_alphanumeric);
            let close = closing_quote(line, at);
            // A `'` after a word's text that the word does not run on from
            // may close a string, glued to more of its word or not (`'a
            // b':x`); one after a space or `=` opens the next string, and one
            // the word runs on from opens one inside it (`prefix'./a b'`).
            let closes_a_word = |close: usize| {
                line[..close]
                    .chars()
                    .next_back()
                    .is_some_and(|c| !c.is_whitespace() && !"=:([{,".contains(c))
                    && !runs_on(&line[close + 1..])
            };
            let ends_a_word = line[next..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            match close {
                // A string that runs on into more of its word after it closes
                // (`"a"b`, `x='a'/b`) may have paired a stray quote with the
                // next string's opening one, so the line is uncertain.
                Some(close) if !apostrophe && runs_on(&line[close + 1..]) => {
                    quotes.uncertain = true;
                    break;
                }
                Some(close) if !apostrophe => {
                    quotes.strings.push((at, close));
                    quotes.hiding.push(hides_text_in(&line[at + 1..close]));
                    quotes.marks.extend([at, close]);
                    at = close + 1;
                }
                Some(close) if closes_a_word(close) => {
                    quotes.unsure.push((at, close));
                    quotes.marks.extend([at, close]);
                    split(at, close + 1, &mut word, &mut quotes.words);
                    at = close + 1;
                }
                // A quote no quote closes, or an apostrophe that could close
                // a string (`users'`, not `can't`) that no later `'` ending a
                // word closes, leaves the whole line uncertain.
                None if !apostrophe => {
                    quotes.uncertain = true;
                    break;
                }
                _ if ends_a_word => {
                    quotes.uncertain = true;
                    break;
                }
                _ => {
                    quotes.marks.push(at);
                    at = next;
                }
            }
        }
        if quotes.uncertain {
            // Its strings may be read out of step, so its words are read by
            // whitespace alone.
            word = None;
            quotes.words.clear();
            split(0, line.len(), &mut word, &mut quotes.words);
        }
        if let Some(start) = word {
            quotes.words.push((start, line.len()));
        }
        quotes
    }

    /// The paths written as a quoted string's whole text, which may then
    /// hold spaces and characters the bare pattern stops at: `"docs/annual
    /// report.pdf"`, `` `out/report #1.pdf` ``, Python's `File "/my
    /// app/x.py"`. Yields the byte range of each path inside `line` and its
    /// string's closing quote, after a location suffix the string may also
    /// hold (`"src/my file.rs:12"`).
    ///
    /// The quotes make the ends certain, not the contents, so the contents
    /// must read as a path: a separator, no empty segment, no segment that
    /// starts or ends with a space, and no character a name rarely holds.
    /// Quoted prose must not become a path, so a relative path's first
    /// segment has no space (`"see src/main.rs"` stays prose), and the path
    /// ends in an extension (a dotfile's name, `.env`, is one) or a
    /// separator, or is anchored (`/`, `~/`, `./`, `../`, a drive) with at
    /// least two separators (`"~/Library/Application Support"`). The string
    /// starts a token after whitespace, an opening bracket, `<`, `=`, `,` or
    /// `;`, never after a `:`, which joins a remote host to its path
    /// (`host:"/srv/a b.pdf"`, `'user@host':"…"`) as often as a compact JSON
    /// key to its value. Nothing here touches the filesystem.
    pub(crate) fn quoted_paths<'a>(
        &'a self,
        line: &'a str,
    ) -> impl Iterator<Item = (usize, usize, usize)> + 'a {
        self.strings.iter().filter_map(move |&(open, close)| {
            if !starts_a_token(line, open, "([{<=,;") || !quote_has_end(line, close) {
                return None;
            }
            let quote = char::from(line.as_bytes()[open]);
            let body = &line[open + 1..close];
            // A quoted network (`"10.0.0.1/24/"`) is an address, as unquoted.
            quoted_path_len(body, quote)
                .filter(|&len| !is_cidr(&body[..len]))
                .map(|len| (open + 1, open + 1 + len, close))
        })
    }

    /// Whether no quote on the line is left open, so its strings read as
    /// written. On an uncertain line a quoted path is copied, never opened.
    pub(crate) fn certain(&self) -> bool {
        !self.uncertain
    }

    /// Whether a closed string holding the bare path at `start..end` closes
    /// right at `end` and ends the token there, so its quote marks where the
    /// name ends: `"/tmp/a.."` names `a..`.
    pub(crate) fn closes_at(&self, line: &str, start: usize, end: usize) -> bool {
        let held = self.strings.partition_point(|&(open, _)| open < start);
        held.checked_sub(1)
            .is_some_and(|at| self.strings[at].1 == end && quote_has_end(line, end))
    }

    /// Whether the bare path at `start..end` may open, by where it sits
    /// among the quotes. The token rules ([`match_has_end`] and the rest)
    /// still apply on top. On an uncertain line only a path from a root that
    /// starts its word, or follows an `=`, `:`, `(`, `[` or `{` that does,
    /// may open. Otherwise these may open:
    ///
    /// - a path outside strings and unsure text whose word holds no quote
    ///   character: a whole word (`src/main.rs`, `--out=/tmp/x.png`), or a
    ///   word after a closed string's, which the string never affects
    ///   (`echo "hi " src/main.rs`, `key="a/b";other=/tmp/c.txt`). A closed
    ///   string glued to more of its word keeps the whole word quoted, so no
    ///   piece of it opens (`pre"fix"=/tmp/r.pdf` is `prefix=/tmp/r.pdf`);
    /// - the whole text of a string that starts a token at a word's start or
    ///   after `=`, `:`, `,`, `;`, an opening bracket, `<`, `>`, `|` or `&`
    ///   (`x="a/b.rs"`, `{"file":"a/b.rs"}`), but not one glued to a word,
    ///   which a shell joins to it (`prefix"a/b.rs"` names `prefixa/b.rs`);
    /// - a path from a root that starts its word, or follows an `=`, `:`,
    ///   `(`, `[` or `{` that does, whatever follows it in the word, and in
    ///   unsure text too (`/tmp/x.log"`, `(/tmp/x.log`);
    /// - inside a string, a path from a root after a space that ends the
    ///   string (`"see /tmp/report.pdf"`), as quoted prose prints one,
    ///   unless the string holds a character that hides text.
    ///
    /// Any other piece of a word holding a quote character, or of a string's
    /// text, is copied, never opened: `docs/a` and `/tmp/b.pdf` in `"docs/a
    /// b=/tmp/b.pdf"` and in `prefix'docs/a*b=/tmp/b.pdf'`, `b/notes.txt` in
    /// `"a b/notes.txt"`, and a relative path in unsure text.
    pub(crate) fn lets_open(&self, line: &str, start: usize, end: usize) -> bool {
        let word = self
            .words
            .partition_point(|&(word, _)| word <= start)
            .checked_sub(1)
            .map_or((start, end), |at| self.words[at]);
        let leads =
            start == word.0 || (start == word.0 + 1 && b"=:([{".contains(&line.as_bytes()[word.0]));
        let rooted = leads && starts_its_own_path(&line[start..]);
        if self.uncertain {
            return rooted;
        }
        let held = self.strings.partition_point(|&(open, _)| open < start);
        if let Some(at) = held.checked_sub(1)
            && end <= self.strings[at].1
        {
            // Each check costs no more than this path's own length, so a
            // string listing many paths is read in linear time.
            let (open, close) = self.strings[at];
            let reaches = skip_location(&line[end..close])
                .0
                .trim_start_matches(|c| ",;:?!.".contains(c))
                .is_empty();
            let whole = start == open + 1 && starts_a_token(line, open, "=:,;([{<>|&");
            let from_a_root = line[..start].ends_with(char::is_whitespace)
                && starts_its_own_path(&line[start..])
                && !self.hiding[at];
            return reaches && (whole || from_a_root);
        }
        let unsure = self.unsure.partition_point(|&(open, _)| open < start);
        if unsure
            .checked_sub(1)
            .is_some_and(|at| start < self.unsure[at].1)
        {
            return rooted;
        }
        let quoted = |from: usize, to: usize| {
            let at = self.marks.partition_point(|&mark| mark < from);
            self.marks.get(at).is_some_and(|&mark| mark < to)
        };
        !quoted(word.0, start) && (rooted || !quoted(end, word.1))
    }
}

#[cfg(test)]
thread_local! {
    /// Bytes of string text [`hides_text_in`] has searched on this thread,
    /// so a test can bound the work detection does.
    pub(crate) static HIDES_SCANNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether `text` holds a character that hides text, a space aside.
fn hides_text_in(text: &str) -> bool {
    #[cfg(test)]
    HIDES_SCANNED.with(|scanned| scanned.set(scanned.get() + text.len()));
    text.chars()
        .any(|c| c != ' ' && crate::links::hides_text(c))
}

/// Whether `rest`, right after a string's closing quote, goes on with more
/// of a word: a letter, a digit, `/`, `~` or `\`, or a `.` before one of
/// them, so not punctuation that ends a sentence (`"a/b.txt".`).
fn runs_on(rest: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || "/~\\".contains(c);
    let mut chars = rest.chars();
    match chars.next() {
        Some('.') => chars.next().is_some_and(word),
        next => next.is_some_and(word),
    }
}

/// The quote that closes a string opened at `open`, as [`Quotes::read`]
/// closes one.
fn closing_quote(line: &str, open: usize) -> Option<usize> {
    let quote = line[open..].chars().next()?;
    let mut backslashes = 0usize;
    let mut chars = line[open + quote.len_utf8()..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        let escaped = backslashes % 2 == 1;
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
        let next = chars.peek().map(|&(_, next)| next);
        if c == quote && (!escaped || (c != '"' && shell_boundary(next))) {
            return Some(open + quote.len_utf8() + offset);
        }
    }
    None
}

/// Whether the quote at `open` starts a token: at the line's start, after
/// whitespace no backslash escapes, or after one of `separators`.
fn starts_a_token(line: &str, open: usize, separators: &str) -> bool {
    let mut back = line[..open].chars().rev();
    match back.next() {
        None => true,
        Some(c) if c.is_whitespace() => back.next() != Some('\\'),
        Some(c) => separators.contains(c),
    }
}

/// Whether `next` ends a shell token: whitespace, the line's end, or `;`,
/// `&`, `|`, `)`, `>` or `<`.
fn shell_boundary(next: Option<char>) -> bool {
    next.is_none_or(|c| c.is_whitespace() || ";&|)><".contains(c))
}

/// Whether a closing quote at `close` ends a token: what follows it, past a
/// location suffix, is whitespace, the line's end, punctuation that ends a
/// match, or a separator.
fn quote_has_end(line: &str, close: usize) -> bool {
    if close >= line.len() {
        return false;
    }
    let (tail, grep) = skip_location(&line[close + 1..]);
    grep || ends_a_match(tail) || tail.starts_with([',', ';', ':', ')', ']', '}', '>'])
}

/// Whether `path` starts from a root, `~/`, `./`, `../` or a drive, so it
/// cannot continue a name before it in quoted text.
fn starts_its_own_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(['/', '\\'])
        || ["~/", "./", "../"]
            .iter()
            .any(|root| path.starts_with(root))
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

/// Whether a location (`:12`, `:12:5`) follows a match at `rest`, which
/// ends the name before it as a closing quote does: `/tmp/a..:3` names
/// `a..`, as the link pattern, which takes the location, reads it.
fn location_follows(rest: &str) -> bool {
    rest.strip_prefix(':')
        .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_digit()))
}

/// The length of the path in quoted text `body` (without a trailing `:12`
/// or `:12:5`), or `None` when `body` does not read as a path (see
/// [`Quotes::quoted_paths`]).
fn quoted_path_len(body: &str, quote: char) -> Option<usize> {
    if body.is_empty() || body.len() > MAX_QUOTED_PATH || body.contains("://") {
        return None;
    }
    let mut path = body;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, digits))
                if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
            {
                path = head;
            }
            _ => break,
        }
    }
    let bytes = path.as_bytes();
    // Only Windows has drives; elsewhere `h:/srv/a b.pdf` is a remote host's
    // path (`scp "h:/srv/a b.pdf" .`), and a colon disqualifies it below.
    let drive = cfg!(windows)
        && bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    // A backslash separates only after a drive; elsewhere it is an escape.
    let is_separator = |c: char| c == '/' || (drive && c == '\\');
    let rest = if drive { &path[2..] } else { path };
    // Unicode `\w` takes letters, digits and combining marks, so a name in
    // decomposed form (`cafe\u{301}`, as macOS often writes it) is whole.
    static NAME_CHARS: OnceLock<Regex> = OnceLock::new();
    let name_chars =
        NAME_CHARS.get_or_init(|| Regex::new(r"^[\w\p{Zs}.\-@+,()\[\]{}#&!$%=~;'/\\]*$").unwrap());
    if !name_chars.is_match(rest)
        || rest.contains(quote)
        || rest.chars().any(|c| c == '\\' && !is_separator(c))
    {
        return None;
    }
    let segments = rest.split(is_separator).collect::<Vec<_>>();
    let separators = segments.len() - 1;
    if separators == 0 {
        return None;
    }
    let first = segments[0];
    let anchored = drive || matches!(first, "" | "~" | "." | "..");
    // A visible space, such as the narrow no-break space macOS writes
    // before AM in a screenshot's name, is a space here as anywhere.
    let space = crate::links::visible_space;
    if !anchored && first.contains(space) {
        return None;
    }
    if segments[usize::from(anchored)..]
        .iter()
        .all(|segment| segment.is_empty())
    {
        return None;
    }
    let last = segments.len() - 1;
    for (index, segment) in segments.iter().enumerate() {
        let edge = (index == 0 && anchored) || index == last;
        if (segment.is_empty() && !edge) || segment.starts_with(space) || segment.ends_with(space) {
            return None;
        }
    }
    let name = segments[last];
    let directory = name.is_empty();
    // One dot after a name ends a sentence, inside quotes as after a bare
    // path (`"/tmp/report.pdf."`), so it is no part of the path. Two or
    // more, which trimming would turn into another file's name, the closing
    // quote marks as the name's, and they leave its extension and last
    // character as they were (`"/tmp/my report.pdf.."`). A name of dots
    // alone has neither.
    let sentence_dot = name.len() > 1 && name.ends_with('.') && !name.ends_with("..");
    let named = if sentence_dot {
        &name[..name.len() - 1]
    } else {
        name.trim_end_matches('.')
    };
    // An extension of any length (`.swiftinterface`): ASCII letters and
    // digits, in words joined by `-` or `_` (`.code-workspace`).
    let extension = named.rsplit_once('.').is_some_and(|(_, extension)| {
        extension
            .split(['-', '_'])
            .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric()))
    });
    // Ends like a name, not prose punctuation; Unicode `\w` again, so a
    // decomposed name (`café` as `cafe\u{301}`) ends in a word character.
    static NAME_END: OnceLock<Regex> = OnceLock::new();
    let deep = anchored
        && separators >= 2
        && NAME_END
            .get_or_init(|| Regex::new(r"[\w)\]]$").unwrap())
            .is_match(named);
    (directory || extension || deep).then_some(path.len() - usize::from(sentence_dot))
}

/// Whether a path does not open as the name it spells: it holds a
/// character that hides text, a space aside, or names a parent once
/// decoded (`%2e%2e`).
fn path_misleads(path: &str) -> bool {
    path.chars()
        .any(|c| c != ' ' && crate::links::hides_text(c))
        || crate::links::climbs_once_decoded(path)
}

/// Whether a match starting after `before` and followed by `rest` meets a
/// boundary on both sides (see [`HintSpan::bounded`]).
///
/// Before it: the line's start, a space, an opening delimiter or `=`
/// (`--out=dir/x`). A `:` may join a remote `host:path` and a `,` or `;` may
/// sit inside a name, so they do not count there. A quote or Markdown `*`
/// before it must close right after it: in `"docs/annual report.pdf` the
/// unclosed quote leaves `docs/annual` cut at the space (a closed one is a
/// [`Quotes::quoted_paths`] match), and the `s/report.pdf` in `./user's/report.pdf`
/// starts mid-name.
///
/// After it, past a location (`:12`, `:12:5`, `(12)`, `(12,5)`): see
/// [`ends_a_match`]. grep's `path:12:text` counts as whole, so a rare file
/// name containing `:12:` reads the same way.
fn is_bounded(before: Option<char>, rest: &str) -> bool {
    let left = before.is_none_or(|c| c.is_whitespace() || "\"'`*([{<=".contains(c));
    left && match_has_end(before, rest)
}

/// Whether a match ends a token, allowing a location suffix and closing quote.
pub(crate) fn match_has_end(before: Option<char>, rest: &str) -> bool {
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
///
/// It looks at most [`MAX_TRAILING`] characters ahead, so checking every
/// match on a line stays linear however the line repeats punctuation: a
/// longer run of it is not a boundary.
fn ends_a_match(rest: &str) -> bool {
    for c in rest.chars().take(MAX_TRAILING + 1) {
        if c.is_whitespace() {
            return true;
        }
        if !",;:?!.\"'`)]}>*".contains(c) {
            return false;
        }
    }
    rest.chars().nth(MAX_TRAILING).is_none()
}

/// How many punctuation and closing-delimiter characters [`ends_a_match`]
/// reads past a match before giving up.
const MAX_TRAILING: usize = 16;

/// Ranges already taken on a line, half-open and non-overlapping, keyed by
/// start, so checking or taking one costs a logarithm of how many the line
/// holds, in any order.
#[derive(Default)]
pub(crate) struct Taken(std::collections::BTreeMap<usize, usize>);

impl Taken {
    /// Whether `start..end` overlaps a taken range.
    pub(crate) fn overlaps(&self, start: usize, end: usize) -> bool {
        // Taken ranges do not overlap, so only the last one starting before
        // `end` can reach past `start`.
        self.0
            .range(..end)
            .next_back()
            .is_some_and(|(_, &taken_end)| taken_end > start)
    }

    /// Take `start..end`, which must not overlap a taken range.
    pub(crate) fn insert(&mut self, start: usize, end: usize) {
        debug_assert!(!self.overlaps(start, end));
        self.0.insert(start, end);
    }

    /// Take whatever of `start..end` is not taken yet, around the ranges
    /// already taken inside it.
    pub(crate) fn fill(&mut self, start: usize, end: usize) {
        // Only the last range starting before `start` can reach into it.
        let mut at = self
            .0
            .range(..start)
            .next_back()
            .map_or(start, |(_, &taken_end)| taken_end.max(start));
        let mut gaps = Vec::new();
        for (&taken_start, &taken_end) in self.0.range(start..end) {
            if taken_start > at {
                gaps.push((at, taken_start));
            }
            at = at.max(taken_end);
        }
        if at < end {
            gaps.push((at, end));
        }
        for (gap_start, gap_end) in gaps {
            self.0.insert(gap_start, gap_end);
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
    let mut taken = Taken::default();
    let quotes = Quotes::read(line);
    // Text shaped like a URL of any scheme holds no path, though an address
    // or hash in it may still be copied.
    let mut url_shaped = Taken::default();
    for m in crate::links::url_shaped_re().find_iter(line) {
        url_shaped.insert(m.start(), m.end());
    }
    for (kind, re) in res() {
        if *kind == Kind::Path {
            // A quoted path is whole by construction; it goes first so the
            // bare pattern cannot take a piece of it.
            for (bs, be, close) in quotes.quoted_paths(line) {
                let fragment = (cut.0 && bs == 1) || (cut.1 && close + 1 == line.len());
                if fragment || taken.overlaps(bs, be) || url_shaped.overlaps(bs, be) {
                    continue;
                }
                taken.insert(bs, be);
                out.push(HintSpan {
                    row,
                    start: col_of_byte[bs],
                    end: col_of_byte[be - 1],
                    kind: Kind::Path,
                    text: line[bs..be].to_string(),
                    bounded: quotes.certain() && !path_misleads(&line[bs..be]),
                });
            }
        }
        for m in re.find_iter(line) {
            let (m_start, raw) = if *kind == Kind::Url {
                // After an invisible character the URL is part of the word
                // before it, and no piece of it is a hint.
                let Some(url) = crate::links::url_match(line, &m) else {
                    taken.fill(m.start(), m.end());
                    continue;
                };
                url
            } else {
                (m.start(), m.as_str())
            };
            // A path ending in two or more dots may name a file ending in
            // them: the exact name opens where a closing quote marks it, and
            // otherwise the hint copies the text as printed.
            let mut uncertain = false;
            let trimmed = if *kind == Kind::Path {
                let trimmed = crate::url_trim::trim_path_trailing(raw);
                let tail = &raw[trimmed.len()..];
                match crate::url_trim::dot_run_after(tail, &line[m.end()..]) {
                    Some(run)
                        if run > 0
                            && trimmed.len() + run == raw.len()
                            && (quotes.closes_at(line, m_start, m.end())
                                || location_follows(&line[m.end()..])) =>
                    {
                        raw
                    }
                    Some(run) => {
                        uncertain = true;
                        &raw[..trimmed.len() + run]
                    }
                    None => trimmed,
                }
            } else {
                trim_trailing(raw)
            };
            let before = line[..m_start].chars().next_back();
            let fragment = (cut.0 && m_start == 0) || (cut.1 && m.end() == line.len());
            // A scheme with nothing after it (`see https://...`) is no URL,
            // and no piece of it is a path either.
            if *kind == Kind::Url
                && !fragment
                && (!crate::links::names_a_target(trimmed)
                    || crate::links::url_loses_a_dot_run(raw, trimmed, &line[m.end()..]))
            {
                taken.fill(m_start, m.end());
                continue;
            }
            if trimmed.is_empty()
                || fragment
                || (*kind == Kind::Path
                    && (is_cidr(trimmed)
                        || !has_path_context(trimmed)
                        || !path_may_start_after(before)))
            {
                continue;
            }
            // A piece of a quoted word or string is copied, never opened:
            // `Files/a` in `'My Files/a b.txt'` (see `Quotes::lets_open`).
            let bounded = !uncertain
                && is_bounded(before, &line[m.end()..])
                && (*kind != Kind::Path
                    || (quotes.lets_open(line, m_start, m.end()) && !path_misleads(trimmed)));
            let (bs, be) = (m_start, m_start + trimmed.len());
            if taken.overlaps(bs, be) || (*kind == Kind::Path && url_shaped.overlaps(bs, be)) {
                continue;
            }
            taken.insert(bs, be);
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
    detect_logical_lines(grid, lines, cols, |mut span, cells| {
        let (row, start) = cells[0];
        let end = cells
            .iter()
            .rev()
            .find(|(r, _)| *r == row)
            .map_or(start, |&(_, col)| col);
        span.row = row;
        span.start = start;
        span.end = through_spacer(grid, lines[row], end, cols);
        out.push(span);
    });
    out.sort_by(|a, b| a.row.cmp(&b.row).then(a.start.cmp(&b.start)));
    out
}

/// Detect targets on each soft-wrapped logical line of `lines` and hand each
/// one to `found` with, for every byte of its text, the index into `lines`
/// and the grid column that byte came from.
fn detect_logical_lines(
    grid: &Grid<Cell>,
    lines: &[i32],
    cols: usize,
    mut found: impl FnMut(HintSpan, &[(usize, usize)]),
) {
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
        // columns; `found` maps each span back to rows and columns.
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
        for span in spans.drain(..) {
            let cells = &pos_of_byte[span.start..=span.end];
            found(span, cells);
        }
        first = read.next;
    }
}

/// `col` on grid line `line`, or the spacer cell after it when it holds a wide
/// character: a target covers both of a wide character's cells.
fn through_spacer(grid: &Grid<Cell>, line: i32, col: usize, cols: usize) -> usize {
    let wide = col + 1 < cols
        && grid[Point::new(Line(line), Column(col))]
            .flags
            .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR);
    col + usize::from(wide)
}

/// The complete structured token under one grid cell, as its first and last
/// cells in grid coordinates (inclusive, wide spacers included). A token the
/// terminal soft-wrapped across rows is found from any of them, so its first
/// and last cells can be on different rows. Detection reads the clicked row's
/// logical line, at most [`MAX_LOGICAL_ROWS`](crate::grid_text::MAX_LOGICAL_ROWS)
/// rows of it around the click.
pub fn selection_at(grid: &Grid<Cell>, line: i32, col: usize) -> Option<(Point, Point)> {
    use crate::grid_text::{MAX_LOGICAL_ROWS, soft_wraps};
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32 - 1;
    if !(top..=bottom).contains(&line) {
        return None;
    }
    let wraps_into_next = |line: i32| soft_wraps(grid, line, cols);
    let rows = |first: i32, last: i32| (last - first) as usize + 1;
    // Half the rows above the click, the rest below, then any rows the line
    // did not use below go above.
    let (mut first, mut last) = (line, line);
    while first > top && rows(first, last) <= MAX_LOGICAL_ROWS / 2 && wraps_into_next(first - 1) {
        first -= 1;
    }
    while last < bottom && rows(first, last) < MAX_LOGICAL_ROWS && wraps_into_next(last) {
        last += 1;
    }
    while first > top && rows(first, last) < MAX_LOGICAL_ROWS && wraps_into_next(first - 1) {
        first -= 1;
    }
    let lines: Vec<i32> = (first..=last).collect();
    let clicked = Point::new(Line(line), Column(col));
    let mut token = None;
    detect_logical_lines(grid, &lines, cols, |_, cells| {
        let (start_row, start_col) = cells[0];
        let (end_row, end_col) = cells[cells.len() - 1];
        let start = Point::new(Line(lines[start_row]), Column(start_col));
        let end_col = through_spacer(grid, lines[end_row], end_col, cols);
        let end = Point::new(Line(lines[end_row]), Column(end_col));
        if token.is_none() && start <= clicked && clicked <= end {
            token = Some((start, end));
        }
    });
    token
}

/// Default label alphabet — home-row first so the common (few) targets are
/// one comfortable keystroke.
pub const ALPHABET: &str = "asdfghjklqwertyuiopzxcvbnm";

/// Shortest prefix-free labels, keeping easy alphabet keys short. Expand the
/// last shortest leaf first; no assigned label can consume another's prefix.
/// Empty or unary alphabets cannot represent multiple prefix-free labels.
pub fn labels(n: usize, alphabet: &str) -> Vec<String> {
    let mut alphabet_chars = Vec::new();
    for c in alphabet.chars() {
        if !alphabet_chars.contains(&c) {
            alphabet_chars.push(c);
        }
    }
    let base = alphabet_chars.len();
    if n == 0 || base == 0 || (base == 1 && n > 1) {
        return Vec::new();
    }
    if n <= base {
        return alphabet_chars.iter().take(n).map(char::to_string).collect();
    }
    let mut width = 1;
    let mut capacity = base;
    while let Some(next) = capacity.checked_mul(base).filter(|next| *next <= n) {
        capacity = next;
        width += 1;
    }
    let expanded = n.saturating_sub(capacity).div_ceil(base - 1);
    let short = capacity - expanded;
    let mut labels = Vec::with_capacity(n);
    for mut index in 0..capacity {
        let ordinal = index;
        let mut digits = vec![alphabet_chars[0]; width];
        for digit in digits.iter_mut().rev() {
            *digit = alphabet_chars[index % base];
            index /= base;
        }
        let prefix: String = digits.into_iter().collect();
        if ordinal < short {
            labels.push(prefix);
        } else {
            let remaining_prefixes = capacity - ordinal - 1;
            let children = base.min(n - labels.len() - remaining_prefixes * base);
            labels.extend(
                alphabet_chars
                    .iter()
                    .take(children)
                    .map(|c| format!("{prefix}{c}")),
            );
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_context_rejects_ratios_and_keeps_real_paths() {
        for text in ["158/158", "1/2", "N/N", "a/b", "1.2/3.4"] {
            assert!(
                detect(&[text]).iter().all(|h| h.kind != Kind::Path),
                "{text}"
            );
        }
        for text in [
            "src/main.rs",
            "./build.sh",
            "~/project",
            "/usr/bin/env",
            "crates/a/b.rs:12:4",
        ] {
            assert!(
                detect(&[text]).iter().any(|h| h.kind == Kind::Path),
                "{text}"
            );
        }
    }

    #[test]
    fn labels_keep_short_keys_and_are_prefix_free() {
        let labels = labels(28, ALPHABET);
        assert_eq!(labels.iter().filter(|label| label.len() == 1).count(), 25);
        for (i, label) in labels.iter().enumerate() {
            for (j, other) in labels.iter().enumerate() {
                assert!(
                    i == j || !other.starts_with(label),
                    "{label} prefixes {other}"
                );
            }
        }
    }

    #[test]
    fn label_codes_are_prefix_free_at_each_width_boundary() {
        for n in [1, 25, 26, 27, 28, 675, 676, 677, 900] {
            let codes = labels(n, ALPHABET);
            assert_eq!(codes.len(), n);
            for (i, code) in codes.iter().enumerate() {
                assert!(
                    codes
                        .iter()
                        .enumerate()
                        .all(|(j, other)| i == j || !other.starts_with(code))
                );
            }
            let shortest = codes.iter().map(String::len).min().unwrap();
            assert!(codes.iter().all(|code| code.len() <= shortest + 1));
        }
        assert_eq!(labels(3, "aabc"), ["a", "b", "c"]);
    }

    /// The columns of a [`selection_at`] token that starts and ends on `line`.
    fn selection_columns(grid: &Grid<Cell>, line: i32, col: usize) -> Option<(usize, usize)> {
        selection_at(grid, line, col).map(|(start, end)| {
            assert_eq!((start.line.0, end.line.0), (line, line));
            (start.column.0, end.column.0)
        })
    }

    #[test]
    fn trailing_dot_components_remain_in_path_hints() {
        for path in ["/tmp/..", "/tmp/."] {
            let spans = detect(&[path]);
            let hint = spans.iter().find(|span| span.kind == Kind::Path).unwrap();
            assert_eq!(hint.text, path);
            assert_eq!((hint.start, hint.end), (0, path.len() - 1));
            assert!(hint.bounded);
            let mut grid: Grid<Cell> = Grid::new(1, path.len(), 0);
            for (col, ch) in path.chars().enumerate() {
                grid[alacritty_terminal::index::Point::new(
                    alacritty_terminal::index::Line(0),
                    alacritty_terminal::index::Column(col),
                )]
                .c = ch;
            }
            assert_eq!(
                selection_columns(&grid, 0, path.len() - 1),
                Some((0, path.len() - 1))
            );
        }
    }

    /// A path ending in two or more dots may name a file ending in them or
    /// trail off a sentence, so its hint copies the text as printed and
    /// never opens a shorter path (`/tmp/a`) or the folder (`/tmp/`). A
    /// closing quote right after the dots makes them the name's.
    #[test]
    fn a_path_ending_in_a_dot_run_is_never_opened_shorter() {
        for (line, text) in [
            ("open /tmp/...", "/tmp/..."),
            ("open /tmp/....", "/tmp/...."),
            ("open /tmp/...).", "/tmp/..."),
            ("ls /tmp/a..", "/tmp/a.."),
            ("see foo/bar...", "foo/bar..."),
        ] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert_eq!(spans[0].kind, Kind::Path);
            assert_eq!(spans[0].text, text, "{line}");
            assert!(!spans[0].bounded, "{line}");
        }
        for (line, text) in [
            ("ls /tmp/a..:3", "/tmp/a.."),
            ("ls \"/tmp/a..\"", "/tmp/a.."),
            ("'/tmp/...'", "/tmp/..."),
            ("open \"/tmp/my report.pdf..\" now", "/tmp/my report.pdf.."),
            ("open \"/tmp/my dir/notes...\" now", "/tmp/my dir/notes..."),
        ] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert_eq!(
                (spans[0].text.as_str(), spans[0].bounded),
                (text, true),
                "{line}"
            );
        }
        // Dots after other punctuation, and a file link losing a dot run
        // off its name, are no target to open either. A web link's dots are
        // prose, after its path or its host.
        let spans = detect(&["see /tmp/report.pdf,.."]);
        assert!(spans.iter().all(|span| !span.bounded), "{spans:?}");
        for line in [
            "see file:///tmp/a..",
            "see file:///tmp/%2e.",
            "see file:///tmp/a%2e.",
            "see http://127.1..",
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| span.kind != Kind::Url),
                "{line}: {spans:?}"
            );
        }
        for (line, url) in [
            (
                "see https://example.test/docs...",
                "https://example.test/docs",
            ),
            ("see https://example.test..", "https://example.test"),
        ] {
            assert_eq!(detect(&[line])[0].text, url, "{line}");
        }
        // One dot ends a sentence, inside quotes too.
        for (line, text) in [
            ("cat \"/tmp/report.pdf.\"", "/tmp/report.pdf"),
            ("Saved to '/tmp/report.pdf.'", "/tmp/report.pdf"),
            ("Saved to `/tmp/report.pdf.`", "/tmp/report.pdf"),
            ("Saved to \"/tmp/my report.pdf.\"", "/tmp/my report.pdf"),
        ] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert_eq!(
                (spans[0].text.as_str(), spans[0].bounded),
                (text, true),
                "{line}"
            );
        }
        let spans = detect(&["see /tmp/a."]);
        assert_eq!((spans[0].text.as_str(), spans[0].bounded), ("/tmp/a", true));
    }

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
    fn smart_selection_at_returns_full_token_range() {
        let smart_selection_at = |text: &str, col| {
            use alacritty_terminal::index::{Column, Line, Point};
            let mut grid: Grid<Cell> = Grid::new(1, text.len(), 0);
            for (column, ch) in text.chars().enumerate() {
                grid[Point::new(Line(0), Column(column))].c = ch;
            }
            selection_columns(&grid, 0, col)
        };
        let url_line = "see https://example.com/path?x=1 for more";
        // cursor anywhere inside the URL gets the whole URL.
        let (s, e) = smart_selection_at(url_line, 10).unwrap();
        assert_eq!(&url_line[s..=e], "https://example.com/path?x=1");
        let (s, e) = smart_selection_at(url_line, 31).unwrap();
        assert_eq!(&url_line[s..=e], "https://example.com/path?x=1");
        // Cursor outside the URL returns None — the caller falls back to
        // the alacritty Semantic word.
        assert!(smart_selection_at(url_line, 0).is_none());
        assert!(smart_selection_at(url_line, 39).is_none());

        // IPv4 — dots aren't word chars so the alacritty Semantic word
        // would under-select; smart selection grabs the whole thing.
        let ip_line = "connect to 192.168.1.100 now";
        let (s, e) = smart_selection_at(ip_line, 15).unwrap();
        assert_eq!(&ip_line[s..=e], "192.168.1.100");

        // git SHA.
        let sha_line = "commit a1b2c3d4e5f6 landed";
        let (s, e) = smart_selection_at(sha_line, 10).unwrap();
        assert_eq!(&sha_line[s..=e], "a1b2c3d4e5f6");

        // A relative path, from its first segment: double-clicking `out`
        // selects the whole path, as does double-clicking the file name.
        let path_line = "wrote out/diagram.png ok";
        for col in [7, 15] {
            let (s, e) = smart_selection_at(path_line, col).unwrap();
            assert_eq!(&path_line[s..=e], "out/diagram.png");
        }

        // No hint at all — None.
        assert!(smart_selection_at("plain prose with nothing structured", 5).is_none());
    }

    /// Write `text` into `grid` from `line`, soft-wrapping each full row.
    fn write_wrapped(grid: &mut Grid<Cell>, line: i32, text: &str) {
        use alacritty_terminal::term::cell::Flags;
        let cols = grid.columns();
        for (index, ch) in text.chars().enumerate() {
            let row = line + (index / cols) as i32;
            grid[Point::new(Line(row), Column(index % cols))].c = ch;
            if index % cols == cols - 1 && index + 1 < text.chars().count() {
                grid[Point::new(Line(row), Column(cols - 1))]
                    .flags
                    .insert(Flags::WRAPLINE);
            }
        }
    }

    /// A URL the terminal soft-wrapped onto the next row is one token: a
    /// double-click on either half selects all of it.
    #[test]
    fn smart_selection_follows_a_soft_wrap() {
        use alacritty_terminal::term::cell::Flags;
        let mut grid: Grid<Cell> = Grid::new(3, 20, 0);
        write_wrapped(&mut grid, 0, "go https://example.com/very-long-path ok");
        let url = Some((
            Point::new(Line(0), Column(3)),
            Point::new(Line(1), Column(16)),
        ));
        for (line, col) in [(0, 3), (0, 11), (0, 19), (1, 0), (1, 2), (1, 16)] {
            assert_eq!(selection_at(&grid, line, col), url, "({line}, {col})");
        }
        assert_eq!(selection_at(&grid, 0, 0), None);
        assert_eq!(selection_at(&grid, 1, 18), None);

        // A hard line break ends a token.
        grid[Point::new(Line(0), Column(19))]
            .flags
            .remove(Flags::WRAPLINE);
        assert_eq!(selection_columns(&grid, 0, 11), Some((3, 19)));
    }

    /// In a logical line longer than one detection reads, the clicked row
    /// still sees the rows on both sides of it.
    #[test]
    fn smart_selection_reads_around_the_click_in_a_long_wrapped_line() {
        let cols = 10;
        let mut text = "x".repeat(cols * 30);
        text.replace_range(cols * 25 - 4..cols * 25 + 8, " /tmp/a/b/c ");
        let mut grid: Grid<Cell> = Grid::new(40, cols, 0);
        write_wrapped(&mut grid, 0, &text);
        let path = Some((
            Point::new(Line(24), Column(7)),
            Point::new(Line(25), Column(6)),
        ));
        assert_eq!(selection_at(&grid, 24, 8), path);
        assert_eq!(selection_at(&grid, 25, 2), path);
    }

    #[test]
    fn smart_selection_keeps_a_wide_path_whole() {
        use alacritty_terminal::index::{Column, Line, Point};
        use alacritty_terminal::term::cell::Flags;
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
        for col in 0..10 {
            assert_eq!(
                selection_columns(&grid, 0, col),
                Some((0, 9)),
                "column={col}"
            );
        }
        assert_eq!(selection_columns(&grid, 0, 10), None);
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

        // A target ending in a wide character ends on its spacer cell.
        let mut grid: Grid<Cell> = Grid::new(1, 6, 0);
        for (column, ch) in [(0, '/'), (1, 'a'), (2, '/'), (3, '用')] {
            grid[Point::new(Line(0), Column(column))].c = ch;
        }
        grid[Point::new(Line(0), Column(3))]
            .flags
            .insert(Flags::WIDE_CHAR);
        grid[Point::new(Line(0), Column(4))]
            .flags
            .insert(Flags::WIDE_CHAR_SPACER);
        let hints = detect_rows(&grid, &[0], 6);
        assert_eq!(hints.len(), 1);
        assert_eq!((hints[0].start, hints[0].end), (0, 4));
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
            ("?? \"docs/annual report.pdf", "docs/annual"),
            ("./docs/it's.md", "./docs/it"),
            ("out/report(1).pdf", "out/report"),
        ] {
            assert_eq!(span(line), Some((text.to_string(), false)), "{line:?}");
        }
    }

    /// A path between matching quotes or backticks is whole, spaces and all,
    /// and opens; quoted prose, a missing or mismatched quote, or one that
    /// does not end the token leaves the bare match.
    #[test]
    fn a_quoted_path_is_detected_whole() {
        let paths = |line: &str| {
            detect(&[line])
                .into_iter()
                .filter(|span| span.kind == Kind::Path)
                .map(|span| (span.text, span.bounded))
                .collect::<Vec<_>>()
        };
        let whole = |text: &str| vec![(text.to_string(), true)];
        for (line, text) in [
            ("?? \"docs/annual report.pdf\"", "docs/annual report.pdf"),
            (
                "  File \"/Users/me/my app/main.py\", line 12, in <module>",
                "/Users/me/my app/main.py",
            ),
            ("see `out/report #1.pdf`.", "out/report #1.pdf"),
            ("'docs/Q3 (final).pdf'", "docs/Q3 (final).pdf"),
            ("\"./docs/it's here.md\"", "./docs/it's here.md"),
            ("\"src/my file.rs:12\" warning", "src/my file.rs"),
            ("--out=\"out/my a.png\"", "out/my a.png"),
            ("(\"docs/a b.md\")", "docs/a b.md"),
            (
                "\"~/Library/Application Support\"",
                "~/Library/Application Support",
            ),
            ("\"/Volumes/My Disk/\"", "/Volumes/My Disk/"),
            ("\"café/menú del día.png\"", "café/menú del día.png"),
            (
                "\"docs/cafe\u{301} report.pdf\"",
                "docs/cafe\u{301} report.pdf",
            ),
            (
                "\"/tmp/my cafe\u{301}/menu\u{301}\"",
                "/tmp/my cafe\u{301}/menu\u{301}",
            ),
        ] {
            assert_eq!(paths(line), whole(text), "{line:?}");
        }
        // A drive path is whole on Windows. Elsewhere a one-letter prefix is
        // a remote host (`scp "h:/srv/a b.pdf" .`), so it stays partial.
        let drive = "\"C:\\Program Files\\Kettle\\kettle.toml\"";
        #[cfg(windows)]
        assert_eq!(
            paths(drive),
            whole("C:\\Program Files\\Kettle\\kettle.toml")
        );
        #[cfg(not(windows))]
        {
            assert!(paths(drive).iter().all(|(text, _)| !text.contains(' ')));
            assert_eq!(
                paths("scp \"h:/srv/my report.pdf\" ."),
                [("h:/srv/my".to_string(), false)]
            );
        }
        assert_eq!(
            paths("cp \"a/b c.txt\" 'd/e f.txt'"),
            [
                ("a/b c.txt".to_string(), true),
                ("d/e f.txt".to_string(), true)
            ]
        );
        // A relative path after a space inside quotes may be a piece of a
        // quoted name holding a space, which quoted prose cannot be told
        // from, so it is copied; a quoted name that only reads as a path in
        // part is left to the bare pattern.
        for (line, bare) in [
            ("\"see src/main.rs\"", Some(("src/main.rs", false))),
            (
                "\"Please inspect out/a.png,\" she said.",
                Some(("out/a.png", false)),
            ),
            (
                "\"cannot open src/main.rs now\"",
                Some(("src/main.rs", false)),
            ),
            (
                "msg=\"failed to read config/app.toml\"",
                Some(("config/app.toml", false)),
            ),
            (
                "msg=\"failed to read /etc/app.toml\"",
                Some(("/etc/app.toml", true)),
            ),
            ("\"/model sonnet\"", Some(("/model", false))),
            ("\"and/or more words\"", Some(("and/or", false))),
            ("'My Files/a b.txt'", Some(("Files/a", false))),
            (
                "cat \"My Files/a.pdf b.txt\".bak",
                Some(("Files/a.pdf", false)),
            ),
            ("cat \"My Files/a.pdf b.txt", Some(("Files/a.pdf", false))),
            ("\" docs/a b.pdf\"", Some(("docs/a", false))),
            ("\"docs/a b.pdf \"", Some(("docs/a", false))),
            ("\"docs//a b.pdf\"", None),
            ("\"docs/a\\ b.pdf\"", Some(("docs/a", false))),
            ("\"docs/what is this?\"", Some(("docs/what", false))),
            ("\"./\"", None),
            // Unclosed, mismatched, glued to the next token, opened mid-token.
            ("\"docs/annual report.pdf", Some(("docs/annual", false))),
            ("\"docs/annual report.pdf'", Some(("docs/annual", false))),
            ("\"docs/annual report.pdf\"x", Some(("docs/annual", false))),
            ("x\"docs/annual report.pdf\"", Some(("docs/annual", false))),
        ] {
            let expected = bare
                .map(|(text, bounded)| vec![(text.to_string(), bounded)])
                .unwrap_or_default();
            assert_eq!(paths(line), expected, "{line:?}");
        }
        // A quoted network stays an address.
        assert!(paths("route \"10.0.0.1/24/\" via eth0").is_empty());
        assert!(
            detect(&["route \"10.0.0.1/24/\" via eth0"])
                .iter()
                .any(|span| span.kind == Kind::Ip && span.text == "10.0.0.1")
        );
        // A quoted URL stays a URL.
        assert!(paths("\"https://x.test/a b.pdf\"").is_empty());
        // The scan is bounded: an overlong quoted name is not one path.
        let long = format!("\"docs/{} b.pdf\"", "a".repeat(1100));
        assert!(paths(&long).iter().all(|(text, _)| !text.contains(' ')));
        // The bound may fall inside a character.
        for pad in 0..4 {
            let wide = format!("\"{}docs/{} b.pdf\"", "a".repeat(pad), "é".repeat(600));
            assert!(paths(&wide).iter().all(|(text, _)| !text.contains(' ')));
        }
    }

    /// A path holding a character that hides text reads as another name, so
    /// its hint copies rather than opens, quoted or bare.
    #[test]
    fn a_path_hiding_a_character_is_not_bounded() {
        for line in [
            "open \"docs/my a\u{200d}b.pdf\" now",
            "open docs/a\u{200c}b.pdf now",
            "open /tmp/a\u{fe0f}b.pdf now",
            "open /tmp/a\u{3164}b.pdf now",
        ] {
            let spans = detect(&[line]);
            let paths = spans
                .iter()
                .filter(|span| span.kind == Kind::Path)
                .collect::<Vec<_>>();
            assert!(!paths.is_empty(), "{line}: {spans:?}");
            assert!(paths.iter().all(|span| !span.bounded), "{line}: {spans:?}");
        }
        let spans = detect(&["open \"docs/my café.pdf\" now"]);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].bounded);
        // A quoted name holding such a character leaves no piece whole.
        let spans = detect(&["\"see\u{200d}src/main.rs\""]);
        assert!(!spans.is_empty());
        assert!(spans.iter().all(|span| !span.bounded), "{spans:?}");
        // A visible space is a space: the quoted name is one path, whole.
        for (line, text) in [
            (
                "open \"docs/old\u{a0}dir/x.txt\" now",
                "docs/old\u{a0}dir/x.txt",
            ),
            (
                "open \"docs/old\u{2007}dir/.env\" now",
                "docs/old\u{2007}dir/.env",
            ),
        ] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert_eq!(
                (spans[0].text.as_str(), spans[0].bounded),
                (text, true),
                "{line}"
            );
        }
        // A path naming a parent once decoded opens as nothing else either,
        // as a clicked link refuses it.
        for line in [
            "x \"docs/%2e%2e/my report.pdf\" y",
            "x \"docs/.%2E/my report.swiftinterface\" y",
        ] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert!(!spans[0].bounded, "{line}");
        }
        let spans = detect(&["x \"docs/100%2e/my report.pdf\" y"]);
        assert!(spans[0].bounded);
        // A name naming a parent once decoded in another word leaves a later
        // path whole; in the same word, or with a hidden character anywhere
        // in the quoted text, it is copied.
        let spans = detect(&["\"Compare docs/%2e%2e/old.txt with /tmp/report.pdf\""]);
        assert!(
            spans
                .iter()
                .any(|span| span.text == "/tmp/report.pdf" && span.bounded),
            "{spans:?}"
        );
        for line in [
            "\"docs/%2e%2e/a?=/tmp/report.pdf\"",
            "\"Compare docs/a\u{200b}b.txt with /tmp/report.pdf\"",
        ] {
            let spans = detect(&[line]);
            assert!(spans.iter().all(|span| !span.bounded), "{line}: {spans:?}");
        }
        // Without one, a path from a root ending at the quote is whole; a
        // no-break space is a space.
        for line in ["\"see /tmp/main.rs\"", "\"see\u{a0}/tmp/main.rs\""] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert!(spans[0].bounded, "{line}");
        }
    }

    /// A dotfile's name is its extension, so a quoted path ending in one is
    /// whole, not the bare piece after its space (`dir/.env`, which names
    /// another file).
    #[test]
    fn a_quoted_path_ending_in_a_dotfile_is_whole() {
        for path in [
            "docs/my dir/.env",
            "docs/annual report/.gitignore",
            "docs/my dir/.bashrc",
            "docs/my dir/.env.local",
        ] {
            let spans = detect(&[&format!("open \"{path}\" now")]);
            assert_eq!(spans.len(), 1, "{path}: {spans:?}");
            assert_eq!(spans[0].text, path);
            assert!(spans[0].bounded, "{path}");
        }
        // A one-letter dotfile in a one-letter folder is a path too.
        let spans = detect(&["see x/.a ok"]);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(
            (spans[0].kind, spans[0].text.as_str()),
            (Kind::Path, "x/.a")
        );
        // Still no extension: these stay partial, as before.
        for line in [
            "open \"docs/my dir/Makefile\" now",
            "open \"docs/my dir/.\" now",
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| !span.text.contains(' ')),
                "{line}: {spans:?}"
            );
        }
    }

    /// A quoted path's extension may be long or hyphenated, as real ones
    /// are (`.swiftinterface`, `.code-workspace`).
    #[test]
    fn a_quoted_path_with_a_long_extension_is_whole() {
        for path in [
            "docs/annual report.swiftinterface",
            "docs/annual report.documentation",
            "a/b/annual report.swiftinterface",
            "docs/my app.code-workspace",
            "docs/my app.sublime-project",
            "docs/my dir/.DS_Store",
        ] {
            let spans = detect(&[&format!("\"{path}\"")]);
            assert_eq!(spans.len(), 1, "{path}: {spans:?}");
            assert_eq!(spans[0].text, path);
            assert!(spans[0].bounded, "{path}");
        }
        // Not an extension: an empty word, or punctuation ending prose.
        for body in [
            "docs/a b.-x",
            "docs/a b.x--y",
            "docs/a b.x-",
            "docs/see the end.",
        ] {
            let spans = detect(&[&format!("\"{body}\"")]);
            assert!(
                spans.iter().all(|span| !span.text.contains(' ')),
                "{body}: {spans:?}"
            );
        }
    }

    /// Inside quotes, every visible space is a space: a quoted path holding
    /// a no-break or narrow no-break space reads as it does with an ASCII
    /// one, as a quoted macOS screenshot name needs.
    #[test]
    fn a_visible_space_in_a_quoted_path_is_a_space() {
        let spaces = [
            '\u{a0}', '\u{1680}', '\u{2000}', '\u{2007}', '\u{200a}', '\u{202f}', '\u{205f}',
            '\u{3000}',
        ];
        let read = |line: &str| {
            detect(&[line])
                .into_iter()
                .map(|span| (span.kind, span.text, span.bounded))
                .collect::<Vec<_>>()
        };
        // The line with `c` for each space inside its quotes.
        let inside = |line: &str, c: char| {
            let (open, close) = (line.find('"').unwrap(), line.rfind('"').unwrap());
            let quoted = line[open..close].replace(' ', &c.to_string());
            format!("{}{quoted}{}", &line[..open], &line[close..])
        };
        for c in spaces {
            for line in [
                "open \"/tmp/a b.png\" now",
                "\"docs/a b=/tmp/report.pdf\"",
                "open \"docs/my dir/a/b\" now",
                "open \"/tmp/my dir/report.pdf.\" now",
            ] {
                let visible = inside(line, c);
                let ascii = read(line);
                let spans = read(&visible)
                    .into_iter()
                    .map(|(kind, text, bounded)| (kind, text.replace(c, " "), bounded))
                    .collect::<Vec<_>>();
                assert_eq!(spans, ascii, "U+{:04X} in {visible:?}", u32::from(c));
            }
        }
        let shot = "open \"/Users/me/Desktop/Screenshot 2026-11-26 at 10.00.00\u{202f}AM.png\"";
        assert_eq!(
            read(shot),
            [(
                Kind::Path,
                "/Users/me/Desktop/Screenshot 2026-11-26 at 10.00.00\u{202f}AM.png".to_string(),
                true
            )]
        );
    }

    /// Quoted prose that starts with a path keeps a later path from a root
    /// whole; only a relative piece can continue the quoted name.
    #[test]
    fn quoted_prose_starting_with_a_path_keeps_a_rooted_one_whole() {
        for (line, path) in [
            (
                "msg=\"./build.sh failed, see /tmp/build.log\"",
                "/tmp/build.log",
            ),
            ("\"src/a.rs uses /tmp/b.txt\"", "/tmp/b.txt"),
            ("\"src/a.rs: cannot open /tmp/b.txt\"", "/tmp/b.txt"),
            ("{\"msg\":\"./a.sh failed, see /tmp/b.log\"}", "/tmp/b.log"),
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().any(|span| span.text == path && span.bounded),
                "{line}: {spans:?}"
            );
        }
        let spans = detect(&["open \"docs/my dir/a/b\" now"]);
        assert!(spans.iter().all(|span| !span.bounded), "{spans:?}");
    }

    /// A backslash-escaped quote is text inside its string, as JSON and a
    /// shell's double quotes read it: it neither closes the string nor
    /// makes the rest of it a path of its own. A string that does close
    /// still leaves a later value's path whole.
    #[test]
    fn an_escaped_quote_stays_inside_its_string() {
        for line in [
            r#"{"file":"docs/a\"b=/tmp/report.pdf"}"#,
            r#""docs/a\"b=/tmp/report.pdf""#,
            r#"x="docs/a\"b=/tmp/report.pdf""#,
            r#"{"file":"docs/a\\\"b=/tmp/report.pdf"}"#,
        ] {
            let spans = detect(&[line]);
            assert!(
                spans
                    .iter()
                    .all(|span| !(span.text == "/tmp/report.pdf" && span.bounded)),
                "{line}: {spans:?}"
            );
        }
        for line in [
            r#"{"msg":"say \"hi\"","file":"/tmp/c.txt"}"#,
            r#"{"msg":"a\\","file":"/tmp/c.txt"}"#,
        ] {
            let spans = detect(&[line]);
            assert!(
                spans
                    .iter()
                    .any(|span| span.text == "/tmp/c.txt" && span.bounded),
                "{line}: {spans:?}"
            );
        }
    }

    /// Inside a shell's single quotes a backslash is a backslash: the next
    /// `'` closes the string whatever comes before it, so a later path on
    /// the line is its own.
    #[test]
    fn a_single_quote_closes_after_a_backslash() {
        for line in [
            r#"export A='C:\work\';B=/tmp/c.txt"#,
            r#"x='a\';y=/tmp/c.txt"#,
            r#"x='a\\';y=/tmp/c.txt"#,
        ] {
            let spans = detect(&[line]);
            assert!(
                spans
                    .iter()
                    .any(|span| span.text == "/tmp/c.txt" && span.bounded),
                "{line}: {spans:?}"
            );
        }
        // A double quote is still escaped by one.
        let spans = detect(&[r#"{"file":"docs/a\"b=/tmp/report.pdf"}"#]);
        assert!(spans.iter().all(|span| !span.bounded), "{spans:?}");
    }

    /// A `\'` inside single quotes closes them only where a shell token
    /// boundary follows, as a shell's literal backslash does; glued to more
    /// text it is an escape, as in JavaScript or Python, and the string goes
    /// on, so a path after it is a piece of the string. Outside any string an
    /// escaped quote is a plain character and opens none.
    #[test]
    fn an_escaped_single_quote_closes_only_at_a_shell_boundary() {
        for line in [
            r#"{'file':'docs/a\'b=/tmp/report.pdf'}"#,
            r#"x='docs/a\'b=/tmp/report.pdf'"#,
            r#"x=`docs/a\`b=/tmp/report.pdf`"#,
            r#"x='a\'/tmp/report.pdf'"#,
        ] {
            let spans = detect(&[line]);
            assert!(
                spans
                    .iter()
                    .all(|span| !(span.text == "/tmp/report.pdf" && span.bounded)),
                "{line}: {spans:?}"
            );
        }
        for (line, path) in [
            (r#"export A='C:\work\';B=/tmp/c.txt"#, "/tmp/c.txt"),
            (r#"x='C:\work\'&&cat=/tmp/c.txt"#, "/tmp/c.txt"),
            (r#"{'msg':'it\'s','file':'/tmp/c.txt'}"#, "/tmp/c.txt"),
            (r#"x=\'a/b;y=/tmp/x.log"#, "/tmp/x.log"),
            (r#"x=\`a/b;y=/tmp/x.log"#, "/tmp/x.log"),
            (r#"x=\"a/b;y=/tmp/x.log"#, "/tmp/x.log"),
            (r#"echo \`date\` /tmp/x.log"#, "/tmp/x.log"),
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().any(|span| span.text == path && span.bounded),
                "{line}: {spans:?}"
            );
        }
    }

    /// An apostrophe inside a word opens no quoted string, so the tail of a
    /// quoted JSON name after it is no openable path.
    #[test]
    fn an_apostrophe_in_a_word_opens_no_quote() {
        for line in [
            r#"can't open {"file":"/Users/me/my app/x.py"}"#,
            r#"it's in {"file":"/Users/me/my app/x.py"}"#,
        ] {
            let spans = detect(&[line]);
            assert!(
                spans
                    .iter()
                    .all(|span| span.text != "app/x.py" || !span.bounded),
                "{line}: {spans:?}"
            );
        }
    }

    /// A relative path with a quote after it in its word, one after a quote
    /// no quote closes, or a piece of a string, is copied; a path from a
    /// root that starts its word still opens, and one after a closed string
    /// is its own.
    #[test]
    fn an_uncertain_quote_leaves_only_rooted_tokens_openable() {
        for (line, path) in [
            (r#"a "b "c/d x/y.txt" e"#, "x/y.txt"),
            (r#"it 'oops {"file":"docs/my dir/a/b"}"#, "dir/a/b"),
            (r#"x="a\"b c/d.txt""#, "c/d.txt"),
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| span.text != path || !span.bounded),
                "{line}: {spans:?}"
            );
        }
        for (line, path) in [
            (r#"x='a"b /tmp/x.log'"#, "/tmp/x.log"),
            (r#"echo "a b" c/d.txt "x""#, "c/d.txt"),
            (r#"see src/x.rs, it's fine"#, "src/x.rs"),
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().any(|span| span.text == path && span.bounded),
                "{line}: {spans:?}"
            );
        }
    }

    /// Compact JSON closes each string before the next opens, so a later
    /// value's path is its own token, whatever an earlier value held.
    #[test]
    fn a_compact_json_path_is_whole_after_any_earlier_value() {
        for (line, path) in [
            (
                "{\"cwd\":\"/work\",\"file\":\"/Users/me/x.py\"}",
                "/Users/me/x.py",
            ),
            (
                "{\"url\":\"https://x.test/a\",\"path\":\"/tmp/c.txt\"}",
                "/tmp/c.txt",
            ),
            (
                "{\"type\":\"application/json\",\"file\":\"/tmp/c.txt\"}",
                "/tmp/c.txt",
            ),
            (
                "{\"date\":\"2026/11/26\",\"file\":\"/tmp/c.txt\"}",
                "/tmp/c.txt",
            ),
            (
                "{\"msg\":\"it's a/b\",\"file\":\"/tmp/c.txt\"}",
                "/tmp/c.txt",
            ),
            ("key=\"a/b\";other=/tmp/c.txt", "/tmp/c.txt"),
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().any(|span| span.text == path && span.bounded),
                "{line}: {spans:?}"
            );
        }
    }

    /// Quotes around one path-shaped token the quoted pattern cannot read
    /// whole leave no whole piece of it: its end is not a file of its own.
    /// A path after a space in quoted prose, or unquoted, stays whole.
    #[test]
    fn the_end_of_a_quoted_path_token_is_not_whole() {
        for line in [
            "\"docs/\u{b2}www.example.com.txt#file=/tmp/report.pdf\"",
            "\"docs/a*b=/tmp/report.pdf\"",
            "\"docs/%2e%2e/a?=/tmp/report.pdf\"",
            // A string after a colon is no quoted path, but still one token.
            "host:\"docs/a*b=/tmp/report.pdf\"",
            "host:\"docs/a'b*=/tmp/report.pdf\"",
            "x='docs/a*b=/tmp/report.pdf'",
        ] {
            let spans = detect(&[line]);
            assert!(spans.iter().all(|span| !span.bounded), "{line}: {spans:?}");
        }
        // A quoted path with a space the pattern refused leaves no whole
        // piece either.
        for line in [
            "open \"docs/my dir/notes...\" now",
            "open \"docs/my dir/Makefile\" now",
        ] {
            let spans = detect(&[line]);
            assert!(spans.iter().all(|span| !span.bounded), "{line}: {spans:?}");
        }
        for (line, text) in [
            ("\"see /tmp/report.pdf\"", "/tmp/report.pdf"),
            ("file=/tmp/report.pdf", "/tmp/report.pdf"),
            (
                "\"Compare a/b*c.txt with /tmp/report.pdf\"",
                "/tmp/report.pdf",
            ),
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().any(|span| span.text == text && span.bounded),
                "{line}: {spans:?}"
            );
        }
    }

    /// A quote after a `:` never opens a quoted path: `host:"…"` and
    /// `'user@host':"…"` name remote files, which stay partial matches that
    /// copy. Pretty-printed JSON (`"file": "…"`) and arrays still take each
    /// quoted path whole; compact JSON's `"file":"…"` reads like a remote
    /// path and does not.
    #[test]
    fn a_quoted_remote_path_is_not_whole() {
        let paths = |line: &str| {
            detect(&[line])
                .into_iter()
                .filter(|span| span.kind == Kind::Path)
                .map(|span| (span.text, span.bounded))
                .collect::<Vec<_>>()
        };
        for line in [
            "scp user@host:\"/srv/my report.pdf\" .",
            "scp 'user@host':\"/srv/my report.pdf\" .",
            "rsync -a \"host\":\"/srv/my report.pdf\" .",
            // An escaped space continues the remote word before the quote.
            "scp user@host:dir\\ \"/srv/my report.pdf\" .",
            // A quote inside a quoted command belongs to that command, also
            // when the command's closing quote is out of reach.
            "ssh host \"cat '/srv/my report.pdf'; true\"&&echo done",
        ] {
            assert_eq!(paths(line), [("/srv/my".to_string(), false)], "{line}");
        }
        let unclosed = format!(
            "ssh host \"cat '/srv/my report.pdf'; true #{}\"&&echo done",
            "x".repeat(1100)
        );
        assert_eq!(paths(&unclosed), [("/srv/my".to_string(), false)]);
        assert_eq!(
            paths("{\"file\": \"/Users/me/my app/x.py\", \"line\": 3}"),
            [("/Users/me/my app/x.py".to_string(), true)]
        );
        assert_eq!(
            paths("[\"a/b c.txt\",\"d/e f.txt\"]"),
            [
                ("a/b c.txt".to_string(), true),
                ("d/e f.txt".to_string(), true)
            ]
        );
        // A quoted path with a space after a colon is one token as well, so
        // its last piece is no file of its own.
        assert_eq!(
            paths("{\"file\":\"/Users/me/my app/x.py\"}"),
            [
                ("/Users/me/my".to_string(), false),
                ("app/x.py".to_string(), false)
            ]
        );
    }

    /// Taken ranges answer overlap by binary search: touching ends do not
    /// overlap, and order of insertion does not matter.
    #[test]
    fn taken_ranges_overlap_only_when_they_share_a_position() {
        let mut taken = Taken::default();
        taken.insert(10, 20);
        taken.insert(0, 5);
        taken.insert(30, 31);
        for (start, end, overlaps) in [
            (5, 10, false),
            (20, 30, false),
            (4, 6, true),
            (19, 21, true),
            (12, 13, true),
            (0, 100, true),
            (30, 31, true),
            (31, 40, false),
        ] {
            assert_eq!(taken.overlaps(start, end), overlaps, "{start}..{end}");
        }
    }

    /// Filling takes only the gaps around ranges already taken.
    #[test]
    fn filling_takes_only_what_is_free() {
        let mut taken = Taken::default();
        taken.insert(0, 4);
        taken.insert(6, 8);
        taken.insert(20, 22);
        taken.fill(2, 12);
        let ranges = taken.0.iter().map(|(&s, &e)| (s, e)).collect::<Vec<_>>();
        assert_eq!(ranges, [(0, 4), (4, 6), (6, 8), (8, 12), (20, 22)]);
        taken.fill(0, 30);
        assert!((0..30).all(|at| taken.overlaps(at, at + 1)));
        taken.fill(5, 7);
        assert_eq!(taken.0.len(), 7);
    }

    /// Checking a line's matches stays linear however it repeats quotes and
    /// punctuation: the boundary check reads a bounded distance ahead.
    #[test]
    fn boundary_checks_stay_linear() {
        assert!(ends_a_match(&";".repeat(MAX_TRAILING)));
        assert!(!ends_a_match(&";".repeat(MAX_TRAILING + 1)));
        assert!(ends_a_match(&format!("{} x", ";".repeat(MAX_TRAILING - 1))));
        // Quadratic scanning took seconds here (2.8 s at 40,000 bytes in a
        // debug build); linear takes milliseconds.
        let line = ";\"\"".repeat(20_000);
        let started = std::time::Instant::now();
        assert!(detect(&[line.as_str()]).is_empty());
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
    }

    /// A quoted path the terminal wrapped is one hint with its whole text.
    #[test]
    fn a_soft_wrapped_quoted_path_is_one_hint() {
        use alacritty_terminal::grid::Grid;
        use alacritty_terminal::index::{Column, Line, Point};
        use alacritty_terminal::term::cell::{Cell, Flags};

        let mut grid: Grid<Cell> = Grid::new(2, 12, 0);
        for (line, text) in ["see \"docs/an", "nual a.pdf\""].iter().enumerate() {
            for (column, c) in text.chars().enumerate() {
                grid[Point::new(Line(line as i32), Column(column))].c = c;
            }
        }
        grid[Point::new(Line(0), Column(11))]
            .flags
            .insert(Flags::WRAPLINE);
        let spans = detect_rows(&grid, &[0, 1], 12);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].text, "docs/annual a.pdf");
        assert_eq!((spans[0].row, spans[0].start, spans[0].end), (0, 5, 11));
        assert!(spans[0].bounded);
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

    /// An uppercase or mixed-case scheme is a URL hint with its text as
    /// printed, not a path.
    #[test]
    fn uppercase_schemes_are_url_hints() {
        for url in [
            "HTTPS://example.com/a",
            "Http://example.com/a",
            "FTP://host/file",
            "FILE:///etc/hosts",
            "WWW.example.com/a",
        ] {
            let spans = detect(&[&format!("see {url} now")]);
            assert_eq!(spans.len(), 1, "{url}: {spans:?}");
            assert_eq!((spans[0].kind, spans[0].text.as_str()), (Kind::Url, url));
        }
        // A `www.` inside a longer name is part of it, in any case.
        for path in ["docs/WWW.example.com.txt", "docs/www.example.com.txt"] {
            let spans = detect(&[&format!("cat {path}")]);
            assert_eq!(spans.len(), 1, "{path}: {spans:?}");
            assert_eq!((spans[0].kind, spans[0].text.as_str()), (Kind::Path, path));
        }
        for line in ["docs/\u{b2}www.example.com.txt", "x\u{2167}www.example.com"] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| span.kind != Kind::Url),
                "{line}: {spans:?}"
            );
        }
        // A scheme matches in ASCII case only: `ſ` is no `s` here, and no
        // piece of the text shaped like a URL is a path.
        for line in [
            "see httpſ://example.test/ok",
            "see httpſ://example.test/?file=/tmp/report.pdf",
            "see vscode://open?file=/tmp/report.pdf",
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| span.kind != Kind::Url),
                "{line}: {spans:?}"
            );
            assert!(spans.iter().all(|span| !span.bounded), "{line}: {spans:?}");
        }
        // A control before `www.` starts no URL either.
        let spans = detect(&["docs/a\u{85}WWW.example.test/path.txt"]);
        assert!(spans.iter().all(|span| span.kind != Kind::Url), "{spans:?}");
        // An address in such text is still a hint to copy.
        assert!(
            detect(&["ssh://10.0.0.1/x"])
                .iter()
                .any(|span| span.kind == Kind::Ip && span.text == "10.0.0.1")
        );
        // Nor does an invisible character end the word before a URL.
        for line in [
            "file docs/a\u{200b}WWW.example.test/path.txt",
            "see a\u{2060}https://x.test/p.txt",
        ] {
            let spans = detect(&[line]);
            assert!(
                spans.iter().all(|span| span.kind != Kind::Url),
                "{line}: {spans:?}"
            );
            assert!(spans.iter().all(|span| !span.bounded), "{line}: {spans:?}");
        }
        // A scheme after a refused `www.` still starts a URL.
        let spans = detect(&["docs/www.cfg=HTTPS://example.com/a"]);
        assert!(
            spans
                .iter()
                .any(|span| span.kind == Kind::Url && span.text == "HTTPS://example.com/a"),
            "{spans:?}"
        );
        // One pass, however many `www.` a token runs into.
        let line = "xwww.".repeat(20_000);
        let started = std::time::Instant::now();
        assert!(
            detect(&[line.as_str()])
                .iter()
                .all(|span| span.kind != Kind::Url)
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
    }

    /// A URL trimmed down to a bare scheme or `www` names nothing: no hint
    /// opens it, and no piece of it becomes a path hint.
    #[test]
    fn a_bare_scheme_is_no_url_hint() {
        for line in [
            "see www...",
            "see www.,",
            "see www./",
            "see https://...",
            "see https:///",
            "see https://?q=1",
            "see https://#frag",
            "see https://:8080/x",
            "see ftp://***",
            "see file://...",
            "see https://?q=/etc/hosts",
            "see file://?q=/etc/hosts",
            "see file://localhost?q=/etc/hosts",
            "see www.example@/etc/hosts",
        ] {
            let spans = detect(&[line]);
            assert!(spans.is_empty(), "{line}: {spans:?}");
        }
        let spans = detect(&["see https://a"]);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            (spans[0].kind, spans[0].text.as_str()),
            (Kind::Url, "https://a")
        );
    }

    /// A URL in Markdown code or emphasis is detected without the marks.
    #[test]
    fn markdown_marks_around_a_url_are_not_part_of_it() {
        for line in [
            "see `https://x.test/a/b` here",
            "**https://x.test/a/b** is the page",
            "*https://x.test/a/b*.",
        ] {
            let spans = detect(&[line]);
            assert_eq!(spans.len(), 1, "{line}: {spans:?}");
            assert_eq!(spans[0].text, "https://x.test/a/b", "{line}");
        }
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

        // Just past the alphabet keeps easy single keys; only the tail grows.
        let n = abc.len() + 2;
        let many = labels(n, abc);
        assert_eq!(many.iter().filter(|l| l.len() == 1).count(), 25);
        assert_eq!(many[0], "a");
        assert_eq!(many[1], "b");
        let uniq: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(uniq.len(), n, "all labels distinct");
        // Deterministic for a given n (labels are assigned once per scan).
        assert_eq!(labels(n, abc), labels(n, abc));
        // Degenerate alphabet → no labels (caller falls back).
        assert!(labels(5, "").is_empty());
        assert_eq!(labels(1, "x"), vec!["x"]);
        assert!(labels(3, "x").is_empty());
    }
}
