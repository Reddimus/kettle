//! The Mermaid source in text a user copied or selected from an agent's
//! reply. Agents show a diagram as text: Claude Code under a dim `mermaid`
//! label, Codex after a notice when it cannot draw it, both hard-wrapped to
//! the pane. A `/copy` puts the exact block on the clipboard, fenced or not;
//! a selection is the rows on screen. This takes back what the harness
//! added: fences, the label, the notice, a message bullet and the common
//! indentation, and, for a selection whose width is known, the hard wraps,
//! by inverting the harness's greedy word wrap. A row that could be a
//! wrapped one or a new statement alike is refused, never guessed: a wrong
//! join would render a different diagram. Agents indent a diagram's body
//! consistently, so a body row back at the header's indentation, among
//! indented ones, is taken for a wrap when the row above was long enough to
//! wrap it. A wrapper drops the spaces at a break, however many there were;
//! a join puts back one, and Mermaid draws a run of spaces as one.

use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

/// Most text a copy or a selection may hold.
pub const MAX_DIAGRAM_COPY_BYTES: usize = 1024 * 1024;
/// Most fenced diagrams one copy may hold: as many as a gallery shows
/// (`kettle_media::MAX_FENCES`). More are refused, never cut short.
pub const MAX_COPIED_DIAGRAMS: usize = 32;

/// Why copied text gave no diagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagramCopyError {
    /// Nothing but blank text.
    Empty,
    /// More than [`MAX_DIAGRAM_COPY_BYTES`].
    TooLarge,
    /// No Mermaid fence, and no row a diagram starts with.
    NoDiagram,
    /// The rows look hard-wrapped and could not be joined back for sure:
    /// the width they were wrapped at is unknown, or a row could be a
    /// wrapped one or a new statement alike.
    Wrapped,
    /// Text follows an unindented diagram, after a blank row, in a reply:
    /// that may be prose or more of the diagram.
    Unbounded,
    /// More Mermaid fences than [`MAX_COPIED_DIAGRAMS`].
    TooMany,
}

/// The notices Codex prints before a diagram it shows as text, as captured.
const NOTICES: [&str; 3] = [
    "This Mermaid diagram uses features the terminal renderer doesn't support.",
    "This Mermaid diagram exceeds the terminal renderer's size limits.",
    "This Mermaid diagram doesn't fit the current terminal width.",
];

/// The words a Mermaid diagram starts with: every type the renderer
/// detects, each whole, so `stateDiagram` also starts `stateDiagram-v2` and
/// `xychart` starts `xychart-beta`.
const HEADERS: [&str; 42] = [
    "flowchart",
    "graph",
    "sequenceDiagram",
    "stateDiagram",
    "classDiagram",
    "erDiagram",
    "gantt",
    "pie",
    "journey",
    "mindmap",
    "timeline",
    "gitGraph",
    "quadrantChart",
    "xychart",
    "block",
    "sankey",
    "requirementDiagram",
    "C4Context",
    "C4Container",
    "C4Component",
    "C4Dynamic",
    "C4Deployment",
    "kanban",
    "architecture",
    "packet",
    "info",
    "treemap",
    "radar-beta",
    "venn-beta",
    "zenuml",
    "eventmodeling",
    "ishikawa",
    "railroad-beta",
    "railroad-ebnf-beta",
    "railroad-abnf-beta",
    "railroad-peg-beta",
    "wardley-beta",
    "cynefin-beta",
    "usecase-beta",
    "agentflow-beta",
    "swimlane-beta",
    "treeView-beta",
];

/// The harness a reply came from, by its message bullet or what it adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Harness {
    Claude,
    Codex,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `row` starts a diagram: a header word, whole.
fn is_header(row: &str) -> bool {
    let row = row.trim();
    HEADERS.iter().any(|header| {
        row.strip_prefix(header)
            .is_some_and(|rest| rest.chars().next().is_none_or(|c| !word_char(c)))
    })
}

/// Leading spaces.
fn indent(row: &str) -> usize {
    row.len() - row.trim_start_matches(' ').len()
}

/// The bytes and columns of `line`'s leading spaces and tabs, a tab
/// reaching the next multiple of four columns, as CommonMark counts them.
fn leading_columns(line: &str) -> (usize, usize) {
    let mut columns = 0;
    for (at, c) in line.char_indices() {
        match c {
            ' ' => columns += 1,
            '\t' => columns += 4 - columns % 4,
            _ => return (at, columns),
        }
    }
    (line.len(), columns)
}

/// The Mermaid sources in copied or selected text. Each Mermaid fence is
/// one, verbatim. Otherwise the text is one diagram among what a harness
/// added; `width`, the pane's columns when the text was selected from it,
/// lets hard wraps be joined back, and without it rows that look wrapped
/// are refused.
pub fn diagram_sources(text: &str, width: Option<usize>) -> Result<Vec<String>, DiagramCopyError> {
    if text.len() > MAX_DIAGRAM_COPY_BYTES {
        return Err(DiagramCopyError::TooLarge);
    }
    if text.trim().is_empty() {
        return Err(DiagramCopyError::Empty);
    }
    let (fenced, covered) = fenced_sources(text);
    if fenced.len() > MAX_COPIED_DIAGRAMS {
        return Err(DiagramCopyError::TooMany);
    }
    if !fenced.is_empty() {
        return Ok(fenced);
    }
    if covered.is_empty() {
        return unfenced_source(text, width).map(|source| vec![source]);
    }
    // A fence of another language is that language's text: a diagram is
    // looked for only outside fences.
    let mut lines: Vec<&str> = text.split('\n').collect();
    for range in covered {
        lines[range].fill("");
    }
    match unfenced_source(&lines.join("\n"), width) {
        // The text had fences, just no diagram.
        Err(DiagramCopyError::Empty) => Err(DiagramCopyError::NoDiagram),
        found => found.map(|source| vec![source]),
    }
}

/// Every ```` ```mermaid ```` or `~~~mermaid` fence's body, verbatim, in
/// order, up to one past [`MAX_COPIED_DIAGRAMS`], and the lines every fence
/// covers, of any language. A fence's body is its own text, so a fence
/// inside one is not a fence. Unclosed, a fence runs to the end of the
/// text, as in CommonMark. Each line is looked at once.
fn fenced_sources(text: &str) -> (Vec<String>, Vec<std::ops::Range<usize>>) {
    let lines: Vec<&str> = text.split('\n').collect();
    let (mut sources, mut covered) = (Vec::new(), Vec::new());
    let mut at = 0;
    while at < lines.len() && sources.len() <= MAX_COPIED_DIAGRAMS {
        let Some((indentation, fence, mermaid)) = fence_open(lines[at]) else {
            at += 1;
            continue;
        };
        let closing =
            (at + 1..lines.len()).find(|&line| fence_close(lines[line], indentation, fence));
        let end = closing.unwrap_or(lines.len());
        let body = lines[at + 1..end].join("\n");
        if mermaid && !body.trim().is_empty() {
            sources.push(body);
        }
        covered.push(at..closing.map_or(lines.len(), |closing| closing + 1));
        at = end + 1;
    }
    (sources, covered)
}

/// A fence's opening line: three or more backticks or tildes, indented by
/// any spaces or tabs, as a fence nested in a list may be. Its indentation,
/// in columns, its fence, and whether its info string says `mermaid`.
fn fence_open(line: &str) -> Option<(usize, &str, bool)> {
    let (leading, columns) = leading_columns(line);
    let rest = &line[leading..];
    let mark = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = rest.len() - rest.trim_start_matches(mark).len();
    if length < 3 {
        return None;
    }
    let info = rest[length..].trim_start_matches([' ', '\t']);
    // A backtick fence's info string holds no backtick, as in CommonMark.
    if mark == '`' && info.contains('`') {
        return None;
    }
    let mermaid = info
        .strip_prefix("mermaid")
        .is_some_and(|after| after.chars().next().is_none_or(|c| !word_char(c)));
    Some((columns, &rest[..length], mermaid))
}

/// Whether `line` closes a fence opened `indentation` columns in with
/// `fence`, as CommonMark has it: indented at most three columns past the
/// opening, the same mark at least as many times, and nothing after it but
/// spaces.
fn fence_close(line: &str, indentation: usize, fence: &str) -> bool {
    let (leading, columns) = leading_columns(line);
    let Some(mark) = fence.chars().next() else {
        return false;
    };
    let rest = &line[leading..];
    let marks = rest.len() - rest.trim_start_matches(mark).len();
    columns <= indentation + 3
        && marks >= fence.len()
        && rest[marks..]
            .chars()
            .all(|c| c == ' ' || c == '\t' || c == '\r')
}

/// The one diagram in unfenced text, without what a harness added.
fn unfenced_source(text: &str, width: Option<usize>) -> Result<String, DiagramCopyError> {
    let mut rows: Vec<String> = text
        .split('\n')
        .map(|row| row.trim_end().to_owned())
        .collect();
    let first = rows
        .iter()
        .position(|row| !row.is_empty())
        .ok_or(DiagramCopyError::Empty)?;
    rows.drain(..first);
    let mut harness = None;
    for (bullet, by) in [("⏺ ", Harness::Claude), ("• ", Harness::Codex)] {
        if let Some(rest) = rows[0].strip_prefix(bullet) {
            rows[0] = format!("  {rest}");
            harness = Some(by);
            break;
        }
    }
    let (top, header, marked) = diagram_start(&rows)?.ok_or(DiagramCopyError::NoDiagram)?;
    harness = harness.or(marked);
    // A row at the left edge, past the first, says the text was not copied
    // from a reply on screen, whose rows all keep the message's
    // indentation, wrapped ones too: nothing in it is a harness's wrap.
    let at_edge = |rows: &[String]| rows.iter().any(|row| !row.is_empty() && indent(row) == 0);
    // Such text that starts with its diagram is the source, as from
    // `/copy`, to its end.
    let end = if top == 0 && at_edge(&rows[1..]) {
        rows.iter()
            .rposition(|row| !row.is_empty())
            .map_or(0, |last| last + 1)
    } else {
        diagram_end(&rows, header)?
    };
    rows.truncate(end);
    let rows = &rows[top..];
    let at_edge = at_edge(&rows[1..]);
    let common = rows
        .iter()
        .filter(|row| !row.is_empty())
        .map(|row| indent(row))
        .min()
        .unwrap_or(0);
    let rows: Vec<&str> = rows
        .iter()
        .map(|row| row.get(common..).unwrap_or(""))
        .collect();
    let (prelude, diagram) = rows.split_at(header - top);
    // The width the rows were wrapped at, or else the least they all fit in.
    let limit = match width {
        Some(width) => width.saturating_sub(common),
        None => rows.iter().map(|row| row.width()).max().unwrap_or(0),
    };
    let whole = harness != Some(Harness::Codex);
    // Could some wrapper, the harness's or either, have wrapped `row` off
    // the row above at that width: it would not have kept its first word.
    let could_wrap = |above: &str, row: &str| WrapCost::of(above, row.trim()).most(whole) > limit;
    // Front matter and directives are taken as they are: a wrap in one could
    // stand for a space or a line break in a string. One a wrap could have
    // made is refused, and so is a header a wrap could have made of the
    // prelude's last row: it would be that row's text.
    if !at_edge
        && rows[..=prelude.len()]
            .windows(2)
            .any(|pair| !pair[0].is_empty() && !pair[1].is_empty() && could_wrap(pair[0], pair[1]))
    {
        return Err(DiagramCopyError::Wrapped);
    }
    let diagram = match width {
        _ if at_edge => diagram.join("\n"),
        Some(width) => rejoin(diagram, width.saturating_sub(common), harness)?,
        None => {
            // Without the width, a row that could be a wrap, by its shape or
            // in an unindented body, is one unless no width the rows fit in
            // could have wrapped it.
            let body_min = body_indent(diagram);
            let wrapped = diagram.windows(2).any(|pair| {
                let (above, row) = (pair[0], pair[1]);
                !above.is_empty()
                    && !row.is_empty()
                    && (body_min == 0 || indent(row) < body_min)
                    && could_wrap(above, row)
            });
            if wrapped {
                return Err(DiagramCopyError::Wrapped);
            }
            diagram.join("\n")
        }
    };
    if prelude.is_empty() {
        return Ok(diagram);
    }
    Ok(format!("{}\n{diagram}", prelude.join("\n")))
}

/// Where the diagram among `rows` starts: its top row, its header row, and
/// whose reply what is directly above it says it is. That is the first
/// header that starts its paragraph or follows Claude's `mermaid` label or
/// a Codex notice, else, as in prose run into a diagram, the first header
/// at all; with the comments, directives and front matter directly above
/// it. A notice or label further down is the diagram's own text.
fn diagram_start(
    rows: &[String],
) -> Result<Option<(usize, usize, Option<Harness>)>, DiagramCopyError> {
    let preludes = Preludes::of(rows);
    let mut first = None;
    // Headers under one run of prelude rows share its top and marker.
    let mut seen: Option<(usize, Option<Harness>)> = None;
    for (header, row) in rows.iter().enumerate() {
        if preludes.inner[header] || !is_header(row) {
            continue;
        }
        let (start, top) = preludes.run_above(header);
        if start > 0 && preludes.refused[start - 1] {
            return Err(DiagramCopyError::Wrapped);
        }
        let marked = match seen {
            Some((at, marked)) if at == start => marked,
            _ => marker_above(rows, top),
        };
        seen = Some((start, marked));
        if top == 0 || rows[top - 1].is_empty() || marked.is_some() {
            return Ok(Some((top, header, marked)));
        }
        first.get_or_insert((top, header, None));
    }
    Ok(first)
}

/// What Mermaid reads before a header, found in one pass over the rows, so
/// that each header's look above it costs no more than a lookup.
struct Preludes {
    /// For each row Mermaid could read before a header (a blank row, a
    /// comment, a directive's rows, front matter's): where its run starts,
    /// and the first of its rows that is not blank, if any.
    runs: Vec<Option<(usize, Option<usize>)>>,
    /// The closing rules of what stands where front matter would but holds
    /// rows that are not YAML, as wrapped ones would not be: it cannot be
    /// told from prose between two rules, and is refused rather than
    /// dropped.
    refused: Vec<bool>,
    /// The rows inside front matter or a directive, which are their text:
    /// a header word there starts no diagram.
    inner: Vec<bool>,
}

/// Whether `text`, a trimmed row, closes a directive: at its first `}%%`,
/// with nothing after that but a comment, which Mermaid drops once the
/// directive is gone.
fn closes_directive(text: &str) -> bool {
    text.find("}%%").is_some_and(|at| {
        let rest = text[at + 3..].trim_start();
        rest.is_empty() || rest.starts_with("%%")
    })
}

impl Preludes {
    fn of(rows: &[String]) -> Self {
        let mut marked = vec![false; rows.len()];
        let mut refused = vec![false; rows.len()];
        let mut inner = vec![false; rows.len()];
        // Where each kind of closer is, in order, so a block's end is found
        // by a search and no row is scanned twice.
        let mut directive_ends = Vec::new();
        let mut rules: std::collections::HashMap<usize, Vec<usize>> = Default::default();
        for (at, row) in rows.iter().enumerate() {
            let text = row.trim();
            if closes_directive(text) {
                directive_ends.push(at);
            }
            if text == "---" {
                rules.entry(indent(row)).or_default().push(at);
            }
        }
        // Rows are asked about in order, so a cursor into each list only
        // moves forward and passes each entry once.
        let next = |list: &[usize], cursor: &mut usize, after: usize| {
            while list.get(*cursor).is_some_and(|&row| row <= after) {
                *cursor += 1;
            }
            list.get(*cursor).copied()
        };
        let mut directive_cursor = 0;
        let mut rule_cursors: std::collections::HashMap<usize, usize> = Default::default();
        let mut at = 0;
        while at < rows.len() {
            let row = &rows[at];
            let text = row.trim();
            if text.starts_with("%%{") {
                // A directive runs to the first row that closes it, blank
                // rows and all, as Mermaid reads it. Unclosed, it is none.
                let end = if closes_directive(text) {
                    Some(at)
                } else {
                    next(&directive_ends, &mut directive_cursor, at)
                };
                match end {
                    Some(end) => {
                        marked[at..=end].fill(true);
                        inner[at..=end].fill(true);
                        at = end + 1;
                    }
                    None => at += 1,
                }
                continue;
            }
            if text.is_empty() || text.starts_with("%%") {
                marked[at] = true;
                at += 1;
                continue;
            }
            // Front matter opens a block: first in the text, after a blank
            // row, or right under the harness's label or notice.
            let opens = text == "---"
                && (at == 0 || rows[at - 1].is_empty() || marker_above(rows, at).is_some());
            let base = indent(row);
            let end = opens
                .then(|| {
                    let rules = rules.get(&base)?;
                    next(rules, rule_cursors.entry(base).or_default(), at)
                })
                .flatten();
            let Some(end) = end else {
                at += 1;
                continue;
            };
            // Front matter holds YAML: keys, nested or listed values,
            // comments and blank rows. A `---` indented further is its text.
            let yaml = rows[at + 1..end].iter().all(|row| {
                let row = row.get(base..).unwrap_or("");
                row.is_empty()
                    || row.starts_with([' ', '#', '-'])
                    || row.split_once(':').is_some_and(|(key, _)| {
                        let key = key.trim_end();
                        !key.is_empty() && !key.contains(' ')
                    })
            });
            if yaml {
                marked[at..=end].fill(true);
            } else {
                refused[end] = true;
            }
            inner[at + 1..end].fill(true);
            at = end + 1;
        }
        let mut runs = Vec::with_capacity(rows.len());
        let mut run: Option<(usize, Option<usize>)> = None;
        for (at, row) in rows.iter().enumerate() {
            run = if marked[at] {
                let (start, first) = run.unwrap_or((at, None));
                let first = first.or_else(|| (!row.trim().is_empty()).then_some(at));
                Some((start, first))
            } else {
                None
            };
            runs.push(run);
        }
        Self {
            runs,
            refused,
            inner,
        }
    }

    /// Where the run of prelude rows directly above `header` starts, and
    /// the first of them that is not blank (`header` itself with none).
    fn run_above(&self, header: usize) -> (usize, usize) {
        match header.checked_sub(1).and_then(|above| self.runs[above]) {
            Some((start, first)) => (start, first.unwrap_or(header)),
            None => (header, header),
        }
    }
}

/// The row the diagram in a reply, whose header is `rows[header]`, ends
/// before. It runs over blank rows, as a source may hold, while the row
/// after them is indented as far as its body: prose comes back to the
/// message's indentation. After an unindented body, text after a blank row
/// could be either.
fn diagram_end(rows: &[String], header: usize) -> Result<usize, DiagramCopyError> {
    let base = indent(&rows[header]);
    // The least indentation past the header's that a body row has so far.
    let mut body: Option<usize> = None;
    let mut at = header;
    loop {
        while at < rows.len() && !rows[at].is_empty() {
            let indent = indent(&rows[at]);
            if at > header && indent > base {
                body = Some(body.map_or(indent, |body| body.min(indent)));
            }
            at += 1;
        }
        let Some(next) = rows[at..].iter().position(|row| !row.is_empty()) else {
            return Ok(at);
        };
        let indent = indent(&rows[at + next]);
        let continues = match body {
            Some(body) => indent >= body,
            None => indent > base,
        };
        if !continues {
            if body.is_none() {
                return Err(DiagramCopyError::Unbounded);
            }
            return Ok(at);
        }
        at += next;
    }
}

/// Whose reply the rows directly above `top` say it is: Claude's
/// `mermaid` label, or a Codex notice, perhaps wrapped over rows.
fn marker_above(rows: &[String], top: usize) -> Option<Harness> {
    let above = rows[..top].iter().rev().map(|row| row.trim());
    let mut above = above.take_while(|row| !row.is_empty()).peekable();
    if above.peek() == Some(&"mermaid") {
        return Some(Harness::Claude);
    }
    let longest = NOTICES.iter().map(|notice| notice.len()).max().unwrap_or(0);
    let mut joined = String::new();
    for row in above {
        if joined.len() + row.len() > longest {
            break;
        }
        joined = if joined.is_empty() {
            row.to_owned()
        } else {
            format!("{row} {joined}")
        };
        if NOTICES.contains(&joined.as_str()) {
            return Some(Harness::Codex);
        }
    }
    None
}

/// The least indentation a diagram's body rows have, past its header; zero
/// when none is indented. Blank rows have none.
fn body_indent(rows: &[&str]) -> usize {
    rows[1..]
        .iter()
        .map(|row| indent(row))
        .filter(|indent| *indent > 0)
        .min()
        .unwrap_or(0)
}

/// Whether `row` ends with a letter or digit, in any script, then `-` or
/// `/`: where Codex's wrapping may break inside a word.
fn ends_in_word_break(row: &str) -> bool {
    let mut tail = row.chars().rev();
    matches!(tail.next(), Some('-' | '/')) && tail.next().is_some_and(char::is_alphanumeric)
}

/// Whether `row` starts with a letter or digit, in any script, as Codex's
/// wrapper counts them.
fn starts_with_word(row: &str) -> bool {
    row.chars().next().is_some_and(char::is_alphanumeric)
}

/// The first thing on `row` a wrapper could have placed on the row above:
/// its first word, or, where it `splits` words as Codex does after `-` and
/// `/`, up to and including the first of those.
fn first_unit(row: &str, splits: bool) -> &str {
    let word = row.split(' ').next().unwrap_or(row);
    if splits && let Some(at) = word.find(['-', '/']) {
        return &word[..=at];
    }
    word
}

/// The columns a greedy wrapper needs on the row `above` to have kept the
/// first word of the row below, `next`, there.
struct WrapCost {
    /// `above` ends in `-` or `/` after a letter and `next` starts with
    /// one: Codex would place it with no space between.
    joins_word: bool,
    /// Codex's, which also breaks after a `-` or `/` inside a word.
    codex: usize,
    /// Claude's, which breaks only at spaces.
    claude: usize,
}

impl WrapCost {
    fn of(above: &str, next: &str) -> Self {
        let joins_word = ends_in_word_break(above) && starts_with_word(next);
        Self {
            joins_word,
            codex: above.width() + usize::from(!joins_word) + first_unit(next, true).width(),
            claude: above.width() + 1 + first_unit(next, false).width(),
        }
    }

    /// The least of what the harnesses that may have wrapped it need: Codex
    /// among them where it `splits` words.
    fn least(&self, splits: bool) -> usize {
        if splits { self.codex } else { self.claude }
    }

    /// The most of what they need: Claude among them where it wraps
    /// `whole` words.
    fn most(&self, whole: bool) -> usize {
        if whole { self.claude } else { self.codex }
    }
}

/// The source lines `rows`, from the header on, were wrapped from at
/// `available` columns. A row is a continuation only when it is indented
/// less than the body and no greedy wrapper at this width, the harness's or
/// either when it is unknown, could have placed its first word on the row
/// above or cut a word at that row's edge; it joins with the space the wrap
/// took. A row that could be either is refused.
fn rejoin(
    rows: &[&str],
    available: usize,
    harness: Option<Harness>,
) -> Result<String, DiagramCopyError> {
    let body_min = body_indent(rows);
    // Codex also breaks inside a word, after `-` or `/`; Claude only at
    // spaces.
    let splits = harness != Some(Harness::Claude);
    let whole = harness != Some(Harness::Codex);
    // An unindented body's wrap could have been made at any width its rows
    // fit in, the pane's before it was widened too: the narrowest decides.
    let widest = rows.iter().map(|row| row.width()).max().unwrap_or(0);
    let flat_limit = if body_min == 0 {
        widest.min(available)
    } else {
        available
    };
    let mut source = String::with_capacity(rows.iter().map(|row| row.len() + 1).sum());
    source.push_str(rows[0]);
    let (mut head, mut above) = (rows[0], rows[0]);
    for row in &rows[1..] {
        // No wrap leaves a blank row, nor starts the row after one.
        if row.is_empty() || above.is_empty() {
            source.push('\n');
            source.push_str(row);
            (head, above) = (row, row);
            continue;
        }
        let trimmed = row.trim();
        let cost = WrapCost::of(above, trimmed);
        // `a-` then `b`: Codex broke `a-b`, or wrapped `a- b`.
        let joins_word = splits && cost.joins_word;
        if body_min > 0 && indent(row) < body_min {
            // Indented less than the body, it has a wrap's shape. Yet a
            // greedy wrapper might have kept it on the row above: the text
            // was wrapped at another width.
            let kept = cost.least(splits) <= available;
            // A full row: a word too long for a row could have been cut at
            // its edge as well as wrapped at a space there.
            let next = trimmed.chars().next().and_then(|c| c.width()).unwrap_or(0);
            let last = above.rsplit(' ').next().unwrap_or(above);
            let first = trimmed.split(' ').next().unwrap_or(trimmed);
            let cut = above.width() + next > available && last.width() + first.width() > available;
            if joins_word || kept || cut {
                return Err(DiagramCopyError::Wrapped);
            }
            source.push(' ');
            source.push_str(trimmed);
        } else {
            if cost.most(whole) > flat_limit && indent(row) == indent(head) {
                // It might be a wrap, yet one at the indentation of the
                // statement above, or of an unindented body, looks like a
                // new statement.
                return Err(DiagramCopyError::Wrapped);
            }
            source.push('\n');
            source.push_str(row);
            head = row;
        }
        above = row;
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fenced copy gives each Mermaid fence's body verbatim, in order;
    /// other fences, and an unclosed one, give nothing, and nothing after an
    /// unclosed one is a fence: it runs to the end of the text.
    #[test]
    fn each_mermaid_fence_is_a_source_verbatim() {
        let text = "Here:\n```mermaid\nflowchart TD\n  A-->B\n```\n\n~~~~ mermaid title\nsequenceDiagram\n  A->>B: Hi\n~~~~~\n```rust\nfn main() {}\n```";
        assert_eq!(
            diagram_sources(text, None).unwrap(),
            ["flowchart TD\n  A-->B", "sequenceDiagram\n  A->>B: Hi"]
        );
        assert_eq!(
            diagram_sources("- Steps:\n\n    ```mermaid\n    graph LR\n    ```", None).unwrap(),
            ["    graph LR"],
            "nested in a list, indented, kept verbatim"
        );
        assert_eq!(
            diagram_sources("```mermaidish\nhello\n```", None),
            Err(DiagramCopyError::NoDiagram),
            "a fence of another language"
        );
        assert_eq!(
            diagram_sources("```mermaid\nflowchart TD\n  A-->B", None).unwrap(),
            ["flowchart TD\n  A-->B"],
            "unclosed: read as unfenced text from its header"
        );
        assert_eq!(
            diagram_sources(
                "```mermaid\npie\n```\n```mermaid\n~~~mermaid\ngantt\n~~~",
                None
            )
            .unwrap(),
            ["pie", "~~~mermaid\ngantt\n~~~"],
            "an unclosed fence runs to the end: the fence after it is its text"
        );
        assert_eq!(
            diagram_sources("```mermaid\ngraph LR\n  A --> B\n  ```", None).unwrap(),
            ["graph LR\n  A --> B"],
            "a closing fence may be indented up to three columns"
        );
        assert_eq!(
            diagram_sources("```mermaid\ngraph LR\n    ```\n```", None).unwrap(),
            ["graph LR\n    ```"],
            "four columns in, it is the diagram's text"
        );
        let tabbed = "- item\n\n    ```mermaid\n    graph LR\n      A --> B\n\t```\n\n```mermaid\ngraph LR\n  C --> D\n```";
        assert_eq!(
            diagram_sources(tabbed, None).unwrap(),
            ["    graph LR\n      A --> B", "graph LR\n  C --> D"],
            "a tab reaches column four"
        );
        assert_eq!(
            diagram_sources("```mermaid\ngraph LR\n\t\t```\n```", None).unwrap(),
            ["graph LR\n\t\t```"],
            "two tabs are eight columns in: the diagram's text"
        );
        let mixed = "```mermaid\nflowchart TD\nA[\"alpha\n```~~~\nomega\"] --> B\n```";
        assert_eq!(
            diagram_sources(mixed, None).unwrap(),
            ["flowchart TD\nA[\"alpha\n```~~~\nomega\"] --> B"],
            "a fence closes with its own mark only"
        );
        let nested = "````text\n```mermaid\ngraph LR\n  X --> Y\n```\n````\n\n```mermaid\ngraph TD\n  A --> B\n```";
        assert_eq!(
            diagram_sources(nested, None).unwrap(),
            ["graph TD\n  A --> B"],
            "a fence inside another language's is that language's text"
        );
        assert_eq!(
            diagram_sources("```text\nflowchart TD\n  A --> B\n```", None),
            Err(DiagramCopyError::NoDiagram),
            "nor is a diagram looked for inside one"
        );
        assert_eq!(
            diagram_sources("Before\n```mermaid\ngraph LR\n  A --> B", None).unwrap(),
            ["graph LR\n  A --> B"],
            "unclosed, it runs to the end, verbatim"
        );
        // As many as a gallery shows, and one more is refused, never cut
        // short; blank fences do not count.
        let many = |count| "```mermaid\ngraph LR\n```\n".repeat(count);
        let full = many(MAX_COPIED_DIAGRAMS) + "```mermaid\n\n```\n";
        assert_eq!(
            diagram_sources(&full, None).unwrap().len(),
            MAX_COPIED_DIAGRAMS
        );
        for count in [MAX_COPIED_DIAGRAMS + 1, MAX_COPIED_DIAGRAMS + 40] {
            assert_eq!(
                diagram_sources(&many(count), None),
                Err(DiagramCopyError::TooMany),
                "{count}"
            );
        }
    }

    /// Claude's label and bullet, Codex's notices wrapped over rows and its
    /// bullet, leading prose and trailing prose go; the common indentation
    /// goes; nothing else changes.
    #[test]
    fn what_a_harness_adds_around_a_diagram_goes() {
        let claude = "⏺ Here it is:\n\n  mermaid\n  flowchart TD\n      A[One] --> B[Two]\n\n  The diagram shows two steps.";
        assert_eq!(
            diagram_sources(claude, Some(80)).unwrap(),
            ["flowchart TD\n    A[One] --> B[Two]"]
        );
        for notice in NOTICES {
            let codex = format!("• {notice}\n\n  graph LR\n    A --> B");
            assert_eq!(
                diagram_sources(&codex, Some(120)).unwrap(),
                ["graph LR\n  A --> B"]
            );
        }
        let wrapped_notice = "• This Mermaid diagram doesn't fit the\n  current terminal width.\n  graph LR\n    A --> B";
        assert_eq!(
            diagram_sources(wrapped_notice, Some(40)).unwrap(),
            ["graph LR\n  A --> B"]
        );
        let near = "This Mermaid diagram doesn't fit the old width.\ngraph LR\n  A --> B";
        assert_eq!(
            diagram_sources(near, None).unwrap(),
            ["graph LR\n  A --> B"],
            "a sentence that is no notice is prose before the header"
        );
        let literal = format!("graph LR\n  A[{}] --> B", NOTICES[0]);
        assert_eq!(diagram_sources(&literal, None).unwrap(), [literal]);
        assert_eq!(
            diagram_sources("Just prose.", None),
            Err(DiagramCopyError::NoDiagram)
        );
        assert_eq!(
            diagram_sources("graphical", None),
            Err(DiagramCopyError::NoDiagram),
            "a header is a whole word"
        );
        for blank in ["", " \n\t\r\n"] {
            assert_eq!(diagram_sources(blank, None), Err(DiagramCopyError::Empty));
        }
        assert!(diagram_sources(&"x".repeat(MAX_DIAGRAM_COPY_BYTES), None).is_err());
        assert_eq!(
            diagram_sources(&"x".repeat(MAX_DIAGRAM_COPY_BYTES + 1), None),
            Err(DiagramCopyError::TooLarge)
        );
    }

    /// With the width known, a wrapped row joins the row above, with the
    /// space the wrap took or, for a word cut for want of room, without; a
    /// row that could be a new statement too is refused. Without the width,
    /// rows that look wrapped are refused.
    #[test]
    fn hard_wraps_join_back_only_when_sure() {
        // 30 columns, two of them the message indent.
        let selected = "  flowchart TD\n      A[Receive the pull\n  request] --> B\n      B --> C";
        assert_eq!(
            diagram_sources(selected, Some(30)).unwrap(),
            ["flowchart TD\n    A[Receive the pull request] --> B\n    B --> C"]
        );
        assert_eq!(
            diagram_sources(selected, None),
            Err(DiagramCopyError::Wrapped)
        );
        // A full row could end in a word cut at its edge or at a space
        // there: refused.
        let full = "  graph LR\n      A[aaaaaaaaaaaaaaaaaa\n  bbbb] --> B";
        assert_eq!(
            diagram_sources(full, Some(26)),
            Err(DiagramCopyError::Wrapped)
        );
        // Wrapped at another width than the one given: the wrapped row
        // would have fit, so it is no wrap there, yet it has a wrap's shape.
        assert_eq!(
            diagram_sources(selected, Some(80)),
            Err(DiagramCopyError::Wrapped)
        );
        // Without the width, a row is no wrap when no width the rows fit in
        // could have wrapped it, or at the left edge, where no reply on
        // screen puts a row.
        let nested = "treemap-beta\n\"Category A\"\n    \"Item A1\": 10\n\"Category B\"\n    \"Item B1\": 15";
        assert_eq!(diagram_sources(nested, None).unwrap(), [nested]);
        let on_screen = nested.replace('\n', "\n  ");
        assert_eq!(
            diagram_sources(&format!("  {on_screen}"), None),
            Err(DiagramCopyError::Wrapped),
            "copied from a reply, `\"Category B\"` could be a wrap at 17 to 26"
        );
        let short = "  graph LR\n      A[a long label here] --> B\n      B --> E\n  C --> D";
        assert_eq!(
            diagram_sources(short, None).unwrap(),
            ["graph LR\n    A[a long label here] --> B\n    B --> E\nC --> D"],
            "`C` would have fit after `B --> E` at the widest row's width"
        );
        assert_eq!(
            diagram_sources(&short.replace("\n      B --> E", ""), None),
            Err(DiagramCopyError::Wrapped),
            "after the widest row, it could be a wrap"
        );
        // Unindented, any row could be a wrap: `A` after `%% note note`, at
        // 12 to 13 columns, as the comment `%% note note A --> B`.
        let flat_comment = "  graph LR\n  X --> Y\n  %% note note\n  A --> B";
        assert_eq!(
            diagram_sources(flat_comment, None),
            Err(DiagramCopyError::Wrapped)
        );
        // Unknown, the harness may be Claude, who would have needed the whole
        // `left-right]` to keep it above.
        let either = "  graph LR\n      A[see] --> B\n      C --> D\n  left-right] --> E";
        assert_eq!(
            diagram_sources(either, None),
            Err(DiagramCopyError::Wrapped)
        );
        // At the left edge it is a source, with the width known too.
        let exact = "graph LR\n  X --> Y\n  %% 1234567890\nA --> B";
        assert_eq!(diagram_sources(exact, Some(16)).unwrap(), [exact]);
        // Nothing indented: a wrap and a statement look alike.
        let flat = "  graph LR\n  A[a long label that\n  wraps] --> B";
        assert_eq!(
            diagram_sources(flat, Some(22)),
            Err(DiagramCopyError::Wrapped)
        );
        // Wide characters take two columns each: 24 of the 25 left.
        let wide = "  graph LR\n      A[日本語の長いラベル\n  です] --> B";
        assert_eq!(
            diagram_sources(wide, Some(27)).unwrap(),
            ["graph LR\n    A[日本語の長いラベル です] --> B"]
        );
        assert_eq!(
            diagram_sources(wide, Some(26)),
            Err(DiagramCopyError::Wrapped),
            "all 24 of 24: full"
        );
        // 24 of 25, and `で` takes two: a word of 29 could have been cut.
        let cut = "  graph LR\n      A[日本語の長いラベル\n  ですです] --> B";
        assert_eq!(
            diagram_sources(cut, Some(27)),
            Err(DiagramCopyError::Wrapped)
        );
    }

    /// Codex also breaks inside a word, after `-` or `/`: a row ending in
    /// one and a row starting with a word could be a word it broke or a wrap
    /// at a space after it, so it is refused, from Codex or from an unknown
    /// harness; Claude breaks only at spaces, so its rows join with one. A
    /// row whose first word Codex could have broken earlier, at a hyphen
    /// that fit above, was wrapped at another width.
    #[test]
    fn a_word_codex_may_have_broken_is_refused() {
        let path = "  graph LR\n      A[see /very/long/path/to/a-\n  file] --> B";
        for (selected, by) in [
            (path.replacen("  ", "• ", 1), "Codex"),
            (path.to_owned(), "an unknown harness"),
        ] {
            assert_eq!(
                diagram_sources(&selected, Some(34)),
                Err(DiagramCopyError::Wrapped),
                "{by}"
            );
        }
        let unbulleted = format!("{}\n{path}", NOTICES[2]);
        assert_eq!(
            diagram_sources(&unbulleted, Some(34)),
            Err(DiagramCopyError::Wrapped),
            "the notice alone says Codex"
        );
        // A word in any script: "café-noir" and "café- noir" wrap alike.
        let accented = "• graph LR\n      A[\"caf\u{e9}-\n  noir\"] --> B\n      B --> C";
        assert_eq!(
            diagram_sources(accented, Some(18)),
            Err(DiagramCopyError::Wrapped)
        );
        // "foo abc-def" and "foo abc- def" wrap alike at 18.
        let label = "• graph LR\n      A[\"foo abc-\n  def\"] --> B";
        assert_eq!(
            diagram_sources(label, Some(18)),
            Err(DiagramCopyError::Wrapped)
        );
        assert_eq!(
            diagram_sources(&path.replacen("  ", "⏺ ", 1), Some(34)).unwrap(),
            ["graph LR\n    A[see /very/long/path/to/a- file] --> B"],
            "Claude broke at the space"
        );
        // `left-` would have fit after `see` within 18 columns.
        let earlier = "• graph LR\n      A[see\n  left-right] --> B";
        assert_eq!(
            diagram_sources(earlier, Some(20)),
            Err(DiagramCopyError::Wrapped)
        );
        assert_eq!(
            diagram_sources(&earlier.replacen("• ", "  ", 1), Some(20)),
            Err(DiagramCopyError::Wrapped),
            "an unknown harness may be Codex"
        );
        // Unindented: `left-right]` is a line of its own for Codex, which
        // would have kept `left-` above, but Claude could have wrapped it.
        let flat = "  graph LR\n  A[see\n  left-right] --> B";
        assert_eq!(
            diagram_sources(flat, Some(18)),
            Err(DiagramCopyError::Wrapped)
        );
        let noticed = format!("{}\n{flat}", NOTICES[0]);
        assert_eq!(
            diagram_sources(&noticed, Some(18)).unwrap(),
            ["graph LR\nA[see\nleft-right] --> B"],
            "the notice says Codex"
        );
        let claude = "  mermaid\n  graph LR\n      A[see\n  left-right] --> B";
        assert_eq!(
            diagram_sources(claude, Some(20)).unwrap(),
            ["graph LR\n    A[see left-right] --> B"],
            "Claude's label says Claude, who breaks only at spaces"
        );
    }

    /// The diagram starts at the first header that starts its paragraph or
    /// follows Claude's label or a Codex notice, not at a header word in
    /// the prose before; a notice or label below that is the diagram's own
    /// text, and stays.
    #[test]
    fn the_diagram_starts_where_a_harness_shows_it() {
        let prose =
            "⏺ Here is the\n  flowchart of the steps:\n\n  mermaid\n  flowchart TD\n      A --> B";
        assert_eq!(
            diagram_sources(prose, Some(80)).unwrap(),
            ["flowchart TD\n    A --> B"]
        );
        let joined = format!(
            "• The\n  graph below:\n  {}\n  graph LR\n    A --> B",
            NOTICES[1]
        );
        assert_eq!(
            diagram_sources(&joined, Some(120)).unwrap(),
            ["graph LR\n  A --> B"],
            "after a notice in the bullet's paragraph, not at the prose's header word"
        );
        let inside = format!("graph LR\n  A[\"first\n  {}\n  last\"] --> B", NOTICES[2]);
        assert_eq!(
            diagram_sources(&inside, None).unwrap(),
            std::slice::from_ref(&inside)
        );
        let marked = format!("• {}\n  {}", NOTICES[2], inside.replace('\n', "\n  "));
        assert_eq!(diagram_sources(&marked, None).unwrap(), [inside]);
        let label = "  mermaid\n  mindmap\n    root\n      mermaid\n      graph";
        assert_eq!(
            diagram_sources(label, None).unwrap(),
            ["mindmap\n  root\n    mermaid\n    graph"]
        );
    }

    /// Front matter, comments and directives directly above the header are
    /// the diagram's, as Mermaid reads them; the header still decides where
    /// it starts, and the harness's label or notice comes above them.
    #[test]
    fn what_mermaid_reads_before_a_header_stays() {
        let prelude = "---\ntitle: Steps\n---\n%%{init: {'theme': 'forest'}}%%\n%% two steps\nflowchart TD\n  A --> B";
        assert_eq!(diagram_sources(prelude, None).unwrap(), [prelude]);
        let claude = format!(
            "⏺ Here:\n\n  mermaid\n{}",
            prelude
                .split('\n')
                .map(|row| format!("  {row}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(diagram_sources(&claude, Some(80)).unwrap(), [prelude]);
        assert_eq!(
            diagram_sources("Notes:\n---\nflowchart TD\n  A --> B", None).unwrap(),
            ["flowchart TD\n  A --> B"],
            "a rule with no front matter above it is prose"
        );
        let spaced =
            "---\ntitle: Important\n\nconfig:\n  theme: dark\n---\nflowchart TD\n  A --> B";
        assert_eq!(
            diagram_sources(spaced, None).unwrap(),
            [spaced],
            "front matter may hold blank rows"
        );
        let colon = "---\ntitle : Steps\n---\nflowchart TD\n  A --> B";
        assert_eq!(
            diagram_sources(colon, None).unwrap(),
            [colon],
            "a key may have spaces before its colon"
        );
        // Not all YAML, as wrapped front matter would not be: refused.
        let wrapped = "  mermaid\n  ---\n  title: Receive the pull\n  request\n  ---\n  flowchart TD\n      A --> B";
        assert_eq!(
            diagram_sources(wrapped, Some(30)),
            Err(DiagramCopyError::Wrapped)
        );
        // Front matter opens a block, and closes: a rule under prose, or one
        // with nothing to close it, is prose.
        let rules = "Intro\n---\nMore prose\n\n---\nflowchart TD\n  A --> B";
        assert_eq!(
            diagram_sources(rules, None).unwrap(),
            ["flowchart TD\n  A --> B"]
        );
        let scalar = "---\ntitle: |-\n  First\n  ---\n  Last\n---\nflowchart TD\n  A --> B";
        assert_eq!(
            diagram_sources(scalar, None).unwrap(),
            [scalar],
            "an indented rule is the YAML's text"
        );
        let directive =
            "%%{init: {\n  'flowchart': {'curve': 'linear'}\n}}%%\nflowchart TD\n  A --> B";
        assert_eq!(
            diagram_sources(directive, None).unwrap(),
            [directive],
            "a directive over several rows"
        );
        for directive in [
            "%%{init: {\"theme\": \"dark\"}}%% %% configuration\nflowchart TD\n  A --> B",
            "%%{init: {\n  \"theme\": \"dark\"\n}}%%  %%dark\nflowchart TD\n  A --> B",
        ] {
            assert_eq!(
                diagram_sources(directive, None).unwrap(),
                [directive],
                "a comment may follow a directive's end"
            );
        }
        assert_eq!(
            diagram_sources(&directive.replace("\n}}%%", ""), None).unwrap(),
            ["flowchart TD\n  A --> B"],
            "unclosed, it is no directive: above the header is prose"
        );
        assert_eq!(
            diagram_sources(
                "%%{init: {\n'theme': 'dark'\n\nflowchart TD\n  A --> B",
                None
            )
            .unwrap(),
            ["flowchart TD\n  A --> B"],
            "unclosed: prose before the diagram's"
        );
        let spaced =
            "%%{init: {\n\n  \"flowchart\": {\"curve\": \"linear\"}\n}}%%\nflowchart TD\n  A --> B";
        assert_eq!(
            diagram_sources(spaced, None).unwrap(),
            [spaced],
            "a directive runs to its end over blank rows"
        );
        assert_eq!(
            diagram_sources(
                "  mermaid\n  %%{init: {\n  flowchart TD\n      A --> B",
                Some(80)
            )
            .unwrap(),
            ["flowchart TD\n    A --> B"],
            "an unclosed directive's first row is not kept either"
        );
        // A header a wrap could have made of the comment above it is that
        // comment's text: refused.
        let comment = "  mermaid\n  %% This is an example of a\n  graph LR\n      A --> B";
        assert_eq!(
            diagram_sources(comment, Some(30)),
            Err(DiagramCopyError::Wrapped)
        );
        assert_eq!(
            diagram_sources(comment, Some(40)).unwrap(),
            ["%% This is an example of a\ngraph LR\n    A --> B"],
            "wider, it would have fit: a header of its own"
        );
        // A header word inside front matter is its text.
        let scalar = "  mermaid\n  ---\n  title: |\n    graph LR\n      X\n\n    Y\n  ---\n  flowchart TD\n      A --> B";
        assert_eq!(
            diagram_sources(scalar, Some(80)).unwrap(),
            ["---\ntitle: |\n  graph LR\n    X\n\n  Y\n---\nflowchart TD\n    A --> B"]
        );
        // A row a wrap could have made, inside a directive or front matter,
        // could stand for a space in a string: refused.
        let wrapped = "  mermaid\n  %%{init: {\"themeVariables\": {\"fontFamily\": \"Times\n  New Roman\"}}}%%\n  flowchart TD\n      A --> B";
        assert_eq!(
            diagram_sources(wrapped, Some(52)),
            Err(DiagramCopyError::Wrapped)
        );
        assert_eq!(
            diagram_sources(&wrapped.replace("\n  New", " New"), Some(80)).unwrap(),
            [
                "%%{init: {\"themeVariables\": {\"fontFamily\": \"Times New Roman\"}}}%%\nflowchart TD\n    A --> B"
            ],
            "unwrapped, it is taken"
        );
    }

    /// Text at the cap built to make the search for a diagram's start, its
    /// prelude and its end revisit rows takes about as long as text that
    /// does not: each row is looked at a bounded number of times.
    #[test]
    fn hostile_text_at_the_cap_stays_linear() {
        let fill = |unit: &str| unit.repeat((MAX_DIAGRAM_COPY_BYTES - 64) / unit.len());
        for text in [
            fill("x}%%\ngraph\n"),
            format!("%%{{\n{}", fill("graph\n")),
            format!("{}%%{{\n{}", fill("%% c\n"), fill("}%%\ngraph\n")),
            fill("---\ngraph\n"),
            fill("  ---\n---\ngraph\n"),
            fill("```mermaid\n"),
            fill("%%{\n"),
            fill("%%{\n}%%\n"),
            // One header after another under the same long row and directive.
            format!(
                "{}prose\n%%{{\n{}}}%%\n",
                " ".repeat(MAX_DIAGRAM_COPY_BYTES * 2 / 5),
                "graph\n".repeat(MAX_DIAGRAM_COPY_BYTES / 11),
            ),
            // Front matter at many indentations over one long stretch.
            {
                let mut text = String::from("prose\n");
                for indent in 6..256 {
                    text.push_str(&format!("\n{}---\n", " ".repeat(indent)));
                }
                text.push_str(&"#\n".repeat(MAX_DIAGRAM_COPY_BYTES / 4));
                for indent in 6..256 {
                    text.push_str(&format!("{}---\ngraph\n", " ".repeat(indent)));
                }
                text
            },
            // Headers in one directive, under prose and long front matter:
            // what is above them is looked at once.
            format!(
                "prose\n---\n{}---\n%%{{\n{}}}%%\n",
                "k: v\n".repeat(MAX_DIAGRAM_COPY_BYTES / 12),
                "graph\n".repeat(MAX_DIAGRAM_COPY_BYTES / 12),
            ),
            fill("\n"),
            fill("graph\n  x\n\n"),
        ] {
            let started = std::time::Instant::now();
            for width in [None, Some(80)] {
                let _ = diagram_sources(&text, width);
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "{:?} took {:?}",
                &text[..20],
                started.elapsed()
            );
        }
    }

    /// Every type the renderer detects starts a diagram, whole: with or
    /// without its `-beta` or `-v2`, never as part of a longer word.
    #[test]
    fn every_type_the_renderer_detects_starts_a_diagram() {
        for header in HEADERS {
            assert_eq!(diagram_sources(header, None).unwrap(), [header]);
            let suffixed = format!("{header}-beta");
            assert_eq!(diagram_sources(&suffixed, None).unwrap(), [suffixed]);
            assert_eq!(
                diagram_sources(&format!("{header}s"), None),
                Err(DiagramCopyError::NoDiagram)
            );
        }
        for header in [
            "xychart",
            "kanban",
            "architecture-beta",
            "C4Container",
            "stateDiagram-v2",
        ] {
            let text = format!("{header}\n  line [10, 30, 20]");
            assert_eq!(diagram_sources(&text, None).unwrap(), [text]);
        }
    }

    /// A diagram runs over blank rows while the row after them is indented
    /// as far as its body, and ends where prose comes back to the message's
    /// indentation. A text it starts with rows at the left edge, where no
    /// reply on screen puts one, is the source to its end, as from `/copy`;
    /// in a reply, what follows a blank row after an unindented body could
    /// be prose or more of it.
    #[test]
    fn a_diagram_ends_where_its_body_does() {
        let reply = "⏺ Steps:\n\n  mermaid\n  sequenceDiagram\n      A->>B: Hi\n\n      B->>A: Hello\n\n  That is all.";
        assert_eq!(
            diagram_sources(reply, Some(80)).unwrap(),
            ["sequenceDiagram\n    A->>B: Hi\n\n    B->>A: Hello"]
        );
        let header_first = "  mermaid\n  flowchart TD\n\n      A --> B\n\n  Done.";
        assert_eq!(
            diagram_sources(header_first, Some(80)).unwrap(),
            ["flowchart TD\n\n    A --> B"],
            "its body may start after a blank row"
        );
        let copied = "sequenceDiagram\nA->>B: Hi\n\nB->>A: Hello\n";
        assert_eq!(
            diagram_sources(copied, None).unwrap(),
            ["sequenceDiagram\nA->>B: Hi\n\nB->>A: Hello"]
        );
        let sections = "usecase-beta\nsystemBoundary \"Shop\"\n  Pay(\"Pay\")\nend\n\nactor Customer\nCustomer --> Pay";
        assert_eq!(
            diagram_sources(sections, None).unwrap(),
            [sections],
            "a source at the left edge runs to its end"
        );
        let classed = "graph LR\n    A --> B\n\nclassDef x fill:#f00";
        assert_eq!(
            diagram_sources(classed, Some(80)).unwrap(),
            [classed],
            "no wrap starts the row after a blank one"
        );
        let on_screen = "  sequenceDiagram\n  A->>B: Hi\n\n  B->>A: Hello";
        assert_eq!(
            diagram_sources(on_screen, Some(80)),
            Err(DiagramCopyError::Unbounded),
            "on screen, the rows after the blank could be prose"
        );
        let unindented = "⏺ Steps:\n\n  mermaid\n  sequenceDiagram\n  A->>B: Hi\n\n  That is all.";
        assert_eq!(
            diagram_sources(unindented, Some(80)),
            Err(DiagramCopyError::Unbounded)
        );
        // With nothing after it, its end is known; yet unindented, its row
        // could be a wrap at any width the rows fit in, a narrower pane's
        // before it was widened too.
        assert_eq!(
            diagram_sources(&unindented.replace("\n\n  That is all.", ""), Some(80)),
            Err(DiagramCopyError::Wrapped)
        );
        let flat = "  graph LR\n  X --> Y\n  %% note note\n  A --> B";
        assert_eq!(
            diagram_sources(flat, Some(80)),
            Err(DiagramCopyError::Wrapped),
            "`A` could have wrapped off the comment at 12 or 13 columns"
        );
    }
}
