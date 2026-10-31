//! Exports the `Term` type which is a high-level API for the Grid.

use std::ops::{Index, IndexMut, Range};
use std::sync::Arc;
use std::{cmp, mem, ptr, slice, str};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as Base64;
use bitflags::bitflags;
use log::{debug, trace};
use unicode_width::UnicodeWidthChar;

use crate::event::{Event, EventListener};
use crate::grid::{Dimensions, Grid, GridIterator, Scroll};
use crate::index::{self, Boundary, Column, Direction, Line, Point, Side};
use crate::selection::{Selection, SelectionRange, SelectionType};
use crate::term::cell::{Cell, Flags, LineLength};
use crate::term::color::Colors;
use crate::vi_mode::{ViModeCursor, ViMotion};
use crate::vte::ansi::{
    self, Attr, CharsetIndex, Color, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, ModifyOtherKeys, NamedColor, NamedMode, NamedPrivateMode,
    PrivateMode, Rgb, StandardCharset,
};

pub mod cell;
pub mod color;
mod rectangles;
pub mod search;

#[cfg(test)]
mod rectangle_tests;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ProtectionMode {
    #[default]
    Off,
    Dec,
    Iso,
}

/// Minimum number of columns.
///
/// A minimum of 2 is necessary to hold fullwidth unicode characters.
pub const MIN_COLUMNS: usize = 2;

/// Minimum number of visible lines.
pub const MIN_SCREEN_LINES: usize = 1;

/// Max size of the window title stack.
const TITLE_STACK_MAX_DEPTH: usize = 4096;

/// Default semantic escape characters.
pub const SEMANTIC_ESCAPE_CHARS: &str = ",│`|:\"' ()[]{}<>\t";

/// Max size of the keyboard modes.
const KEYBOARD_MODE_STACK_MAX_DEPTH: usize = TITLE_STACK_MAX_DEPTH;

/// Default tab interval, corresponding to terminfo `it` value.
const INITIAL_TABSTOPS: usize = 8;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct TermMode: u32 {
        const NONE                    = 0;
        const SHOW_CURSOR             = 1;
        const APP_CURSOR              = 1 << 1;
        const APP_KEYPAD              = 1 << 2;
        const MOUSE_REPORT_CLICK      = 1 << 3;
        const BRACKETED_PASTE         = 1 << 4;
        const SGR_MOUSE               = 1 << 5;
        const MOUSE_MOTION            = 1 << 6;
        const LINE_WRAP               = 1 << 7;
        const LINE_FEED_NEW_LINE      = 1 << 8;
        const ORIGIN                  = 1 << 9;
        const INSERT                  = 1 << 10;
        const FOCUS_IN_OUT            = 1 << 11;
        const ALT_SCREEN              = 1 << 12;
        const MOUSE_DRAG              = 1 << 13;
        const UTF8_MOUSE              = 1 << 14;
        const ALTERNATE_SCROLL        = 1 << 15;
        const VI                      = 1 << 16;
        const URGENCY_HINTS           = 1 << 17;
        const DISAMBIGUATE_ESC_CODES  = 1 << 18;
        const REPORT_EVENT_TYPES      = 1 << 19;
        const REPORT_ALTERNATE_KEYS   = 1 << 20;
        const REPORT_ALL_KEYS_AS_ESC  = 1 << 21;
        const REPORT_ASSOCIATED_TEXT  = 1 << 22;
        /// Effective xterm modifyOtherKeys level one.
        ///
        /// This is mutually exclusive with [`TermMode::MODIFY_OTHER_KEYS_2`].
        const MODIFY_OTHER_KEYS_1     = 1 << 23;
        /// Effective xterm modifyOtherKeys level two.
        ///
        /// This is mutually exclusive with [`TermMode::MODIFY_OTHER_KEYS_1`].
        const MODIFY_OTHER_KEYS_2     = 1 << 24;
        /// Whether a client has set, restored, disabled, or queried modifyOtherKeys.
        const MODIFY_OTHER_KEYS_NEGOTIATED = 1 << 25;
        /// Kettle's modified-Enter fallback before a client negotiates key encoding.
        const UNNEGOTIATED_MODIFIED_ENTER = 1 << 26;
        /// DEC mode 2031: report colour-scheme changes.
        const COLOR_SCHEME_REPORTS    = 1 << 27;
        const LEFT_RIGHT_MARGIN       = 1 << 28;
        const MOUSE_MODE              = Self::MOUSE_REPORT_CLICK.bits() | Self::MOUSE_MOTION.bits() | Self::MOUSE_DRAG.bits();
        /// All effective xterm modifyOtherKeys levels.
        const MODIFY_OTHER_KEYS       = Self::MODIFY_OTHER_KEYS_1.bits()
                                      | Self::MODIFY_OTHER_KEYS_2.bits();
        const KITTY_KEYBOARD_PROTOCOL = Self::DISAMBIGUATE_ESC_CODES.bits()
                                      | Self::REPORT_EVENT_TYPES.bits()
                                      | Self::REPORT_ALTERNATE_KEYS.bits()
                                      | Self::REPORT_ALL_KEYS_AS_ESC.bits()
                                      | Self::REPORT_ASSOCIATED_TEXT.bits();
         const ANY                    = u32::MAX;
    }
}

impl From<KeyboardModes> for TermMode {
    fn from(value: KeyboardModes) -> Self {
        let mut mode = Self::empty();

        let disambiguate_esc_codes = value.contains(KeyboardModes::DISAMBIGUATE_ESC_CODES);
        mode.set(TermMode::DISAMBIGUATE_ESC_CODES, disambiguate_esc_codes);

        let report_event_types = value.contains(KeyboardModes::REPORT_EVENT_TYPES);
        mode.set(TermMode::REPORT_EVENT_TYPES, report_event_types);

        let report_alternate_keys = value.contains(KeyboardModes::REPORT_ALTERNATE_KEYS);
        mode.set(TermMode::REPORT_ALTERNATE_KEYS, report_alternate_keys);

        let report_all_keys_as_esc = value.contains(KeyboardModes::REPORT_ALL_KEYS_AS_ESC);
        mode.set(TermMode::REPORT_ALL_KEYS_AS_ESC, report_all_keys_as_esc);

        let report_associated_text = value.contains(KeyboardModes::REPORT_ASSOCIATED_TEXT);
        mode.set(TermMode::REPORT_ASSOCIATED_TEXT, report_associated_text);

        mode
    }
}

impl Default for TermMode {
    fn default() -> TermMode {
        TermMode::SHOW_CURSOR
            | TermMode::LINE_WRAP
            | TermMode::ALTERNATE_SCROLL
            | TermMode::URGENCY_HINTS
    }
}

fn modify_other_keys_term_mode(mode: ModifyOtherKeys) -> TermMode {
    match mode {
        ModifyOtherKeys::Restore | ModifyOtherKeys::Disable => TermMode::empty(),
        ModifyOtherKeys::EnableExceptWellDefined => TermMode::MODIFY_OTHER_KEYS_1,
        ModifyOtherKeys::EnableAll => TermMode::MODIFY_OTHER_KEYS_2,
    }
}

fn unnegotiated_modified_enter_term_mode(enabled: bool) -> TermMode {
    if enabled {
        TermMode::UNNEGOTIATED_MODIFIED_ENTER
    } else {
        TermMode::empty()
    }
}

/// Convert a terminal point to a viewport relative point.
#[inline]
pub fn point_to_viewport(display_offset: usize, point: Point) -> Option<Point<usize>> {
    let viewport_line = point.line.0 + display_offset as i32;
    usize::try_from(viewport_line)
        .ok()
        .map(|line| Point::new(line, point.column))
}

/// Convert a viewport relative point to a terminal point.
#[inline]
pub fn viewport_to_point(display_offset: usize, point: Point<usize>) -> Point {
    let line = Line(point.line as i32) - display_offset;
    Point::new(line, point.column)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineDamageBounds {
    /// Damaged line number.
    pub line: usize,

    /// Leftmost damaged column.
    pub left: usize,

    /// Rightmost damaged column.
    pub right: usize,
}

impl LineDamageBounds {
    #[inline]
    pub fn new(line: usize, left: usize, right: usize) -> Self {
        Self { line, left, right }
    }

    #[inline]
    pub fn undamaged(line: usize, num_cols: usize) -> Self {
        Self {
            line,
            left: num_cols,
            right: 0,
        }
    }

    #[inline]
    pub fn reset(&mut self, num_cols: usize) {
        *self = Self::undamaged(self.line, num_cols);
    }

    #[inline]
    pub fn expand(&mut self, left: usize, right: usize) {
        self.left = cmp::min(self.left, left);
        self.right = cmp::max(self.right, right);
    }

    #[inline]
    pub fn is_damaged(&self) -> bool {
        self.left <= self.right
    }
}

/// Terminal damage information collected since the last [`Term::reset_damage`] call.
#[derive(Debug)]
pub enum TermDamage<'a> {
    /// The entire terminal is damaged.
    Full,

    /// Iterator over damaged lines in the terminal.
    Partial(TermDamageIterator<'a>),
}

/// Iterator over the terminal's viewport damaged lines.
#[derive(Clone, Debug)]
pub struct TermDamageIterator<'a> {
    line_damage: slice::Iter<'a, LineDamageBounds>,
    display_offset: usize,
}

impl<'a> TermDamageIterator<'a> {
    pub fn new(line_damage: &'a [LineDamageBounds], display_offset: usize) -> Self {
        let num_lines = line_damage.len();
        // Filter out invisible damage.
        let line_damage = &line_damage[..num_lines.saturating_sub(display_offset)];
        Self {
            display_offset,
            line_damage: line_damage.iter(),
        }
    }
}

impl Iterator for TermDamageIterator<'_> {
    type Item = LineDamageBounds;

    fn next(&mut self) -> Option<Self::Item> {
        self.line_damage.find_map(|line| {
            line.is_damaged().then_some(LineDamageBounds::new(
                line.line + self.display_offset,
                line.left,
                line.right,
            ))
        })
    }
}

/// State of the terminal damage.
struct TermDamageState {
    /// Hint whether terminal should be damaged entirely regardless of the actual damage changes.
    full: bool,

    /// Information about damage on terminal lines.
    lines: Vec<LineDamageBounds>,

    /// Old terminal cursor point.
    last_cursor: Point,
}

impl TermDamageState {
    fn new(num_cols: usize, num_lines: usize) -> Self {
        let lines = (0..num_lines)
            .map(|line| LineDamageBounds::undamaged(line, num_cols))
            .collect();

        Self {
            full: true,
            lines,
            last_cursor: Default::default(),
        }
    }

    #[inline]
    fn resize(&mut self, num_cols: usize, num_lines: usize) {
        // Reset point, so old cursor won't end up outside of the viewport.
        self.last_cursor = Default::default();
        self.full = true;

        self.lines.clear();
        self.lines.reserve(num_lines);
        for line in 0..num_lines {
            self.lines.push(LineDamageBounds::undamaged(line, num_cols));
        }
    }

    /// Damage point inside of the viewport.
    #[inline]
    fn damage_point(&mut self, point: Point<usize>) {
        self.damage_line(point.line, point.column.0, point.column.0);
    }

    /// Expand `line`'s damage to span at least `left` to `right` column.
    #[inline]
    fn damage_line(&mut self, line: usize, left: usize, right: usize) {
        self.lines[line].expand(left, right);
    }

    /// Reset information about terminal damage.
    fn reset(&mut self, num_cols: usize) {
        self.full = false;
        self.lines.iter_mut().for_each(|line| line.reset(num_cols));
    }
}

pub struct Term<T> {
    /// Terminal focus controlling the cursor shape.
    pub is_focused: bool,

    /// Cursor for keyboard selection.
    pub vi_mode_cursor: ViModeCursor,

    pub selection: Option<Selection>,

    /// Currently active grid.
    ///
    /// Tracks the screen buffer currently in use. While the alternate screen buffer is active,
    /// this will be the alternate grid. Otherwise it is the primary screen buffer.
    grid: Grid<Cell>,

    /// Currently inactive grid.
    ///
    /// Opposite of the active grid. While the alternate screen buffer is active, this will be the
    /// primary grid. Otherwise it is the alternate screen buffer.
    inactive_grid: Grid<Cell>,

    /// Index into `charsets`, pointing to what ASCII is currently being mapped to.
    active_charset: CharsetIndex,

    /// Tabstops.
    tabs: TabStops,

    /// Mode flags.
    mode: TermMode,

    /// Whether the colour scheme is dark, when known (DEC mode 2031).
    color_scheme_dark: Option<bool>,

    /// A `CSI ? 996 n` arrived while the scheme was unknown; it is answered
    /// as soon as the scheme is set.
    color_scheme_query_pending: bool,

    /// Scroll region.
    ///
    /// Range going from top to bottom of the terminal, indexed from the top of the viewport.
    scroll_region: Range<Line>,
    protection_mode: ProtectionMode,
    horizontal_margins: Range<Column>,
    conformance_level: u8,

    /// Modified terminal colors.
    colors: Colors,

    /// Current style of the cursor.
    cursor_style: Option<CursorStyle>,

    /// Proxy for sending events to the event loop.
    event_proxy: T,

    /// Current title of the window.
    title: Option<String>,

    /// Stack of saved window titles. When a title is popped from this stack, the `title` for the
    /// term is set.
    title_stack: Vec<Option<String>>,

    /// The stack for the keyboard modes.
    keyboard_mode_stack: Vec<KeyboardModes>,

    /// The active keyboard mode for the current screen.
    keyboard_mode: KeyboardModes,

    /// Currently inactive keyboard mode stack.
    inactive_keyboard_mode_stack: Vec<KeyboardModes>,

    /// The active keyboard mode for the inactive screen.
    inactive_keyboard_mode: KeyboardModes,

    /// Information about damaged cells.
    damage: TermDamageState,

    /// Config directly for the terminal.
    config: Config,

    /// Bounded, ordered terminal mutations consumed by graphics integrations.
    graphics_events: GraphicsEventJournal,
}

/// Maximum number of unconsumed graphics events retained by a terminal.
pub const GRAPHICS_EVENT_JOURNAL_CAPACITY: usize = 256;

/// Direction of a committed grid scroll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsScrollDirection {
    Up,
    Down,
}

/// A committed text-grid mutation that must be mirrored by terminal graphics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsEvent {
    SyncMarker {
        id: u64,
    },
    EraseDisplay,
    EnterAlternate {
        mode: u16,
        clear: bool,
    },
    LeaveAlternate {
        mode: u16,
        clear: bool,
    },
    Reset,
    PartialScroll {
        direction: GraphicsScrollDirection,
        top: usize,
        bottom: usize,
        left: usize,
        right: usize,
        lines: usize,
        screen_top: u64,
        screen_lines: usize,
    },
    Scroll {
        direction: GraphicsScrollDirection,
        columns: Option<(usize, usize)>,
        top: usize,
        bottom: usize,
        lines: usize,
        old_screen_top: u64,
        new_screen_top: u64,
        screen_lines: usize,
    },
}

/// One ordered journal drain.
#[derive(Debug, PartialEq, Eq)]
pub struct GraphicsEventBatch {
    pub events: Vec<GraphicsEvent>,
    pub overflowed: bool,
    pub alternate_screen: bool,
}

struct GraphicsEventJournal {
    events: Vec<GraphicsEvent>,
    overflowed: bool,
}

impl GraphicsEventJournal {
    fn new() -> Self {
        Self {
            events: Vec::with_capacity(GRAPHICS_EVENT_JOURNAL_CAPACITY),
            overflowed: false,
        }
    }

    fn push(&mut self, event: GraphicsEvent) {
        if self.overflowed {
            return;
        }

        if let (
            Some(GraphicsEvent::Scroll {
                direction: previous_direction,
                columns: previous_columns,
                top: previous_top,
                bottom: previous_bottom,
                lines: previous_lines,
                new_screen_top: previous_screen_top,
                screen_lines: previous_screen_lines,
                ..
            }),
            GraphicsEvent::Scroll {
                direction,
                columns,
                top,
                bottom,
                lines,
                old_screen_top,
                new_screen_top,
                screen_lines,
                ..
            },
        ) = (self.events.last_mut(), event)
            && *previous_direction == direction
            && *previous_columns == columns
            && *previous_top == top
            && *previous_bottom == bottom
            && *previous_screen_top == old_screen_top
            && *previous_screen_lines == screen_lines
        {
            *previous_lines = previous_lines
                .saturating_add(lines)
                .min(bottom.saturating_sub(top));
            *previous_screen_top = new_screen_top;
            return;
        }

        if let (
            Some(GraphicsEvent::PartialScroll {
                direction: previous_direction,
                top: previous_top,
                bottom: previous_bottom,
                left: previous_left,
                right: previous_right,
                lines: previous_lines,
                screen_top: previous_screen_top,
                screen_lines: previous_screen_lines,
            }),
            GraphicsEvent::PartialScroll {
                direction,
                top,
                bottom,
                left,
                right,
                lines,
                screen_top,
                screen_lines,
            },
        ) = (self.events.last_mut(), event)
            && *previous_direction == direction
            && *previous_top == top
            && *previous_bottom == bottom
            && *previous_left == left
            && *previous_right == right
            && *previous_screen_top == screen_top
            && *previous_screen_lines == screen_lines
        {
            *previous_lines = previous_lines
                .saturating_add(lines)
                .min(bottom.saturating_sub(top));
            return;
        }

        if self.events.len() == GRAPHICS_EVENT_JOURNAL_CAPACITY {
            self.events.clear();
            self.overflowed = true;
            return;
        }
        self.events.push(event);
    }

    fn drain(&mut self, alternate_screen: bool) -> GraphicsEventBatch {
        let events = if self.overflowed {
            self.events.clear();
            Vec::new()
        } else {
            mem::replace(
                &mut self.events,
                Vec::with_capacity(GRAPHICS_EVENT_JOURNAL_CAPACITY),
            )
        };
        let overflowed = mem::take(&mut self.overflowed);
        GraphicsEventBatch {
            events,
            overflowed,
            alternate_screen,
        }
    }
}

/// Configuration options for the [`Term`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The maximum amount of scrolling history.
    pub scrolling_history: usize,

    /// Default cursor style to reset the cursor to.
    pub default_cursor_style: CursorStyle,

    /// Cursor style for Vi mode.
    pub vi_mode_cursor_style: Option<CursorStyle>,

    /// The characters which terminate semantic selection.
    ///
    /// The default value is [`SEMANTIC_ESCAPE_CHARS`].
    pub semantic_escape_chars: String,

    /// Whether to enable kitty keyboard protocol.
    pub kitty_keyboard: bool,

    /// Whether Kettle distinguishes modified Enter before keyboard negotiation.
    pub unnegotiated_modified_enter: bool,

    /// OSC52 support mode.
    pub osc52: Osc52,

    /// The `name(version)` XTVERSION (`CSI > q`) reports, such as
    /// `kettle(4.9.0)`; `None` leaves the query unanswered.
    pub xtversion: Option<String>,

    /// Whether the colour scheme is dark, for `CSI ? 996 n` and mode 2031;
    /// `None` leaves both silent. Only seeds a new terminal: change it with
    /// [`Term::set_color_scheme`].
    pub color_scheme_dark: Option<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrolling_history: 10000,
            semantic_escape_chars: SEMANTIC_ESCAPE_CHARS.to_owned(),
            default_cursor_style: Default::default(),
            vi_mode_cursor_style: Default::default(),
            kitty_keyboard: Default::default(),
            unnegotiated_modified_enter: true,
            osc52: Default::default(),
            xtversion: None,
            color_scheme_dark: None,
        }
    }
}

/// OSC 52 behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(rename_all = "lowercase")
)]
pub enum Osc52 {
    /// The handling of the escape sequence is disabled.
    Disabled,
    /// Only copy sequence is accepted.
    ///
    /// This option is the default as a compromise between entirely
    /// disabling it (the most secure) and allowing `paste` (the less secure).
    #[default]
    OnlyCopy,
    /// Only paste sequence is accepted.
    OnlyPaste,
    /// Both are accepted.
    CopyPaste,
}

impl<T> Term<T> {
    #[inline]
    pub fn scroll_display(&mut self, scroll: Scroll)
    where
        T: EventListener,
    {
        let old_display_offset = self.grid.display_offset();
        self.grid.scroll_display(scroll);
        self.event_proxy.send_event(Event::MouseCursorDirty);

        // Clamp vi mode cursor to the viewport.
        let viewport_start = -(self.grid.display_offset() as i32);
        let viewport_end = viewport_start + self.bottommost_line().0;
        let vi_cursor_line = &mut self.vi_mode_cursor.point.line.0;
        *vi_cursor_line = cmp::min(viewport_end, cmp::max(viewport_start, *vi_cursor_line));
        self.vi_mode_recompute_selection();

        // Damage everything if display offset changed.
        if old_display_offset != self.grid().display_offset() {
            self.mark_fully_damaged();
        }
    }

    pub fn new<D: Dimensions>(config: Config, dimensions: &D, event_proxy: T) -> Term<T> {
        let num_cols = dimensions.columns();
        let num_lines = dimensions.screen_lines();

        let history_size = config.scrolling_history;
        let grid = Grid::new(num_lines, num_cols, history_size);
        let inactive_grid = Grid::new(num_lines, num_cols, 0);

        let tabs = TabStops::new(grid.columns());

        let scroll_region = Line(0)..Line(grid.screen_lines() as i32);

        // Initialize terminal damage, covering the entire terminal upon launch.
        let damage = TermDamageState::new(num_cols, num_lines);
        let mode = TermMode::default()
            | unnegotiated_modified_enter_term_mode(config.unnegotiated_modified_enter);

        Term {
            inactive_grid,
            scroll_region,
            protection_mode: ProtectionMode::Off,
            horizontal_margins: Column(0)..Column(grid.columns()),
            conformance_level: 5,
            event_proxy,
            damage,
            color_scheme_dark: config.color_scheme_dark,
            color_scheme_query_pending: false,
            config,
            grid,
            tabs,
            inactive_keyboard_mode_stack: Default::default(),
            inactive_keyboard_mode: Default::default(),
            keyboard_mode_stack: Default::default(),
            keyboard_mode: Default::default(),
            active_charset: Default::default(),
            vi_mode_cursor: Default::default(),
            cursor_style: Default::default(),
            colors: color::Colors::default(),
            title_stack: Default::default(),
            is_focused: Default::default(),
            selection: Default::default(),
            title: Default::default(),
            mode,
            graphics_events: GraphicsEventJournal::new(),
        }
    }

    /// Drain committed graphics-relevant mutations in terminal execution order.
    pub fn take_graphics_events(&mut self) -> GraphicsEventBatch {
        self.graphics_events
            .drain(self.mode.contains(TermMode::ALT_SCREEN))
    }

    fn graphics_screen_top(&self) -> u64 {
        self.grid
            .history_origin()
            .saturating_add(u64::try_from(self.grid.history_size()).unwrap_or(u64::MAX))
    }

    /// Collect the information about the changes in the lines, which
    /// could be used to minimize the amount of drawing operations.
    ///
    /// The user controlled elements, like `Vi` mode cursor and `Selection` are **not** part of the
    /// collected damage state. Those could easily be tracked by comparing their old and new
    /// value between adjacent frames.
    ///
    /// After reading damage [`reset_damage`] should be called.
    ///
    /// [`reset_damage`]: Self::reset_damage
    #[must_use]
    pub fn damage(&mut self) -> TermDamage<'_> {
        // Ensure the entire terminal is damaged after entering insert mode.
        // Leaving is handled in the ansi handler.
        if self.mode.contains(TermMode::INSERT) {
            self.mark_fully_damaged();
        }

        let previous_cursor = mem::replace(&mut self.damage.last_cursor, self.grid.cursor.point);

        if self.damage.full {
            return TermDamage::Full;
        }

        // Add information about old cursor position and new one if they are not the same, so we
        // cover everything that was produced by `Term::input`.
        if self.damage.last_cursor != previous_cursor {
            // Cursor coordinates are always inside viewport even if you have `display_offset`.
            let point = Point::new(previous_cursor.line.0 as usize, previous_cursor.column);
            self.damage.damage_point(point);
        }

        // Always damage current cursor.
        self.damage_cursor();

        // NOTE: damage which changes all the content when the display offset is non-zero (e.g.
        // scrolling) is handled via full damage.
        let display_offset = self.grid().display_offset();
        TermDamage::Partial(TermDamageIterator::new(&self.damage.lines, display_offset))
    }

    /// Resets the terminal damage information.
    pub fn reset_damage(&mut self) {
        self.damage.reset(self.columns());
    }

    #[inline]
    fn mark_fully_damaged(&mut self) {
        self.damage.full = true;
    }

    #[inline]
    fn normalize_selection_to_grid(&mut self) {
        let topmost_line = self.topmost_line();
        let bottommost_line = self.bottommost_line();
        self.selection = self
            .selection
            .take()
            .filter(|selection| selection.intersects_range(topmost_line..=bottommost_line));
    }

    /// Set new options for the [`Term`].
    pub fn set_options(&mut self, options: Config)
    where
        T: EventListener,
    {
        self.set_options_inner(options, false);
    }

    /// Apply resize options without evicting history until output scrolls.
    pub fn set_options_preserving_history(&mut self, options: Config)
    where
        T: EventListener,
    {
        self.set_options_inner(options, true);
    }

    fn set_options_inner(&mut self, options: Config, preserve_history: bool)
    where
        T: EventListener,
    {
        let old_config = mem::replace(&mut self.config, options);

        let title_event = match &self.title {
            Some(title) => Event::Title(title.clone()),
            None => Event::ResetTitle,
        };

        self.event_proxy.send_event(title_event);

        let grid = if self.mode.contains(TermMode::ALT_SCREEN) {
            &mut self.inactive_grid
        } else {
            &mut self.grid
        };
        if preserve_history {
            grid.update_history_on_growth(self.config.scrolling_history);
        } else if self.config.scrolling_history != old_config.scrolling_history {
            grid.update_history(self.config.scrolling_history);
        }
        self.normalize_selection_to_grid();

        if self.config.kitty_keyboard != old_config.kitty_keyboard {
            self.keyboard_mode_stack = Vec::new();
            self.inactive_keyboard_mode_stack = Vec::new();
            self.keyboard_mode = KeyboardModes::NO_MODE;
            self.inactive_keyboard_mode = KeyboardModes::NO_MODE;
            self.mode.remove(TermMode::KITTY_KEYBOARD_PROTOCOL);
        }

        if self.config.unnegotiated_modified_enter != old_config.unnegotiated_modified_enter {
            // A config reload changes only Kettle's fallback; replacing the
            // application-selected level would break an active protocol.
            self.mode.set(
                TermMode::UNNEGOTIATED_MODIFIED_ENTER,
                self.config.unnegotiated_modified_enter,
            );
        }

        // Damage everything on config updates.
        self.mark_fully_damaged();
    }

    /// Convert the active selection to a String.
    pub fn selection_to_string(&self) -> Option<String> {
        let selection_range = self.selection.as_ref().and_then(|s| s.to_range(self))?;
        let SelectionRange { start, end, .. } = selection_range;

        let mut res = String::new();

        match self.selection.as_ref() {
            Some(Selection {
                ty: SelectionType::Block,
                ..
            }) => {
                for line in (start.line.0..end.line.0).map(Line::from) {
                    res += self
                        .line_to_string(line, start.column..end.column, start.column.0 != 0)
                        .trim_end();
                    res += "\n";
                }

                res += self
                    .line_to_string(end.line, start.column..end.column, true)
                    .trim_end();
            }
            Some(Selection {
                ty: SelectionType::Lines,
                ..
            }) => {
                res = self.bounds_to_string(start, end) + "\n";
            }
            _ => {
                res = self.bounds_to_string(start, end);
            }
        }

        Some(res)
    }

    /// Convert range between two points to a String.
    pub fn bounds_to_string(&self, start: Point, end: Point) -> String {
        let mut res = String::new();

        for line in (start.line.0..=end.line.0).map(Line::from) {
            let start_col = if line == start.line {
                start.column
            } else {
                Column(0)
            };
            let end_col = if line == end.line {
                end.column
            } else {
                self.last_column()
            };

            res += &self.line_to_string(line, start_col..end_col, line == end.line);
        }

        res.strip_suffix('\n').map(str::to_owned).unwrap_or(res)
    }

    /// Convert a single line in the grid to a String.
    fn line_to_string(
        &self,
        line: Line,
        mut cols: Range<Column>,
        include_wrapped_wide: bool,
    ) -> String {
        let mut text = String::new();

        let grid_line = &self.grid[line];
        let line_length = cmp::min(grid_line.line_length(), cols.end + 1);

        // Include wide char when trailing spacer is selected.
        if grid_line[cols.start]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            cols.start -= 1;
        }

        let mut tab_mode = false;
        for column in (cols.start.0..line_length.0).map(Column::from) {
            let cell = &grid_line[column];

            // Skip over cells until next tab-stop once a tab was found.
            if tab_mode {
                if self.tabs[column] || cell.c != ' ' {
                    tab_mode = false;
                } else {
                    continue;
                }
            }

            if cell.c == '\t' {
                tab_mode = true;
            }

            if !cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                // Push cells primary character.
                text.push(cell.c);

                // Push zero-width characters.
                for c in cell.zerowidth().into_iter().flatten() {
                    text.push(*c);
                }
            }
        }

        if cols.end >= self.columns() - 1
            && (line_length.0 == 0
                || !self.grid[line][line_length - 1]
                    .flags
                    .contains(Flags::WRAPLINE))
        {
            text.push('\n');
        }

        // If wide char is not part of the selection, but leading spacer is, include it.
        if line_length == self.columns()
            && line_length.0 >= 2
            && grid_line[line_length - 1]
                .flags
                .contains(Flags::LEADING_WIDE_CHAR_SPACER)
            && include_wrapped_wide
            && line < self.bottommost_line()
        {
            let cell = &self.grid[line + 1i32][Column(0)];
            text.push(cell.c);
            for c in cell.zerowidth().into_iter().flatten() {
                text.push(*c);
            }
        }

        text
    }

    /// Terminal content required for rendering.
    #[inline]
    pub fn renderable_content(&self) -> RenderableContent<'_>
    where
        T: EventListener,
    {
        RenderableContent::new(self)
    }

    /// Access to the raw grid data structure.
    pub fn grid(&self) -> &Grid<Cell> {
        &self.grid
    }

    /// Mutable access to the raw grid data structure.
    pub fn grid_mut(&mut self) -> &mut Grid<Cell> {
        &mut self.grid
    }

    /// Resize terminal to new dimensions.
    pub fn resize<S: Dimensions>(&mut self, size: S) {
        let old_cols = self.columns();
        let old_lines = self.screen_lines();

        let num_cols = size.columns();
        let num_lines = size.screen_lines();

        if old_cols == num_cols && old_lines == num_lines {
            debug!("Term::resize dimensions unchanged");
            return;
        }

        debug!("New num_cols is {num_cols} and num_lines is {num_lines}");

        // Move vi mode cursor with the content.
        let history_size = self.history_size();
        let mut delta = num_lines as i32 - old_lines as i32;
        let min_delta = cmp::min(0, num_lines as i32 - self.grid.cursor.point.line.0 - 1);
        delta = cmp::min(cmp::max(delta, min_delta), history_size as i32);
        self.vi_mode_cursor.point.line += delta;

        let is_alt = self.mode.contains(TermMode::ALT_SCREEN);
        self.grid.resize(!is_alt, num_lines, num_cols);
        self.inactive_grid.resize(is_alt, num_lines, num_cols);

        // Invalidate selection and tabs only when necessary.
        if old_cols != num_cols {
            self.selection = None;

            // Recreate tabs list.
            self.tabs.resize(num_cols);
        } else if let Some(selection) = self.selection.take() {
            let max_lines = cmp::max(num_lines, old_lines) as i32;
            let range = Line(0)..Line(max_lines);
            self.selection = selection.rotate(self, &range, -delta);
        }
        self.normalize_selection_to_grid();

        // Clamp vi cursor to viewport.
        let vi_point = self.vi_mode_cursor.point;
        let viewport_top = Line(-(self.grid.display_offset() as i32));
        let viewport_bottom = viewport_top + self.bottommost_line();
        self.vi_mode_cursor.point.line =
            cmp::max(cmp::min(vi_point.line, viewport_bottom), viewport_top);
        self.vi_mode_cursor.point.column = cmp::min(vi_point.column, self.last_column());

        // Reset scrolling region.
        self.scroll_region = Line(0)..Line(self.screen_lines() as i32);
        self.horizontal_margins = Column(0)..Column(self.columns());

        // Resize damage information.
        self.damage.resize(num_cols, num_lines);
    }

    /// Active terminal modes.
    #[inline]
    pub fn mode(&self) -> &TermMode {
        &self.mode
    }

    /// Swap primary and alternate screen buffer.
    pub fn swap_alt(&mut self) {
        self.swap_alt_with_clear(true, true);
    }

    /// Swap primary and alternate screen buffer without clearing on entry.
    fn swap_alt_preserving(&mut self) {
        self.swap_alt_with_clear(false, false);
    }

    fn swap_alt_with_clear(&mut self, clear_on_entry: bool, save_restore_cursor: bool) {
        if !self.mode.contains(TermMode::ALT_SCREEN) {
            // Set alt screen cursor to the current primary screen cursor.
            self.inactive_grid.cursor = self.grid.cursor.clone();

            if save_restore_cursor {
                // Mode 1049 saves the cursor as DECSC does, origin mode
                // included, replacing the primary screen's saved cursor.
                self.grid.saved_cursor = self.grid.cursor.clone();
                self.grid.saved_origin_mode = self.mode.contains(TermMode::ORIGIN);
            }

            if clear_on_entry {
                // Reset alternate screen contents.
                self.inactive_grid.reset_region(..);
            }
        } else if !save_restore_cursor {
            // Modes 47 and 1047 switch buffers without DECSC/DECRC, so the
            // current cursor state follows the active screen in both directions.
            self.inactive_grid.cursor = self.grid.cursor.clone();
        }

        mem::swap(
            &mut self.keyboard_mode_stack,
            &mut self.inactive_keyboard_mode_stack,
        );
        mem::swap(&mut self.keyboard_mode, &mut self.inactive_keyboard_mode);
        self.set_keyboard_term_mode(self.keyboard_mode.into());

        let leaving_with_restore = save_restore_cursor && self.mode.contains(TermMode::ALT_SCREEN);
        mem::swap(&mut self.grid, &mut self.inactive_grid);
        self.mode ^= TermMode::ALT_SCREEN;
        if leaving_with_restore {
            // Resetting 1049 restores the cursor as DECRC does; the primary
            // grid kept its cursor, so origin mode comes back here.
            self.restore_saved_origin_mode();
        }
        self.selection = None;
        self.mark_fully_damaged();
    }

    fn horizontal_region(&self) -> Range<Column> {
        if self.mode.contains(TermMode::LEFT_RIGHT_MARGIN) {
            self.horizontal_margins.clone()
        } else {
            Column(0)..Column(self.columns())
        }
    }

    fn cursor_in_horizontal_region(&self) -> bool {
        self.horizontal_region()
            .contains(&self.grid.cursor.point.column)
    }

    fn input_right_edge(&self) -> Column {
        let margins = self.horizontal_region();
        if self.grid.cursor.point.column < margins.end {
            margins.end
        } else {
            Column(self.columns())
        }
    }

    /// Restore the saved origin mode (DECOM), keeping the cursor inside the
    /// current margins under it, as xterm's `CursorRestore` does.
    fn restore_saved_origin_mode(&mut self) {
        self.mode.set(TermMode::ORIGIN, self.grid.saved_origin_mode);
        if self.grid.saved_origin_mode {
            let line = &mut self.grid.cursor.point.line;
            *line = (*line).clamp(self.scroll_region.start, self.scroll_region.end - 1);
            let margins = self.horizontal_region();
            self.grid.cursor.point.column = self
                .grid
                .cursor
                .point
                .column
                .clamp(margins.start, margins.end - 1);
        }
    }

    /// Partial-width scrolls cannot rotate rows into history or rotate a whole-line selection.
    fn scroll_partial_region(&mut self, origin: Line, lines: usize, down: bool) -> bool {
        let columns = self.horizontal_region();
        if columns.start == 0 && columns.end == self.columns() {
            return false;
        }
        let bottom = self.scroll_region.end;
        let height = (bottom - origin).0 as usize;
        let lines = lines.min(height);
        let bg = self.grid.cursor.template.bg;
        let selection = self.selection.take().filter(|selection| {
            let Some(range) = selection.to_range(self) else {
                return true;
            };
            !(origin.0.max(range.start.line.0)..bottom.0.min(range.end.line.0 + 1)).any(|line| {
                let left = if range.is_block || line == range.start.line.0 {
                    range.start.column
                } else {
                    Column(0)
                };
                let right = if range.is_block || line == range.end.line.0 {
                    range.end.column + 1
                } else {
                    Column(self.columns())
                };
                let row = &self.grid[Line(line)];
                let affected_left = if columns.start > 0
                    && row[columns.start].flags.contains(Flags::WIDE_CHAR_SPACER)
                {
                    columns.start - 1
                } else {
                    columns.start
                };
                let affected_right = if columns.end < self.columns()
                    && row[columns.end - 1].flags.contains(Flags::WIDE_CHAR)
                {
                    columns.end + 1
                } else {
                    columns.end
                };
                left < affected_right && right > affected_left
            })
        });
        for offset in 0..height {
            let offset = if down { height - 1 - offset } else { offset };
            let line = origin + offset;
            let source = if down {
                offset.checked_sub(lines)
            } else {
                offset.checked_add(lines).filter(|&source| source < height)
            };
            // A boundary-straddling wide glyph is cleared as a unit, including
            // its outside half, as xterm's scrollInMargins does.
            // Erase split wide glyphs at both boundaries before replacing this slice.
            let row = source.map(|source| self.grid[origin + source][columns.clone()].to_vec());
            self.erase_cells(line, columns.start, columns.end);
            if let Some(mut row) = row {
                if row[0].flags.contains(Flags::WIDE_CHAR_SPACER) {
                    row[0] = bg.into();
                }
                let last = row.len() - 1;
                if row[last].flags.contains(Flags::WIDE_CHAR) {
                    row[last] = bg.into();
                }
                for cell in &mut row {
                    cell.flags
                        .remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
                }
                self.grid[line][columns.clone()].clone_from_slice(&row);
            }
        }
        self.selection = selection;
        if (origin..bottom).contains(&self.vi_mode_cursor.point.line)
            && columns.contains(&self.vi_mode_cursor.point.column)
        {
            let line = self.vi_mode_cursor.point.line;
            self.vi_mode_cursor.point.line = if down {
                cmp::min(line + lines, bottom - 1)
            } else {
                cmp::max(line - lines, origin)
            };
        }
        self.graphics_events.push(GraphicsEvent::PartialScroll {
            direction: if down {
                GraphicsScrollDirection::Down
            } else {
                GraphicsScrollDirection::Up
            },
            top: origin.0 as usize,
            bottom: bottom.0 as usize,
            left: columns.start.0,
            right: columns.end.0,
            lines,
            screen_top: self.graphics_screen_top(),
            screen_lines: self.screen_lines(),
        });
        self.mark_fully_damaged();
        true
    }

    /// Scroll screen down.
    ///
    /// Text moves down; clear at bottom
    /// Expects origin to be in scroll range.
    #[inline]
    fn scroll_down_relative(&mut self, origin: Line, mut lines: usize) {
        trace!("Scrolling down relative: origin={origin}, lines={lines}");

        lines = cmp::min(
            lines,
            (self.scroll_region.end - self.scroll_region.start).0 as usize,
        );
        lines = cmp::min(lines, (self.scroll_region.end - origin).0 as usize);
        if lines == 0 {
            return;
        }

        if lines > 0 && self.scroll_partial_region(origin, lines, true) {
            return;
        }

        let region = origin..self.scroll_region.end;
        let old_screen_top = self.graphics_screen_top();
        let screen_lines = self.screen_lines();

        // Scroll selection.
        self.selection = self
            .selection
            .take()
            .and_then(|s| s.rotate(self, &region, -(lines as i32)));

        // Scroll vi mode cursor.
        let line = &mut self.vi_mode_cursor.point.line;
        if region.start <= *line && region.end > *line {
            *line = cmp::min(*line + lines, region.end - 1);
        }

        // Scroll between origin and bottom
        self.grid.scroll_down(&region, lines);
        self.mark_fully_damaged();
        let new_screen_top = self.graphics_screen_top();
        self.graphics_events.push(GraphicsEvent::Scroll {
            columns: self.mode.contains(TermMode::LEFT_RIGHT_MARGIN).then_some((
                self.horizontal_margins.start.0,
                self.horizontal_margins.end.0,
            )),
            direction: GraphicsScrollDirection::Down,
            top: region.start.0 as usize,
            bottom: region.end.0 as usize,
            lines,
            old_screen_top,
            new_screen_top,
            screen_lines,
        });
    }

    /// Scroll screen up
    ///
    /// Text moves up; clear at top
    /// Expects origin to be in scroll range.
    #[inline]
    fn scroll_up_relative(&mut self, origin: Line, mut lines: usize) {
        trace!("Scrolling up relative: origin={origin}, lines={lines}");

        lines = cmp::min(
            lines,
            (self.scroll_region.end - self.scroll_region.start).0 as usize,
        );

        if lines > 0 && self.scroll_partial_region(origin, lines, false) {
            return;
        }

        let region = origin..self.scroll_region.end;
        lines = cmp::min(lines, (region.end - region.start).0 as usize);
        if lines == 0 {
            return;
        }
        let old_screen_top = self.graphics_screen_top();
        let screen_lines = self.screen_lines();

        // Scroll selection.
        self.selection = self
            .selection
            .take()
            .and_then(|s| s.rotate(self, &region, lines as i32));

        self.grid.scroll_up(&region, lines);

        // Drop selections only after the grid has updated its retained history.
        // Checking before the scroll would discard valid selections while the
        // scrollback buffer is still growing.
        self.normalize_selection_to_grid();

        // Scroll vi mode cursor.
        let viewport_top = Line(-(self.grid.display_offset() as i32));
        let top = if region.start == 0 {
            viewport_top
        } else {
            region.start
        };
        let line = &mut self.vi_mode_cursor.point.line;
        if (top <= *line) && region.end > *line {
            *line = cmp::max(*line - lines, top);
        }
        self.mark_fully_damaged();
        let new_screen_top = self.graphics_screen_top();
        self.graphics_events.push(GraphicsEvent::Scroll {
            columns: self.mode.contains(TermMode::LEFT_RIGHT_MARGIN).then_some((
                self.horizontal_margins.start.0,
                self.horizontal_margins.end.0,
            )),
            direction: GraphicsScrollDirection::Up,
            top: region.start.0 as usize,
            bottom: region.end.0 as usize,
            lines,
            old_screen_top,
            new_screen_top,
            screen_lines,
        });
    }

    fn deccolm(&mut self)
    where
        T: EventListener,
    {
        // Setting 132 column font makes no sense, but run the other side effects.
        // Clear scrolling region.
        self.mode.remove(TermMode::LEFT_RIGHT_MARGIN);
        self.horizontal_margins = Column(0)..Column(self.columns());
        self.set_scrolling_region(1, None);

        // Clear grid.
        self.grid.reset_region(..);
        self.mark_fully_damaged();
    }

    #[inline]
    pub fn exit(&mut self)
    where
        T: EventListener,
    {
        self.event_proxy.send_event(Event::Exit);
    }

    /// Toggle the vi mode.
    #[inline]
    pub fn toggle_vi_mode(&mut self)
    where
        T: EventListener,
    {
        self.mode ^= TermMode::VI;

        if self.mode.contains(TermMode::VI) {
            let display_offset = self.grid.display_offset() as i32;
            if self.grid.cursor.point.line > self.bottommost_line() - display_offset {
                // Move cursor to top-left if terminal cursor is not visible.
                let point = Point::new(Line(-display_offset), Column(0));
                self.vi_mode_cursor = ViModeCursor::new(point);
            } else {
                // Reset vi mode cursor position to match primary cursor.
                self.vi_mode_cursor = ViModeCursor::new(self.grid.cursor.point);
            }
        }

        // Update UI about cursor blinking state changes.
        self.event_proxy.send_event(Event::CursorBlinkingChange);
    }

    /// Move vi mode cursor.
    #[inline]
    pub fn vi_motion(&mut self, motion: ViMotion)
    where
        T: EventListener,
    {
        // Require vi mode to be active.
        if !self.mode.contains(TermMode::VI) {
            return;
        }

        // Move cursor.
        self.vi_mode_cursor = self.vi_mode_cursor.motion(self, motion);
        self.vi_mode_recompute_selection();
    }

    /// Move vi cursor to a point in the grid.
    #[inline]
    pub fn vi_goto_point(&mut self, point: Point)
    where
        T: EventListener,
    {
        // Move viewport to make point visible.
        self.scroll_to_point(point);

        // Move vi cursor to the point.
        self.vi_mode_cursor.point = point;

        self.vi_mode_recompute_selection();
    }

    /// Update the active selection to match the vi mode cursor position.
    #[inline]
    fn vi_mode_recompute_selection(&mut self) {
        // Require vi mode to be active.
        if !self.mode.contains(TermMode::VI) {
            return;
        }

        // Update only if non-empty selection is present.
        if let Some(selection) = self.selection.as_mut().filter(|s| !s.is_empty()) {
            selection.update(self.vi_mode_cursor.point, Side::Left);
            selection.include_all();
        }
    }

    /// Scroll display to point if it is outside of viewport.
    pub fn scroll_to_point(&mut self, point: Point)
    where
        T: EventListener,
    {
        let display_offset = self.grid.display_offset() as i32;
        let screen_lines = self.grid.screen_lines() as i32;

        if point.line < -display_offset {
            let lines = point.line + display_offset;
            self.scroll_display(Scroll::Delta(-lines.0));
        } else if point.line >= (screen_lines - display_offset) {
            let lines = point.line + display_offset - screen_lines + 1i32;
            self.scroll_display(Scroll::Delta(-lines.0));
        }
    }

    /// Jump to the end of a wide cell.
    pub fn expand_wide(&self, mut point: Point, direction: Direction) -> Point {
        let flags = self.grid[point.line][point.column].flags;

        match direction {
            Direction::Right if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) => {
                point.column = Column(1);
                point.line += 1;
            }
            Direction::Right if flags.contains(Flags::WIDE_CHAR) => {
                point.column = cmp::min(point.column + 1, self.last_column());
            }
            Direction::Left if flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) => {
                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                    point.column -= 1;
                }

                let prev = point.sub(self, Boundary::Grid, 1);
                if self.grid[prev]
                    .flags
                    .contains(Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    point = prev;
                }
            }
            _ => (),
        }

        point
    }

    #[inline]
    pub fn semantic_escape_chars(&self) -> &str {
        &self.config.semantic_escape_chars
    }

    #[cfg(test)]
    pub(crate) fn set_semantic_escape_chars(&mut self, semantic_escape_chars: &str) {
        self.config.semantic_escape_chars = semantic_escape_chars.into();
    }

    /// Active terminal cursor style.
    ///
    /// While vi mode is active, this will automatically return the vi mode cursor style.
    #[inline]
    pub fn cursor_style(&self) -> CursorStyle {
        let cursor_style = self
            .cursor_style
            .unwrap_or(self.config.default_cursor_style);

        if self.mode.contains(TermMode::VI) {
            self.config.vi_mode_cursor_style.unwrap_or(cursor_style)
        } else {
            cursor_style
        }
    }

    pub fn colors(&self) -> &Colors {
        &self.colors
    }

    /// Insert a linebreak at the current cursor position.
    #[inline]
    fn wrapline(&mut self)
    where
        T: EventListener,
    {
        if !self.mode.contains(TermMode::LINE_WRAP) {
            return;
        }

        trace!("Wrapping input");

        let margins = self.horizontal_region();
        if margins.start == 0 && margins.end == self.columns() {
            self.grid.cursor_cell().flags.insert(Flags::WRAPLINE);
        }

        if self.grid.cursor.point.line + 1 >= self.scroll_region.end {
            self.linefeed();
        } else {
            self.damage_cursor();
            self.grid.cursor.point.line += 1;
        }

        self.grid.cursor.point.column = self.horizontal_region().start;
        self.grid.cursor.input_needs_wrap = false;
        self.damage_cursor();
    }

    /// Remove a wide glyph bisected by an editing boundary before moving cells.
    fn clear_wide_boundary(&mut self, line: Line, boundary: Column) {
        if boundary > 0
            && boundary < self.columns()
            && self.grid[line][boundary]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        {
            self.erase_cells(line, boundary, boundary + 1);
        }
    }

    /// Editing through a wide glyph must not leave an unpaired lead or spacer.
    fn repair_wide_cells(&mut self, line: Line) {
        let bg = self.grid.cursor.template.bg;
        let columns = self.columns();
        for col in 0..columns {
            let cell = &self.grid[line][Column(col)];
            let invalid = if cell.flags.contains(Flags::WIDE_CHAR) {
                col + 1 == columns
                    || !self.grid[line][Column(col + 1)]
                        .flags
                        .contains(Flags::WIDE_CHAR_SPACER)
            } else if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                col == 0
                    || !self.grid[line][Column(col - 1)]
                        .flags
                        .contains(Flags::WIDE_CHAR)
            } else {
                false
            };
            if invalid {
                self.grid[line][Column(col)] = bg.into();
            }
        }
    }

    fn clear_line_cells(&mut self, mode: ansi::LineClearMode, selective: bool) {
        trace!("Clearing line: {mode:?}");

        let cursor = &self.grid.cursor;
        let point = cursor.point;

        let (left, right) = match mode {
            ansi::LineClearMode::Right => (point.column, Column(self.columns())),
            ansi::LineClearMode::Left => (Column(0), point.column + 1),
            ansi::LineClearMode::All => (Column(0), Column(self.columns())),
        };

        self.erase_protected_cells(point.line, left, right, selective);

        let range = self.grid.cursor.point.line..=self.grid.cursor.point.line;
        self.selection = self.selection.take().filter(|s| !s.intersects_range(range));
    }

    fn clear_protected_screen(&mut self, mode: ansi::ClearMode, selective: bool) {
        let point = self.grid.cursor.point;
        let columns = Column(self.columns());
        let bottom = Line(self.screen_lines() as i32 - 1);
        let (first, last, left, right) = match mode {
            ansi::ClearMode::Above => (Line(0), point.line, Column(0), point.column + 1),
            ansi::ClearMode::Below => (point.line, bottom, point.column, columns),
            ansi::ClearMode::All => (Line(0), bottom, Column(0), columns),
            ansi::ClearMode::Saved => return,
        };
        for line in first.0..=last.0 {
            let start = if line == first.0 { left } else { Column(0) };
            let end = if line == last.0 { right } else { columns };
            self.erase_protected_cells(Line(line), start, end, selective);
        }
        self.selection = self
            .selection
            .take()
            .filter(|selection| !selection.intersects_range(first..=last));
        self.mark_fully_damaged();
        if matches!(mode, ansi::ClearMode::All) {
            self.graphics_events.push(GraphicsEvent::EraseDisplay);
        }
    }

    fn erase_protected_cells(
        &mut self,
        line: Line,
        mut start: Column,
        mut end: Column,
        selective: bool,
    ) {
        let respect = self.protection_mode == ProtectionMode::Iso
            || (selective && self.protection_mode == ProtectionMode::Dec);
        if !respect {
            self.erase_cells(line, start, end);
            return;
        }
        if start >= end {
            return;
        }
        let columns = self.columns();
        let bg = self.grid.cursor.template.bg;
        let row = &mut self.grid[line];
        if start > 0 && row[start].flags.contains(Flags::WIDE_CHAR_SPACER) {
            start -= 1;
        }
        if end < columns && row[end - 1].flags.contains(Flags::WIDE_CHAR) {
            end += 1;
        }
        self.damage.damage_line(line.0 as usize, start.0, end.0 - 1);
        for col in start.0..end.0 {
            let flags = row[Column(col)].flags;
            let protected = flags.contains(Flags::PROTECTED)
                || (flags.contains(Flags::WIDE_CHAR)
                    && col + 1 < columns
                    && row[Column(col + 1)].flags.contains(Flags::PROTECTED))
                || (flags.contains(Flags::WIDE_CHAR_SPACER)
                    && col > 0
                    && row[Column(col - 1)].flags.contains(Flags::PROTECTED));
            if !protected {
                row[Column(col)] = bg.into();
            }
        }
    }

    fn erase_cells(&mut self, line: Line, mut start: Column, mut end: Column) {
        if start >= end {
            return;
        }

        let columns = self.columns();
        let bg = self.grid.cursor.template.bg;
        let row = &mut self.grid[line];
        if start > 0 && row[start].flags.contains(Flags::WIDE_CHAR_SPACER) {
            start -= 1;
        }
        if end < columns && row[end - 1].flags.contains(Flags::WIDE_CHAR) {
            end += 1;
        }

        self.damage.damage_line(line.0 as usize, start.0, end.0 - 1);
        for cell in &mut row[start..end] {
            *cell = bg.into();
        }
    }

    /// Write `c` to the cell at the cursor position.
    #[inline(always)]
    fn write_at_cursor(&mut self, c: char) {
        let c = self.grid.cursor.charsets[self.active_charset].map(c);
        let fg = self.grid.cursor.template.fg;
        let bg = self.grid.cursor.template.bg;
        let flags = self.grid.cursor.template.flags;
        let extra = self.grid.cursor.template.extra.clone();

        let mut cursor_cell = self.grid.cursor_cell();

        // Clear all related cells when overwriting a fullwidth cell.
        if cursor_cell
            .flags
            .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
        {
            // Remove wide char and spacer.
            let wide = cursor_cell.flags.contains(Flags::WIDE_CHAR);
            let point = self.grid.cursor.point;
            if wide && point.column < self.last_column() {
                let spacer = &mut self.grid[point.line][point.column + 1];
                spacer.clear_wide();
                spacer.flags.remove(Flags::WIDE_CHAR_SPACER);
            } else if point.column > 0 {
                self.grid[point.line][point.column - 1].clear_wide();
            }

            // Remove leading spacers.
            if point.column <= 1 && point.line != self.topmost_line() {
                let column = self.last_column();
                let spacer = &mut self.grid[point.line - 1i32][column];
                if spacer.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    spacer
                        .flags
                        .remove(Flags::LEADING_WIDE_CHAR_SPACER | Flags::PROTECTED);
                }
            }

            cursor_cell = self.grid.cursor_cell();
        }

        cursor_cell.c = c;
        cursor_cell.fg = fg;
        cursor_cell.bg = bg;
        cursor_cell.flags = flags;
        cursor_cell.extra = extra;
    }

    #[inline]
    fn damage_cursor(&mut self) {
        // The normal cursor coordinates are always in viewport.
        let point = Point::new(
            self.grid.cursor.point.line.0 as usize,
            self.grid.cursor.point.column,
        );
        self.damage.damage_point(point);
    }

    #[inline]
    fn set_keyboard_term_mode(&mut self, mode: TermMode) {
        self.mode &= !TermMode::KITTY_KEYBOARD_PROTOCOL;
        trace!("Setting keyboard mode to {mode:?}");
        self.mode |= mode;
    }

    /// The terminal's colours changed: record whether the scheme is dark
    /// (`None` when unknown) and report it at once to a program that turned on
    /// DEC mode 2031, even when it stayed dark or light. Such programs (Claude
    /// Code, Neovim) take the report as a cue to re-read the background with
    /// OSC 11 and classify it themselves, so a dark-to-dark switch matters too.
    /// The caller decides when the colours changed; a program that did not
    /// turn the mode on asks with `CSI ? 996 n`.
    pub fn set_color_scheme(&mut self, dark: Option<bool>)
    where
        T: EventListener,
    {
        self.color_scheme_dark = dark;
        // A query that came while the scheme was unknown is answered now, once,
        // whether or not the program also turned on mode 2031.
        let pending = dark.is_some() && std::mem::take(&mut self.color_scheme_query_pending);
        if pending || self.mode.contains(TermMode::COLOR_SCHEME_REPORTS) {
            if let Some(text) = self.color_scheme_report() {
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
        }
    }

    /// `CSI ? 997 ; 1 n` for a dark scheme, `; 2 n` for light.
    fn color_scheme_report(&self) -> Option<String> {
        self.color_scheme_dark
            .map(|dark| format!("\x1b[?997;{}n", if dark { 1 } else { 2 }))
    }

    /// The 1-based cursor position a cursor position report gives. Under
    /// origin mode (DECOM) the row counts from the top margin, as xterm and
    /// kitty report it, so a program can feed it back to CUP unchanged.
    /// Under origin mode the cursor stays inside the margins (DECRC clamps a
    /// restored row into them, as xterm does); should it ever sit above the
    /// top margin, the row reports as 1, the row CUP would put it on, rather
    /// than as zero or negative.
    fn cursor_report_position(&self) -> (i32, usize) {
        let point = self.grid.cursor.point;
        let top = if self.mode.contains(TermMode::ORIGIN) {
            self.scroll_region.start.0
        } else {
            0
        };
        let left = if self.mode.contains(TermMode::ORIGIN) {
            self.horizontal_region().start.0
        } else {
            0
        };
        (
            (point.line.0 - top).max(0) + 1,
            point.column.0.saturating_sub(left) + 1,
        )
    }
}

impl<T> Dimensions for Term<T> {
    #[inline]
    fn columns(&self) -> usize {
        self.grid.columns()
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.grid.screen_lines()
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.grid.total_lines()
    }
}

impl<T: EventListener> Handler for Term<T> {
    fn sync_marker(&mut self, id: u64) {
        self.graphics_events.push(GraphicsEvent::SyncMarker { id });
    }

    /// A character to be displayed.
    #[inline(never)]
    fn input(&mut self, c: char) {
        // Number of cells the char will occupy.
        let width = match c.width() {
            Some(width) => width,
            None => return,
        };

        // Handle zero-width characters.
        if width == 0 {
            // Get previous column.
            let mut column = self.grid.cursor.point.column;
            if !self.grid.cursor.input_needs_wrap {
                column.0 = column.saturating_sub(1);
            }

            // Put zerowidth characters over first fullwidth character cell.
            let line = self.grid.cursor.point.line;
            if self.grid[line][column]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
            {
                column.0 = column.saturating_sub(1);
            }

            self.grid[line][column].push_zerowidth(c);
            return;
        }

        // Move cursor to next line.
        if self.grid.cursor.input_needs_wrap {
            self.wrapline();
        }

        // Wrap a wide glyph before reserving insertion space on its destination row.
        if width > 1 && self.grid.cursor.point.column + 1 >= self.input_right_edge() {
            if self.mode.contains(TermMode::LINE_WRAP) {
                // Insert placeholder before wide char if glyph does not fit in this row.
                let margins = self.horizontal_region();
                if margins.start == 0 && margins.end == self.columns() {
                    self.grid
                        .cursor
                        .template
                        .flags
                        .insert(Flags::LEADING_WIDE_CHAR_SPACER);
                }
                self.write_at_cursor(' ');
                self.grid
                    .cursor
                    .template
                    .flags
                    .remove(Flags::LEADING_WIDE_CHAR_SPACER);
                self.wrapline();
            } else {
                // Prevent out of bounds crash when linewrapping is disabled.
                self.grid.cursor.input_needs_wrap = true;
                return;
            }
        }

        if self.mode.contains(TermMode::INSERT) {
            self.insert_blank(width);
        }
        let columns = self.input_right_edge().0;
        if width == 1 {
            self.write_at_cursor(c);
        } else {
            // Write full width glyph to current cursor cell.
            self.grid.cursor.template.flags.insert(Flags::WIDE_CHAR);
            self.write_at_cursor(c);
            self.grid.cursor.template.flags.remove(Flags::WIDE_CHAR);

            // Write spacer to cell following the wide glyph.
            self.grid.cursor.point.column += 1;
            self.grid
                .cursor
                .template
                .flags
                .insert(Flags::WIDE_CHAR_SPACER);
            self.write_at_cursor(' ');
            self.grid
                .cursor
                .template
                .flags
                .remove(Flags::WIDE_CHAR_SPACER);
        }

        if self.grid.cursor.point.column + 1 < columns {
            self.grid.cursor.point.column += 1;
        } else {
            self.grid.cursor.input_needs_wrap = true;
        }
    }

    #[inline]
    fn decaln(&mut self) {
        trace!("Decalnning");

        // DECALN also resets the margins and homes the cursor (DEC STD 070,
        // xterm), so a test pattern is never drawn into a stale scroll region.
        self.scroll_region = Line(0)..Line(self.screen_lines() as i32);
        self.horizontal_margins = Column(0)..Column(self.columns());
        self.goto(0, 0);

        for line in (0..self.screen_lines()).map(Line::from) {
            for column in 0..self.columns() {
                let cell = &mut self.grid[line][Column(column)];
                *cell = Cell::default();
                cell.c = 'E';
            }
        }

        self.mark_fully_damaged();
    }

    #[inline]
    fn goto(&mut self, line: i32, col: usize) {
        let line = Line(line);
        let col = Column(col);

        trace!("Going to: line={line}, col={col}");
        let (y_offset, max_y) = if self.mode.contains(TermMode::ORIGIN) {
            (self.scroll_region.start, self.scroll_region.end - 1)
        } else {
            (Line(0), self.bottommost_line())
        };

        self.damage_cursor();
        self.grid.cursor.point.line = cmp::max(cmp::min(line + y_offset, max_y), Line(0));
        let margins = self.horizontal_region();
        self.grid.cursor.point.column = if self.mode.contains(TermMode::ORIGIN) {
            cmp::min(col + margins.start.0, margins.end - 1)
        } else {
            cmp::min(col, self.last_column())
        };
        self.damage_cursor();
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn goto_line(&mut self, line: i32) {
        trace!("Going to line: {line}");
        let col = self.grid.cursor.point.column;
        self.goto(line, 0);
        self.grid.cursor.point.column = col;
        self.damage_cursor();
    }

    #[inline]
    fn goto_col(&mut self, col: usize) {
        trace!("Going to column: {col}");
        self.damage_cursor();
        let margins = self.horizontal_region();
        self.grid.cursor.point.column = if self.mode.contains(TermMode::ORIGIN) {
            cmp::min(Column(col) + margins.start.0, margins.end - 1)
        } else {
            cmp::min(Column(col), self.last_column())
        };
        self.damage_cursor();
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn insert_blank(&mut self, count: usize) {
        if !self.cursor_in_horizontal_region() {
            return;
        }
        let columns = self.horizontal_region().end.0;
        let cursor = &self.grid.cursor;
        let bg = cursor.template.bg;

        // Ensure inserting within terminal bounds
        let count = cmp::min(count, columns - cursor.point.column.0);

        let source = cursor.point.column;
        let destination = cursor.point.column.0 + count;
        let num_cells = columns - destination;

        let line = cursor.point.line;
        if count == 0 {
            return;
        }
        self.clear_wide_boundary(line, source);
        self.clear_wide_boundary(line, Column(columns - count));
        self.clear_wide_boundary(line, Column(columns));
        self.damage
            .damage_line(line.0 as usize, 0, self.columns() - 1);

        let row = &mut self.grid[line][..];

        for offset in (0..num_cells).rev() {
            row.swap(destination + offset, source.0 + offset);
        }

        // Cells were just moved out toward the end of the line;
        // fill in between source and dest with blanks.
        for cell in &mut row[source.0..destination] {
            *cell = bg.into();
        }
        self.repair_wide_cells(line);
    }

    #[inline]
    fn move_up(&mut self, lines: usize) {
        trace!("Moving up: {lines}");

        // The top margin stops the cursor whenever it starts at or below it,
        // with or without origin mode; from above the margin it stops at the
        // top line (VT510 CUU, xterm's CursorUp).
        let line = self.grid.cursor.point.line;
        let top = if line >= self.scroll_region.start {
            self.scroll_region.start
        } else {
            Line(0)
        };
        self.damage_cursor();
        self.grid.cursor.point.line = cmp::max(line - lines, top);
        self.damage_cursor();
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn move_down(&mut self, lines: usize) {
        trace!("Moving down: {lines}");

        // The bottom margin stops the cursor whenever it starts at or above
        // it, with or without origin mode; from below the margin it stops at
        // the bottom line (VT510 CUD, xterm's CursorDown).
        let line = self.grid.cursor.point.line;
        let bottom = if line < self.scroll_region.end {
            self.scroll_region.end - 1
        } else {
            self.bottommost_line()
        };
        self.damage_cursor();
        self.grid.cursor.point.line = cmp::min(line + lines, bottom);
        self.damage_cursor();
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn move_forward(&mut self, cols: usize) {
        trace!("Moving forward: {cols}");
        let point = self.grid.cursor.point.column;
        let margins = self.horizontal_region();
        let limit = if point < margins.end {
            margins.end - 1
        } else {
            self.last_column()
        };
        let last_column = cmp::min(point + cols, limit);

        let cursor_line = self.grid.cursor.point.line.0 as usize;
        self.damage
            .damage_line(cursor_line, self.grid.cursor.point.column.0, last_column.0);

        self.grid.cursor.point.column = last_column;
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn move_backward(&mut self, cols: usize) {
        trace!("Moving backward: {cols}");
        let point = self.grid.cursor.point.column;
        let left = self.horizontal_region().start;
        let limit = if point >= left { left.0 } else { 0 };
        let column = point.saturating_sub(cols).max(limit);

        let cursor_line = self.grid.cursor.point.line.0 as usize;
        self.damage
            .damage_line(cursor_line, column, self.grid.cursor.point.column.0);

        self.grid.cursor.point.column = Column(column);
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => {
                trace!("Reporting primary device attributes");
                let text = String::from("\x1b[?6c");
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
            Some('>') => {
                trace!("Reporting secondary device attributes");
                let version = version_number(env!("CARGO_PKG_VERSION"));
                let text = format!("\x1b[>0;{version};1c");
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
            _ => debug!("Unsupported device attributes intermediate"),
        }
    }

    #[inline]
    fn report_keyboard_mode(&mut self) {
        if !self.config.kitty_keyboard {
            return;
        }

        trace!("Reporting active keyboard mode");
        let current_mode = self.keyboard_mode.bits();
        let text = format!("\x1b[?{current_mode}u");
        self.event_proxy.send_event(Event::PtyWrite(text));
    }

    #[inline]
    fn set_modify_other_keys(&mut self, mode: ModifyOtherKeys) {
        trace!("Setting modifyOtherKeys mode to {mode:?}");
        self.mode.remove(TermMode::MODIFY_OTHER_KEYS);
        self.mode.insert(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED);
        self.mode.insert(modify_other_keys_term_mode(mode));
    }

    #[inline]
    fn report_modify_other_keys(&mut self) {
        trace!("Reporting modifyOtherKeys mode");
        // A probing client has entered the xterm negotiation path, so Kettle's
        // fallback must stop producing bytes outside the reported level zero.
        self.mode.insert(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED);
        let level = if self.mode.contains(TermMode::MODIFY_OTHER_KEYS_2) {
            2
        } else if self.mode.contains(TermMode::MODIFY_OTHER_KEYS_1) {
            1
        } else {
            0
        };

        // XTerm's ctlseqs documents XTQMODKEYS replies as an XTMODKEYS
        // control, so keep the modifyOtherKeys resource selector before the
        // effective value the application can restore.
        let text = format!("\x1b[>4;{level}m");
        self.event_proxy.send_event(Event::PtyWrite(text));
    }

    #[inline]
    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        if !self.config.kitty_keyboard {
            return;
        }

        trace!("Pushing `{mode:?}` keyboard mode into the stack");

        if self.keyboard_mode_stack.len() >= KEYBOARD_MODE_STACK_MAX_DEPTH {
            let removed = self.keyboard_mode_stack.remove(0);
            trace!(
                "Removing '{removed:?}' from bottom of keyboard mode stack that exceeds its \
                 maximum depth"
            );
        }

        self.keyboard_mode_stack.push(mode);
        self.keyboard_mode = mode;
        self.set_keyboard_term_mode(mode.into());
    }

    #[inline]
    fn pop_keyboard_modes(&mut self, to_pop: u16) {
        if !self.config.kitty_keyboard {
            return;
        }

        trace!("Attempting to pop {to_pop} keyboard modes from the stack");
        let new_len = self
            .keyboard_mode_stack
            .len()
            .saturating_sub(to_pop as usize);
        self.keyboard_mode_stack.truncate(new_len);

        // Reload active mode.
        let mode = self
            .keyboard_mode_stack
            .last()
            .copied()
            .unwrap_or(KeyboardModes::NO_MODE);
        self.keyboard_mode = mode;
        self.set_keyboard_term_mode(mode.into());
    }

    #[inline]
    fn set_keyboard_mode(&mut self, mode: KeyboardModes, apply: KeyboardModesApplyBehavior) {
        if !self.config.kitty_keyboard {
            return;
        }

        self.keyboard_mode = match apply {
            KeyboardModesApplyBehavior::Replace => mode,
            KeyboardModesApplyBehavior::Union => self.keyboard_mode.union(mode),
            KeyboardModesApplyBehavior::Difference => self.keyboard_mode.difference(mode),
        };

        if let Some(active_mode) = self.keyboard_mode_stack.last_mut() {
            *active_mode = self.keyboard_mode;
        }

        self.set_keyboard_term_mode(self.keyboard_mode.into());
    }

    #[inline]
    fn device_status(&mut self, arg: usize) {
        trace!("Reporting device status: {arg}");
        match arg {
            5 => {
                let text = String::from("\x1b[0n");
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
            6 => {
                let (line, column) = self.cursor_report_position();
                let text = format!("\x1b[{line};{column}R");
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
            _ => debug!("unknown device status query: {arg}"),
        };
    }

    #[inline]
    fn device_status_private(&mut self, arg: usize) {
        trace!("Reporting private device status: {arg}");
        match arg {
            // DECXCPR: the cursor position with DEC's `?` marker, which a
            // program can tell apart from a function key with modifiers.
            6 => {
                let (line, column) = self.cursor_report_position();
                let text = format!("\x1b[?{line};{column}R");
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
            // The colour-scheme query, answered while the scheme is known.
            996 => match self.color_scheme_report() {
                Some(text) => self.event_proxy.send_event(Event::PtyWrite(text)),
                // Answered by `set_color_scheme` once the scheme is known, so
                // a program asking early at startup still hears back.
                None => self.color_scheme_query_pending = true,
            },
            _ => debug!("unknown private device status query: {arg}"),
        }
    }

    #[inline]
    fn report_version(&mut self) {
        match &self.config.xtversion {
            Some(version) => {
                trace!("Reporting terminal version {version}");
                let text = format!("\x1bP>|{version}\x1b\\");
                self.event_proxy.send_event(Event::PtyWrite(text));
            }
            None => debug!("XTVERSION left unanswered"),
        }
    }

    #[inline]
    fn request_setting(&mut self, request: Option<&[u8]>) {
        trace!("DECRQSS {request:?}");
        let setting = match request {
            Some(b"m") => Some(format!("{}m", sgr_report(&self.grid.cursor.template))),
            Some(b"\"q") => Some(format!(
                "{}\"q",
                u8::from(self.grid.cursor.template.flags.contains(Flags::PROTECTED))
            )),
            Some(b"\"p") if self.conformance_level == 1 => Some(String::from("61\"p")),
            Some(b"\"p") => Some(format!("6{};1\"p", self.conformance_level)),
            Some(b"s") => Some(format!(
                "{};{}s",
                self.horizontal_margins.start.0 + 1,
                self.horizontal_margins.end.0
            )),
            Some(b"r") => Some(format!(
                "{};{}r",
                self.scroll_region.start.0 + 1,
                self.scroll_region.end.0
            )),
            Some(b" q") => {
                let style = self
                    .cursor_style
                    .unwrap_or(self.config.default_cursor_style);
                Some(format!("{} q", decscusr_number(style)))
            }
            _ => None,
        };
        // An unknown request is answered as invalid and never echoed back.
        let text = match setting {
            Some(setting) => format!("\x1bP1$r{setting}\x1b\\"),
            None => String::from("\x1bP0$r\x1b\\"),
        };
        self.event_proxy.send_event(Event::PtyWrite(text));
    }

    #[inline]
    fn request_termcap(&mut self, request: Option<&[u8]>) {
        trace!("XTGETTCAP {request:?}");
        let Some(request) = request else {
            self.event_proxy
                .send_event(Event::PtyWrite(String::from("\x1bP0+r\x1b\\")));
            return;
        };
        // One reply per name, as kitty and Ghostty send them, so a reader
        // that matches one capability per reply (Neovim's) sees each of them.
        // xterm instead joins known names into one reply and stops at the
        // first unknown one. All the replies go out in one write.
        let text: String = request
            .split(|&byte| byte == b';')
            .take(TERMCAP_NAMES_MAX)
            .map(termcap_reply)
            .collect();
        self.event_proxy.send_event(Event::PtyWrite(text));
    }

    #[inline]
    fn move_down_and_cr(&mut self, lines: usize) {
        trace!("Moving down and cr: {lines}");

        self.move_down(lines);
        self.carriage_return();
    }

    #[inline]
    fn move_up_and_cr(&mut self, lines: usize) {
        trace!("Moving up and cr: {lines}");

        self.move_up(lines);
        self.carriage_return();
    }

    /// Insert tab at cursor position.
    #[inline]
    fn put_tab(&mut self, mut count: u16) {
        // A tab after the last column is the same as a linebreak.
        if self.grid.cursor.input_needs_wrap {
            self.wrapline();
            return;
        }

        let columns = self.input_right_edge().0;
        while self.grid.cursor.point.column < columns && count != 0 {
            count -= 1;

            let c = self.grid.cursor.charsets[self.active_charset].map('\t');
            let cell = self.grid.cursor_cell();
            if cell.c == ' ' {
                cell.c = c;
            }

            loop {
                if (self.grid.cursor.point.column + 1) == columns {
                    break;
                }

                self.grid.cursor.point.column += 1;

                if self.tabs[self.grid.cursor.point.column] {
                    break;
                }
            }
        }
    }

    /// Backspace.
    #[inline]
    fn backspace(&mut self) {
        trace!("Backspace");

        let col = self.grid.cursor.point.column;
        let left = self.horizontal_region().start;
        if col > Column(0) && col != left {
            let line = self.grid.cursor.point.line.0 as usize;
            let column = self.grid.cursor.point.column.0;
            self.grid.cursor.point.column -= 1;
            self.grid.cursor.input_needs_wrap = false;
            self.damage.damage_line(line, column - 1, column);
        }
    }

    /// Carriage return.
    #[inline]
    fn carriage_return(&mut self) {
        trace!("Carriage return");
        let left = self.horizontal_region().start;
        let new_col =
            if self.mode.contains(TermMode::ORIGIN) || self.grid.cursor.point.column >= left {
                left.0
            } else {
                0
            };
        let line = self.grid.cursor.point.line.0 as usize;
        self.damage
            .damage_line(line, new_col, self.grid.cursor.point.column.0);
        self.grid.cursor.point.column = Column(new_col);
        self.grid.cursor.input_needs_wrap = false;
    }

    /// Linefeed.
    #[inline]
    fn linefeed(&mut self) {
        trace!("Linefeed");
        let next = self.grid.cursor.point.line + 1;
        if next == self.scroll_region.end {
            if self.cursor_in_horizontal_region() {
                self.scroll_up(1);
            }
        } else if next < self.screen_lines() {
            self.damage_cursor();
            self.grid.cursor.point.line += 1;
            self.damage_cursor();
        }
    }

    /// Set current position as a tabstop.
    #[inline]
    fn bell(&mut self) {
        trace!("Bell");
        self.event_proxy.send_event(Event::Bell);
    }

    #[inline]
    fn substitute(&mut self) {
        trace!("[unimplemented] Substitute");
    }

    /// Run LF/NL.
    ///
    /// LF/NL mode has some interesting history. According to ECMA-48 4th
    /// edition, in LINE FEED mode,
    ///
    /// > The execution of the formatter functions LINE FEED (LF), FORM FEED
    /// > (FF), LINE TABULATION (VT) cause only movement of the active position in
    /// > the direction of the line progression.
    ///
    /// In NEW LINE mode,
    ///
    /// > The execution of the formatter functions LINE FEED (LF), FORM FEED
    /// > (FF), LINE TABULATION (VT) cause movement to the line home position on
    /// > the following line, the following form, etc. In the case of LF this is
    /// > referred to as the New Line (NL) option.
    ///
    /// Additionally, ECMA-48 4th edition says that this option is deprecated.
    /// ECMA-48 5th edition only mentions this option (without explanation)
    /// saying that it's been removed.
    ///
    /// As an emulator, we need to support it since applications may still rely
    /// on it.
    #[inline]
    fn newline(&mut self) {
        self.linefeed();

        if self.mode.contains(TermMode::LINE_FEED_NEW_LINE) {
            self.carriage_return();
        }
    }

    #[inline]
    fn set_horizontal_tabstop(&mut self) {
        trace!("Setting horizontal tabstop");
        self.tabs[self.grid.cursor.point.column] = true;
    }

    #[inline]
    fn scroll_up(&mut self, lines: usize) {
        let origin = self.scroll_region.start;
        self.scroll_up_relative(origin, lines);
    }

    #[inline]
    fn scroll_down(&mut self, lines: usize) {
        let origin = self.scroll_region.start;
        self.scroll_down_relative(origin, lines);
    }

    #[inline]
    fn insert_blank_lines(&mut self, lines: usize) {
        trace!("Inserting blank {lines} lines");

        let origin = self.grid.cursor.point.line;
        if self.scroll_region.contains(&origin) && self.cursor_in_horizontal_region() {
            self.scroll_down_relative(origin, lines);
        }
    }

    #[inline]
    fn delete_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.point.line;
        let lines = cmp::min(self.screen_lines() - origin.0 as usize, lines);

        trace!("Deleting {lines} lines");

        if lines > 0 && self.scroll_region.contains(&origin) && self.cursor_in_horizontal_region() {
            self.scroll_up_relative(origin, lines);
        }
    }

    #[inline]
    fn erase_chars(&mut self, count: usize) {
        let cursor = &self.grid.cursor;

        trace!(
            "Erasing chars: count={}, col={}",
            count, cursor.point.column
        );

        let start = cursor.point.column;
        let end = cmp::min(start + count, Column(self.columns()));

        let line = cursor.point.line;
        self.erase_protected_cells(line, start, end, false);
    }

    #[inline]
    fn delete_chars(&mut self, count: usize) {
        if !self.cursor_in_horizontal_region() {
            return;
        }
        let columns = self.horizontal_region().end.0;
        let cursor = &self.grid.cursor;
        let bg = cursor.template.bg;

        // Delete at most the cells from the cursor to the end of the line, so
        // the cleared tail never reaches back before the cursor.
        let start = cursor.point.column.0;
        let count = cmp::min(count, columns - start);
        let end = start + count;
        let num_cells = columns - end;

        let line = cursor.point.line;
        if count == 0 {
            return;
        }
        self.clear_wide_boundary(line, Column(start));
        self.clear_wide_boundary(line, Column(end));
        self.clear_wide_boundary(line, Column(columns));
        self.damage
            .damage_line(line.0 as usize, 0, self.columns() - 1);
        let row = &mut self.grid[line][..];

        for offset in 0..num_cells {
            row.swap(start + offset, end + offset);
        }

        // Clear last `count` cells in the row. If deleting 1 char, need to delete
        // 1 cell.
        let end = columns - count;
        for cell in &mut row[end..columns] {
            *cell = bg.into();
        }
        self.repair_wide_cells(line);
    }

    #[inline]
    fn move_backward_tabs(&mut self, count: u16) {
        trace!("Moving backward {count} tabs");

        let old_col = self.grid.cursor.point.column.0;
        let left = if self.mode.contains(TermMode::ORIGIN) {
            self.horizontal_region().start.0
        } else {
            0
        };
        for _ in 0..count {
            let col = self.grid.cursor.point.column.0;
            if col <= left {
                break;
            }
            let previous = (left..col)
                .rev()
                .find(|&col| self.tabs[Column(col)])
                .unwrap_or(left);
            self.grid.cursor.point.column = Column(previous);
        }

        let line = self.grid.cursor.point.line.0 as usize;
        self.damage
            .damage_line(line, self.grid.cursor.point.column.0, old_col);
    }

    #[inline]
    fn move_forward_tabs(&mut self, count: u16) {
        trace!("Moving forward {count} tabs");

        let num_cols = self.input_right_edge().0;
        let old_col = self.grid.cursor.point.column.0;
        for _ in 0..count {
            let mut col = self.grid.cursor.point.column;

            if col == num_cols - 1 {
                break;
            }

            for i in col.0 + 1..num_cols {
                col = index::Column(i);
                if self.tabs[col] {
                    break;
                }
            }

            self.grid.cursor.point.column = col;
        }

        let line = self.grid.cursor.point.line.0 as usize;
        self.damage
            .damage_line(line, old_col, self.grid.cursor.point.column.0);
    }

    #[inline]
    fn save_cursor_position(&mut self) {
        trace!("Saving cursor position");

        self.grid.saved_cursor = self.grid.cursor.clone();
        self.grid.saved_origin_mode = self.mode.contains(TermMode::ORIGIN);
    }

    #[inline]
    fn restore_cursor_position(&mut self) {
        trace!("Restoring cursor position");

        self.damage_cursor();
        self.grid.cursor = self.grid.saved_cursor.clone();
        // DECRC restores DECOM too, and under origin mode the restored row
        // is kept inside the current margins (xterm's CursorRestore).
        self.restore_saved_origin_mode();
        self.damage_cursor();
    }

    #[inline]
    fn clear_line(&mut self, mode: ansi::LineClearMode) {
        self.clear_line_cells(mode, false);
    }

    fn selective_clear_line(&mut self, mode: ansi::LineClearMode) {
        self.clear_line_cells(mode, true);
    }

    fn selective_clear_screen(&mut self, mode: ansi::ClearMode) {
        self.clear_protected_screen(mode, true);
    }

    fn fill_rectangle(&mut self, character: u16, area: ansi::Rectangle) {
        if self.conformance_level < 4 || !matches!(character, 32..=126 | 160..=255) {
            return;
        }
        self.fill_rect(area, char::from_u32(character as u32).unwrap());
    }

    fn erase_rectangle(&mut self, area: ansi::Rectangle, selective: bool) {
        if self.conformance_level >= 4 {
            self.erase_rect(area, selective);
        }
    }

    fn copy_rectangle(&mut self, area: ansi::Rectangle, top: u16, left: u16) {
        if self.conformance_level >= 4 {
            self.copy_rect(area, top, left);
        }
    }

    fn set_character_protection(&mut self, protected: bool) {
        self.protection_mode = ProtectionMode::Dec;
        self.grid
            .cursor
            .template
            .flags
            .set(Flags::PROTECTED, protected);
    }

    fn set_protected_area(&mut self, start: bool) {
        if start {
            self.protection_mode = ProtectionMode::Iso;
        }
        self.grid.cursor.template.flags.set(Flags::PROTECTED, start);
    }

    /// Set the indexed color value.
    #[inline]
    fn set_color(&mut self, index: usize, color: Rgb) {
        trace!("Setting color[{index}] = {color:?}");

        // Damage terminal if the color changed and it's not the cursor.
        if index != NamedColor::Cursor as usize && self.colors[index] != Some(color) {
            self.mark_fully_damaged();
        }

        self.colors[index] = Some(color);
    }

    /// Respond to a color query escape sequence.
    #[inline]
    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        trace!("Requested write of escape sequence for color code {prefix}: color[{index}]");

        let terminator = terminator.to_owned();
        self.event_proxy.send_event(Event::ColorRequest(
            index,
            Arc::new(move |color| {
                format!(
                    "\x1b]{};rgb:{1:02x}{1:02x}/{2:02x}{2:02x}/{3:02x}{3:02x}{4}",
                    prefix, color.r, color.g, color.b, terminator
                )
            }),
        ));
    }

    /// Reset the indexed color to original value.
    #[inline]
    fn reset_color(&mut self, index: usize) {
        trace!("Resetting color[{index}]");

        // Damage terminal if the color changed and it's not the cursor.
        if index != NamedColor::Cursor as usize && self.colors[index].is_some() {
            self.mark_fully_damaged();
        }

        self.colors[index] = None;
    }

    /// Store data into clipboard.
    #[inline]
    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        if !matches!(self.config.osc52, Osc52::OnlyCopy | Osc52::CopyPaste) {
            debug!("Denied osc52 store");
            return;
        }

        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        if let Ok(bytes) = Base64.decode(base64) {
            if let Ok(text) = String::from_utf8(bytes) {
                self.event_proxy
                    .send_event(Event::ClipboardStore(clipboard_type, text));
            }
        }
    }

    /// Load data from clipboard.
    #[inline]
    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        if !matches!(self.config.osc52, Osc52::OnlyPaste | Osc52::CopyPaste) {
            debug!("Denied osc52 load");
            return;
        }

        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        let terminator = terminator.to_owned();

        self.event_proxy.send_event(Event::ClipboardLoad(
            clipboard_type,
            Arc::new(move |text| {
                let base64 = Base64.encode(text);
                format!("\x1b]52;{};{}{}", clipboard as char, base64, terminator)
            }),
        ));
    }

    #[inline]
    fn clear_screen(&mut self, mode: ansi::ClearMode) {
        if self.protection_mode == ProtectionMode::Iso && !matches!(mode, ansi::ClearMode::Saved) {
            self.clear_protected_screen(mode, false);
            return;
        }
        trace!("Clearing screen: {mode:?}");
        let erase_display = matches!(&mode, ansi::ClearMode::All);

        let screen_lines = self.screen_lines();

        match mode {
            ansi::ClearMode::Above => {
                let cursor = self.grid.cursor.point;

                // If clearing more than one line.
                if cursor.line > 0 {
                    // Fully clear all lines before the current line.
                    self.grid.reset_region(..cursor.line);
                }

                // Clear up to the current column in the current line.
                let end = cmp::min(cursor.column + 1, Column(self.columns()));
                self.erase_cells(cursor.line, Column(0), end);

                let range = Line(0)..=cursor.line;
                self.selection = self.selection.take().filter(|s| !s.intersects_range(range));
            }
            ansi::ClearMode::Below => {
                let cursor = self.grid.cursor.point;
                self.erase_cells(cursor.line, cursor.column, Column(self.columns()));

                if (cursor.line.0 as usize) < screen_lines - 1 {
                    self.grid.reset_region((cursor.line + 1)..);
                }

                let range = cursor.line..Line(screen_lines as i32);
                self.selection = self.selection.take().filter(|s| !s.intersects_range(range));
            }
            ansi::ClearMode::All => {
                if self.mode.contains(TermMode::ALT_SCREEN) {
                    self.grid.reset_region(..);
                } else {
                    let old_offset = self.grid.display_offset();

                    self.grid.clear_viewport();

                    // Compute number of lines scrolled by clearing the viewport.
                    let lines = self.grid.display_offset().saturating_sub(old_offset);

                    self.vi_mode_cursor.point.line =
                        (self.vi_mode_cursor.point.line - lines).grid_clamp(self, Boundary::Grid);
                }

                self.selection = None;
            }
            ansi::ClearMode::Saved if self.history_size() > 0 => {
                self.grid.clear_history();

                self.vi_mode_cursor.point.line = self
                    .vi_mode_cursor
                    .point
                    .line
                    .grid_clamp(self, Boundary::Cursor);

                self.selection = self
                    .selection
                    .take()
                    .filter(|s| !s.intersects_range(..Line(0)));
            }
            // We have no history to clear.
            ansi::ClearMode::Saved => (),
        }

        self.mark_fully_damaged();
        if erase_display {
            self.graphics_events.push(GraphicsEvent::EraseDisplay);
        }
    }

    #[inline]
    fn clear_tabs(&mut self, mode: ansi::TabulationClearMode) {
        trace!("Clearing tabs: {mode:?}");
        match mode {
            ansi::TabulationClearMode::Current => {
                self.tabs[self.grid.cursor.point.column] = false;
            }
            ansi::TabulationClearMode::All => {
                self.tabs.clear_all();
            }
        }
    }

    /// Reset all important fields in the term struct.
    #[inline]
    fn reset_state(&mut self) {
        self.protection_mode = ProtectionMode::Off;
        self.conformance_level = 5;
        if self.mode.contains(TermMode::ALT_SCREEN) {
            mem::swap(&mut self.grid, &mut self.inactive_grid);
        }
        self.active_charset = Default::default();
        self.cursor_style = None;
        self.grid.reset();
        self.inactive_grid.reset();
        self.scroll_region = Line(0)..Line(self.screen_lines() as i32);
        self.horizontal_margins = Column(0)..Column(self.columns());
        self.tabs = TabStops::new(self.columns());
        self.title_stack = Vec::new();
        self.title = None;
        self.selection = None;
        self.vi_mode_cursor = Default::default();
        self.keyboard_mode_stack = Default::default();
        self.inactive_keyboard_mode_stack = Default::default();
        self.keyboard_mode = Default::default();
        self.inactive_keyboard_mode = Default::default();

        // Preserve vi mode across resets.
        self.mode &= TermMode::VI;
        self.mode.insert(TermMode::default());
        self.mode.insert(unnegotiated_modified_enter_term_mode(
            self.config.unnegotiated_modified_enter,
        ));

        self.event_proxy.send_event(Event::CursorBlinkingChange);
        self.mark_fully_damaged();
        self.graphics_events.push(GraphicsEvent::Reset);
    }

    #[inline]
    fn set_conformance_level(&mut self, level: u16, controls: u16) {
        // Replies use the existing UTF-8 String transport. Do not claim to have
        // selected raw eight-bit C1 replies; VT100 always uses seven-bit controls.
        if !(61..=65).contains(&level) || (level != 61 && controls != 1) {
            return;
        }
        self.reset_state();
        self.conformance_level = (level - 60) as u8;
    }

    #[inline]
    fn soft_reset_state(&mut self) {
        self.protection_mode = ProtectionMode::Off;
        // DECSTR restores terminal state while leaving the display, scrollback and
        // title alone. Programs emit `CSI ! p` from terminfo `is2`/`rs2` while
        // initializing, so clearing here would destroy a user's scrollback
        // whenever a program started.
        // Colour-scheme reports (DEC mode 2031) go too, as in kitty, so a
        // program that crashed with them on cannot leave its successor
        // receiving reports it never asked for.
        self.mode.remove(
            TermMode::MODIFY_OTHER_KEYS
                | TermMode::MODIFY_OTHER_KEYS_NEGOTIATED
                | TermMode::COLOR_SCHEME_REPORTS
                | TermMode::LEFT_RIGHT_MARGIN
                | TermMode::INSERT
                | TermMode::ORIGIN
                | TermMode::APP_CURSOR
                | TermMode::APP_KEYPAD,
        );
        self.mode
            .insert(TermMode::SHOW_CURSOR | TermMode::LINE_WRAP);
        self.mode.insert(unnegotiated_modified_enter_term_mode(
            self.config.unnegotiated_modified_enter,
        ));
        self.active_charset = Default::default();
        self.grid.cursor.template = Cell::default();
        self.grid.cursor.charsets = Default::default();
        self.grid.cursor.input_needs_wrap = false;
        self.grid.saved_cursor = Default::default();
        self.grid.saved_origin_mode = false;
        self.scroll_region = Line(0)..Line(self.screen_lines() as i32);
        self.horizontal_margins = Column(0)..Column(self.columns());
    }

    #[inline]
    fn reverse_index(&mut self) {
        trace!("Reversing index");
        // If cursor is at the top.
        if self.grid.cursor.point.line == self.scroll_region.start {
            if self.cursor_in_horizontal_region() {
                self.scroll_down(1);
            }
        } else {
            self.damage_cursor();
            self.grid.cursor.point.line = cmp::max(self.grid.cursor.point.line - 1, Line(0));
            self.damage_cursor();
        }
    }

    #[inline]
    fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        trace!("Setting hyperlink: {hyperlink:?}");
        self.grid
            .cursor
            .template
            .set_hyperlink(hyperlink.map(|e| e.into()));
    }

    /// Set a terminal attribute.
    #[inline]
    fn terminal_attribute(&mut self, attr: Attr) {
        trace!("Setting attribute: {attr:?}");
        let cursor = &mut self.grid.cursor;
        match attr {
            Attr::Foreground(color) => cursor.template.fg = color,
            Attr::Background(color) => cursor.template.bg = color,
            Attr::UnderlineColor(color) => cursor.template.set_underline_color(color),
            Attr::Reset => {
                cursor.template.fg = Color::Named(NamedColor::Foreground);
                cursor.template.bg = Color::Named(NamedColor::Background);
                cursor.template.flags &= Flags::PROTECTED;
                cursor.template.set_underline_color(None);
            }
            Attr::Reverse => cursor.template.flags.insert(Flags::INVERSE),
            Attr::CancelReverse => cursor.template.flags.remove(Flags::INVERSE),
            Attr::Bold => cursor.template.flags.insert(Flags::BOLD),
            Attr::CancelBold => cursor.template.flags.remove(Flags::BOLD),
            Attr::Dim => cursor.template.flags.insert(Flags::DIM),
            Attr::CancelBoldDim => cursor.template.flags.remove(Flags::BOLD | Flags::DIM),
            Attr::Italic => cursor.template.flags.insert(Flags::ITALIC),
            Attr::CancelItalic => cursor.template.flags.remove(Flags::ITALIC),
            Attr::Underline => {
                cursor.template.flags.remove(Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(Flags::UNDERLINE);
            }
            Attr::DoubleUnderline => {
                cursor.template.flags.remove(Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(Flags::DOUBLE_UNDERLINE);
            }
            Attr::Undercurl => {
                cursor.template.flags.remove(Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(Flags::UNDERCURL);
            }
            Attr::DottedUnderline => {
                cursor.template.flags.remove(Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(Flags::DOTTED_UNDERLINE);
            }
            Attr::DashedUnderline => {
                cursor.template.flags.remove(Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(Flags::DASHED_UNDERLINE);
            }
            Attr::CancelUnderline => cursor.template.flags.remove(Flags::ALL_UNDERLINES),
            Attr::Hidden => cursor.template.flags.insert(Flags::HIDDEN),
            Attr::CancelHidden => cursor.template.flags.remove(Flags::HIDDEN),
            Attr::Strike => cursor.template.flags.insert(Flags::STRIKEOUT),
            Attr::CancelStrike => cursor.template.flags.remove(Flags::STRIKEOUT),
            _ => {
                debug!("Term got unhandled attr: {attr:?}");
            }
        }
    }

    #[inline]
    fn set_private_mode(&mut self, mode: PrivateMode) {
        let mode = match mode {
            PrivateMode::Named(mode) => mode,
            PrivateMode::Unknown(mode @ (47 | 1047)) => {
                trace!("Setting private mode: {mode}");
                if !self.mode.contains(TermMode::ALT_SCREEN) {
                    self.swap_alt_preserving();
                    self.graphics_events
                        .push(GraphicsEvent::EnterAlternate { mode, clear: false });
                }
                return;
            }
            // xterm's mode 1048 saves the cursor as DECSC does.
            PrivateMode::Unknown(1048) => {
                trace!("Setting private mode: 1048");
                self.save_cursor_position();
                return;
            }
            PrivateMode::Unknown(mode) => {
                debug!("Ignoring unknown mode {mode} in set_private_mode");
                return;
            }
        };

        trace!("Setting private mode: {mode:?}");
        match mode {
            NamedPrivateMode::UrgencyHints => self.mode.insert(TermMode::URGENCY_HINTS),
            NamedPrivateMode::SwapScreenAndSetRestoreCursor => {
                if !self.mode.contains(TermMode::ALT_SCREEN) {
                    self.swap_alt();
                    self.graphics_events.push(GraphicsEvent::EnterAlternate {
                        mode: 1049,
                        clear: true,
                    });
                }
            }
            NamedPrivateMode::ShowCursor => self.mode.insert(TermMode::SHOW_CURSOR),
            NamedPrivateMode::CursorKeys => self.mode.insert(TermMode::APP_CURSOR),
            // Mouse protocols are mutually exclusive.
            NamedPrivateMode::ReportMouseClicks => {
                self.mode.remove(TermMode::MOUSE_MODE);
                self.mode.insert(TermMode::MOUSE_REPORT_CLICK);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            }
            NamedPrivateMode::ReportCellMouseMotion => {
                self.mode.remove(TermMode::MOUSE_MODE);
                self.mode.insert(TermMode::MOUSE_DRAG);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            }
            NamedPrivateMode::ReportAllMouseMotion => {
                self.mode.remove(TermMode::MOUSE_MODE);
                self.mode.insert(TermMode::MOUSE_MOTION);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            }
            NamedPrivateMode::ReportFocusInOut => self.mode.insert(TermMode::FOCUS_IN_OUT),
            NamedPrivateMode::BracketedPaste => self.mode.insert(TermMode::BRACKETED_PASTE),
            NamedPrivateMode::ColorSchemeReports => {
                self.mode.insert(TermMode::COLOR_SCHEME_REPORTS)
            }
            // Mouse encodings are mutually exclusive.
            NamedPrivateMode::SgrMouse => {
                self.mode.remove(TermMode::UTF8_MOUSE);
                self.mode.insert(TermMode::SGR_MOUSE);
            }
            NamedPrivateMode::Utf8Mouse => {
                self.mode.remove(TermMode::SGR_MOUSE);
                self.mode.insert(TermMode::UTF8_MOUSE);
            }
            NamedPrivateMode::AlternateScroll => self.mode.insert(TermMode::ALTERNATE_SCROLL),
            NamedPrivateMode::LeftRightMargin => {
                if self.conformance_level >= 4 {
                    self.mode.insert(TermMode::LEFT_RIGHT_MARGIN);
                }
            }
            NamedPrivateMode::LineWrap => self.mode.insert(TermMode::LINE_WRAP),
            NamedPrivateMode::Origin => {
                self.mode.insert(TermMode::ORIGIN);
                self.goto(0, 0);
            }
            NamedPrivateMode::ColumnMode => self.deccolm(),
            NamedPrivateMode::BlinkingCursor => {
                let style = self
                    .cursor_style
                    .get_or_insert(self.config.default_cursor_style);
                style.blinking = true;
                self.event_proxy.send_event(Event::CursorBlinkingChange);
            }
            NamedPrivateMode::SyncUpdate => (),
        }
    }

    #[inline]
    fn unset_private_mode(&mut self, mode: PrivateMode) {
        let mode = match mode {
            PrivateMode::Named(mode) => mode,
            PrivateMode::Unknown(mode @ (47 | 1047)) => {
                trace!("Unsetting private mode: {mode}");
                if self.mode.contains(TermMode::ALT_SCREEN) {
                    let clear = mode == 1047;
                    if clear {
                        self.grid.reset_region(..);
                    }
                    self.swap_alt_preserving();
                    self.graphics_events
                        .push(GraphicsEvent::LeaveAlternate { mode, clear });
                }
                return;
            }
            // Resetting mode 1048 restores the cursor as DECRC does.
            PrivateMode::Unknown(1048) => {
                trace!("Unsetting private mode: 1048");
                self.restore_cursor_position();
                return;
            }
            PrivateMode::Unknown(mode) => {
                debug!("Ignoring unknown mode {mode} in unset_private_mode");
                return;
            }
        };

        trace!("Unsetting private mode: {mode:?}");
        match mode {
            NamedPrivateMode::UrgencyHints => self.mode.remove(TermMode::URGENCY_HINTS),
            NamedPrivateMode::SwapScreenAndSetRestoreCursor => {
                if self.mode.contains(TermMode::ALT_SCREEN) {
                    self.swap_alt();
                    self.graphics_events.push(GraphicsEvent::LeaveAlternate {
                        mode: 1049,
                        clear: false,
                    });
                }
            }
            NamedPrivateMode::ShowCursor => self.mode.remove(TermMode::SHOW_CURSOR),
            NamedPrivateMode::CursorKeys => self.mode.remove(TermMode::APP_CURSOR),
            NamedPrivateMode::ReportMouseClicks => {
                self.mode.remove(TermMode::MOUSE_REPORT_CLICK);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            }
            NamedPrivateMode::ReportCellMouseMotion => {
                self.mode.remove(TermMode::MOUSE_DRAG);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            }
            NamedPrivateMode::ReportAllMouseMotion => {
                self.mode.remove(TermMode::MOUSE_MOTION);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            }
            NamedPrivateMode::ReportFocusInOut => self.mode.remove(TermMode::FOCUS_IN_OUT),
            NamedPrivateMode::BracketedPaste => self.mode.remove(TermMode::BRACKETED_PASTE),
            NamedPrivateMode::ColorSchemeReports => {
                self.mode.remove(TermMode::COLOR_SCHEME_REPORTS)
            }
            NamedPrivateMode::SgrMouse => self.mode.remove(TermMode::SGR_MOUSE),
            NamedPrivateMode::Utf8Mouse => self.mode.remove(TermMode::UTF8_MOUSE),
            NamedPrivateMode::AlternateScroll => self.mode.remove(TermMode::ALTERNATE_SCROLL),
            NamedPrivateMode::LeftRightMargin => {
                self.mode.remove(TermMode::LEFT_RIGHT_MARGIN);
                self.horizontal_margins = Column(0)..Column(self.columns());
            }
            NamedPrivateMode::LineWrap => self.mode.remove(TermMode::LINE_WRAP),
            NamedPrivateMode::Origin => self.mode.remove(TermMode::ORIGIN),
            NamedPrivateMode::ColumnMode => self.deccolm(),
            NamedPrivateMode::BlinkingCursor => {
                let style = self
                    .cursor_style
                    .get_or_insert(self.config.default_cursor_style);
                style.blinking = false;
                self.event_proxy.send_event(Event::CursorBlinkingChange);
            }
            NamedPrivateMode::SyncUpdate => (),
        }
    }

    #[inline]
    fn report_private_mode(&mut self, mode: PrivateMode) {
        if self.conformance_level < 3 {
            return;
        }
        trace!("Reporting private mode {mode:?}");
        let state = match mode {
            PrivateMode::Named(mode) => match mode {
                NamedPrivateMode::CursorKeys => self.mode.contains(TermMode::APP_CURSOR).into(),
                NamedPrivateMode::Origin => self.mode.contains(TermMode::ORIGIN).into(),
                NamedPrivateMode::LeftRightMargin if self.conformance_level < 4 => {
                    ModeState::NotSupported
                }
                NamedPrivateMode::LeftRightMargin => {
                    self.mode.contains(TermMode::LEFT_RIGHT_MARGIN).into()
                }
                NamedPrivateMode::LineWrap => self.mode.contains(TermMode::LINE_WRAP).into(),
                NamedPrivateMode::BlinkingCursor => {
                    let style = self
                        .cursor_style
                        .get_or_insert(self.config.default_cursor_style);
                    style.blinking.into()
                }
                NamedPrivateMode::ShowCursor => self.mode.contains(TermMode::SHOW_CURSOR).into(),
                NamedPrivateMode::ReportMouseClicks => {
                    self.mode.contains(TermMode::MOUSE_REPORT_CLICK).into()
                }
                NamedPrivateMode::ReportCellMouseMotion => {
                    self.mode.contains(TermMode::MOUSE_DRAG).into()
                }
                NamedPrivateMode::ReportAllMouseMotion => {
                    self.mode.contains(TermMode::MOUSE_MOTION).into()
                }
                NamedPrivateMode::ReportFocusInOut => {
                    self.mode.contains(TermMode::FOCUS_IN_OUT).into()
                }
                NamedPrivateMode::Utf8Mouse => self.mode.contains(TermMode::UTF8_MOUSE).into(),
                NamedPrivateMode::SgrMouse => self.mode.contains(TermMode::SGR_MOUSE).into(),
                NamedPrivateMode::AlternateScroll => {
                    self.mode.contains(TermMode::ALTERNATE_SCROLL).into()
                }
                NamedPrivateMode::UrgencyHints => {
                    self.mode.contains(TermMode::URGENCY_HINTS).into()
                }
                NamedPrivateMode::SwapScreenAndSetRestoreCursor => {
                    self.mode.contains(TermMode::ALT_SCREEN).into()
                }
                NamedPrivateMode::BracketedPaste => {
                    self.mode.contains(TermMode::BRACKETED_PASTE).into()
                }
                NamedPrivateMode::SyncUpdate => ModeState::Reset,
                NamedPrivateMode::ColorSchemeReports => {
                    self.mode.contains(TermMode::COLOR_SCHEME_REPORTS).into()
                }
                NamedPrivateMode::ColumnMode => ModeState::NotSupported,
            },
            // The older alternate-screen modes `set_private_mode` handles.
            PrivateMode::Unknown(47 | 1047) => self.mode.contains(TermMode::ALT_SCREEN).into(),
            PrivateMode::Unknown(_) => ModeState::NotSupported,
        };

        self.event_proxy.send_event(Event::PtyWrite(format!(
            "\x1b[?{};{}$y",
            mode.raw(),
            state as u8,
        )));
    }

    #[inline]
    fn set_mode(&mut self, mode: ansi::Mode) {
        let mode = match mode {
            ansi::Mode::Named(mode) => mode,
            ansi::Mode::Unknown(mode) => {
                debug!("Ignoring unknown mode {mode} in set_mode");
                return;
            }
        };

        trace!("Setting public mode: {mode:?}");
        match mode {
            NamedMode::Insert => self.mode.insert(TermMode::INSERT),
            NamedMode::LineFeedNewLine => self.mode.insert(TermMode::LINE_FEED_NEW_LINE),
        }
    }

    #[inline]
    fn unset_mode(&mut self, mode: ansi::Mode) {
        let mode = match mode {
            ansi::Mode::Named(mode) => mode,
            ansi::Mode::Unknown(mode) => {
                debug!("Ignoring unknown mode {mode} in unset_mode");
                return;
            }
        };

        trace!("Setting public mode: {mode:?}");
        match mode {
            NamedMode::Insert => {
                self.mode.remove(TermMode::INSERT);
                self.mark_fully_damaged();
            }
            NamedMode::LineFeedNewLine => self.mode.remove(TermMode::LINE_FEED_NEW_LINE),
        }
    }

    #[inline]
    fn report_mode(&mut self, mode: ansi::Mode) {
        if self.conformance_level < 3 {
            return;
        }
        trace!("Reporting mode {mode:?}");
        let state = match mode {
            ansi::Mode::Named(mode) => match mode {
                NamedMode::Insert => self.mode.contains(TermMode::INSERT).into(),
                NamedMode::LineFeedNewLine => {
                    self.mode.contains(TermMode::LINE_FEED_NEW_LINE).into()
                }
            },
            ansi::Mode::Unknown(_) => ModeState::NotSupported,
        };

        self.event_proxy.send_event(Event::PtyWrite(format!(
            "\x1b[{};{}$y",
            mode.raw(),
            state as u8,
        )));
    }

    #[inline]
    fn set_horizontal_margins(&mut self, left: usize, right: Option<usize>) {
        if !self.mode.contains(TermMode::LEFT_RIGHT_MARGIN) {
            self.save_cursor_position();
            return;
        }
        let right = right.unwrap_or(self.columns()).min(self.columns());
        if left == 0 || left >= right {
            return;
        }
        self.horizontal_margins = Column(left - 1)..Column(right);
        self.goto(0, 0);
    }

    #[inline]
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        // Fallback to the last line as default. A bottom margin past the
        // screen is clipped to its last line first, as xterm does, so a
        // region that would then be empty is refused rather than stored:
        // the region always holds at least one line.
        let bottom = bottom
            .unwrap_or_else(|| self.screen_lines())
            .min(self.screen_lines());

        if top >= bottom {
            debug!("Invalid scrolling region: ({top};{bottom})");
            return;
        }

        // Bottom should be included in the range, but range end is not
        // usually included. One option would be to use an inclusive
        // range, but instead we just let the open range end be 1
        // higher.
        let start = Line(top as i32 - 1);
        let end = Line(bottom as i32);

        trace!("Setting scrolling region: ({start};{end})");

        self.scroll_region = start..end;
        self.goto(0, 0);
    }

    #[inline]
    fn set_keypad_application_mode(&mut self) {
        trace!("Setting keypad application mode");
        self.mode.insert(TermMode::APP_KEYPAD);
    }

    #[inline]
    fn unset_keypad_application_mode(&mut self) {
        trace!("Unsetting keypad application mode");
        self.mode.remove(TermMode::APP_KEYPAD);
    }

    #[inline]
    fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        trace!("Configuring charset {index:?} as {charset:?}");
        self.grid.cursor.charsets[index] = charset;
    }

    #[inline]
    fn set_active_charset(&mut self, index: CharsetIndex) {
        trace!("Setting active charset {index:?}");
        self.active_charset = index;
    }

    #[inline]
    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        trace!("Setting cursor style {style:?}");
        self.cursor_style = style;

        // Notify UI about blinking changes.
        self.event_proxy.send_event(Event::CursorBlinkingChange);
    }

    #[inline]
    fn set_cursor_shape(&mut self, shape: CursorShape) {
        trace!("Setting cursor shape {shape:?}");

        let style = self
            .cursor_style
            .get_or_insert(self.config.default_cursor_style);
        style.shape = shape;
    }

    #[inline]
    fn set_title(&mut self, title: Option<String>) {
        trace!("Setting title to '{title:?}'");

        self.title.clone_from(&title);

        let title_event = match title {
            Some(title) => Event::Title(title),
            None => Event::ResetTitle,
        };

        self.event_proxy.send_event(title_event);
    }

    #[inline]
    fn push_title(&mut self) {
        trace!("Pushing '{:?}' onto title stack", self.title);

        if self.title_stack.len() >= TITLE_STACK_MAX_DEPTH {
            let removed = self.title_stack.remove(0);
            trace!(
                "Removing '{removed:?}' from bottom of title stack that exceeds its maximum depth"
            );
        }

        self.title_stack.push(self.title.clone());
    }

    #[inline]
    fn pop_title(&mut self) {
        trace!("Attempting to pop title from stack...");

        if let Some(popped) = self.title_stack.pop() {
            trace!("Title '{popped:?}' popped from stack");
            self.set_title(popped);
        }
    }

    #[inline]
    fn text_area_size_pixels(&mut self) {
        self.event_proxy
            .send_event(Event::TextAreaSizeRequest(Arc::new(move |window_size| {
                let height = window_size.num_lines * window_size.cell_height;
                let width = window_size.num_cols * window_size.cell_width;
                format!("\x1b[4;{height};{width}t")
            })));
    }

    #[inline]
    fn text_area_size_chars(&mut self) {
        let text = format!("\x1b[8;{};{}t", self.screen_lines(), self.columns());
        self.event_proxy.send_event(Event::PtyWrite(text));
    }
}

/// The state of the [`Mode`] and [`PrivateMode`].
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum ModeState {
    /// The mode is not supported.
    NotSupported = 0,
    /// The mode is currently set.
    Set = 1,
    /// The mode is currently not set.
    Reset = 2,
}

impl From<bool> for ModeState {
    fn from(value: bool) -> Self {
        if value { Self::Set } else { Self::Reset }
    }
}

/// The most capability names one XTGETTCAP request is answered for.
const TERMCAP_NAMES_MAX: usize = 32;

/// The SGR parameters that rebuild `cell`'s attributes from a reset, in the
/// DECRQSS form: `0`, then each attribute, `;`-separated, with colour and
/// underline-style sub-parameters `:`-separated (`0;1;4:3;48:2::1:2:3`).
fn sgr_report(cell: &Cell) -> String {
    let flags = cell.flags;
    let mut parts: Vec<String> = vec![String::from("0")];
    for (flag, sgr) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::UNDERCURL, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if flags.contains(flag) {
            parts.push(sgr.to_owned());
        }
    }
    parts.extend(sgr_color(cell.fg, 30, 90, 38));
    parts.extend(sgr_color(cell.bg, 40, 100, 48));
    if let Some(color) = cell.underline_color() {
        // Underline colour has no short form; a palette colour is `58:5:n`.
        let color = match color {
            Color::Named(named) if (named as usize) < 16 => Color::Indexed(named as u8),
            other => other,
        };
        parts.extend(sgr_color(color, 58, 58, 58));
    }
    parts.join(";")
}

/// One colour's SGR attribute: `base + n` for the first 8 palette colours,
/// `bright + n - 8` for the next 8, and the colon form of `extended` for the
/// 256-colour and direct forms. The default colours need no attribute.
fn sgr_color(color: Color, base: u8, bright: u8, extended: u8) -> Option<String> {
    match color {
        Color::Named(named) => match named as usize {
            index @ 0..=7 => Some((usize::from(base) + index).to_string()),
            index @ 8..=15 => Some((usize::from(bright) + index - 8).to_string()),
            _ => None,
        },
        Color::Indexed(index) => Some(format!("{extended}:5:{index}")),
        Color::Spec(rgb) => Some(format!("{extended}:2::{}:{}:{}", rgb.r, rgb.g, rgb.b)),
    }
}

/// The DECSCUSR number that sets `style`.
fn decscusr_number(style: CursorStyle) -> u8 {
    let steady = match style.shape {
        CursorShape::Underline => 4,
        CursorShape::Beam => 6,
        CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => 2,
    };
    if style.blinking { steady - 1 } else { steady }
}

/// The capabilities XTGETTCAP answers, with literal control bytes. Only what
/// this terminal does, and nothing a program would read as a key: truecolor,
/// styled and coloured underlines, cursor shapes, and the palette size.
fn termcap_value(name: &[u8]) -> Option<Option<&'static str>> {
    Some(match name {
        b"Tc" | b"RGB" => None,
        b"setrgbf" => Some("\x1b[38;2;%p1%d;%p2%d;%p3%dm"),
        b"setrgbb" => Some("\x1b[48;2;%p1%d;%p2%d;%p3%dm"),
        b"Smulx" => Some("\x1b[4:%p1%dm"),
        b"Setulc" => Some("\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m"),
        b"Ss" => Some("\x1b[%p1%d q"),
        b"Se" => Some("\x1b[0 q"),
        b"Co" | b"colors" => Some("256"),
        _ => return None,
    })
}

/// The XTGETTCAP reply for one hex-encoded capability name: `DCS 1 + r
/// name[=value] ST` when known, `DCS 0 + r name ST` when not. The name is
/// echoed only once it is known to be hex digits, so a request can never
/// smuggle a control sequence into the reply.
fn termcap_reply(hex_name: &[u8]) -> String {
    let valid_hex = !hex_name.is_empty()
        && hex_name.len() % 2 == 0
        && hex_name.iter().all(u8::is_ascii_hexdigit);
    if !valid_hex {
        return String::from("\x1bP0+r\x1b\\");
    }
    let echo = String::from_utf8_lossy(hex_name).to_ascii_uppercase();
    let name: Vec<u8> = hex_name
        .chunks(2)
        .map(|pair| {
            let digits = str::from_utf8(pair).unwrap_or("00");
            u8::from_str_radix(digits, 16).unwrap_or(0)
        })
        .collect();
    match termcap_value(&name) {
        Some(None) => format!("\x1bP1+r{echo}\x1b\\"),
        Some(Some(value)) => {
            let value: String = value.bytes().map(|byte| format!("{byte:02X}")).collect();
            format!("\x1bP1+r{echo}={value}\x1b\\")
        }
        None => format!("\x1bP0+r{echo}\x1b\\"),
    }
}

/// Terminal version for escape sequence reports.
///
/// This returns the current terminal version as a unique number based on alacritty_terminal's
/// semver version. The different versions are padded to ensure that a higher semver version will
/// always report a higher version number.
fn version_number(mut version: &str) -> usize {
    if let Some(separator) = version.rfind('-') {
        version = &version[..separator];
    }

    let mut version_number = 0;

    let semver_versions = version.split('.');
    for (i, semver_version) in semver_versions.rev().enumerate() {
        let semver_number = semver_version.parse::<usize>().unwrap_or(0);
        version_number += usize::pow(100, i as u32) * semver_number;
    }

    version_number
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardType {
    Clipboard,
    Selection,
}

struct TabStops {
    tabs: Vec<bool>,
}

impl TabStops {
    #[inline]
    fn new(columns: usize) -> TabStops {
        TabStops {
            tabs: (0..columns).map(|i| i % INITIAL_TABSTOPS == 0).collect(),
        }
    }

    /// Remove all tabstops.
    #[inline]
    fn clear_all(&mut self) {
        unsafe {
            ptr::write_bytes(self.tabs.as_mut_ptr(), 0, self.tabs.len());
        }
    }

    /// Increase tabstop capacity.
    #[inline]
    fn resize(&mut self, columns: usize) {
        let mut index = self.tabs.len();
        self.tabs.resize_with(columns, || {
            let is_tabstop = index % INITIAL_TABSTOPS == 0;
            index += 1;
            is_tabstop
        });
    }
}

impl Index<Column> for TabStops {
    type Output = bool;

    fn index(&self, index: Column) -> &bool {
        &self.tabs[index.0]
    }
}

impl IndexMut<Column> for TabStops {
    fn index_mut(&mut self, index: Column) -> &mut bool {
        self.tabs.index_mut(index.0)
    }
}

/// Terminal cursor rendering information.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct RenderableCursor {
    pub shape: CursorShape,
    pub point: Point,
}

impl RenderableCursor {
    fn new<T>(term: &Term<T>) -> Self {
        // Cursor position.
        let vi_mode = term.mode().contains(TermMode::VI);
        let mut point = if vi_mode {
            term.vi_mode_cursor.point
        } else {
            term.grid.cursor.point
        };
        if term.grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
            point.column -= 1;
        }

        // Cursor shape.
        let shape = if !vi_mode && !term.mode().contains(TermMode::SHOW_CURSOR) {
            CursorShape::Hidden
        } else {
            term.cursor_style().shape
        };

        Self { shape, point }
    }
}

/// Visible terminal content.
///
/// This contains all content required to render the current terminal view.
pub struct RenderableContent<'a> {
    pub display_iter: GridIterator<'a, Cell>,
    pub selection: Option<SelectionRange>,
    pub cursor: RenderableCursor,
    pub display_offset: usize,
    pub colors: &'a color::Colors,
    pub mode: TermMode,
}

impl<'a> RenderableContent<'a> {
    fn new<T>(term: &'a Term<T>) -> Self {
        Self {
            display_iter: term.grid().display_iter(),
            display_offset: term.grid().display_offset(),
            cursor: RenderableCursor::new(term),
            selection: term.selection.as_ref().and_then(|s| s.to_range(term)),
            colors: &term.colors,
            mode: *term.mode(),
        }
    }
}

/// Terminal test helpers.
pub mod test {
    use super::*;

    #[cfg(feature = "serde")]
    use serde::{Deserialize, Serialize};

    use crate::event::VoidListener;

    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    pub struct TermSize {
        pub columns: usize,
        pub screen_lines: usize,
    }

    impl TermSize {
        pub fn new(columns: usize, screen_lines: usize) -> Self {
            Self {
                columns,
                screen_lines,
            }
        }
    }

    impl Dimensions for TermSize {
        fn total_lines(&self) -> usize {
            self.screen_lines()
        }

        fn screen_lines(&self) -> usize {
            self.screen_lines
        }

        fn columns(&self) -> usize {
            self.columns
        }
    }

    /// Construct a terminal from its content as string.
    ///
    /// A `\n` will break line and `\r\n` will break line without wrapping.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use alacritty_terminal::term::test::mock_term;
    ///
    /// // Create a terminal with the following cells:
    /// //
    /// // [h][e][l][l][o] <- WRAPLINE flag set
    /// // [:][)][ ][ ][ ]
    /// // [t][e][s][t][ ]
    /// mock_term(
    ///     "\
    ///     hello\n:)\r\ntest",
    /// );
    /// ```
    pub fn mock_term(content: &str) -> Term<VoidListener> {
        let lines: Vec<&str> = content.split('\n').collect();
        let num_cols = lines
            .iter()
            .map(|line| {
                line.chars()
                    .filter(|c| *c != '\r')
                    .map(|c| c.width().unwrap())
                    .sum()
            })
            .max()
            .unwrap_or(0);

        // Create terminal with the appropriate dimensions.
        let size = TermSize::new(num_cols, lines.len());
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Fill terminal with content.
        for (line, text) in lines.iter().enumerate() {
            let line = Line(line as i32);
            if !text.ends_with('\r') && line + 1 != lines.len() {
                term.grid[line][Column(num_cols - 1)]
                    .flags
                    .insert(Flags::WRAPLINE);
            }

            let mut index = 0;
            for c in text.chars().take_while(|c| *c != '\r') {
                term.grid[line][Column(index)].c = c;

                // Handle fullwidth characters.
                let width = c.width().unwrap();
                if width == 2 {
                    term.grid[line][Column(index)]
                        .flags
                        .insert(Flags::WIDE_CHAR);
                    term.grid[line][Column(index + 1)]
                        .flags
                        .insert(Flags::WIDE_CHAR_SPACER);
                }

                index += width;
            }
        }

        term
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::mem;
    use std::sync::{Arc, Mutex};

    use crate::event::{Event, EventListener, VoidListener};
    use crate::grid::{Grid, Scroll};
    use crate::index::{Column, Point, Side};
    use crate::selection::{Selection, SelectionType};
    use crate::term::cell::{Cell, Flags};
    use crate::term::test::TermSize;
    use crate::vte::ansi::{self, CharsetIndex, Handler, Processor, StandardCharset};

    #[derive(Clone, Default)]
    struct PtyWriteListener(Arc<Mutex<Vec<String>>>);

    impl PtyWriteListener {
        fn drain(&self) -> Vec<String> {
            mem::take(&mut *self.0.lock().unwrap())
        }
    }

    impl EventListener for PtyWriteListener {
        fn send_event(&self, event: Event) {
            if let Event::PtyWrite(text) = event {
                self.0.lock().unwrap().push(text);
            }
        }
    }

    #[test]
    fn modify_other_keys_tracks_mutually_exclusive_effective_levels() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();

        assert!(!term.mode().intersects(TermMode::MODIFY_OTHER_KEYS));
        assert!(!term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));
        assert!(term.mode().contains(TermMode::UNNEGOTIATED_MODIFIED_ENTER));

        processor.advance(&mut term, b"\x1b[>4;1m");
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_1));
        assert!(!term.mode().contains(TermMode::MODIFY_OTHER_KEYS_2));
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));

        processor.advance(&mut term, b"\x1b[>4;2m");
        assert!(!term.mode().contains(TermMode::MODIFY_OTHER_KEYS_1));
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_2));

        processor.advance(&mut term, b"\x1b[>4;0m");
        assert!(!term.mode().intersects(TermMode::MODIFY_OTHER_KEYS));
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));
    }

    #[test]
    fn terminal_queries_are_answered() {
        let size = TermSize::new(20, 6);
        let listener = PtyWriteListener::default();
        let config = Config {
            xtversion: Some("kettle(9.9.9)".into()),
            ..Config::default()
        };
        let mut term = Term::new(config, &size, listener.clone());
        let mut processor: Processor = Processor::new();

        // XTVERSION, and DECXCPR next to the plain cursor position report.
        processor.advance(&mut term, b"\x1b[3;5H\x1b[>q\x1b[?6n\x1b[6n");
        assert_eq!(
            listener.drain(),
            ["\x1bP>|kettle(9.9.9)\x1b\\", "\x1b[?3;5R", "\x1b[3;5R"]
        );

        // Under origin mode both count rows from the top margin, so the
        // report goes back to CUP unchanged.
        processor.advance(
            &mut term,
            b"\x1b[2;5r\x1b[?6h\x1b[1;4H\x1b[?6n\x1b[6n\x1b[?6l\x1b[r",
        );
        assert_eq!(listener.drain(), ["\x1b[?1;4R", "\x1b[1;4R"]);

        // DECRC restores DECOM as it was saved (off here), so the cursor
        // above the margin reports its absolute row.
        processor.advance(
            &mut term,
            b"\x1b[1;3H\x1b7\x1b[3;5r\x1b[?6h\x1b8\x1b[?6n\x1b[6n\x1b[?6l\x1b[r",
        );
        assert_eq!(listener.drain(), ["\x1b[?1;3R", "\x1b[1;3R"]);

        // DECRQSS for the scroll region, set and reset, and every cursor
        // style.
        processor.advance(&mut term, b"\x1b[2;5r\x1bP$qr\x1b\\\x1b[r\x1bP$qr\x1b\\");
        assert_eq!(
            listener.drain(),
            ["\x1bP1$r2;5r\x1b\\", "\x1bP1$r1;6r\x1b\\"]
        );
        for style in 1..=6 {
            let query = format!("\x1b[{style} q\x1bP$q q\x1b\\");
            processor.advance(&mut term, query.as_bytes());
            assert_eq!(listener.drain(), [format!("\x1bP1$r{style} q\x1b\\")]);
        }

        // Unknown and oversized requests get the bare invalid reply: a body
        // is never echoed, not even one holding a command line.
        processor.advance(&mut term, b"\x1bP$q\rtouch x\n\x1b\\\x1bP$qzz\x1b\\");
        let mut long = b"\x1bP$q".to_vec();
        long.extend(std::iter::repeat_n(b'm', 2048));
        long.extend(b"\x1b\\");
        processor.advance(&mut term, &long);
        assert_eq!(listener.drain(), ["\x1bP0$r\x1b\\"; 3]);

        // A query cut off before ST is not answered at all.
        processor.advance(&mut term, b"\x1bP$qm\x18\x1bP$qm\x1b[0m");
        assert!(listener.drain().is_empty());

        // DECRQM knows the older alternate-screen modes.
        processor.advance(
            &mut term,
            b"\x1b[?1047$p\x1b[?1047h\x1b[?47$p\x1b[?1047$p\x1b[?1047l",
        );
        assert_eq!(
            listener.drain(),
            ["\x1b[?1047;2$y", "\x1b[?47;1$y", "\x1b[?1047;1$y"]
        );

        // Without a configured name, XTVERSION stays unanswered.
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        processor.advance(&mut term, b"\x1b[>0q");
        assert!(listener.drain().is_empty());
    }

    /// The exact probes Neovim sends: undercurl (`CSI 4:3 m` then DECRQSS
    /// SGR, matched against `1$r0;4:3m`) and truecolor (`CSI 0 m CSI
    /// 48;2;1;2;3 m` then DECRQSS SGR), plus every other attribute.
    #[test]
    fn decrqss_reports_sgr_as_neovim_reads_it() {
        let size = TermSize::new(20, 6);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut processor: Processor = Processor::new();

        for (sgr, report) in [
            ("\x1b[4:3m", "0;4:3"),
            ("\x1b[0m\x1b[48;2;1;2;3m", "0;48:2::1:2:3"),
            ("\x1b[0m", "0"),
            ("\x1b[0;1;2;3;7;8;9;4:5;31;102m", "0;1;2;3;4:5;7;8;9;31;102"),
            (
                "\x1b[0;4;38;5;200;48:2::9:8:7;58:5:3m",
                "0;4;38:5:200;48:2::9:8:7;58:5:3",
            ),
            ("\x1b[0;4:2;39;49;59m", "0;4:2"),
            ("\x1b[0;4:4;58:2::1:2:3;93m", "0;4:4;93;58:2::1:2:3"),
        ] {
            processor.advance(&mut term, sgr.as_bytes());
            processor.advance(&mut term, b"\x1bP$qm\x1b\\");
            assert_eq!(
                listener.drain(),
                [format!("\x1bP1$r{report}m\x1b\\")],
                "{sgr:?}"
            );
        }
    }

    #[test]
    fn xtgettcap_undercurl_expands_to_a_control_sequence() {
        let size = TermSize::new(20, 6);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"\x1bP+q536D756C78\x1b\\");
        let reply = listener.drain().concat();
        let value = reply
            .strip_prefix("\x1bP1+r536D756C78=")
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap();
        let bytes: Vec<u8> = value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let expanded = String::from_utf8(bytes).unwrap().replace("%p1%d", "3");
        assert_eq!(expanded.as_bytes(), b"\x1b[4:3m");
        processor.advance(&mut term, expanded.as_bytes());
        processor.advance(&mut term, b"X");
        assert!(
            term.grid()[Line(0)][Column(0)]
                .flags
                .contains(Flags::UNDERCURL)
        );
    }

    #[test]
    fn xtgettcap_answers_each_name_and_echoes_only_hex() {
        let size = TermSize::new(20, 6);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut processor: Processor = Processor::new();

        let hex = |text: &str| text.bytes().map(|b| format!("{b:02X}")).collect::<String>();
        let flag = |name: &str| format!("\x1bP1+r{}\x1b\\", hex(name));
        let value = |name: &str, value: &str| format!("\x1bP1+r{}={}\x1b\\", hex(name), hex(value));

        // Every capability, in one write: Neovim's truecolor query
        // (`Tc;RGB;setrgbf;setrgbb`) and the rest.
        let names = [
            "Tc", "RGB", "setrgbf", "setrgbb", "Smulx", "Setulc", "Ss", "Se", "Co", "colors",
        ];
        let request = names.map(hex).join(";");
        processor.advance(&mut term, format!("\x1bP+q{request}\x1b\\").as_bytes());
        let every = [
            flag("Tc"),
            flag("RGB"),
            value("setrgbf", "\x1b[38;2;%p1%d;%p2%d;%p3%dm"),
            value("setrgbb", "\x1b[48;2;%p1%d;%p2%d;%p3%dm"),
            value("Smulx", "\x1b[4:%p1%dm"),
            value(
                "Setulc",
                "\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
            ),
            value("Ss", "\x1b[%p1%d q"),
            value("Se", "\x1b[0 q"),
            value("Co", "256"),
            value("colors", "256"),
        ];
        assert_eq!(listener.drain(), [every.concat()]);

        // Lowercase hex is accepted and echoed uppercase; an unknown name is
        // echoed as hex; a key capability is not answered.
        processor.advance(&mut term, b"\x1bP+q536d756c78;6b637575\x1b\\");
        assert_eq!(
            listener.drain(),
            [format!(
                "{}\x1bP0+r6B637575\x1b\\",
                value("Smulx", "\x1b[4:%p1%dm")
            )]
        );

        // Anything that is not an even run of hex digits is never echoed, and
        // an oversized request gets one bare reply.
        processor.advance(&mut term, b"\x1bP+q1b5b;545;zz;\x1b\\");
        let mut long = b"\x1bP+q".to_vec();
        long.extend(std::iter::repeat_n(b'4', 2048));
        long.extend(b"\x1b\\");
        processor.advance(&mut term, &long);
        assert_eq!(
            listener.drain(),
            [
                format!("\x1bP0+r1B5B\x1b\\{}", "\x1bP0+r\x1b\\".repeat(3)),
                String::from("\x1bP0+r\x1b\\"),
            ]
        );

        // At most TERMCAP_NAMES_MAX replies per request.
        let many = vec![hex("Tc"); TERMCAP_NAMES_MAX + 5].join(";");
        processor.advance(&mut term, format!("\x1bP+q{many}\x1b\\").as_bytes());
        assert_eq!(listener.drain(), [flag("Tc").repeat(TERMCAP_NAMES_MAX)]);
    }

    #[test]
    fn color_scheme_is_queried_and_reported_on_change() {
        let size = TermSize::new(20, 6);
        let listener = PtyWriteListener::default();
        let config = Config {
            color_scheme_dark: Some(true),
            ..Config::default()
        };
        let mut term = Term::new(config, &size, listener.clone());
        let mut processor: Processor = Processor::new();

        // The query answers from the start; nothing is reported unasked.
        processor.advance(&mut term, b"\x1b[?996n\x1b[?2031$p");
        assert_eq!(listener.drain(), ["\x1b[?997;1n", "\x1b[?2031;2$y"]);
        term.set_color_scheme(Some(false));
        assert!(listener.drain().is_empty(), "mode 2031 is off");

        // With mode 2031 on, every colour change is reported, a dark-to-dark
        // one included: the program re-reads the background itself.
        processor.advance(&mut term, b"\x1b[?2031h\x1b[?2031$p");
        assert_eq!(listener.drain(), ["\x1b[?2031;1$y"]);
        term.set_color_scheme(Some(true));
        term.set_color_scheme(Some(true));
        assert_eq!(listener.drain(), ["\x1b[?997;1n", "\x1b[?997;1n"]);
        term.set_color_scheme(Some(false));
        assert_eq!(listener.drain(), ["\x1b[?997;2n"]);

        // An unknown scheme is not reported, and a query waits for it: the
        // next known scheme answers it once, even with mode 2031 off.
        term.set_color_scheme(None);
        processor.advance(&mut term, b"\x1b[?996n");
        assert!(listener.drain().is_empty());
        processor.advance(&mut term, b"\x1b[?2031l\x1b[?996n");
        term.set_color_scheme(None);
        assert!(listener.drain().is_empty());
        term.set_color_scheme(Some(false));
        assert_eq!(
            listener.drain(),
            ["\x1b[?997;2n"],
            "the waiting query, answered once"
        );
        term.set_color_scheme(Some(false));
        assert!(
            listener.drain().is_empty(),
            "mode 2031 is off and nothing waits"
        );
        processor.advance(&mut term, b"\x1b[?2031h");

        // Turning the mode off, or RIS, stops the reports; RIS keeps the
        // scheme itself.
        term.set_color_scheme(Some(true));
        assert_eq!(listener.drain(), ["\x1b[?997;1n"]);
        processor.advance(&mut term, b"\x1b[?2031l");
        term.set_color_scheme(Some(false));
        assert!(listener.drain().is_empty());
        processor.advance(&mut term, b"\x1b[?2031h\x1bc");
        term.set_color_scheme(Some(true));
        assert!(listener.drain().is_empty(), "RIS clears mode 2031");
        processor.advance(&mut term, b"\x1b[?2031h\x1b[!p\x1b[?2031$p");
        term.set_color_scheme(Some(true));
        assert_eq!(
            listener.drain(),
            ["\x1b[?2031;2$y"],
            "DECSTR clears mode 2031"
        );
        processor.advance(&mut term, b"\x1b[?996n");
        assert_eq!(listener.drain(), ["\x1b[?997;1n"]);
    }

    #[test]
    fn modify_other_keys_reports_the_effective_level() {
        let size = TermSize::new(5, 3);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut processor: Processor = Processor::new();

        processor.advance(&mut term, b"\x1b[?4m");
        assert_eq!(listener.drain(), ["\x1b[>4;0m"]);
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));

        for (set, reply) in [
            (b"\x1b[>4;0m".as_slice(), "\x1b[>4;0m"),
            (b"\x1b[>4;1m".as_slice(), "\x1b[>4;1m"),
            (b"\x1b[>4;2m".as_slice(), "\x1b[>4;2m"),
        ] {
            processor.advance(&mut term, set);
            processor.advance(&mut term, b"\x1b[?4m");
            assert_eq!(listener.drain(), [reply]);
        }
    }

    #[test]
    fn modify_other_keys_restore_and_disable_both_reach_the_initial_zero_level() {
        let size = TermSize::new(5, 3);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut processor: Processor = Processor::new();

        for restore in [b"\x1b[>4m".as_slice(), b"\x1b[>m".as_slice()] {
            processor.advance(&mut term, b"\x1b[>4;2m");
            processor.advance(&mut term, restore);
            processor.advance(&mut term, b"\x1b[?4m");
            assert_eq!(listener.drain(), ["\x1b[>4;0m"]);
        }

        processor.advance(&mut term, b"\x1b[>4;1m\x1b[>4;0m\x1b[?4m");
        assert_eq!(listener.drain(), ["\x1b[>4;0m"]);
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));
    }

    #[test]
    fn ris_and_decstr_restore_initial_modify_other_keys_state() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();

        for reset in [b"\x1bc".as_slice(), b"\x1b[!p".as_slice()] {
            processor.advance(&mut term, b"\x1b[>4;2m");
            assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));

            processor.advance(&mut term, reset);
            assert!(!term.mode().intersects(TermMode::MODIFY_OTHER_KEYS));
            assert!(!term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));
            assert!(term.mode().contains(TermMode::UNNEGOTIATED_MODIFIED_ENTER));
        }
    }

    #[test]
    fn left_right_margins_scroll_only_the_rectangle_without_history() {
        for (control, expected) in [
            ("\x1b[S", ["aa111aaa", "bb222bbb", "cc   ccc", "dddddddd"]),
            ("\x1b[T", ["aa   aaa", "bb000bbb", "cc111ccc", "dddddddd"]),
        ] {
            let size = TermSize::new(8, 4);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            parser.advance(&mut term, b"aaaaaaaa\r\nbbbbbbbb\r\ncccccccc\r\ndddddddd");
            parser.advance(
                &mut term,
                b"\x1b[?69h\x1b[3;5s\x1b[1;3r\x1b[1;3H000\x1b[2;3H111\x1b[3;3H222",
            );
            term.take_graphics_events();
            parser.advance(&mut term, control.as_bytes());
            for (line, expected) in expected.iter().enumerate() {
                let actual: String = term.grid[Line(line as i32)][..]
                    .iter()
                    .map(|cell| cell.c)
                    .collect();
                assert_eq!(&actual, expected, "{control:?} row {line}");
            }
            assert_eq!(term.grid.history_size(), 0);
            assert!(matches!(
                term.take_graphics_events().events.as_slice(),
                [GraphicsEvent::PartialScroll {
                    left: 2,
                    right: 5,
                    ..
                }]
            ));
        }
    }

    #[test]
    fn left_right_margins_constrain_editing_wrapping_and_linefeed() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"abcdefgh\x1b[?69h\x1b[3;6s\x1b[1;4H\x1b[P");
        let row = |term: &Term<VoidListener>, line| {
            term.grid[Line(line)][..]
                .iter()
                .map(|cell| cell.c)
                .collect::<String>()
        };
        assert_eq!(row(&term, 0), "abcef gh");
        parser.advance(&mut term, b"\x1b[@");
        assert_eq!(row(&term, 0), "abc efgh");
        parser.advance(&mut term, b"\x1b[1;2H\x1b[99P\x1b[99@");
        assert_eq!(
            row(&term, 0),
            "abc efgh",
            "editing outside margins is ignored"
        );
        parser.advance(&mut term, b"\x1b[2;3H12345");
        assert_eq!(row(&term, 1), "  1234  ");
        assert_eq!(row(&term, 2), "  5     ");
        assert!(
            !term.grid[Line(1)][Column(5)]
                .flags
                .contains(Flags::WRAPLINE)
        );
        parser.advance(&mut term, b"\x1b[1;3r\x1b[3;8H\n");
        assert_eq!(
            term.grid.cursor.point,
            Point::new(Line(2), Column(7)),
            "LF outside horizontal region neither scrolls nor crosses the bottom margin"
        );
    }

    #[test]
    fn left_right_margins_clamp_movement_and_restore_absolute_columns() {
        let size = TermSize::new(12, 5);
        let mut term = Term::new(Config::default(), &size, PtyWriteListener::default());
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(
            &mut term,
            b"\x1b[?69h\x1b[3;8s\x1b[2;4r\x1b[?6h\x1b[2;3H\x1b7\x1b[99C\x1b[6n",
        );
        assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(7)));
        parser.advance(&mut term, b"\x1b[99D\x1b8");
        assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(4)));
        parser.advance(&mut term, b"\x1b[1d");
        assert_eq!(
            term.grid.cursor.point,
            Point::new(Line(1), Column(4)),
            "VPA must not apply horizontal origin twice"
        );
        parser.advance(&mut term, b"\x1bP$qs\x1b\\");
        assert_eq!(
            *term.event_proxy.0.lock().unwrap(),
            ["\x1b[2;6R", "\x1bP1$r3;8s\x1b\\"]
        );
    }

    #[test]
    fn left_right_margins_preserve_selection_and_vi_cursor_outside_rectangle() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
        let mut selection = Selection::new(
            SelectionType::Block,
            Point::new(Line(1), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(2), Column(1)), Side::Right);
        term.selection = Some(selection.clone());
        term.vi_mode_cursor.point = Point::new(Line(1), Column(0));
        parser.advance(&mut term, b"\x1b[S");
        assert_eq!(term.selection, Some(selection));
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(1), Column(0)));
        let mut selection = Selection::new(
            SelectionType::Block,
            Point::new(Line(1), Column(3)),
            Side::Left,
        );
        selection.update(Point::new(Line(1), Column(4)), Side::Right);
        term.selection = Some(selection);
        term.vi_mode_cursor.point = Point::new(Line(1), Column(3));
        parser.advance(&mut term, b"\x1b[S");
        assert!(term.selection.is_none());
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(3)));
    }

    #[test]
    fn horizontal_edits_do_not_borrow_spacers_from_a_different_wide_glyph() {
        for (text, edit, expected) in [
            ("abc界語h", "\x1b[1;4H\x1b[2@", "abc    h"),
            ("a界語bch", "\x1b[1;3H\x1b[2P", "a  b  ch"),
        ] {
            let size = TermSize::new(8, 2);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            parser.advance(&mut term, text.as_bytes());
            parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s");
            parser.advance(&mut term, edit.as_bytes());
            let actual: String = term.grid[Line(0)][..].iter().map(|cell| cell.c).collect();
            assert_eq!(actual, expected, "{text} {edit:?}");
        }
    }

    #[test]
    fn horizontal_edits_match_whole_glyph_motion_across_all_margin_edges() {
        let wide = Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER;
        for text in ["abc界語h", "a界語bch", "界a語bcd", "ab界c語d"] {
            let mut position = 0;
            let glyphs: Vec<_> = text
                .chars()
                .map(|ch| {
                    let width = if matches!(ch, '\u{754c}' | '\u{8a9e}') {
                        2
                    } else {
                        1
                    };
                    let glyph = (position, ch, width);
                    position += width;
                    glyph
                })
                .collect();
            assert_eq!(position, 8);
            for left in 0..7 {
                for right in left + 2..=8 {
                    for cursor in left..right {
                        for count in 0..=8 {
                            for delete in [false, true] {
                                let size = TermSize::new(8, 2);
                                let mut term = Term::new(Config::default(), &size, VoidListener);
                                let mut parser: ansi::Processor = ansi::Processor::new();
                                parser.advance(&mut term, text.as_bytes());
                                parser.advance(
                                    &mut term,
                                    format!(
                                        "\x1b[?69h\x1b[{};{}s\x1b[1;{}H",
                                        left + 1,
                                        right,
                                        cursor + 1
                                    )
                                    .as_bytes(),
                                );
                                if delete {
                                    term.delete_chars(count);
                                } else {
                                    term.insert_blank(count);
                                }
                                let count = count.min(right - cursor);
                                let mut expected = [(' ', Flags::empty()); 8];
                                // Model whole glyphs, independent of the engine's cell swaps.
                                for &(start, ch, width) in &glyphs {
                                    let end = start + width;
                                    let target = if count == 0 || end <= cursor || start >= right {
                                        Some(start)
                                    } else if delete && start >= cursor + count && end <= right {
                                        Some(start - count)
                                    } else if !delete && start >= cursor && end <= right - count {
                                        Some(start + count)
                                    } else {
                                        None
                                    };
                                    if let Some(target) = target {
                                        expected[target] = (
                                            ch,
                                            if width == 2 {
                                                Flags::WIDE_CHAR
                                            } else {
                                                Flags::empty()
                                            },
                                        );
                                        if width == 2 {
                                            expected[target + 1] = (' ', Flags::WIDE_CHAR_SPACER);
                                        }
                                    }
                                }
                                for (col, expected) in expected.iter().enumerate() {
                                    let cell = &term.grid[Line(0)][Column(col)];
                                    assert_eq!(
                                        (cell.c, cell.flags & wide),
                                        *expected,
                                        "{text} margin{left}..{right} cursor{cursor} count{count} delete{delete} col{col}"
                                    );
                                }
                                assert_eq!(
                                    term.grid.cursor.point,
                                    Point::new(Line(0), Column(cursor))
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn left_right_margins_leave_no_orphan_wide_cells() {
        for edit in ["\x1b[@", "\x1b[P", "\x1b[S", "\x1b[T"] {
            let size = TermSize::new(8, 4);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            parser.advance(
                &mut term,
                "\x1b[1;2H界界界\x1b[2;2H界界界\x1b[?69h\x1b[3;6s\x1b[1;3H".as_bytes(),
            );
            parser.advance(&mut term, edit.as_bytes());
            for line in 0..4 {
                for col in 0..8 {
                    let flags = term.grid[Line(line)][Column(col)].flags;
                    if flags.contains(Flags::WIDE_CHAR) {
                        assert!(
                            col < 7
                                && term.grid[Line(line)][Column(col + 1)]
                                    .flags
                                    .contains(Flags::WIDE_CHAR_SPACER),
                            "{edit:?} ({line},{col}) orphan lead"
                        );
                    }
                    if flags.contains(Flags::WIDE_CHAR_SPACER) {
                        assert!(
                            col > 0
                                && term.grid[Line(line)][Column(col - 1)]
                                    .flags
                                    .contains(Flags::WIDE_CHAR),
                            "{edit:?} ({line},{col}) orphan spacer"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn margin_wrapping_wide_glyphs_does_not_create_a_full_row_spacer() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, "\x1b[?69h\x1b[3;6s\x1b[1;6H界".as_bytes());
        assert!(
            !term.grid[Line(0)][Column(5)]
                .flags
                .contains(Flags::LEADING_WIDE_CHAR_SPACER)
        );
        assert_eq!(term.grid[Line(1)][Column(2)].c, '\u{754c}');
        parser.advance(&mut term, b"\x1b[2;4r\x1b[2;8H\x1bM");
        assert_eq!(
            term.grid.cursor.point,
            Point::new(Line(1), Column(7)),
            "RI outside horizontal region stops at top margin"
        );
    }

    #[test]
    fn insert_mode_uses_the_same_horizontal_bounds_as_ich() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"abcdefgh\x1b[?69h\x1b[3;6s\x1b[4h\x1b[1;1HZ");
        let row = |term: &Term<VoidListener>| {
            term.grid[Line(0)][..]
                .iter()
                .map(|cell| cell.c)
                .collect::<String>()
        };
        assert_eq!(
            row(&term),
            "Zbcdefgh",
            "IRM outside horizontal margins only overwrites"
        );
        parser.advance(&mut term, b"\x1b[1;4HQ");
        assert_eq!(
            row(&term),
            "ZbcQdegh",
            "IRM inside horizontal margins inserts and clips"
        );
    }

    #[test]
    fn wide_wrap_recomputes_destination_bounds_and_inserts_there() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, "\x1b[?69h\x1b[3;4s\x1b[1;8H界".as_bytes());
        assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(3)));
        assert!(term.grid.cursor.input_needs_wrap);
        parser.advance(&mut term, b"Z");
        assert_eq!(term.grid[Line(2)][Column(2)].c, 'Z');
        parser.advance(
            &mut term,
            "\x1bc\x1b[2;1HABCDEFGH\x1b[?69h\x1b[3;6s\x1b[4h\x1b[1;6H界".as_bytes(),
        );
        assert_eq!(term.grid[Line(1)][Column(2)].c, '\u{754c}');
        assert_eq!(term.grid[Line(1)][Column(4)].c, 'C');
        assert_eq!(term.grid[Line(1)][Column(5)].c, 'D');
        assert_eq!(term.grid[Line(1)][Column(6)].c, 'G');
        assert_eq!(term.grid[Line(1)][Column(7)].c, 'H');
    }

    #[test]
    fn repeated_partial_scrolls_do_not_overflow_the_graphics_journal() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[?69h\x1b[3;6s\x1b[4;3H");
        parser.advance(
            &mut term,
            &vec![b'\n'; GRAPHICS_EVENT_JOURNAL_CAPACITY + 10],
        );
        let batch = term.take_graphics_events();
        assert!(!batch.overflowed);
        assert!(matches!(
            batch.events.as_slice(),
            [GraphicsEvent::PartialScroll {
                lines: 4,
                left: 2,
                right: 6,
                ..
            }]
        ));
    }

    #[test]
    fn conformance_report_omits_the_encoding_parameter_at_vt100_level() {
        let size = TermSize::new(8, 4);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[61\"p\x1bP$q\"p\x1b\\");
        assert_eq!(listener.drain(), ["\x1bP1$r61\"p\x1b\\"]);
    }

    #[test]
    fn graphics_scrolls_do_not_coalesce_across_margin_mode_changes() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[S\x1b[?69h\x1b[S\x1b[?69l\x1b[S");
        assert!(matches!(
            term.take_graphics_events().events.as_slice(),
            [
                GraphicsEvent::Scroll { columns: None, .. },
                GraphicsEvent::Scroll {
                    columns: Some((0, 8)),
                    ..
                },
                GraphicsEvent::Scroll { columns: None, .. },
            ]
        ));
    }

    #[test]
    fn an_omitted_conformance_level_does_not_reset_text_or_history() {
        let size = TermSize::new(12, 3);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"one\r\ntwo\r\nthree\r\nfour\r\nfive\x1b[?69h");
        let history = term.grid.history_size();
        assert!(history > 0);
        let text: Vec<char> = term.grid.display_iter().map(|cell| cell.cell.c).collect();
        parser.advance(&mut term, b"\x1b[\"p\x1b[0\"p\x1bP$q\"p\x1b\\");
        assert_eq!(term.grid.history_size(), history);
        assert_eq!(
            term.grid
                .display_iter()
                .map(|cell| cell.cell.c)
                .collect::<Vec<_>>(),
            text
        );
        assert!(term.mode.contains(TermMode::LEFT_RIGHT_MARGIN));
        assert_eq!(listener.drain(), ["\x1bP1$r65;1\"p\x1b\\"]);
    }

    #[test]
    fn conformance_selection_resets_state_and_refuses_unsupported_encodings() {
        let size = TermSize::new(12, 5);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(
            &mut term,
            b"A\x1b[4h\x1b[?69h\x1b[3;8s\x1b[66;1\"p\x1b[64;2\"p\x1bP$q\"p\x1b\\",
        );
        assert_eq!(term.grid[Line(0)][Column(0)].c, 'A');
        assert!(
            term.mode
                .contains(TermMode::INSERT | TermMode::LEFT_RIGHT_MARGIN)
        );
        assert_eq!(listener.drain(), ["\x1bP1$r65;1\"p\x1b\\"]);
        parser.advance(&mut term, b"\x1b[63;1\"p\x1b[4$p\x1b[?69$p");
        assert_eq!(term.grid[Line(0)][Column(0)].c, ' ');
        assert!(
            !term
                .mode
                .intersects(TermMode::INSERT | TermMode::LEFT_RIGHT_MARGIN)
        );
        assert_eq!(listener.drain(), ["\x1b[4;2$y", "\x1b[?69;0$y"]);
        parser.advance(&mut term, b"\x1b[62;1\"p\x1b[4$p\x1b[?69$p");
        assert!(listener.drain().is_empty());
        parser.advance(&mut term, b"\x1bc\x1b[?69$p");
        assert_eq!(listener.drain(), ["\x1b[?69;2$y"]);
    }

    #[test]
    fn conformance_level_three_disables_left_right_margins() {
        let size = TermSize::new(12, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[63;1\"p\x1b[?69h\x1b[3;6s\x1b[1;5Habc");
        assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(7)));
        parser.advance(&mut term, b"\x1b[64;1\"p\x1b[?69h\x1b[3;6s\x1b[1;5Habc");
        assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(3)));
    }

    #[test]
    fn backward_tabs_honor_origin_but_otherwise_ignore_margins() {
        let size = TermSize::new(16, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[?69h\x1b[3;12s\x1b[?6h\x1b[Z");
        assert_eq!(term.grid.cursor.point.column, Column(2));
        parser.advance(&mut term, b"\x1b[?6l\x1b[1;7H\x1b[2Z");
        assert_eq!(term.grid.cursor.point.column, Column(0));
        parser.advance(&mut term, b"\x1b[3g\x1b[1;10H\x1b[Z");
        assert_eq!(
            term.grid.cursor.point.column,
            Column(0),
            "no tab stops still goes to the left edge"
        );
    }

    #[test]
    fn left_right_margins_reject_empty_regions_and_reset() {
        let size = TermSize::new(12, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"\x1b[?69h\x1b[3;8s\x1b[7;7s\x1b[99;100s");
        assert_eq!(term.horizontal_margins, Column(2)..Column(8));
        parser.advance(&mut term, b"\x1b[?69l");
        assert_eq!(term.horizontal_margins, Column(0)..Column(12));
        parser.advance(&mut term, b"\x1b[?69h\x1b[3;8s\x1b[!p");
        assert!(!term.mode.contains(TermMode::LEFT_RIGHT_MARGIN));
        assert_eq!(term.horizontal_margins, Column(0)..Column(12));
        parser.advance(&mut term, b"\x1b[?69h\x1b[3;8s\x1b#8");
        assert_eq!(term.horizontal_margins, Column(0)..Column(12));
        parser.advance(&mut term, b"\x1b[3;8s\x1bc");
        assert!(!term.mode.contains(TermMode::LEFT_RIGHT_MARGIN));
        assert_eq!(term.horizontal_margins, Column(0)..Column(12));
    }

    #[test]
    fn left_right_margins_offset_an_origin_mode_cursor() {
        let size = TermSize::new(12, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"\x1b[?69h\x1b[3;8s\x1b[?6h\x1b[1;1H");
        assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(2)));
        processor.advance(&mut term, b"\x1b[1;99H");
        assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(7)));
    }

    #[test]
    fn left_right_margin_mode_reports_its_actual_state() {
        let size = TermSize::new(12, 5);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut processor: Processor = Processor::new();
        for (sequence, expected) in [
            (b"\x1b[?69$p".as_slice(), "\x1b[?69;2$y"),
            (b"\x1b[?69h\x1b[?69$p".as_slice(), "\x1b[?69;1$y"),
            (b"\x1b[?69l\x1b[?69$p".as_slice(), "\x1b[?69;2$y"),
        ] {
            processor.advance(&mut term, sequence);
            assert_eq!(listener.drain(), [expected]);
        }
    }

    #[test]
    fn protection_is_cleared_only_from_a_linked_leading_wide_spacer() {
        for (text, expected_char, expected_bg) in [
            (
                "abc\x1b[1\"q界\x1b[0\"q\x1b[2;1HX",
                ' ',
                Color::Named(NamedColor::Red),
            ),
            (
                "abc\x1b[1\"qP\x1b[2;1H界\x1b[0\"q\x1b[2;1HX",
                'P',
                Color::Named(NamedColor::Background),
            ),
        ] {
            let size = TermSize::new(4, 2);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            parser.advance(&mut term, text.as_bytes());
            parser.advance(&mut term, b"\x1b[41m\x1b[1;4H\x1b[?K");
            let cell = &term.grid[Line(0)][Column(3)];
            assert_eq!(cell.c, expected_char);
            assert_eq!(cell.bg, expected_bg, "{text:?}");
        }
    }

    #[test]
    fn protection_scrolls_with_cells_while_vacated_margin_cells_are_cleared() {
        let size = TermSize::new(8, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(
            &mut term,
            b"\x1bVAB\x1bW\x1b[4;3H\x1bVP\x1bWQ\x1b[?69h\x1b[3;6s\x1b[S\x1b[?2J",
        );
        for (line, expected) in ["AB      ", "        ", "  P     ", "        "]
            .iter()
            .enumerate()
        {
            let actual: String = term.grid[Line(line as i32)][..]
                .iter()
                .map(|cell| cell.c)
                .collect();
            assert_eq!(&actual, expected, "row{line}");
        }
        assert_eq!(term.grid.history_size(), 0);
    }

    #[test]
    fn protection_does_not_survive_in_the_cleared_half_of_an_overwritten_wide_glyph() {
        for column in [1, 2] {
            let size = TermSize::new(8, 2);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            parser.advance(&mut term, "\x1b[1\"q界\x1b[0\"q".as_bytes());
            parser.advance(&mut term, format!("\x1b[1;{column}HX").as_bytes());
            let cleared = Column(if column == 1 { 1 } else { 0 });
            assert!(
                crate::grid::GridCell::is_empty(&term.grid[Line(0)][cleared]),
                "the erased half must not retain a protected blank at column{cleared}"
            );
        }
    }

    #[test]
    fn protection_erase_right_includes_the_last_cell_while_wrap_is_pending() {
        for protected in [false, true] {
            for (erase, selective) in [(b"\x1b[K".as_slice(), false), (b"\x1b[?K".as_slice(), true)]
            {
                let size = TermSize::new(4, 2);
                let mut term = Term::new(Config::default(), &size, VoidListener);
                let mut parser: ansi::Processor = ansi::Processor::new();
                parser.advance(&mut term, b"abc");
                if protected {
                    parser.advance(&mut term, b"\x1b[1\"q");
                }
                parser.advance(&mut term, b"P");
                assert!(term.grid.cursor.input_needs_wrap);
                parser.advance(&mut term, erase);
                let actual: String = term.grid[Line(0)][..].iter().map(|cell| cell.c).collect();
                assert_eq!(
                    actual,
                    if protected && selective {
                        "abcP"
                    } else {
                        "abc "
                    }
                );
                assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(3)));
            }
        }
    }

    #[test]
    fn protection_selective_display_erase_covers_all_three_ranges_without_history() {
        for (erase, expected) in [
            ("\x1b[?J", ["ABcdefgh", "ABc     ", "AB      "]),
            ("\x1b[?1J", ["AB      ", "AB  efgh", "ABcdefgh"]),
            ("\x1b[?2J", ["AB      ", "AB      ", "AB      "]),
        ] {
            let size = TermSize::new(8, 3);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            for row in 1..=3 {
                parser.advance(
                    &mut term,
                    format!("\x1b[{row};1H\x1b[1\"qAB\x1b[0\"qcdefgh").as_bytes(),
                );
            }
            parser.advance(&mut term, b"\x1b[2;4H");
            parser.advance(&mut term, erase.as_bytes());
            for (line, expected) in expected.iter().enumerate() {
                let actual: String = term.grid[Line(line as i32)][..]
                    .iter()
                    .map(|cell| cell.c)
                    .collect();
                assert_eq!(&actual, expected, "{erase:?} row {line}");
            }
            assert_eq!(term.grid.history_size(), 0);
        }
    }

    #[test]
    fn protection_reports_and_cursor_save_restore_are_independent_of_sgr() {
        let size = TermSize::new(8, 3);
        let listener = PtyWriteListener::default();
        let mut term = Term::new(Config::default(), &size, listener.clone());
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(
            &mut term,
            b"\x1b[1\"qA\x1b7\x1b[0\"qB\x1b8\x1b[0mZ\x1bP$q\"q\x1b\\",
        );
        assert_eq!(listener.drain(), ["\x1bP1$r1\"q\x1b\\"]);
        parser.advance(&mut term, b"\x1b[?2K");
        assert_eq!(term.grid[Line(0)][Column(0)].c, 'A');
        assert_eq!(term.grid[Line(0)][Column(1)].c, 'Z');
        parser.advance(&mut term, b"\x1b[2\"q\x1bP$q\"q\x1b\\");
        assert_eq!(listener.drain(), ["\x1bP1$r0\"q\x1b\\"]);
        parser.advance(&mut term, b"\x1b[1\"q\x1b[!pX\x1b[?2J");
        assert!(term.grid.display_iter().all(|cell| cell.cell.c == ' '));
    }

    #[test]
    fn protection_keeps_a_wide_glyph_whole_when_erasing_its_spacer() {
        let size = TermSize::new(8, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, "\x1bV界\x1bWX\x1b[1;2H\x1b[X".as_bytes());
        assert_eq!(term.grid[Line(0)][Column(0)].c, '\u{754c}');
        assert!(
            term.grid[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
        parser.advance(&mut term, b"\x1b[0\"q\x1b[X");
        assert_eq!(term.grid[Line(0)][Column(0)].c, ' ');
        assert!(
            !term.grid[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
    }

    #[test]
    fn dec_protection_survives_sgr_and_only_selective_erase_respects_it() {
        let size = TermSize::new(12, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(
            &mut term,
            b"\x1b[1\"qA\x1b[0mBC\x1b[0\"qDEF\x1b[1;1H\x1b[?2K",
        );
        let text = |term: &Term<VoidListener>| {
            term.grid[Line(0)][..]
                .iter()
                .map(|cell| cell.c)
                .collect::<String>()
        };
        assert_eq!(text(&term), "ABC         ");
        parser.advance(&mut term, b"\x1b[2K");
        assert_eq!(text(&term), "            ");
    }

    #[test]
    fn iso_protection_survives_normal_line_display_and_character_erasure() {
        for erase in ["\x1b[2K", "\x1b[2J", "\x1b[12X"] {
            let size = TermSize::new(12, 3);
            let mut term = Term::new(Config::default(), &size, VoidListener);
            let mut parser: ansi::Processor = ansi::Processor::new();
            parser.advance(&mut term, b"\x1bVABC\x1bWDEF\x1b[1;1H");
            parser.advance(&mut term, erase.as_bytes());
            let actual: String = term.grid[Line(0)][..].iter().map(|cell| cell.c).collect();
            assert_eq!(actual, "ABC         ", "{erase:?} respects SPA/EPA");
        }
    }

    #[test]
    fn decstr_resets_attributes_modes_and_saved_cursor() {
        let size = TermSize::new(20, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"history\r\nkeep\r\nrows\r\nhere\r\n");
        processor.advance(&mut term, b"\x1b]2;title\x07\x1b[31;1m\x1b)0\x0e\x1b7");
        processor.advance(
            &mut term,
            b"\x1b[4h\x1b[?1h\x1b=\x1b[?7l\x1b[?25l\x1b[2;3r\x1b[?6h",
        );
        let before = term.grid().clone();
        let point = term.grid.cursor.point;
        processor.advance(&mut term, b"\x1b[!p");

        assert_eq!(
            term.grid.cursor.template,
            Cell::default(),
            "DECSTR resets SGR"
        );
        assert!(!term.mode.intersects(
            TermMode::INSERT | TermMode::ORIGIN | TermMode::APP_CURSOR | TermMode::APP_KEYPAD
        ));
        assert!(
            term.mode
                .contains(TermMode::SHOW_CURSOR | TermMode::LINE_WRAP)
        );
        assert_eq!(term.grid.cursor.point, point);
        assert_eq!(term.grid.cursor.charsets, Default::default());
        assert_eq!(term.active_charset, Default::default());
        assert_eq!(term.grid.saved_cursor, Default::default());
        assert_eq!(term.scroll_region, Line(0)..Line(4));
        assert_eq!(term.title.as_deref(), Some("title"));
        assert_eq!(term.grid.history_origin(), before.history_origin());
        assert_eq!(term.grid.history_size(), before.history_size());
        for line in -(before.history_size() as i32)..4 {
            assert_eq!(term.grid[Line(line)], before[Line(line)], "row {line}");
        }
        processor.advance(&mut term, b"q");
        assert_eq!(term.grid[point.line][point.column].c, 'q');
        assert_eq!(term.grid[point.line][point.column].fg, Cell::default().fg);
        assert_eq!(
            term.grid[point.line][point.column + 1],
            before[point.line][point.column + 1]
        );
        processor.advance(&mut term, b"\x1b8");
        assert_eq!(term.grid.cursor.point, Point::default());
        assert_eq!(term.grid.cursor.template, Cell::default());
    }

    /// DECSTR restores the modifier resources like RIS, but it is a SOFT
    /// reset: it must not touch the screen or the scrollback. Programs emit
    /// `CSI ! p` from terminfo `is2`/`rs2` while initializing, so a DECSTR
    /// wired to the hard reset would erase a user's scrollback whenever a
    /// program started — far worse than the state bug it was added to fix.
    #[test]
    fn decstr_restores_modifier_state_without_erasing_the_buffer() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();

        processor.advance(&mut term, b"scrollback\r\nvisible");
        let history_before = term.grid().history_size();
        let total_before = term.grid().total_lines();
        assert!(history_before > 0, "the fixture must produce scrollback");

        processor.advance(&mut term, b"\x1b[>4;2m");
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_2));

        processor.advance(&mut term, b"\x1b[!p");

        assert!(
            !term.mode().intersects(TermMode::MODIFY_OTHER_KEYS),
            "DECSTR must restore the negotiated level"
        );
        assert_eq!(
            term.grid().history_size(),
            history_before,
            "DECSTR must not clear scrollback"
        );
        assert_eq!(
            term.grid().total_lines(),
            total_before,
            "DECSTR must not resize or clear the grid"
        );
    }

    #[test]
    fn config_updates_only_the_unnegotiated_modified_enter_fallback() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();

        processor.advance(&mut term, b"\x1b[>4;2m");
        term.set_options(Config {
            unnegotiated_modified_enter: false,
            ..Config::default()
        });
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_2));
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED));
        assert!(!term.mode().contains(TermMode::UNNEGOTIATED_MODIFIED_ENTER));

        term.set_options(Config::default());
        assert!(term.mode().contains(TermMode::MODIFY_OTHER_KEYS_2));
        assert!(term.mode().contains(TermMode::UNNEGOTIATED_MODIFIED_ENTER));
    }

    #[test]
    fn selection_survives_while_scrollback_history_grows() {
        let size = TermSize::new(5, 3);
        let config = Config {
            scrolling_history: 4,
            ..Default::default()
        };
        let mut term = Term::new(config, &size, VoidListener);

        // Create the first history row and select it.
        term.scroll_up_relative(Line(0), 1);
        let point = Point::new(Line(-1), Column(0));
        let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
        selection.update(Point::new(Line(-1), Column(1)), Side::Right);
        term.selection = Some(selection);

        // The old topmost line moves above the pre-scroll bounds, but remains
        // retained once the grid grows its history.
        term.scroll_up_relative(Line(0), 1);

        let range = term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&term))
            .expect("selection should follow retained text into growing history");
        assert_eq!(term.history_size(), 2);
        assert_eq!(range.start.line, Line(-2));
        assert_eq!(range.end.line, Line(-2));
    }

    #[test]
    fn selection_clears_after_full_scrollback_eviction() {
        let size = TermSize::new(5, 3);
        let config = Config {
            scrolling_history: 4,
            ..Default::default()
        };
        let mut term = Term::new(config, &size, VoidListener);

        // Fill history to capacity and select its oldest retained row.
        for _ in 0..4 {
            term.scroll_up_relative(Line(0), 1);
        }
        let point = Point::new(Line(-4), Column(0));
        let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
        selection.update(Point::new(Line(-4), Column(1)), Side::Right);
        term.selection = Some(selection);

        term.scroll_up_relative(Line(0), 1);

        assert!(
            term.selection.is_none(),
            "selection must clear after all of its rows leave retained history"
        );
    }

    #[test]
    fn history_limit_reduction_normalizes_selection_to_retained_rows() {
        let size = TermSize::new(5, 3);
        let config = Config {
            scrolling_history: 4,
            ..Default::default()
        };
        let mut term = Term::new(config, &size, VoidListener);

        for _ in 0..4 {
            term.scroll_up_relative(Line(0), 1);
        }

        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(-4), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(-3), Column(1)), Side::Right);
        term.selection = Some(selection);

        term.set_options(Config {
            scrolling_history: 3,
            ..Default::default()
        });

        let range = term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&term))
            .expect("selection should survive while it intersects retained history");
        assert_eq!(range.start.line, Line(-3));
        assert_eq!(range.end.line, Line(-3));

        term.set_options(Config {
            scrolling_history: 2,
            ..Default::default()
        });
        assert!(
            term.selection.is_none(),
            "selection must clear once history reduction evicts all selected rows"
        );
    }

    #[test]
    fn row_only_resize_normalizes_selection_to_retained_rows() {
        let size = TermSize::new(5, 4);
        let config = Config {
            scrolling_history: 2,
            ..Default::default()
        };

        let mut retained_term = Term::new(config.clone(), &size, VoidListener);
        for _ in 0..2 {
            retained_term.scroll_up_relative(Line(0), 1);
        }
        retained_term.grid_mut().cursor.point.line = Line(3);
        let mut retained_selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(3), Column(0)),
            Side::Left,
        );
        retained_selection.update(Point::new(Line(3), Column(1)), Side::Right);
        retained_term.selection = Some(retained_selection);

        retained_term.resize(TermSize::new(5, 2));

        let range = retained_term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&retained_term))
            .expect("selection should follow a retained row through row-only resize");
        assert_eq!(range.start.line, Line(1));
        assert_eq!(range.end.line, Line(1));

        let mut evicted_term = Term::new(config, &size, VoidListener);
        for _ in 0..2 {
            evicted_term.scroll_up_relative(Line(0), 1);
        }
        evicted_term.grid_mut().cursor.point.line = Line(3);
        let mut evicted_selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(-2), Column(0)),
            Side::Left,
        );
        evicted_selection.update(Point::new(Line(-2), Column(1)), Side::Right);
        evicted_term.selection = Some(evicted_selection);

        evicted_term.resize(TermSize::new(5, 2));

        assert!(
            evicted_term.selection.is_none(),
            "selection must clear when row-only resize evicts every selected row"
        );
    }

    #[test]
    fn scroll_display_page_up() {
        let size = TermSize::new(5, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 11 lines of scrollback.
        for _ in 0..20 {
            term.newline();
        }

        // Scrollable amount to top is 11.
        term.scroll_display(Scroll::PageUp);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-1), Column(0)));
        assert_eq!(term.grid.display_offset(), 10);

        // Scrollable amount to top is 1.
        term.scroll_display(Scroll::PageUp);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-2), Column(0)));
        assert_eq!(term.grid.display_offset(), 11);

        // Scrollable amount to top is 0.
        term.scroll_display(Scroll::PageUp);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-2), Column(0)));
        assert_eq!(term.grid.display_offset(), 11);
    }

    #[test]
    fn scroll_display_page_down() {
        let size = TermSize::new(5, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 11 lines of scrollback.
        for _ in 0..20 {
            term.newline();
        }

        // Change display_offset to topmost.
        term.grid_mut().scroll_display(Scroll::Top);
        term.vi_mode_cursor = ViModeCursor::new(Point::new(Line(-11), Column(0)));

        // Scrollable amount to bottom is 11.
        term.scroll_display(Scroll::PageDown);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-1), Column(0)));
        assert_eq!(term.grid.display_offset(), 1);

        // Scrollable amount to bottom is 1.
        term.scroll_display(Scroll::PageDown);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(0)));
        assert_eq!(term.grid.display_offset(), 0);

        // Scrollable amount to bottom is 0.
        term.scroll_display(Scroll::PageDown);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(0)));
        assert_eq!(term.grid.display_offset(), 0);
    }

    #[test]
    fn selection_includes_wrapped_wide_glyph_and_combining_marks() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, "X\r\nabcd\u{754c}\u{301}".as_bytes());

        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(1), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(1), Column(4)), Side::Right);
        term.selection = Some(selection);
        assert_eq!(
            term.selection_to_string(),
            Some("abcd\u{754c}\u{301}".into())
        );

        let end = Point::new(Line(2), Column(1));
        term.selection.as_mut().unwrap().update(end, Side::Right);
        assert_eq!(
            term.selection_to_string(),
            Some("abcd\u{754c}\u{301}".into())
        );
    }

    #[test]
    fn wrapped_wide_spacer_without_a_following_row_is_safe() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        term.grid_mut()[Line(2)][Column(4)]
            .flags
            .insert(Flags::LEADING_WIDE_CHAR_SPACER | Flags::WRAPLINE);
        assert_eq!(
            term.bounds_to_string(
                Point::new(Line(2), Column(0)),
                Point::new(Line(2), Column(4)),
            ),
            "    ",
        );
    }

    #[test]
    fn simple_selection_works() {
        let size = TermSize::new(5, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let grid = term.grid_mut();
        for i in 0..4 {
            if i == 1 {
                continue;
            }

            grid[Line(i)][Column(0)].c = '"';

            for j in 1..4 {
                grid[Line(i)][Column(j)].c = 'a';
            }

            grid[Line(i)][Column(4)].c = '"';
        }
        grid[Line(2)][Column(0)].c = ' ';
        grid[Line(2)][Column(4)].c = ' ';
        grid[Line(2)][Column(4)].flags.insert(Flags::WRAPLINE);
        grid[Line(3)][Column(0)].c = ' ';

        // Multiple lines contain an empty line.
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point {
                line: Line(0),
                column: Column(0),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Point {
                    line: Line(2),
                    column: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\"aaa\"\n\n aaa "))
        );

        // A wrapline.
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point {
                line: Line(2),
                column: Column(0),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Point {
                    line: Line(3),
                    column: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from(" aaa  aaa\""))
        );
    }

    #[test]
    fn semantic_selection_works() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut grid: Grid<Cell> = Grid::new(3, 5, 0);
        for i in 0..5 {
            for j in 0..2 {
                grid[Line(j)][Column(i)].c = 'a';
            }
        }
        grid[Line(0)][Column(0)].c = '"';
        grid[Line(0)][Column(3)].c = '"';
        grid[Line(1)][Column(2)].c = '"';
        grid[Line(0)][Column(4)].flags.insert(Flags::WRAPLINE);

        let mut escape_chars = String::from("\"");

        mem::swap(&mut term.grid, &mut grid);
        mem::swap(&mut term.config.semantic_escape_chars, &mut escape_chars);

        {
            term.selection = Some(Selection::new(
                SelectionType::Semantic,
                Point {
                    line: Line(0),
                    column: Column(1),
                },
                Side::Left,
            ));
            assert_eq!(term.selection_to_string(), Some(String::from("aa")));
        }

        {
            term.selection = Some(Selection::new(
                SelectionType::Semantic,
                Point {
                    line: Line(0),
                    column: Column(4),
                },
                Side::Left,
            ));
            assert_eq!(term.selection_to_string(), Some(String::from("aaa")));
        }

        {
            term.selection = Some(Selection::new(
                SelectionType::Semantic,
                Point {
                    line: Line(1),
                    column: Column(1),
                },
                Side::Left,
            ));
            assert_eq!(term.selection_to_string(), Some(String::from("aaa")));
        }
    }

    #[test]
    fn line_selection_works() {
        let size = TermSize::new(5, 1);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut grid: Grid<Cell> = Grid::new(1, 5, 0);
        for i in 0..5 {
            grid[Line(0)][Column(i)].c = 'a';
        }
        grid[Line(0)][Column(0)].c = '"';
        grid[Line(0)][Column(3)].c = '"';

        mem::swap(&mut term.grid, &mut grid);

        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Point {
                line: Line(0),
                column: Column(3),
            },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("\"aa\"a\n")));
    }

    #[test]
    fn block_selection_works() {
        let size = TermSize::new(5, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let grid = term.grid_mut();
        for i in 1..4 {
            grid[Line(i)][Column(0)].c = '"';

            for j in 1..4 {
                grid[Line(i)][Column(j)].c = 'a';
            }

            grid[Line(i)][Column(4)].c = '"';
        }
        grid[Line(2)][Column(2)].c = ' ';
        grid[Line(2)][Column(4)].flags.insert(Flags::WRAPLINE);
        grid[Line(3)][Column(4)].c = ' ';

        term.selection = Some(Selection::new(
            SelectionType::Block,
            Point {
                line: Line(0),
                column: Column(3),
            },
            Side::Left,
        ));

        // The same column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Point {
                    line: Line(3),
                    column: Column(3),
                },
                Side::Right,
            );
        }
        assert_eq!(term.selection_to_string(), Some(String::from("\na\na\na")));

        // The first column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Point {
                    line: Line(3),
                    column: Column(0),
                },
                Side::Left,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\n\"aa\n\"a\n\"aa"))
        );

        // The last column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Point {
                    line: Line(3),
                    column: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\na\"\na\"\na"))
        );
    }

    /// Check that the grid can be serialized back and forth losslessly.
    ///
    /// This test is in the term module as opposed to the grid since we want to
    /// test this property with a T=Cell.
    #[test]
    #[cfg(feature = "serde")]
    fn grid_serde() {
        let grid: Grid<Cell> = Grid::new(24, 80, 0);
        let serialized = serde_json::to_string(&grid).expect("ser");
        let deserialized = serde_json::from_str::<Grid<Cell>>(&serialized).expect("de");

        assert_eq!(deserialized, grid);
    }

    #[test]
    fn input_line_drawing_character() {
        let size = TermSize::new(7, 17);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let cursor = Point::new(Line(0), Column(0));
        term.configure_charset(
            CharsetIndex::G0,
            StandardCharset::SpecialCharacterAndLineDrawing,
        );
        term.input('a');

        assert_eq!(term.grid()[cursor].c, '▒');
    }

    #[test]
    fn clearing_viewport_keeps_history_position() {
        let size = TermSize::new(10, 20);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..29 {
            term.newline();
        }

        // Change the display area.
        term.scroll_display(Scroll::Top);

        assert_eq!(term.grid.display_offset(), 10);

        // Clear the viewport.
        term.clear_screen(ansi::ClearMode::All);

        assert_eq!(term.grid.display_offset(), 10);
    }

    #[test]
    fn clearing_viewport_with_vi_mode_keeps_history_position() {
        let size = TermSize::new(10, 20);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..29 {
            term.newline();
        }

        // Enable vi mode.
        term.toggle_vi_mode();

        // Change the display area and the vi cursor position.
        term.scroll_display(Scroll::Top);
        term.vi_mode_cursor.point = Point::new(Line(-5), Column(3));

        assert_eq!(term.grid.display_offset(), 10);

        // Clear the viewport.
        term.clear_screen(ansi::ClearMode::All);

        assert_eq!(term.grid.display_offset(), 10);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-5), Column(3)));
    }

    #[test]
    fn clearing_scrollback_resets_display_offset() {
        let size = TermSize::new(10, 20);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..29 {
            term.newline();
        }

        // Change the display area.
        term.scroll_display(Scroll::Top);

        assert_eq!(term.grid.display_offset(), 10);

        // Clear the scrollback buffer.
        term.clear_screen(ansi::ClearMode::Saved);

        assert_eq!(term.grid.display_offset(), 0);
    }

    #[test]
    fn clearing_scrollback_sets_vi_cursor_into_viewport() {
        let size = TermSize::new(10, 20);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..29 {
            term.newline();
        }

        // Enable vi mode.
        term.toggle_vi_mode();

        // Change the display area and the vi cursor position.
        term.scroll_display(Scroll::Top);
        term.vi_mode_cursor.point = Point::new(Line(-5), Column(3));

        assert_eq!(term.grid.display_offset(), 10);

        // Clear the scrollback buffer.
        term.clear_screen(ansi::ClearMode::Saved);

        assert_eq!(term.grid.display_offset(), 0);
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(3)));
    }

    #[test]
    fn clear_saved_lines() {
        let size = TermSize::new(7, 17);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Add one line of scrollback.
        term.grid.scroll_up(&(Line(0)..Line(1)), 1);

        // Clear the history.
        term.clear_screen(ansi::ClearMode::Saved);

        // Make sure that scrolling does not change the grid.
        let mut scrolled_grid = term.grid.clone();
        scrolled_grid.scroll_display(Scroll::Top);

        // Truncate grids for comparison.
        scrolled_grid.truncate();
        term.grid.truncate();

        assert_eq!(term.grid, scrolled_grid);
    }

    #[test]
    fn vi_cursor_keep_pos_on_scrollback_buffer() {
        let size = TermSize::new(5, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 11 lines of scrollback.
        for _ in 0..20 {
            term.newline();
        }

        // Enable vi mode.
        term.toggle_vi_mode();

        term.scroll_display(Scroll::Top);
        term.vi_mode_cursor.point.line = Line(-11);

        term.linefeed();
        assert_eq!(term.vi_mode_cursor.point.line, Line(-12));
    }

    #[test]
    fn grow_lines_updates_active_cursor_pos() {
        let mut size = TermSize::new(100, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Increase visible lines.
        size.screen_lines = 30;
        term.resize(size);

        assert_eq!(term.history_size(), 0);
        assert_eq!(term.grid.cursor.point, Point::new(Line(19), Column(0)));
    }

    #[test]
    fn grow_lines_updates_inactive_cursor_pos() {
        let mut size = TermSize::new(100, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Enter alt screen.
        term.set_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

        // Increase visible lines.
        size.screen_lines = 30;
        term.resize(size);

        // Leave alt screen.
        term.unset_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

        assert_eq!(term.history_size(), 0);
        assert_eq!(term.grid.cursor.point, Point::new(Line(19), Column(0)));
    }

    #[test]
    fn shrink_lines_updates_active_cursor_pos() {
        let mut size = TermSize::new(100, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Increase visible lines.
        size.screen_lines = 5;
        term.resize(size);

        assert_eq!(term.history_size(), 15);
        assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
    }

    #[test]
    fn shrink_lines_updates_inactive_cursor_pos() {
        let mut size = TermSize::new(100, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Enter alt screen.
        term.set_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

        // Increase visible lines.
        size.screen_lines = 5;
        term.resize(size);

        // Leave alt screen.
        term.unset_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

        assert_eq!(term.history_size(), 15);
        assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
    }

    #[test]
    fn private_mode_47_preserves_alternate_screen_contents() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        term.grid[Line(0)][Column(0)].c = 'p';
        term.grid.cursor.point = Point::new(Line(0), Column(1));

        term.set_private_mode(PrivateMode::Unknown(47));
        term.grid[Line(0)][Column(0)].c = 'a';
        term.grid.cursor.point = Point::new(Line(1), Column(2));
        term.unset_private_mode(PrivateMode::Unknown(47));
        assert_eq!(term.grid[Line(0)][Column(0)].c, 'p');
        assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(2)));

        term.set_private_mode(PrivateMode::Unknown(47));
        assert_eq!(term.grid[Line(0)][Column(0)].c, 'a');
    }

    #[test]
    fn private_mode_1047_clears_alternate_screen_only_on_exit() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        term.set_private_mode(PrivateMode::Unknown(47));
        term.grid[Line(0)][Column(0)].c = 'a';
        term.unset_private_mode(PrivateMode::Unknown(47));

        term.set_private_mode(PrivateMode::Unknown(1047));
        assert_eq!(term.grid[Line(0)][Column(0)].c, 'a');
        term.unset_private_mode(PrivateMode::Unknown(1047));

        term.set_private_mode(PrivateMode::Unknown(47));
        assert_eq!(term.grid[Line(0)][Column(0)].c, ' ');
    }

    #[test]
    fn private_mode_1049_clears_on_entry_and_preserves_on_exit() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        term.set_private_mode(PrivateMode::Unknown(47));
        term.grid[Line(0)][Column(0)].c = 'a';
        term.unset_private_mode(PrivateMode::Unknown(47));

        term.grid.cursor.point = Point::new(Line(0), Column(1));
        term.set_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());
        assert_eq!(term.grid[Line(0)][Column(0)].c, ' ');
        term.grid[Line(0)][Column(0)].c = 'b';
        term.grid.cursor.point = Point::new(Line(1), Column(2));
        term.unset_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());
        assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(1)));

        term.set_private_mode(PrivateMode::Unknown(47));
        assert_eq!(term.grid[Line(0)][Column(0)].c, 'b');
    }

    #[test]
    fn graphics_event_journal_preserves_parser_execution_order() {
        let size = TermSize::new(5, 5);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut processor: ansi::Processor = ansi::Processor::new();

        processor.advance(
            &mut term,
            b"\x1b[2J\x1b[?47h\x1b[2;4r\x1b[4H\n\x1b[?47l\x1bc",
        );

        assert_eq!(
            term.take_graphics_events(),
            GraphicsEventBatch {
                events: vec![
                    GraphicsEvent::EraseDisplay,
                    GraphicsEvent::EnterAlternate {
                        mode: 47,
                        clear: false,
                    },
                    GraphicsEvent::Scroll {
                        columns: None,
                        direction: GraphicsScrollDirection::Up,
                        top: 1,
                        bottom: 4,
                        lines: 1,
                        old_screen_top: 0,
                        new_screen_top: 0,
                        screen_lines: 5,
                    },
                    GraphicsEvent::LeaveAlternate {
                        mode: 47,
                        clear: false,
                    },
                    GraphicsEvent::Reset,
                ],
                overflowed: false,
                alternate_screen: false,
            }
        );
    }

    #[test]
    fn graphics_event_journal_coalesces_adjacent_scrolls_without_losing_screen_delta() {
        let size = TermSize::new(5, 4);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        for _ in 0..10 {
            term.scroll_up_relative(Line(0), 1);
        }

        assert_eq!(
            term.take_graphics_events(),
            GraphicsEventBatch {
                events: vec![GraphicsEvent::Scroll {
                    columns: None,
                    direction: GraphicsScrollDirection::Up,
                    top: 0,
                    bottom: 4,
                    lines: 4,
                    old_screen_top: 0,
                    new_screen_top: 10,
                    screen_lines: 4,
                }],
                overflowed: false,
                alternate_screen: false,
            }
        );
    }

    #[test]
    fn graphics_event_journal_overflow_is_sticky_until_drain_and_recovers() {
        let size = TermSize::new(5, 3);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        for _ in 0..=(GRAPHICS_EVENT_JOURNAL_CAPACITY / 2) {
            term.set_private_mode(PrivateMode::Unknown(47));
            term.unset_private_mode(PrivateMode::Unknown(47));
        }
        term.set_private_mode(PrivateMode::Unknown(47));

        assert_eq!(
            term.take_graphics_events(),
            GraphicsEventBatch {
                events: Vec::new(),
                overflowed: true,
                alternate_screen: true,
            }
        );

        term.unset_private_mode(PrivateMode::Unknown(47));
        assert_eq!(
            term.take_graphics_events(),
            GraphicsEventBatch {
                events: vec![GraphicsEvent::LeaveAlternate {
                    mode: 47,
                    clear: false,
                }],
                overflowed: false,
                alternate_screen: false,
            }
        );
    }

    #[test]
    fn damage_public_usage() {
        let size = TermSize::new(10, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        // Reset terminal for partial damage tests since it's initialized as fully damaged.
        term.reset_damage();

        // Test that we damage input form [`Term::input`].

        let left = term.grid.cursor.point.column.0;
        term.input('d');
        term.input('a');
        term.input('m');
        term.input('a');
        term.input('g');
        term.input('e');
        let right = term.grid.cursor.point.column.0;

        let mut damaged_lines = match term.damage() {
            TermDamage::Full => panic!("Expected partial damage, however got Full"),
            TermDamage::Partial(damaged_lines) => damaged_lines,
        };
        assert_eq!(
            damaged_lines.next(),
            Some(LineDamageBounds {
                line: 0,
                left,
                right
            })
        );
        assert_eq!(damaged_lines.next(), None);
        term.reset_damage();

        // Create scrollback.
        for _ in 0..20 {
            term.newline();
        }

        match term.damage() {
            TermDamage::Full => (),
            TermDamage::Partial(_) => panic!("Expected Full damage, however got Partial "),
        };
        term.reset_damage();

        term.scroll_display(Scroll::Delta(10));
        term.reset_damage();

        // No damage when scrolled into viewport.
        for idx in 0..term.columns() {
            term.goto(idx as i32, idx);
        }
        let mut damaged_lines = match term.damage() {
            TermDamage::Full => panic!("Expected partial damage, however got Full"),
            TermDamage::Partial(damaged_lines) => damaged_lines,
        };
        assert_eq!(damaged_lines.next(), None);

        // Scroll back into the viewport, so we have 2 visible lines which terminal can write
        // to.
        term.scroll_display(Scroll::Delta(-2));
        term.reset_damage();

        term.goto(0, 0);
        term.goto(1, 0);
        term.goto(2, 0);
        let display_offset = term.grid().display_offset();
        let mut damaged_lines = match term.damage() {
            TermDamage::Full => panic!("Expected partial damage, however got Full"),
            TermDamage::Partial(damaged_lines) => damaged_lines,
        };
        assert_eq!(
            damaged_lines.next(),
            Some(LineDamageBounds {
                line: display_offset,
                left: 0,
                right: 0
            })
        );
        assert_eq!(
            damaged_lines.next(),
            Some(LineDamageBounds {
                line: display_offset + 1,
                left: 0,
                right: 0
            })
        );
        assert_eq!(damaged_lines.next(), None);
    }

    #[test]
    fn damage_cursor_movements() {
        let size = TermSize::new(10, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let num_cols = term.columns();
        // Reset terminal for partial damage tests since it's initialized as fully damaged.
        term.reset_damage();

        term.goto(1, 1);

        // NOTE While we can use `[Term::damage]` to access terminal damage information, in the
        // following tests we will be accessing `term.damage.lines` directly to avoid adding extra
        // damage information (like cursor and Vi cursor), which we're not testing.

        assert_eq!(
            term.damage.lines[0],
            LineDamageBounds {
                line: 0,
                left: 0,
                right: 0
            }
        );
        assert_eq!(
            term.damage.lines[1],
            LineDamageBounds {
                line: 1,
                left: 1,
                right: 1
            }
        );
        term.damage.reset(num_cols);

        term.move_forward(3);
        assert_eq!(
            term.damage.lines[1],
            LineDamageBounds {
                line: 1,
                left: 1,
                right: 4
            }
        );
        term.damage.reset(num_cols);

        term.move_backward(8);
        assert_eq!(
            term.damage.lines[1],
            LineDamageBounds {
                line: 1,
                left: 0,
                right: 4
            }
        );
        term.goto(5, 5);
        term.damage.reset(num_cols);

        term.backspace();
        term.backspace();
        assert_eq!(
            term.damage.lines[5],
            LineDamageBounds {
                line: 5,
                left: 3,
                right: 5
            }
        );
        term.damage.reset(num_cols);

        term.move_up(1);
        assert_eq!(
            term.damage.lines[5],
            LineDamageBounds {
                line: 5,
                left: 3,
                right: 3
            }
        );
        assert_eq!(
            term.damage.lines[4],
            LineDamageBounds {
                line: 4,
                left: 3,
                right: 3
            }
        );
        term.damage.reset(num_cols);

        term.move_down(1);
        term.move_down(1);
        assert_eq!(
            term.damage.lines[4],
            LineDamageBounds {
                line: 4,
                left: 3,
                right: 3
            }
        );
        assert_eq!(
            term.damage.lines[5],
            LineDamageBounds {
                line: 5,
                left: 3,
                right: 3
            }
        );
        assert_eq!(
            term.damage.lines[6],
            LineDamageBounds {
                line: 6,
                left: 3,
                right: 3
            }
        );
        term.damage.reset(num_cols);

        term.wrapline();
        assert_eq!(
            term.damage.lines[6],
            LineDamageBounds {
                line: 6,
                left: 3,
                right: 3
            }
        );
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 0,
                right: 0
            }
        );
        term.move_forward(3);
        term.move_up(1);
        term.damage.reset(num_cols);

        term.linefeed();
        assert_eq!(
            term.damage.lines[6],
            LineDamageBounds {
                line: 6,
                left: 3,
                right: 3
            }
        );
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 3,
                right: 3
            }
        );
        term.damage.reset(num_cols);

        term.carriage_return();
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 0,
                right: 3
            }
        );
        term.damage.reset(num_cols);

        // Columns 0 to 4: the damage covers the erased cells, no more.
        term.erase_chars(5);
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 0,
                right: 4
            }
        );
        term.damage.reset(num_cols);

        term.delete_chars(3);
        let right = term.columns() - 1;
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 0,
                right
            }
        );
        term.move_forward(term.columns());
        term.damage.reset(num_cols);

        term.move_backward_tabs(1);
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 8,
                right
            }
        );
        term.save_cursor_position();
        term.goto(1, 1);
        term.damage.reset(num_cols);

        term.restore_cursor_position();
        assert_eq!(
            term.damage.lines[1],
            LineDamageBounds {
                line: 1,
                left: 1,
                right: 1
            }
        );
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 8,
                right: 8
            }
        );
        term.damage.reset(num_cols);

        term.clear_line(ansi::LineClearMode::All);
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 0,
                right
            }
        );
        term.damage.reset(num_cols);

        term.clear_line(ansi::LineClearMode::Left);
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 0,
                right: 8
            }
        );
        term.damage.reset(num_cols);

        term.clear_line(ansi::LineClearMode::Right);
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 8,
                right
            }
        );
        term.damage.reset(num_cols);

        term.reverse_index();
        assert_eq!(
            term.damage.lines[7],
            LineDamageBounds {
                line: 7,
                left: 8,
                right: 8
            }
        );
        assert_eq!(
            term.damage.lines[6],
            LineDamageBounds {
                line: 6,
                left: 8,
                right: 8
            }
        );
    }

    #[test]
    fn full_damage() {
        let size = TermSize::new(100, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        assert!(term.damage.full);
        for _ in 0..20 {
            term.newline();
        }
        term.reset_damage();

        term.clear_screen(ansi::ClearMode::Above);
        assert!(term.damage.full);
        term.reset_damage();

        term.scroll_display(Scroll::Top);
        assert!(term.damage.full);
        term.reset_damage();

        // Sequential call to scroll display without doing anything shouldn't damage.
        term.scroll_display(Scroll::Top);
        assert!(!term.damage.full);
        term.reset_damage();

        term.set_options(Config::default());
        assert!(term.damage.full);
        term.reset_damage();

        term.scroll_down_relative(Line(5), 2);
        assert!(term.damage.full);
        term.reset_damage();

        term.scroll_up_relative(Line(3), 2);
        assert!(term.damage.full);
        term.reset_damage();

        term.deccolm();
        assert!(term.damage.full);
        term.reset_damage();

        term.decaln();
        assert!(term.damage.full);
        term.reset_damage();

        term.set_mode(NamedMode::Insert.into());
        // Just setting `Insert` mode shouldn't mark terminal as damaged.
        assert!(!term.damage.full);
        term.reset_damage();

        let color_index = 257;
        term.set_color(color_index, Rgb::default());
        assert!(term.damage.full);
        term.reset_damage();

        // Setting the same color once again shouldn't trigger full damage.
        term.set_color(color_index, Rgb::default());
        assert!(!term.damage.full);

        term.reset_color(color_index);
        assert!(term.damage.full);
        term.reset_damage();

        // We shouldn't trigger fully damage when cursor gets update.
        term.set_color(NamedColor::Cursor as usize, Rgb::default());
        assert!(!term.damage.full);

        // However requesting terminal damage should mark terminal as fully damaged in `Insert`
        // mode.
        let _ = term.damage();
        assert!(term.damage.full);
        term.reset_damage();

        term.unset_mode(NamedMode::Insert.into());
        assert!(term.damage.full);
        term.reset_damage();

        // Keep this as a last check, so we don't have to deal with restoring from alt-screen.
        term.swap_alt();
        assert!(term.damage.full);
        term.reset_damage();

        let size = TermSize::new(10, 10);
        term.resize(size);
        assert!(term.damage.full);
    }

    #[test]
    fn window_title() {
        let size = TermSize::new(7, 17);
        let mut term = Term::new(Config::default(), &size, VoidListener);

        // Title None by default.
        assert_eq!(term.title, None);

        // Title can be set.
        term.set_title(Some("Test".into()));
        assert_eq!(term.title, Some("Test".into()));

        // Title can be pushed onto stack.
        term.push_title();
        term.set_title(Some("Next".into()));
        assert_eq!(term.title, Some("Next".into()));
        assert_eq!(term.title_stack.first().unwrap(), &Some("Test".into()));

        // Title can be popped from stack and set as the window title.
        term.pop_title();
        assert_eq!(term.title, Some("Test".into()));
        assert!(term.title_stack.is_empty());

        // Title stack doesn't grow infinitely.
        for _ in 0..4097 {
            term.push_title();
        }
        assert_eq!(term.title_stack.len(), 4096);

        // Title and title stack reset when terminal state is reset.
        term.push_title();
        term.reset_state();
        assert_eq!(term.title, None);
        assert!(term.title_stack.is_empty());

        // Title stack pops back to default.
        term.title = None;
        term.push_title();
        term.set_title(Some("Test".into()));
        term.pop_title();
        assert_eq!(term.title, None);

        // Title can be reset to default.
        term.title = Some("Test".into());
        term.set_title(None);
        assert_eq!(term.title, None);
    }

    #[test]
    fn parse_cargo_version() {
        assert!(version_number(env!("CARGO_PKG_VERSION")) >= 10_01);
        assert_eq!(version_number("0.0.1-dev"), 1);
        assert_eq!(version_number("0.1.2-dev"), 1_02);
        assert_eq!(version_number("1.2.3-dev"), 1_02_03);
        assert_eq!(version_number("999.99.99"), 9_99_99_99);
    }
}
