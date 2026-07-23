//! Regex search across the whole buffer (scrollback + viewport), powering the
//! Ctrl+Shift+F overlay.

use std::collections::{HashSet, VecDeque};
use std::fmt;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use regex::Regex;
use regex_automata::Input;
use regex_automata::meta::Regex as MetaRegex;
use regex_automata::nfa::thompson::WhichCaptures;
use regex_automata::util::syntax::Config as RegexSyntaxConfig;

use crate::event::EventProxy;

/// Maximum UTF-8 byte length accepted by the interactive search compiler.
pub const MAX_SEARCH_QUERY_BYTES: usize = 4096;

/// Hard ceiling for matches returned by one bounded scan.
///
/// Callers should usually request far fewer matches for a viewport. The ceiling makes a mistaken
/// `usize::MAX` request incapable of materializing an unbounded result vector.
pub const MAX_SEARCH_MATCHES: usize = 65_536;

/// Maximum extra physical rows inspected to establish regex BOI/EOI at a
/// soft-wrapped logical-line boundary. This prevents one pathological line
/// from monopolizing an interactive event-loop turn.
pub const MAX_SEARCH_LOGICAL_LINE_CONTEXT: usize = 256;

/// Maximum terminal cells inspected while building one regex haystack.
///
/// Spacer cells count toward this bound even though they do not emit text. A pathological
/// soft-wrapped line that exceeds this or another per-haystack limit is an accuracy barrier;
/// search never advances past uninspected cells.
pub const MAX_SEARCH_MATERIALIZED_CELLS: usize = 262_144;

/// Maximum UTF-8 bytes passed to one synchronous regex engine invocation.
pub const MAX_SEARCH_MATERIALIZED_BYTES: usize = 64 * 1024;

/// Maximum complete logical-line haystacks searched by one bounded call.
pub const MAX_SEARCH_OPERATION_HAYSTACKS: usize = 256;

/// Maximum aggregate UTF-8 bytes copied while preparing and searching one bounded call.
pub const MAX_SEARCH_OPERATION_BYTES: usize = 64 * 1024;

/// Maximum aggregate terminal cells inspected while materializing one bounded call.
pub const MAX_SEARCH_OPERATION_CELLS: usize = MAX_SEARCH_MATERIALIZED_CELLS;

const MAX_SEARCH_NFA_BYTES: usize = 512 * 1024;
const MAX_SEARCH_ONEPASS_BYTES: usize = 256 * 1024;
const MAX_SEARCH_HYBRID_CACHE_BYTES: usize = 256 * 1024;
const MAX_SEARCH_DFA_BYTES: usize = 40 * 1024;

/// A stable terminal coordinate for search APIs. Lines are signed because scrollback is negative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SearchPoint {
    pub line: i32,
    pub column: usize,
}

impl SearchPoint {
    pub const fn new(line: i32, column: usize) -> Self {
        Self { line, column }
    }
}

impl From<Point> for SearchPoint {
    fn from(point: Point) -> Self {
        Self::new(point.line.0, point.column.0)
    }
}

impl From<SearchPoint> for Point {
    fn from(point: SearchPoint) -> Self {
        Point::new(Line(point.line), Column(point.column))
    }
}

/// Inclusive match range. Both directions return it in terminal reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchSpan {
    pub start: SearchPoint,
    pub end: SearchPoint,
}

impl SearchSpan {
    pub const fn new(start: SearchPoint, end: SearchPoint) -> Self {
        Self { start, end }
    }
}

/// Inclusive bounds for one directional scan.
///
/// A returned logical-line match may intersect the bounds while starting outside them. Callers
/// that partition one logical line across independent calls must deduplicate [`SearchSpan`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchBounds {
    pub start: SearchPoint,
    pub end: SearchPoint,
}

impl SearchBounds {
    pub const fn new(start: SearchPoint, end: SearchPoint) -> Self {
        Self { start, end }
    }
}

/// Direction through terminal reading order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SearchDirection {
    #[default]
    Forward,
    Reverse,
}

/// A bounded directional scan result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchBatch {
    pub matches: Vec<SearchSpan>,
    /// True when the caller cancelled between matches.
    pub cancelled: bool,
    /// True only when the iterator reached the requested bound.
    pub exhausted: bool,
    /// True when the requested or hard match cap stopped the scan.
    pub truncated: bool,
    /// True when a pathological logical line exceeded a row/cell/byte materialization bound.
    /// Matches in the batch remain exact, but navigation order across the omitted boundary is
    /// unknown and callers must not claim a definitive first/last/no-match result.
    pub accuracy_limited: bool,
    /// First unscanned point after an exact work-budget yield.
    ///
    /// Continuations occur only between fully materialized and fully searched hard logical lines.
    /// They are never returned for an accuracy-limited batch.
    pub continuation: Option<SearchPoint>,
}

/// Result of navigating to one match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOutcome {
    pub span: Option<SearchSpan>,
    pub wrapped: bool,
    pub accuracy_limited: bool,
    /// First unscanned point when deterministic work limits paused navigation.
    /// Resume it with the original directional edge through [`CompiledSearch::find_in_range`].
    pub continuation: Option<SearchPoint>,
}

/// Why strict interactive regex compilation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchCompileError {
    QueryTooLong { bytes: usize, max_bytes: usize },
    InvalidRegex,
    PatternTooComplex,
}

impl fmt::Display for SearchCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueryTooLong { bytes, max_bytes } => {
                write!(f, "search query is {bytes} bytes; limit is {max_bytes}")
            }
            // Intentionally fixed and query-free: diagnostics stay bounded and never echo a query
            // that could contain control characters or sensitive terminal text.
            Self::InvalidRegex => f.write_str("invalid regular expression"),
            Self::PatternTooComplex => f.write_str("regular expression is too complex"),
        }
    }
}

impl std::error::Error for SearchCompileError {}

/// Dimensions and scrollback extent that determine search coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchLayout {
    pub columns: usize,
    pub screen_lines: usize,
    pub history_size: usize,
}

impl SearchLayout {
    pub fn capture<T>(term: &Term<T>) -> Self {
        Self {
            columns: term.columns(),
            screen_lines: term.screen_lines(),
            history_size: term.grid().history_size(),
        }
    }
}

/// Revision/generation guard for cancelling stale chunked searches after output or reflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchScanToken {
    pub query_revision: u64,
    pub output_generation: u64,
    pub layout: SearchLayout,
}

impl SearchScanToken {
    pub fn capture<T>(term: &Term<T>, query_revision: u64, output_generation: u64) -> Self {
        Self {
            query_revision,
            output_generation,
            layout: SearchLayout::capture(term),
        }
    }

    pub fn is_current<T>(
        self,
        term: &Term<T>,
        query_revision: u64,
        output_generation: u64,
    ) -> bool {
        self.query_revision == query_revision
            && self.output_generation == output_generation
            && self.layout == SearchLayout::capture(term)
    }
}

/// Expand a forward scan endpoint to a soft-wrapped logical-line boundary,
/// while imposing a physical-line ceiling so a hostile/no-newline stream
/// cannot turn one UI event into an unbounded history walk.
pub fn bounded_line_search_right<T>(
    term: &Term<T>,
    point: SearchPoint,
    max_extra_lines: usize,
) -> (SearchPoint, bool) {
    use alacritty_terminal::term::cell::Flags;

    let mut point: Point = point.into();
    point.line = point
        .line
        .max(term.topmost_line())
        .min(term.bottommost_line());
    let bottom = term.bottommost_line();
    let mut advanced = 0usize;
    while point.line < bottom
        && term.grid()[point.line][term.last_column()]
            .flags
            .contains(Flags::WRAPLINE)
        && advanced < max_extra_lines
    {
        point.line += 1;
        advanced += 1;
    }
    let truncated = point.line < bottom
        && term.grid()[point.line][term.last_column()]
            .flags
            .contains(Flags::WRAPLINE);
    point.column = term.last_column();
    (point.into(), truncated)
}

/// Reverse counterpart to [`bounded_line_search_right`].
pub fn bounded_line_search_left<T>(
    term: &Term<T>,
    point: SearchPoint,
    max_extra_lines: usize,
) -> (SearchPoint, bool) {
    use alacritty_terminal::term::cell::Flags;

    let mut point: Point = point.into();
    point.line = point
        .line
        .max(term.topmost_line())
        .min(term.bottommost_line());
    let top = term.topmost_line();
    let mut advanced = 0usize;
    while point.line > top
        && term.grid()[point.line - 1i32][term.last_column()]
            .flags
            .contains(Flags::WRAPLINE)
        && advanced < max_extra_lines
    {
        point.line -= 1;
        advanced += 1;
    }
    let truncated = point.line > top
        && term.grid()[point.line - 1i32][term.last_column()]
            .flags
            .contains(Flags::WRAPLINE);
    point.column = Column(0);
    (point.into(), truncated)
}

/// Strict, reusable terminal regex compiled by regex-automata's meta engine.
///
/// The meta engine retains Rust regex semantics that a streaming DFA alone cannot provide,
/// notably Unicode word boundaries. Search execution is still bounded: terminal logical lines
/// are materialized a chunk at a time with explicit row, cell, and byte ceilings.
#[derive(Clone, Debug)]
pub struct CompiledSearch {
    regex: MetaRegex,
}

impl CompiledSearch {
    /// Compile an interactive regex. An empty query is valid but has no search program.
    pub fn compile(
        pattern: &str,
        mode: CaseSensitivity,
    ) -> Result<Option<Self>, SearchCompileError> {
        if pattern.is_empty() {
            return Ok(None);
        }
        if pattern.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(SearchCompileError::QueryTooLong {
                bytes: pattern.len(),
                max_bytes: MAX_SEARCH_QUERY_BYTES,
            });
        }

        let case_sensitive = match mode {
            CaseSensitivity::Smart => pattern.chars().any(char::is_uppercase),
            CaseSensitivity::Always => true,
            CaseSensitivity::Never => false,
        };
        let regex = MetaRegex::builder()
            .configure(
                MetaRegex::config()
                    .which_captures(WhichCaptures::Implicit)
                    .nfa_size_limit(Some(MAX_SEARCH_NFA_BYTES))
                    .onepass_size_limit(Some(MAX_SEARCH_ONEPASS_BYTES))
                    .hybrid_cache_capacity(MAX_SEARCH_HYBRID_CACHE_BYTES)
                    .dfa_size_limit(Some(MAX_SEARCH_DFA_BYTES)),
            )
            .syntax(RegexSyntaxConfig::new().case_insensitive(!case_sensitive))
            .build(pattern)
            .map_err(|error| {
                if error.size_limit().is_some() {
                    SearchCompileError::PatternTooComplex
                } else {
                    SearchCompileError::InvalidRegex
                }
            })?;
        Ok(Some(Self { regex }))
    }

    fn nonempty_matches<'a>(
        &'a self,
        haystack: &'a SearchHaystack,
    ) -> impl Iterator<Item = regex_automata::Match> + 'a {
        let input = Input::new(&haystack.text).span(haystack.search_start..haystack.search_end);
        // Terminal highlights need at least one cell. Keep the regex engine's standard
        // leftmost-first ordering and discard zero-width results instead of re-running nullable
        // expressions at every byte (which can turn an interactive search into quadratic work).
        // Consequently an earlier empty alternative can shadow a later consuming alternative at
        // the same position, exactly as it does before the UI-level zero-width filter.
        self.regex
            .find_iter(input)
            .filter(|found| found.start() != found.end())
    }

    /// Find one match from `origin` toward the terminal edge, optionally wrapping once.
    ///
    /// A large range can return an exact [`SearchOutcome::continuation`] before a match or edge.
    pub fn find_next<T>(
        &mut self,
        term: &Term<T>,
        origin: SearchPoint,
        direction: SearchDirection,
        wrap: bool,
    ) -> SearchOutcome {
        let origin = clamp_point(term, origin);
        let top_left = SearchPoint::new(term.topmost_line().0, 0);
        let bottom_right = SearchPoint::new(term.bottommost_line().0, term.last_column().0);
        let first_bounds = match direction {
            SearchDirection::Forward => SearchBounds::new(origin, bottom_right),
            SearchDirection::Reverse => SearchBounds::new(origin, top_left),
        };
        let first = self.find_in_range(term, first_bounds, direction, 1);
        if first.accuracy_limited {
            return SearchOutcome {
                span: None,
                wrapped: false,
                accuracy_limited: true,
                continuation: None,
            };
        }
        if let Some(span) = first.matches.into_iter().next() {
            return SearchOutcome {
                span: Some(span),
                wrapped: false,
                accuracy_limited: false,
                continuation: None,
            };
        }
        if let Some(continuation) = first.continuation {
            return SearchOutcome {
                span: None,
                wrapped: false,
                accuracy_limited: false,
                continuation: Some(continuation),
            };
        }

        if !wrap {
            return SearchOutcome {
                span: None,
                wrapped: false,
                accuracy_limited: false,
                continuation: None,
            };
        }

        let wrapped_bounds = match direction {
            SearchDirection::Forward => SearchBounds::new(top_left, origin),
            SearchDirection::Reverse => SearchBounds::new(bottom_right, origin),
        };
        let wrapped = self.find_in_range(term, wrapped_bounds, direction, 1);
        let span = (!wrapped.accuracy_limited)
            .then(|| wrapped.matches.into_iter().next())
            .flatten();
        SearchOutcome {
            wrapped: span.is_some(),
            span,
            accuracy_limited: wrapped.accuracy_limited,
            continuation: wrapped.continuation,
        }
    }

    /// Scan one bounded work slice of an inclusive, monotonic range.
    ///
    /// Resume from [`SearchBatch::continuation`] until the batch is exhausted, a match cap is
    /// reached, or an accuracy barrier is reported.
    pub fn find_in_range<T>(
        &mut self,
        term: &Term<T>,
        bounds: SearchBounds,
        direction: SearchDirection,
        max_matches: usize,
    ) -> SearchBatch {
        self.find_in_range_while(term, bounds, direction, max_matches, || false)
    }

    /// Like [`Self::find_in_range`], checking a cancellation predicate between matches.
    ///
    /// Keep each range small enough for one event-loop chunk: a regex engine invocation cannot be
    /// interrupted in the middle of a single no-match range.
    pub fn find_in_range_while<T, F>(
        &mut self,
        term: &Term<T>,
        bounds: SearchBounds,
        direction: SearchDirection,
        max_matches: usize,
        mut should_cancel: F,
    ) -> SearchBatch
    where
        F: FnMut() -> bool,
    {
        let limit = max_matches.min(MAX_SEARCH_MATCHES);
        if limit == 0 {
            return SearchBatch {
                matches: Vec::new(),
                cancelled: should_cancel(),
                exhausted: false,
                truncated: true,
                accuracy_limited: false,
                continuation: None,
            };
        }

        let start = clamp_point(term, bounds.start);
        let end = clamp_point(term, bounds.end);
        let monotonic = match direction {
            SearchDirection::Forward => start <= end,
            SearchDirection::Reverse => start >= end,
        };
        if !monotonic {
            return SearchBatch {
                matches: Vec::new(),
                cancelled: false,
                exhausted: true,
                truncated: false,
                accuracy_limited: false,
                continuation: None,
            };
        }

        // Anchors and Unicode word boundaries need the real logical-line context, not an
        // arbitrary viewport/chunk edge. Expand to bounded soft-wrap endpoints and materialize one
        // hard logical line at a time. Input::span supplies bounded adjacent context at a
        // pathological boundary; traversal stops there rather than skipping omitted cells.
        let low = start.min(end);
        let high = start.max(end);
        let (engine_low, _left_truncated) =
            bounded_line_search_left(term, low, MAX_SEARCH_LOGICAL_LINE_CONTEXT);
        let (engine_high, _right_truncated) =
            bounded_line_search_right(term, high, MAX_SEARCH_LOGICAL_LINE_CONTEXT);
        let mut matches = Vec::with_capacity(limit.min(256));
        let mut seen = HashSet::with_capacity(limit.min(256));
        let mut haystack = SearchHaystack::default();
        let mut work = SearchWork::default();

        match direction {
            SearchDirection::Forward => {
                let mut chunk_start = engine_low.line;
                let mut frontier = start;
                loop {
                    if should_cancel() {
                        return cancelled_batch(matches, false);
                    }
                    if !work.has_capacity() {
                        return yielded_batch(matches, frontier);
                    }
                    let chunk_end = forward_chunk_end(term, chunk_start, engine_high.line);
                    materialize_chunk(
                        term,
                        chunk_start,
                        chunk_end,
                        work.materialization_limits(),
                        &mut haystack,
                    );
                    if haystack.work_limited {
                        return yielded_batch(matches, frontier);
                    }
                    work.record(&haystack);
                    let matches_before_chunk = matches.len();
                    for found in self.nonempty_matches(&haystack) {
                        if should_cancel() {
                            matches.truncate(matches_before_chunk);
                            return cancelled_batch(matches, false);
                        }
                        let Some(span) = haystack.map_match(found.start(), found.end()) else {
                            continue;
                        };
                        if (span.end < low || span.start > high) || !seen.insert(span) {
                            continue;
                        }
                        matches.push(span);
                        if matches.len() == limit {
                            return SearchBatch {
                                matches,
                                cancelled: false,
                                exhausted: false,
                                truncated: true,
                                accuracy_limited: haystack.truncated,
                                continuation: None,
                            };
                        }
                    }
                    if haystack.truncated {
                        return accuracy_limited_batch(matches);
                    }

                    if chunk_end >= engine_high.line {
                        break;
                    }
                    chunk_start = chunk_end.saturating_add(1);
                    frontier = SearchPoint::new(chunk_start, 0);
                }
            }
            SearchDirection::Reverse => {
                let mut chunk_end = engine_high.line;
                let mut frontier = start;
                let mut rightmost = VecDeque::with_capacity(limit.min(256));
                let mut chunk_seen = HashSet::with_capacity(limit.min(256));
                loop {
                    if should_cancel() {
                        return cancelled_batch(matches, false);
                    }
                    if !work.has_capacity() {
                        return yielded_batch(matches, frontier);
                    }
                    let chunk_start = reverse_chunk_start(term, chunk_end, engine_low.line);
                    materialize_chunk(
                        term,
                        chunk_start,
                        chunk_end,
                        work.materialization_limits(),
                        &mut haystack,
                    );
                    if haystack.work_limited {
                        return yielded_batch(matches, frontier);
                    }
                    work.record(&haystack);
                    let remaining = limit.saturating_sub(matches.len());
                    rightmost.clear();
                    chunk_seen.clear();
                    for found in self.nonempty_matches(&haystack) {
                        if should_cancel() {
                            return cancelled_batch(matches, false);
                        }
                        let Some(span) = haystack.map_match(found.start(), found.end()) else {
                            continue;
                        };
                        if span.end < low
                            || span.start > high
                            || seen.contains(&span)
                            || !chunk_seen.insert(span)
                        {
                            continue;
                        }
                        if rightmost.len() == remaining {
                            rightmost.pop_front();
                        }
                        rightmost.push_back(span);
                    }
                    while let Some(span) = rightmost.pop_back() {
                        if seen.insert(span) {
                            matches.push(span);
                        }
                    }
                    if matches.len() == limit {
                        return SearchBatch {
                            matches,
                            cancelled: false,
                            exhausted: false,
                            truncated: true,
                            accuracy_limited: haystack.truncated,
                            continuation: None,
                        };
                    }
                    if haystack.truncated {
                        return accuracy_limited_batch(matches);
                    }
                    if chunk_start <= engine_low.line {
                        break;
                    }
                    chunk_end = chunk_start.saturating_sub(1);
                    frontier = SearchPoint::new(chunk_end, term.last_column().0);
                }
            }
        }

        SearchBatch {
            matches,
            cancelled: false,
            exhausted: true,
            truncated: false,
            accuracy_limited: false,
            continuation: None,
        }
    }
}

#[derive(Debug, Default)]
struct SearchWork {
    haystacks: usize,
    bytes: usize,
    cells: usize,
}

impl SearchWork {
    fn has_capacity(&self) -> bool {
        self.haystacks < MAX_SEARCH_OPERATION_HAYSTACKS
            && self.bytes < MAX_SEARCH_OPERATION_BYTES
            && self.cells < MAX_SEARCH_OPERATION_CELLS
    }

    fn materialization_limits(&self) -> MaterializationLimits {
        MaterializationLimits {
            bytes: MAX_SEARCH_OPERATION_BYTES.saturating_sub(self.bytes),
            cells: MAX_SEARCH_OPERATION_CELLS.saturating_sub(self.cells),
        }
    }

    fn record(&mut self, haystack: &SearchHaystack) {
        debug_assert!(!haystack.work_limited);
        self.haystacks += 1;
        self.bytes += haystack.text.len();
        self.cells += haystack.inspected_cells;
        debug_assert!(self.haystacks <= MAX_SEARCH_OPERATION_HAYSTACKS);
        debug_assert!(self.bytes <= MAX_SEARCH_OPERATION_BYTES);
        debug_assert!(self.cells <= MAX_SEARCH_OPERATION_CELLS);
    }
}

fn yielded_batch(matches: Vec<SearchSpan>, continuation: SearchPoint) -> SearchBatch {
    SearchBatch {
        matches,
        cancelled: false,
        exhausted: false,
        truncated: false,
        accuracy_limited: false,
        continuation: Some(continuation),
    }
}

fn accuracy_limited_batch(matches: Vec<SearchSpan>) -> SearchBatch {
    SearchBatch {
        matches,
        cancelled: false,
        exhausted: false,
        truncated: true,
        accuracy_limited: true,
        continuation: None,
    }
}

fn cancelled_batch(matches: Vec<SearchSpan>, truncated: bool) -> SearchBatch {
    SearchBatch {
        matches,
        cancelled: true,
        exhausted: false,
        truncated,
        accuracy_limited: truncated,
        continuation: None,
    }
}

#[derive(Clone, Copy, Debug)]
struct ByteMap {
    byte_end: u32,
    start_line: i32,
    end_line: i32,
    start_column: u32,
    end_column: u32,
}

impl ByteMap {
    const UNMAPPED_LINE: i32 = i32::MIN;

    fn unmapped(byte_end: usize) -> Self {
        Self {
            byte_end: byte_end as u32,
            start_line: Self::UNMAPPED_LINE,
            end_line: Self::UNMAPPED_LINE,
            start_column: 0,
            end_column: 0,
        }
    }
}

#[derive(Debug, Default)]
struct SearchHaystack {
    text: String,
    map: Vec<ByteMap>,
    search_start: usize,
    search_end: usize,
    inspected_cells: usize,
    work_limited: bool,
    incomplete_start: bool,
    incomplete_end: bool,
    truncated: bool,
}

#[derive(Clone, Copy, Debug)]
struct MaterializationLimits {
    bytes: usize,
    cells: usize,
}

impl SearchHaystack {
    fn map_offset(&self, offset: usize) -> Option<ByteMap> {
        let offset = u32::try_from(offset).ok()?;
        let index = self.map.partition_point(|entry| entry.byte_end <= offset);
        let entry = *self.map.get(index)?;
        (entry.start_line != ByteMap::UNMAPPED_LINE).then_some(entry)
    }

    fn map_match(&self, start: usize, end: usize) -> Option<SearchSpan> {
        if start == end {
            return None;
        }
        // Matches touching an artificial capacity boundary are only partial evidence. They are
        // omitted, the batch reports an accuracy limit, and traversal does not continue past the
        // unmaterialized part of the logical line.
        if (self.incomplete_start && start == self.search_start)
            || (self.incomplete_end && end == self.search_end)
        {
            return None;
        }
        let start = self.map_offset(start)?;
        let end = self.map_offset(end.saturating_sub(1))?;
        Some(SearchSpan::new(
            SearchPoint::new(start.start_line, start.start_column as usize),
            SearchPoint::new(end.end_line, end.end_column as usize),
        ))
    }
}

fn row_wraps<T>(term: &Term<T>, line: i32) -> bool {
    term.grid()[Line(line)][term.last_column()]
        .flags
        .contains(Flags::WRAPLINE)
}

fn forward_chunk_end<T>(term: &Term<T>, start: i32, maximum: i32) -> i32 {
    let mut end = start;
    let mut rows = 1usize;
    while end < maximum && row_wraps(term, end) && rows < MAX_SEARCH_LOGICAL_LINE_CONTEXT {
        end += 1;
        rows += 1;
    }
    end
}

fn reverse_chunk_start<T>(term: &Term<T>, end: i32, minimum: i32) -> i32 {
    let mut start = end;
    let mut rows = 1usize;
    while start > minimum && row_wraps(term, start - 1) && rows < MAX_SEARCH_LOGICAL_LINE_CONTEXT {
        start -= 1;
        rows += 1;
    }
    start
}

fn is_search_spacer(flags: Flags) -> bool {
    flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
}

fn take_cell_inspection(inspected_cells: &mut usize, max_cells: usize) -> bool {
    if *inspected_cells == max_cells {
        return false;
    }
    *inspected_cells += 1;
    true
}

fn previous_context_char<T>(
    term: &Term<T>,
    line: i32,
    inspected_cells: &mut usize,
    max_cells: usize,
) -> Option<char> {
    if line <= term.topmost_line().0 {
        return None;
    }
    let previous = line - 1;
    for column in (0..term.columns()).rev() {
        if !take_cell_inspection(inspected_cells, max_cells) {
            return None;
        }
        let cell = &term.grid()[Line(previous)][Column(column)];
        if is_search_spacer(cell.flags) {
            continue;
        }
        return cell
            .zerowidth()
            .and_then(|marks| marks.last().copied())
            .or(Some(cell.c));
    }
    None
}

fn next_context_char<T>(
    term: &Term<T>,
    line: i32,
    inspected_cells: &mut usize,
    max_cells: usize,
) -> Option<char> {
    if line >= term.bottommost_line().0 {
        return None;
    }
    let next = line + 1;
    for column in 0..term.columns() {
        if !take_cell_inspection(inspected_cells, max_cells) {
            return None;
        }
        let cell = &term.grid()[Line(next)][Column(column)];
        if !is_search_spacer(cell.flags) {
            return Some(cell.c);
        }
    }
    None
}

fn push_unmapped_char(
    text: &mut String,
    map: &mut Vec<ByteMap>,
    c: char,
    max_bytes: usize,
) -> bool {
    if c.len_utf8() > max_bytes.saturating_sub(text.len()) {
        return false;
    }
    text.push(c);
    map.push(ByteMap::unmapped(text.len()));
    true
}

fn complete_cell_utf8_len(cell: &Cell, remaining: usize) -> Option<usize> {
    let mut len = cell.c.len_utf8();
    if len > remaining {
        return None;
    }
    let marks = cell.zerowidth().unwrap_or_default();
    // Every scalar takes at least one byte. Reject an enormous combining vector in O(1) before
    // walking any prefix of it; accepted cells are then bounded by the haystack byte ceiling.
    if marks.len() > remaining - len {
        return None;
    }
    for mark in marks {
        len = len.checked_add(mark.len_utf8())?;
        if len > remaining {
            return None;
        }
    }
    Some(len)
}

fn materialize_chunk<T>(
    term: &Term<T>,
    start_line: i32,
    end_line: i32,
    limits: MaterializationLimits,
    haystack: &mut SearchHaystack,
) {
    let max_bytes = limits.bytes.min(MAX_SEARCH_MATERIALIZED_BYTES);
    let max_cells = limits.cells.min(MAX_SEARCH_MATERIALIZED_CELLS);
    let byte_limit_is_work = max_bytes < MAX_SEARCH_MATERIALIZED_BYTES;
    let cell_limit_is_work = max_cells < MAX_SEARCH_MATERIALIZED_CELLS;
    let complete_start = start_line <= term.topmost_line().0 || !row_wraps(term, start_line - 1);
    let complete_end = end_line >= term.bottommost_line().0 || !row_wraps(term, end_line);
    let estimated_cells = ((end_line - start_line + 1).max(0) as usize)
        .saturating_mul(term.columns())
        .min(MAX_SEARCH_MATERIALIZED_CELLS);
    haystack.text.clear();
    haystack.map.clear();
    haystack.text.reserve(estimated_cells.min(max_bytes));
    haystack.map.reserve(estimated_cells.min(max_bytes));

    let mut inspected_cells = 0usize;
    let mut capacity_truncated = false;
    let mut work_limited = false;
    if !complete_start
        && let Some(context) =
            previous_context_char(term, start_line, &mut inspected_cells, max_cells)
        && !push_unmapped_char(&mut haystack.text, &mut haystack.map, context, max_bytes)
    {
        if byte_limit_is_work {
            work_limited = true;
        } else {
            capacity_truncated = true;
        }
    }
    haystack.search_start = haystack.text.len();
    let mut truncated = !complete_start || !complete_end;
    let mut omitted_context = None;

    'rows: for line in start_line..=end_line {
        for column in 0..term.columns() {
            if !take_cell_inspection(&mut inspected_cells, max_cells) {
                if cell_limit_is_work {
                    work_limited = true;
                } else {
                    capacity_truncated = true;
                }
                break 'rows;
            }
            let cell = &term.grid()[Line(line)][Column(column)];
            if is_search_spacer(cell.flags) {
                continue;
            }
            let remaining = max_bytes.saturating_sub(haystack.text.len());
            let Some(cell_len) = complete_cell_utf8_len(cell, remaining) else {
                omitted_context = Some(cell.c);
                if byte_limit_is_work {
                    work_limited = true;
                } else {
                    capacity_truncated = true;
                }
                break 'rows;
            };
            let start_len = haystack.text.len();
            haystack.text.push(cell.c);
            if let Some(marks) = cell.zerowidth() {
                haystack.text.extend(marks.iter().copied());
            }
            debug_assert_eq!(haystack.text.len() - start_len, cell_len);
            let end_column = if cell.flags.contains(Flags::WIDE_CHAR)
                && column < term.last_column().0
                && term.grid()[Line(line)][Column(column + 1)]
                    .flags
                    .contains(Flags::WIDE_CHAR_SPACER)
            {
                column + 1
            } else {
                column
            };
            let leading_spacer = column == 0
                && cell.flags.contains(Flags::WIDE_CHAR)
                && line > term.topmost_line().0
                && term.grid()[Line(line - 1)][term.last_column()]
                    .flags
                    .contains(Flags::LEADING_WIDE_CHAR_SPACER);
            let start_line = if leading_spacer { line - 1 } else { line };
            let start_column = if leading_spacer {
                term.last_column().0
            } else {
                column
            };
            let (Ok(start_column), Ok(end_column), Ok(byte_end)) = (
                u32::try_from(start_column),
                u32::try_from(end_column),
                u32::try_from(haystack.text.len()),
            ) else {
                capacity_truncated = true;
                break 'rows;
            };
            haystack.map.push(ByteMap {
                byte_end,
                start_line,
                end_line: line,
                start_column,
                end_column,
            });
        }
    }
    haystack.search_end = haystack.text.len();
    let trailing_context = omitted_context.or_else(|| {
        (!complete_end)
            .then(|| next_context_char(term, end_line, &mut inspected_cells, max_cells))
            .flatten()
    });
    if let Some(context) = trailing_context
        && !push_unmapped_char(&mut haystack.text, &mut haystack.map, context, max_bytes)
    {
        if byte_limit_is_work {
            work_limited = true;
        } else {
            capacity_truncated = true;
        }
    }
    truncated |= capacity_truncated || work_limited;
    haystack.inspected_cells = inspected_cells;
    haystack.work_limited = work_limited;
    haystack.incomplete_start = !complete_start;
    haystack.incomplete_end = !complete_end || capacity_truncated || work_limited;
    haystack.truncated = truncated;
}

fn clamp_point<T>(term: &Term<T>, point: SearchPoint) -> SearchPoint {
    SearchPoint::new(
        point
            .line
            .clamp(term.topmost_line().0, term.bottommost_line().0),
        point.column.min(term.last_column().0),
    )
}

/// A match expressed in absolute grid coordinates (line can be negative for
/// scrollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: i32,
    pub start_col: usize,
    pub end_col: usize,
}

/// Terminator parity (terminatorlib/config.py:117
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

/// Same as `build_regex` but with an explicit override.
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

/// Search with an explicit case-sensitivity override.
pub fn search_with(term: &Term<EventProxy>, pattern: &str, mode: CaseSensitivity) -> Vec<Match> {
    let Some(re) = build_regex_with(pattern, mode) else {
        return Vec::new();
    };

    let grid = term.grid();
    let cols = grid.columns();
    let top = grid.topmost_line().0;
    let bottom = grid.bottommost_line().0;

    let mut matches = Vec::new();
    // Reuse the line-text + byte→column scratch buffers across every
    // scrollback line instead of allocating a fresh String + Vec per line. On a
    // 10k-line scrollback that's 2 allocations total rather than ~20k; `.clear()`
    // keeps the capacity.
    let mut text = String::with_capacity(cols);
    let mut col_of_byte: Vec<usize> = Vec::with_capacity(cols * 2);
    for line in top..=bottom {
        // Reconstruct the line text (spacer-aware) + byte→column map via the
        // shared helper, so the wide-char-spacer fix can't drift (v2.26.0).
        crate::grid_text::row_text_into(grid, line, cols, &mut text, &mut col_of_byte);
        for m in re.find_iter(&text) {
            // Skip zero-width matches. A user pattern that can
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
    use super::*;

    use std::sync::Arc;

    use alacritty_terminal::Term;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::Color;

    use crate::event::EventProxy;
    use crate::term::TermSize;

    fn empty_term(columns: usize, screen_lines: usize) -> Term<EventProxy> {
        let (tx, _rx) = crossbeam_channel::unbounded();
        Term::new(
            Config::default(),
            &TermSize {
                columns,
                screen_lines,
            },
            EventProxy::new(tx, Arc::new(|| {})),
        )
    }

    fn write_ascii(term: &mut Term<EventProxy>, line: i32, start: usize, text: &str) {
        for (offset, c) in text.chars().enumerate() {
            term.grid_mut()[Line(line)][Column(start + offset)].c = c;
        }
    }

    #[test]
    fn compiled_search_maps_base_and_combining_marks_in_both_directions() {
        let mut term = empty_term(4, 1);
        term.grid_mut()[Line(0)][Column(0)].c = 'e';
        term.grid_mut()[Line(0)][Column(0)].push_zerowidth('\u{0301}');
        term.grid_mut()[Line(0)][Column(1)].c = 'x';
        let forward = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 1));
        let reverse = SearchBounds::new(forward.end, forward.start);
        for pattern in ["e", "\u{0301}", "e\u{0301}x"] {
            let mut search = CompiledSearch::compile(pattern, CaseSensitivity::Always)
                .unwrap()
                .unwrap();
            let found = search.find_in_range(&term, forward, SearchDirection::Forward, 4);
            assert_eq!(found.matches.len(), 1, "forward pattern {pattern:?}");
            let backward = search.find_in_range(&term, reverse, SearchDirection::Reverse, 4);
            assert_eq!(
                backward.matches, found.matches,
                "reverse pattern {pattern:?}"
            );
        }
    }

    #[test]
    fn reverse_match_cap_counts_terminal_cells_not_combining_scalars() {
        let mut term = empty_term(2, 1);
        term.grid_mut()[Line(0)][Column(0)].c = 'a';
        term.grid_mut()[Line(0)][Column(1)].c = 'e';
        for mark in ['\u{0301}', '\u{0302}', '\u{0303}', '\u{0304}'] {
            term.grid_mut()[Line(0)][Column(1)].push_zerowidth(mark);
        }
        let mut search = CompiledSearch::compile(".", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let batch = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(0, 1), SearchPoint::new(0, 0)),
            SearchDirection::Reverse,
            2,
        );
        assert_eq!(
            batch.matches,
            vec![
                SearchSpan::new(SearchPoint::new(0, 1), SearchPoint::new(0, 1)),
                SearchSpan::new(SearchPoint::new(0, 0), SearchPoint::new(0, 0)),
            ]
        );
        assert!(!batch.accuracy_limited);
    }

    #[test]
    fn compiled_search_supports_unicode_word_boundaries() {
        let mut term = empty_term(6, 1);
        term.grid_mut()[Line(0)][Column(1)].c = '\u{03b2}';
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 5));
        let mut search = CompiledSearch::compile(r"\b\u{03b2}\b", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let forward = search.find_in_range(&term, bounds, SearchDirection::Forward, 4);
        assert_eq!(
            forward.matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 1),
                SearchPoint::new(0, 1)
            )]
        );
        assert_eq!(
            search
                .find_in_range(
                    &term,
                    SearchBounds::new(bounds.end, bounds.start),
                    SearchDirection::Reverse,
                    4,
                )
                .matches,
            forward.matches
        );
    }

    #[test]
    fn strict_compiler_distinguishes_empty_invalid_and_too_long() {
        assert!(
            CompiledSearch::compile("", CaseSensitivity::Smart)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            CompiledSearch::compile("(", CaseSensitivity::Smart).unwrap_err(),
            SearchCompileError::InvalidRegex
        );

        let oversized = "x".repeat(MAX_SEARCH_QUERY_BYTES + 1);
        let error = CompiledSearch::compile(&oversized, CaseSensitivity::Smart).unwrap_err();
        assert_eq!(
            error,
            SearchCompileError::QueryTooLong {
                bytes: MAX_SEARCH_QUERY_BYTES + 1,
                max_bytes: MAX_SEARCH_QUERY_BYTES,
            }
        );
        assert!(!error.to_string().contains(&oversized));
        assert!(error.to_string().len() < 96);

        assert_eq!(
            CompiledSearch::compile(r"(?:\w?){10}\P{Letter}\b", CaseSensitivity::Always)
                .unwrap_err(),
            SearchCompileError::PatternTooComplex
        );
        assert!(
            CompiledSearch::compile(r"\pL{8}", CaseSensitivity::Always)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn signed_search_points_round_trip_scrollback_coordinates() {
        let point = SearchPoint::new(-42, 7);
        assert_eq!(SearchPoint::from(Point::from(point)), point);
    }

    #[test]
    fn compiled_search_returns_real_negative_history_coordinates() {
        let mut term = empty_term(12, 2);
        write_ascii(&mut term, 0, 1, "history");
        term.grid_mut().scroll_up::<Color>(&(Line(0)..Line(2)), 1);
        assert_eq!(term.grid().history_size(), 1);

        let mut search = CompiledSearch::compile("history", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let batch = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(-1, 0), SearchPoint::new(1, 11)),
            SearchDirection::Forward,
            4,
        );
        assert_eq!(
            batch.matches,
            vec![SearchSpan::new(
                SearchPoint::new(-1, 1),
                SearchPoint::new(-1, 7)
            )]
        );
    }

    #[test]
    fn logical_line_expansion_is_physically_bounded() {
        let mut term = empty_term(2, 300);
        for line in 0..299 {
            term.grid_mut()[Line(line)][Column(1)]
                .flags
                .insert(Flags::WRAPLINE);
        }
        let (right, right_truncated) =
            bounded_line_search_right(&term, SearchPoint::new(0, 1), 256);
        assert_eq!(right, SearchPoint::new(256, 1));
        assert!(right_truncated);

        let (left, left_truncated) = bounded_line_search_left(&term, SearchPoint::new(299, 0), 256);
        assert_eq!(left, SearchPoint::new(43, 0));
        assert!(left_truncated);

        let (clamped_right, _) =
            bounded_line_search_right(&term, SearchPoint::new(i32::MAX, usize::MAX), 1);
        assert_eq!(clamped_right, SearchPoint::new(299, 1));
        let (clamped_left, _) =
            bounded_line_search_left(&term, SearchPoint::new(i32::MIN, usize::MAX), 1);
        assert_eq!(clamped_left, SearchPoint::new(0, 0));
    }

    #[test]
    fn operation_budget_resumes_without_skipping_in_both_directions() {
        let mut term = empty_term(1, 600);
        term.grid_mut()[Line(0)][Column(0)].c = 'a';
        term.grid_mut()[Line(599)][Column(0)].c = 'z';
        let low = SearchPoint::new(0, 0);
        let high = SearchPoint::new(599, 0);

        let mut forward = CompiledSearch::compile("z", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let first_outcome = forward.find_next(&term, low, SearchDirection::Forward, false);
        assert_eq!(first_outcome.span, None);
        assert_eq!(first_outcome.continuation, Some(SearchPoint::new(256, 0)));
        assert!(!first_outcome.accuracy_limited);

        let mut cursor = low;
        let mut continuations = Vec::new();
        let mut matches = Vec::new();
        loop {
            let batch = forward.find_in_range(
                &term,
                SearchBounds::new(cursor, high),
                SearchDirection::Forward,
                8,
            );
            assert!(!batch.accuracy_limited);
            matches.extend(batch.matches);
            if let Some(next) = batch.continuation {
                continuations.push(next);
                cursor = next;
            } else {
                assert!(batch.exhausted);
                break;
            }
        }
        assert_eq!(
            continuations,
            vec![SearchPoint::new(256, 0), SearchPoint::new(512, 0)]
        );
        assert_eq!(
            matches,
            vec![SearchSpan::new(
                SearchPoint::new(599, 0),
                SearchPoint::new(599, 0)
            )]
        );

        let mut reverse = CompiledSearch::compile("a", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        cursor = high;
        continuations.clear();
        matches.clear();
        loop {
            let batch = reverse.find_in_range(
                &term,
                SearchBounds::new(cursor, low),
                SearchDirection::Reverse,
                8,
            );
            assert!(!batch.accuracy_limited);
            matches.extend(batch.matches);
            if let Some(next) = batch.continuation {
                continuations.push(next);
                cursor = next;
            } else {
                assert!(batch.exhausted);
                break;
            }
        }
        assert_eq!(
            continuations,
            vec![SearchPoint::new(343, 0), SearchPoint::new(87, 0)]
        );
        assert_eq!(
            matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 0),
                SearchPoint::new(0, 0)
            )]
        );
    }

    #[test]
    fn work_yield_never_splits_a_complete_soft_wrapped_line() {
        let mut forward_term = empty_term(1, 258);
        forward_term.grid_mut()[Line(256)][Column(0)].c = 'a';
        forward_term.grid_mut()[Line(256)][Column(0)]
            .flags
            .insert(Flags::WRAPLINE);
        forward_term.grid_mut()[Line(257)][Column(0)].c = 'b';
        let mut forward = CompiledSearch::compile("ab", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let first = forward.find_in_range(
            &forward_term,
            SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(257, 0)),
            SearchDirection::Forward,
            2,
        );
        assert_eq!(first.continuation, Some(SearchPoint::new(256, 0)));
        assert!(!first.accuracy_limited);
        let resumed = forward.find_in_range(
            &forward_term,
            SearchBounds::new(first.continuation.unwrap(), SearchPoint::new(257, 0)),
            SearchDirection::Forward,
            2,
        );
        assert_eq!(
            resumed.matches,
            vec![SearchSpan::new(
                SearchPoint::new(256, 0),
                SearchPoint::new(257, 0)
            )]
        );
        assert!(!resumed.accuracy_limited);

        let mut reverse_term = empty_term(1, 258);
        reverse_term.grid_mut()[Line(0)][Column(0)].c = 'a';
        reverse_term.grid_mut()[Line(0)][Column(0)]
            .flags
            .insert(Flags::WRAPLINE);
        reverse_term.grid_mut()[Line(1)][Column(0)].c = 'b';
        let mut reverse = CompiledSearch::compile("ab", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let first = reverse.find_in_range(
            &reverse_term,
            SearchBounds::new(SearchPoint::new(257, 0), SearchPoint::new(0, 0)),
            SearchDirection::Reverse,
            2,
        );
        assert_eq!(first.continuation, Some(SearchPoint::new(1, 0)));
        assert!(!first.accuracy_limited);
        let resumed = reverse.find_in_range(
            &reverse_term,
            SearchBounds::new(first.continuation.unwrap(), SearchPoint::new(0, 0)),
            SearchDirection::Reverse,
            2,
        );
        assert_eq!(
            resumed.matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 0),
                SearchPoint::new(1, 0)
            )]
        );
        assert!(!resumed.accuracy_limited);
    }

    #[test]
    fn byte_budget_resumes_an_ordinary_4k_sized_projection() {
        let mut term = empty_term(480, 216);
        term.grid_mut()[Line(0)][Column(0)].c = 'a';
        term.grid_mut()[Line(215)][Column(479)].c = 'z';
        let low = SearchPoint::new(0, 0);
        let high = SearchPoint::new(215, 479);

        let mut forward = CompiledSearch::compile("z", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let first = forward.find_in_range(
            &term,
            SearchBounds::new(low, high),
            SearchDirection::Forward,
            2,
        );
        assert_eq!(first.continuation, Some(SearchPoint::new(136, 0)));
        assert!(!first.accuracy_limited);
        let resumed = forward.find_in_range(
            &term,
            SearchBounds::new(first.continuation.unwrap(), high),
            SearchDirection::Forward,
            2,
        );
        assert_eq!(resumed.matches[0].start, SearchPoint::new(215, 479));

        let mut reverse = CompiledSearch::compile("a", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let first = reverse.find_in_range(
            &term,
            SearchBounds::new(high, low),
            SearchDirection::Reverse,
            2,
        );
        assert_eq!(first.continuation, Some(SearchPoint::new(79, 479)));
        assert!(!first.accuracy_limited);
        let resumed = reverse.find_in_range(
            &term,
            SearchBounds::new(first.continuation.unwrap(), low),
            SearchDirection::Reverse,
            2,
        );
        assert_eq!(resumed.matches[0].start, SearchPoint::new(0, 0));
    }

    #[test]
    fn oversized_combining_cell_is_atomic_and_blocks_later_rows() {
        let columns = MAX_SEARCH_MATERIALIZED_BYTES;
        let mut term = empty_term(columns, 2);
        let final_column = columns - 1;
        term.grid_mut()[Line(0)][Column(final_column)].c = 'e';
        term.grid_mut()[Line(0)][Column(final_column)].push_zerowidth('\u{0301}');
        term.grid_mut()[Line(1)][Column(0)].c = 'z';

        let mut haystack = SearchHaystack::default();
        materialize_chunk(
            &term,
            0,
            0,
            MaterializationLimits {
                bytes: MAX_SEARCH_MATERIALIZED_BYTES,
                cells: MAX_SEARCH_MATERIALIZED_CELLS,
            },
            &mut haystack,
        );
        assert!(haystack.truncated);
        assert!(!haystack.work_limited);
        assert_eq!(haystack.search_end, final_column);
        assert!(!haystack.text[..haystack.search_end].contains('e'));

        let mut search = CompiledSearch::compile("z", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let batch = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(1, final_column)),
            SearchDirection::Forward,
            2,
        );
        assert!(batch.matches.is_empty());
        assert!(batch.accuracy_limited);
        assert!(!batch.exhausted);
        assert_eq!(batch.continuation, None);
    }

    #[test]
    fn compiled_search_honors_all_case_modes() {
        let mut term = empty_term(11, 1);
        write_ascii(&mut term, 0, 0, "ERROR error");
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 10));

        let mut sensitive = CompiledSearch::compile("error", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let matches = sensitive
            .find_in_range(&term, bounds, SearchDirection::Forward, 4)
            .matches;
        assert_eq!(
            matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 6),
                SearchPoint::new(0, 10)
            )]
        );

        let mut insensitive = CompiledSearch::compile("Error", CaseSensitivity::Never)
            .unwrap()
            .unwrap();
        let matches = insensitive
            .find_in_range(&term, bounds, SearchDirection::Forward, 4)
            .matches;
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start, SearchPoint::new(0, 0));

        let mut smart = CompiledSearch::compile("error", CaseSensitivity::Smart)
            .unwrap()
            .unwrap();
        assert_eq!(
            smart
                .find_in_range(&term, bounds, SearchDirection::Forward, 4)
                .matches
                .len(),
            2
        );
    }

    #[test]
    fn navigation_is_directional_and_wrap_is_explicit() {
        let mut term = empty_term(12, 1);
        write_ascii(&mut term, 0, 0, "one");
        write_ascii(&mut term, 0, 7, "one");
        let mut search = CompiledSearch::compile("one", CaseSensitivity::Always)
            .unwrap()
            .unwrap();

        let next = search.find_next(
            &term,
            SearchPoint::new(0, 4),
            SearchDirection::Forward,
            false,
        );
        assert_eq!(next.span.unwrap().start, SearchPoint::new(0, 7));
        assert!(!next.wrapped);

        let previous = search.find_next(
            &term,
            SearchPoint::new(0, 6),
            SearchDirection::Reverse,
            false,
        );
        assert_eq!(previous.span.unwrap().start, SearchPoint::new(0, 0));
        assert!(!previous.wrapped);

        let edge = SearchPoint::new(0, 11);
        assert_eq!(
            search.find_next(&term, edge, SearchDirection::Forward, false),
            SearchOutcome {
                span: None,
                wrapped: false,
                accuracy_limited: false,
                continuation: None,
            }
        );
        let wrapped = search.find_next(&term, edge, SearchDirection::Forward, true);
        assert_eq!(wrapped.span.unwrap().start, SearchPoint::new(0, 0));
        assert!(wrapped.wrapped);

        let reverse = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(0, 11), SearchPoint::new(0, 0)),
            SearchDirection::Reverse,
            4,
        );
        assert_eq!(
            reverse
                .matches
                .iter()
                .map(|m| m.start.column)
                .collect::<Vec<_>>(),
            vec![7, 0]
        );
    }

    #[test]
    fn search_crosses_soft_wrap_but_not_hard_line_break() {
        let mut term = empty_term(3, 2);
        write_ascii(&mut term, 0, 0, "abc");
        write_ascii(&mut term, 1, 0, "def");
        term.grid_mut()[Line(0)][Column(2)]
            .flags
            .insert(Flags::WRAPLINE);
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(1, 2));
        let mut search = CompiledSearch::compile("cde", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        assert_eq!(
            search
                .find_in_range(&term, bounds, SearchDirection::Forward, 4)
                .matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 2),
                SearchPoint::new(1, 1)
            )]
        );

        term.grid_mut()[Line(0)][Column(2)]
            .flags
            .remove(Flags::WRAPLINE);
        assert!(
            search
                .find_in_range(&term, bounds, SearchDirection::Forward, 4)
                .matches
                .is_empty()
        );
    }

    #[test]
    fn range_edges_do_not_create_false_regex_anchors() {
        let mut term = empty_term(8, 2);
        write_ascii(&mut term, 0, 0, "xfoobar");
        let midline = SearchBounds::new(SearchPoint::new(0, 1), SearchPoint::new(0, 3));

        let mut start_anchor = CompiledSearch::compile("^foo", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        assert!(
            start_anchor
                .find_in_range(&term, midline, SearchDirection::Forward, 4)
                .matches
                .is_empty()
        );
        let mut end_anchor = CompiledSearch::compile("foo$", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        assert!(
            end_anchor
                .find_in_range(&term, midline, SearchDirection::Forward, 4)
                .matches
                .is_empty()
        );

        write_ascii(&mut term, 0, 0, "xxxxxxxr");
        write_ascii(&mut term, 1, 0, "abc");
        term.grid_mut()[Line(0)][Column(7)]
            .flags
            .insert(Flags::WRAPLINE);
        let second_row = SearchBounds::new(SearchPoint::new(1, 0), SearchPoint::new(1, 7));
        let mut false_soft_anchor = CompiledSearch::compile("^abc", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        assert!(
            false_soft_anchor
                .find_in_range(&term, second_row, SearchDirection::Forward, 4)
                .matches
                .is_empty()
        );

        let mut crossing = CompiledSearch::compile("rabc", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let forward = crossing.find_in_range(&term, second_row, SearchDirection::Forward, 4);
        assert_eq!(forward.matches.len(), 1);
        assert_eq!(forward.matches[0].start, SearchPoint::new(0, 7));
        let reverse = crossing.find_in_range(
            &term,
            SearchBounds::new(second_row.end, second_row.start),
            SearchDirection::Reverse,
            4,
        );
        assert_eq!(reverse.matches, forward.matches);
    }

    #[test]
    fn pathological_soft_wrap_splits_never_report_partial_matches() {
        let mut term = empty_term(1, 600);
        for line in 0..600 {
            term.grid_mut()[Line(line)][Column(0)].c = 'x';
            if line < 599 {
                term.grid_mut()[Line(line)][Column(0)]
                    .flags
                    .insert(Flags::WRAPLINE);
            }
        }
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(599, 0));
        for pattern in ["x+", "^x+", "x+$", "^x+$"] {
            let mut search = CompiledSearch::compile(pattern, CaseSensitivity::Always)
                .unwrap()
                .unwrap();
            let batch = search.find_in_range(&term, bounds, SearchDirection::Forward, 8);
            assert!(batch.matches.is_empty(), "partial match for {pattern:?}");
            assert!(batch.truncated);
            assert!(!batch.exhausted);
            let reverse = search.find_in_range(
                &term,
                SearchBounds::new(bounds.end, bounds.start),
                SearchDirection::Reverse,
                8,
            );
            assert!(
                reverse.matches.is_empty(),
                "reverse partial match for {pattern:?}"
            );
            assert!(reverse.accuracy_limited);
        }
        let mut navigation = CompiledSearch::compile("x+", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let outcome = navigation.find_next(&term, bounds.start, SearchDirection::Forward, true);
        assert!(outcome.span.is_none());
        assert!(outcome.accuracy_limited);
        assert!(!outcome.wrapped);

        // Short exact matches away from each artificial edge remain useful; the truncation bit
        // still tells callers that a longer cross-split expression may have been omitted.
        let mut literal = CompiledSearch::compile("x", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let batch = literal.find_in_range(&term, bounds, SearchDirection::Forward, 8);
        assert_eq!(batch.matches.len(), 8);
        assert!(batch.truncated);
    }

    #[test]
    fn variation_selector_zwj_and_wide_cells_search_both_directions() {
        let mut term = empty_term(5, 1);
        term.grid_mut()[Line(0)][Column(0)].c = '\u{2764}';
        term.grid_mut()[Line(0)][Column(0)].push_zerowidth('\u{fe0f}');
        term.grid_mut()[Line(0)][Column(0)].push_zerowidth('\u{200d}');
        term.grid_mut()[Line(0)][Column(1)].c = '\u{1f525}';
        term.grid_mut()[Line(0)][Column(1)]
            .flags
            .insert(Flags::WIDE_CHAR);
        term.grid_mut()[Line(0)][Column(2)]
            .flags
            .insert(Flags::WIDE_CHAR_SPACER);

        let mut search =
            CompiledSearch::compile("\u{2764}\u{fe0f}\u{200d}\u{1f525}", CaseSensitivity::Always)
                .unwrap()
                .unwrap();
        let forward = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 4)),
            SearchDirection::Forward,
            2,
        );
        assert_eq!(forward.matches.len(), 1);
        assert_eq!(forward.matches[0].start, SearchPoint::new(0, 0));
        assert_eq!(forward.matches[0].end, SearchPoint::new(0, 2));

        let reverse = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(0, 4), SearchPoint::new(0, 0)),
            SearchDirection::Reverse,
            2,
        );
        assert_eq!(reverse.matches, forward.matches);
    }

    #[test]
    fn wrapped_wide_glyph_span_includes_its_leading_spacer() {
        let mut term = empty_term(4, 2);
        write_ascii(&mut term, 0, 0, "xxx ");
        term.grid_mut()[Line(0)][Column(3)]
            .flags
            .insert(Flags::LEADING_WIDE_CHAR_SPACER | Flags::WRAPLINE);
        term.grid_mut()[Line(1)][Column(0)].c = '\u{1f987}';
        term.grid_mut()[Line(1)][Column(0)]
            .flags
            .insert(Flags::WIDE_CHAR);
        term.grid_mut()[Line(1)][Column(1)]
            .flags
            .insert(Flags::WIDE_CHAR_SPACER);
        term.grid_mut()[Line(1)][Column(2)].c = 'x';

        let expected = vec![SearchSpan::new(
            SearchPoint::new(0, 3),
            SearchPoint::new(1, 2),
        )];
        let mut search = CompiledSearch::compile("\u{1f987}x", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(1, 3));
        assert_eq!(
            search
                .find_in_range(&term, bounds, SearchDirection::Forward, 2)
                .matches,
            expected
        );
        assert_eq!(
            search
                .find_in_range(
                    &term,
                    SearchBounds::new(bounds.end, bounds.start),
                    SearchDirection::Reverse,
                    2,
                )
                .matches,
            expected
        );
    }

    #[test]
    fn bounded_scan_caps_materialization_and_can_cancel() {
        let mut term = empty_term(32, 1);
        write_ascii(&mut term, 0, 0, &"x".repeat(32));
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 31));
        let mut search = CompiledSearch::compile("x", CaseSensitivity::Always)
            .unwrap()
            .unwrap();

        let limited = search.find_in_range(&term, bounds, SearchDirection::Forward, 3);
        assert_eq!(limited.matches.len(), 3);
        assert!(limited.truncated);
        assert!(!limited.exhausted);

        let zero = search.find_in_range(&term, bounds, SearchDirection::Forward, 0);
        assert!(zero.matches.is_empty());
        assert!(zero.truncated);

        let cancelled =
            search
                .find_in_range_while(&term, bounds, SearchDirection::Forward, usize::MAX, || true);
        assert!(cancelled.cancelled);
        assert!(cancelled.matches.is_empty());
    }

    #[test]
    fn compiled_search_suppresses_empty_regex_matches() {
        let mut term = empty_term(7, 1);
        write_ascii(&mut term, 0, 0, "  aaa  ");
        let mut search = CompiledSearch::compile("a*", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        let batch = search.find_in_range(
            &term,
            SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 6)),
            SearchDirection::Forward,
            16,
        );
        assert_eq!(
            batch.matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 2),
                SearchPoint::new(0, 4)
            )]
        );
    }

    #[test]
    fn nullable_regexes_keep_leftmost_first_semantics_without_rescanning() {
        let mut term = empty_term(2, 1);
        write_ascii(&mut term, 0, 0, "xx");
        let bounds = SearchBounds::new(SearchPoint::new(0, 0), SearchPoint::new(0, 1));

        // The empty `a*` alternative wins at each position, so the UI-level zero-width filter
        // intentionally leaves no highlight instead of attempting an expensive alternate search.
        let mut shadowed = CompiledSearch::compile("a*|x", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        assert!(
            shadowed
                .find_in_range(&term, bounds, SearchDirection::Forward, 4)
                .matches
                .is_empty()
        );

        // A context-only zero-width result must not terminate iteration; a later ordinary match
        // is still returned.
        let mut later = CompiledSearch::compile(r"\b|x", CaseSensitivity::Always)
            .unwrap()
            .unwrap();
        assert_eq!(
            later
                .find_in_range(&term, bounds, SearchDirection::Forward, 4)
                .matches,
            vec![SearchSpan::new(
                SearchPoint::new(0, 1),
                SearchPoint::new(0, 1)
            )]
        );
    }

    #[test]
    fn scan_token_invalidates_on_revision_output_or_reflow() {
        let mut term = empty_term(8, 2);
        let token = SearchScanToken::capture(&term, 4, 9);
        assert!(token.is_current(&term, 4, 9));
        assert!(!token.is_current(&term, 5, 9));
        assert!(!token.is_current(&term, 4, 10));

        term.resize(TermSize {
            columns: 10,
            screen_lines: 2,
        });
        assert!(!token.is_current(&term, 4, 9));
    }

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

    /// Drift guard. The three CaseSensitivity modes drive
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
