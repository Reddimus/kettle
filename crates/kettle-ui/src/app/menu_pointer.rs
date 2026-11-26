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
}

/// Per-menu state kept beside `WindowState::context_menu`, reset each time a
/// menu opens.
#[derive(Debug, Default)]
pub(crate) struct ContextMenuSession {
    pub(super) pointer: MenuPointer,
    /// The tallest the menu's own panel may be, in surface pixels. `None`
    /// leaves it as tall as the surface allows.
    pub(super) root_max_h: Option<f32>,
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
/// [`SUBMENU_SWITCH_DELAY`] outside that triangle or at rest.
pub(super) fn menu_pointer_moved(
    menu: &mut ContextMenuState,
    pointer: &mut MenuPointer,
    hit: MenuHit,
    panels: &[kettle_render::Rect4],
    at: (f32, f32),
    now: std::time::Instant,
) -> MenuHoverEffect {
    let from = pointer.last.replace(at);
    let moved = from != Some(at);
    let (depth, index) = match hit {
        MenuHit::Row(index) => {
            pointer.switch = None;
            return hover_row(menu, pointer, index, now);
        }
        MenuHit::ParentRow { depth, index } => (depth, index),
        MenuHit::Panel | MenuHit::Outside => {
            pointer.cancel_pending();
            return MenuHoverEffect::None;
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
    let aiming = moved
        && from
            .zip(panels.get(depth + 1))
            .is_some_and(|(from, submenu)| aims_at(from, at, *submenu));
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
        return MenuHoverEffect::None;
    }
    pointer.switch = None;
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
    let changed = menu.highlight != index;
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
/// between `from` and the near edge of `submenu`: on its way there across
/// the parent's other rows.
fn aims_at(from: (f32, f32), to: (f32, f32), (x, y, w, h): kettle_render::Rect4) -> bool {
    let edge = if from.0 <= x {
        x
    } else if from.0 >= x + w {
        x + w
    } else {
        return false;
    };
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let corners = [from, (edge, y), (edge, y + h)];
    let sides = [
        cross(corners[0], corners[1], to),
        cross(corners[1], corners[2], to),
        cross(corners[2], corners[0], to),
    ];
    !(sides.iter().any(|side| *side < 0.0) && sides.iter().any(|side| *side > 0.0))
}

/// The row an arrow key moves the highlight to, `delta` rows on.
pub(super) fn menu_step(menu: &ContextMenuState, pointer: &mut MenuPointer, delta: isize) -> usize {
    pointer.cancel_pending();
    next_context_menu_highlight(&menu.items, menu.highlight, delta)
}

/// What Enter runs, or `None` when the highlight is on nothing it can run.
pub(super) fn menu_activate_click(
    menu: &ContextMenuState,
    _pointer: &MenuPointer,
) -> Option<ContextMenuClick> {
    menu.items
        .get(menu.highlight)
        .and_then(|item| item_to_click(item, menu.highlight))
}

/// The highlighted row the menu paints and names to assistive technology.
pub(super) fn menu_shown_highlight(menu: &ContextMenuState, _pointer: &MenuPointer) -> usize {
    menu.highlight
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
}

/// Where a menu's panel goes, and how tall it may be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MenuPlacement {
    /// The panel's top-left corner.
    pub(super) origin: (f32, f32),
    pub(super) max_h: Option<f32>,
}

/// Place a `panel`-sized menu for `anchor` on a `surface`-sized window.
pub(super) fn place_context_menu(
    anchor: MenuAnchor,
    panel: (f32, f32),
    surface: (f32, f32),
) -> MenuPlacement {
    let y = (anchor.top + anchor.bottom) * 0.5;
    MenuPlacement {
        origin: clamp_context_menu_anchor((anchor.x, y), panel, surface),
        max_h: None,
    }
}

#[cfg(test)]
#[path = "menu_pointer_tests.rs"]
mod tests;
