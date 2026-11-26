//! What the pointer and the keys choose in an open context menu, as pure
//! functions over [`ContextMenuState`], so hover, Enter and the arrows can be
//! tested without a window.

use super::*;

/// What the pointer is over in an open menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuHit {
    /// Row `index` of the deepest level, the one the keys move in.
    Row(usize),
    /// Row `index` of the parent level `depth`; the menu itself is depth 0.
    ParentRow { depth: usize, index: usize },
    /// A panel, but no row: a separator or a scroll strip.
    Panel,
    /// No panel.
    Outside,
}

/// What a hover asks of the App.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuHoverEffect {
    None,
    Redraw,
    /// Open the submenu of row `index` of the deepest level.
    OpenSubmenu(usize),
}

/// How long the pointer rests on a submenu's row before the submenu opens. A
/// click or Right opens it at once.
pub(super) const SUBMENU_OPEN_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// How long an open submenu holds while the pointer crosses other rows of its
/// parent: from the pointer's last motion while it heads for the submenu, and
/// from the moment it turned away otherwise.
pub(super) const SUBMENU_SWITCH_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// Pointer state an open menu keeps between hover events. It lives beside
/// the menu in [`ContextMenuSession`] and starts afresh with every menu.
#[derive(Debug, Default)]
pub(crate) struct MenuPointer {
    /// Where the pointer was at the last hover event.
    last: Option<(f32, f32)>,
    /// A submenu row waiting out its dwell.
    open: Option<PendingOpen>,
    /// An open submenu the pointer has left for another row of a parent.
    switch: Option<PendingSwitch>,
    /// Whether the pointer was over one of the menu's panels.
    inside: bool,
    /// The pointer left the window and has not moved in it since. Its last
    /// position in the window may lie on a row, but it is over nothing.
    off_window: bool,
    /// Whether the pointer has been over the menu at all. An open submenu
    /// times out off the panels only then, so one opened from the keyboard
    /// stays while a pointer that never came near it moves.
    entered: bool,
    /// The pointer left the menu with no submenu open, so no row is lit,
    /// as in macOS and GTK menus. The pointer back on a row, or a key,
    /// lights one again. A pointer that was never over the menu, as when
    /// it was opened from the keyboard, leaves the highlight alone.
    away: bool,
}

#[derive(Clone, Copy, Debug)]
struct PendingOpen {
    depth: usize,
    index: usize,
    at: std::time::Instant,
}

#[derive(Clone, Copy, Debug)]
struct PendingSwitch {
    /// When the pointer last moved.
    moved: std::time::Instant,
    /// When it turned away from the submenu; `None` while it heads for it.
    strayed: Option<std::time::Instant>,
}

impl PendingSwitch {
    fn due(self) -> std::time::Instant {
        let rest = self.moved + SUBMENU_SWITCH_DELAY;
        self.strayed
            .map_or(rest, |since| rest.min(since + SUBMENU_SWITCH_DELAY))
    }
}

impl MenuPointer {
    /// When a waiting submenu opens or gives way, if one is waiting. The
    /// scheduler wakes then and looks at the hover again.
    pub(super) fn deadline(&self) -> Option<std::time::Instant> {
        self.open
            .map(|open| open.at)
            .into_iter()
            .chain(self.switch.map(PendingSwitch::due))
            .min()
    }

    /// Drop a waiting open or switch: keys and clicks act at once.
    pub(super) fn cancel_pending(&mut self) {
        self.open = None;
        self.switch = None;
    }

    /// The pointer left the window: until it next moves in the window, it is
    /// over nothing, wherever its last position in the window was.
    pub(super) fn left_window(&mut self) {
        self.off_window = true;
    }

    /// The pointer moved in the window.
    pub(super) fn moved_in_window(&mut self) {
        self.off_window = false;
    }

    /// What the pointer is over for a look at a deadline, given what its last
    /// position in the window, `last`, would hit.
    pub(super) fn due_hit(&self, last: MenuHit) -> MenuHit {
        if self.off_window {
            MenuHit::Outside
        } else {
            last
        }
    }

    /// A key moved the highlight, or opened a level: it shows again.
    pub(super) fn keyboard_moved(&mut self) {
        self.away = false;
    }
}

/// Per-menu state kept beside `WindowState::context_menu`, reset each time a
/// menu opens.
#[derive(Debug, Default)]
pub(crate) struct ContextMenuSession {
    pub(super) pointer: MenuPointer,
    /// The tallest the menu's own panel may be, in surface pixels. `None`
    /// leaves it as tall as the surface allows.
    pub(super) root_max_h: Option<f32>,
    /// The menu was placed beside the text cursor's line, as one opened from
    /// the keyboard is, and follows that line when the window resizes.
    pub(super) line: bool,
    /// What the menu's panel was placed around: the pointer, or the text
    /// cursor's line. `ui_geometry` reports it beside the panel's own rect.
    pub(super) anchor: Option<MenuAnchor>,
    /// For a tab's menu, that tab, by the panes it holds
    /// (`Mux::tab_anchor_panes`), so a tab closing meanwhile cannot
    /// retarget it.
    pub(super) tab: Option<Vec<u64>>,
}

impl ContextMenuSession {
    /// The state a menu starts with: its height limit, and `pointer`, the
    /// pointer's position when the menu opened right there, as a right-click
    /// does. Such a menu counts as entered at that point, so a first motion
    /// away from it leaves the menu and clears its highlight; a menu opened
    /// elsewhere, as from the keyboard, waits for the pointer to come over it.
    pub(super) fn opened(root_max_h: Option<f32>, pointer: Option<(f32, f32)>) -> Self {
        let mut session = Self {
            root_max_h,
            ..Self::default()
        };
        if let Some(at) = pointer {
            session.pointer.last = Some(at);
            session.pointer.inside = true;
            session.pointer.entered = true;
        }
        session
    }
}

/// Move the hover to `hit`. `panels` are the levels' rectangles, the menu
/// itself first, and `at` is the pointer, both in surface pixels, at `now`.
/// The scheduler calls it again at [`MenuPointer::deadline`] with the pointer
/// where it was.
///
/// A submenu opens once the pointer has rested on its row for
/// [`SUBMENU_OPEN_DELAY`]. An open submenu stays while the pointer crosses
/// other rows of its parent inside the triangle from its last position to the
/// submenu's near edge, as macOS and GTK menus do, and gives way after
/// [`SUBMENU_SWITCH_DELAY`] outside that triangle or at rest. Off every panel
/// it gives way the same way, and every submenu closes.
pub(super) fn menu_pointer_moved(
    menu: &mut ContextMenuState,
    pointer: &mut MenuPointer,
    hit: MenuHit,
    panels: &[kettle_render::Rect4],
    at: (f32, f32),
    now: std::time::Instant,
) -> MenuHoverEffect {
    let from = pointer.last.replace(at);
    let over = hit != MenuHit::Outside;
    let was_inside = std::mem::replace(&mut pointer.inside, over);
    pointer.entered |= over;
    let (depth, index) = match hit {
        MenuHit::Row(index) => {
            pointer.switch = None;
            return hover_row(menu, pointer, index, now);
        }
        MenuHit::ParentRow { depth, index } => (depth, index),
        MenuHit::Panel => {
            pointer.cancel_pending();
            return MenuHoverEffect::None;
        }
        MenuHit::Outside => {
            pointer.open = None;
            if menu.drill_stack.is_empty() {
                pointer.switch = None;
                if !was_inside || std::mem::replace(&mut pointer.away, true) {
                    return MenuHoverEffect::None;
                }
                return MenuHoverEffect::Redraw;
            }
            // An open submenu the pointer has left for no panel at all gives
            // way as it would to another row of its parent, unless the
            // pointer heads for it; then every submenu closes and, the
            // pointer being off the menu, no row is lit.
            if !pointer.entered
                || !submenu_gives_way(pointer, panels.last().copied(), from, at, now)
            {
                return MenuHoverEffect::None;
            }
            while menu.leave_submenu() {}
            pointer.away = true;
            return MenuHoverEffect::Redraw;
        }
    };
    pointer.open = None;
    // Back on the open row, or on a row there that cannot run: the open
    // submenu stays.
    if menu.drill_stack.get(depth).is_none_or(|level| {
        level.highlight == index || !level.items.get(index).is_some_and(item_is_dispatchable)
    }) {
        pointer.switch = None;
        return MenuHoverEffect::None;
    }
    // Level `depth + 1` is the submenu the parent row opened.
    if !submenu_gives_way(pointer, panels.get(depth + 1).copied(), from, at, now) {
        return MenuHoverEffect::None;
    }
    while menu.drill_stack.len() > depth {
        menu.leave_submenu();
    }
    menu.highlight = index;
    // The switch waited longer than a dwell, so a submenu opens at once.
    if matches!(menu.items.get(index), Some(ContextMenuItem::Submenu { .. })) {
        MenuHoverEffect::OpenSubmenu(index)
    } else {
        MenuHoverEffect::Redraw
    }
}

/// Run the wait of an open submenu, whose panel is `submenu`, that the
/// pointer has turned from, moving from `from` to `at`. True once the wait is
/// over and the submenu gives way.
fn submenu_gives_way(
    pointer: &mut MenuPointer,
    submenu: Option<kettle_render::Rect4>,
    from: Option<(f32, f32)>,
    at: (f32, f32),
    now: std::time::Instant,
) -> bool {
    let moved = from != Some(at);
    let aiming = moved
        && from
            .zip(submenu)
            .is_some_and(|(from, submenu)| aims_at(from, at, submenu));
    let switch = pointer.switch.get_or_insert(PendingSwitch {
        moved: now,
        strayed: None,
    });
    if moved {
        switch.moved = now;
        if aiming {
            switch.strayed = None;
        } else {
            switch.strayed.get_or_insert(now);
        }
    }
    if now < switch.due() {
        return false;
    }
    pointer.switch = None;
    true
}

/// Hover row `index` of the deepest level. A row that cannot run (a disabled
/// Copy, an information line) is never lit: the highlight stays where it
/// was, so Enter cannot land on it.
fn hover_row(
    menu: &mut ContextMenuState,
    pointer: &mut MenuPointer,
    index: usize,
    now: std::time::Instant,
) -> MenuHoverEffect {
    if !menu.items.get(index).is_some_and(item_is_dispatchable) {
        pointer.open = None;
        return MenuHoverEffect::None;
    }
    let changed = menu.highlight != index || std::mem::take(&mut pointer.away);
    menu.highlight = index;
    let lit = if changed {
        MenuHoverEffect::Redraw
    } else {
        MenuHoverEffect::None
    };
    if !matches!(menu.items.get(index), Some(ContextMenuItem::Submenu { .. })) {
        pointer.open = None;
        return lit;
    }
    let depth = menu.drill_stack.len();
    let open = match pointer.open {
        Some(open) if (open.depth, open.index) == (depth, index) => open,
        _ => *pointer.open.insert(PendingOpen {
            depth,
            index,
            at: now + SUBMENU_OPEN_DELAY,
        }),
    };
    if now < open.at {
        return lit;
    }
    pointer.open = None;
    MenuHoverEffect::OpenSubmenu(index)
}

/// Whether the pointer, moving from `from` to `to`, stays inside the triangle
/// between `from` and the side of `submenu` it faces: the two corners that
/// bound the submenu as seen from `from`. That is its way to the submenu
/// across the parent's other rows, whichever side of the parent the submenu
/// opened on, however much it overlaps the parent, and from below or above
/// it as well as beside it. A pointer on or in the submenu is not aiming.
fn aims_at(from: (f32, f32), to: (f32, f32), (x, y, w, h): kettle_render::Rect4) -> bool {
    if w <= 0.0 || h <= 0.0 || (x..=x + w).contains(&from.0) && (y..=y + h).contains(&from.1) {
        return false;
    }
    let toward = |p: (f32, f32)| (p.0 - from.0, p.1 - from.1);
    let cross = |a: (f32, f32), b: (f32, f32)| a.0 * b.1 - a.1 * b.0;
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)].map(toward);
    // Seen from outside, the corners span less than a half turn. Each bound
    // has every other corner on one side of it; of two in line, the farther.
    let bound = |side: f32| {
        corners
            .into_iter()
            .filter(|&corner| {
                corners
                    .iter()
                    .all(|&other| side * cross(corner, other) >= 0.0)
            })
            .max_by(|a, b| (a.0 * a.0 + a.1 * a.1).total_cmp(&(b.0 * b.0 + b.1 * b.1)))
    };
    let (Some(a), Some(b)) = (bound(1.0), bound(-1.0)) else {
        return false;
    };
    let area = cross(a, b);
    // A triangle with no area would take every point on its line.
    if area.abs() <= f32::EPSILON {
        return false;
    }
    let side = area.signum();
    let p = toward(to);
    side * cross(a, p) >= 0.0
        && side * cross((b.0 - a.0, b.1 - a.1), (p.0 - a.0, p.1 - a.1)) >= 0.0
        && side * cross(p, b) >= 0.0
}

/// The row an arrow key moves the highlight to, `delta` rows on.
/// With no row lit, Down starts from the top and Up from the bottom.
pub(super) fn menu_step(menu: &ContextMenuState, pointer: &mut MenuPointer, delta: isize) -> usize {
    pointer.cancel_pending();
    if std::mem::take(&mut pointer.away) {
        let first = if delta < 0 {
            menu.items.iter().rposition(item_is_dispatchable)
        } else {
            menu.items.iter().position(item_is_dispatchable)
        };
        return first.unwrap_or(menu.highlight);
    }
    next_context_menu_highlight(&menu.items, menu.highlight, delta)
}

/// What Enter runs, or `None` when no row is lit or the lit row cannot run.
pub(super) fn menu_activate_click(
    menu: &ContextMenuState,
    pointer: &MenuPointer,
) -> Option<ContextMenuClick> {
    if pointer.away {
        return None;
    }
    menu.items
        .get(menu.highlight)
        .and_then(|item| item_to_click(item, menu.highlight))
}

/// The highlighted row the menu paints and names to assistive technology;
/// `usize::MAX`, no row at all, while the pointer is away.
pub(super) fn menu_shown_highlight(menu: &ContextMenuState, pointer: &MenuPointer) -> usize {
    if pointer.away {
        usize::MAX
    } else {
        menu.highlight
    }
}

/// Where a menu opens: the point it hangs from, and the vertical span of the
/// line it was opened for. A pointer's span is the point itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MenuAnchor {
    pub(super) x: f32,
    pub(super) top: f32,
    pub(super) bottom: f32,
}

impl MenuAnchor {
    pub(super) fn point(x: f32, y: f32) -> Self {
        Self {
            x,
            top: y,
            bottom: y,
        }
    }

    /// A text line from `top` to `bottom`, which the menu leaves in view.
    pub(super) fn line(x: f32, top: f32, bottom: f32) -> Self {
        Self { x, top, bottom }
    }
}

/// `ui_geometry`'s account of what a menu was placed around: the point, and
/// the line's span for one placed beside a text line (`null` for a pointer's
/// menu). The panel itself is the menu's `rect`.
pub(super) fn menu_anchor_json(anchor: Option<MenuAnchor>) -> serde_json::Value {
    match anchor {
        None => serde_json::Value::Null,
        Some(MenuAnchor { x, top, bottom }) => serde_json::json!({
            "x": x,
            "y": (top + bottom) * 0.5,
            "line": (top < bottom).then(|| serde_json::json!({"top": top, "bottom": bottom})),
        }),
    }
}

/// Where a menu's panel goes, and how tall it may be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MenuPlacement {
    /// The panel's top-left corner.
    pub(super) origin: (f32, f32),
    pub(super) max_h: Option<f32>,
}

/// Place a `panel`-sized menu for `anchor` on a `surface`-sized window: below
/// the anchor when it fits there, else above it, as GTK and macOS menus flip.
/// When it fits on neither side, a pointer's menu slides onto the window over
/// the point, and a line's takes the roomier side, held short enough to leave
/// the line in view; it scrolls. `None` when not one complete row, `row_h`
/// tall, fits beside the line: no menu rather than one over it, or off the
/// window.
pub(super) fn place_context_menu(
    anchor: MenuAnchor,
    panel: (f32, f32),
    surface: (f32, f32),
    row_h: f32,
) -> Option<MenuPlacement> {
    const MARGIN: f32 = 4.0;
    let (x, slid) = clamp_context_menu_anchor((anchor.x, anchor.bottom), panel, surface);
    let below = surface.1 - MARGIN - anchor.bottom;
    let above = anchor.top - MARGIN;
    let height = panel.1;
    let (y, max_h) = if height <= below {
        (anchor.bottom.max(MARGIN), None)
    } else if height <= above {
        (anchor.top - height, None)
    } else if anchor.top >= anchor.bottom {
        (slid, None)
    } else if below.max(above) < row_h {
        return None;
    } else if below >= above {
        (anchor.bottom.max(MARGIN), Some(below))
    } else {
        (MARGIN, Some(above))
    };
    Some(MenuPlacement {
        origin: (x, y),
        max_h,
    })
}

#[cfg(test)]
#[path = "menu_pointer_tests.rs"]
mod tests;
