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

/// Pointer state an open menu keeps between hover events. It lives beside
/// the menu in [`ContextMenuSession`] and starts afresh with every menu.
#[derive(Debug, Default)]
pub(crate) struct MenuPointer {}

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
pub(super) fn menu_pointer_moved(
    menu: &mut ContextMenuState,
    _pointer: &mut MenuPointer,
    hit: MenuHit,
    _panels: &[kettle_render::Rect4],
    _at: (f32, f32),
    _now: std::time::Instant,
) -> MenuHoverEffect {
    let index = match hit {
        MenuHit::Row(index) => index,
        MenuHit::ParentRow { depth, index } => {
            if menu
                .drill_stack
                .get(depth)
                .is_none_or(|level| level.highlight == index)
            {
                return MenuHoverEffect::None;
            }
            while menu.drill_stack.len() > depth {
                menu.leave_submenu();
            }
            index
        }
        MenuHit::Panel | MenuHit::Outside => return MenuHoverEffect::None,
    };
    let changed = menu.highlight != index;
    menu.highlight = index;
    if matches!(menu.items.get(index), Some(ContextMenuItem::Submenu { .. })) {
        MenuHoverEffect::OpenSubmenu(index)
    } else if changed {
        MenuHoverEffect::Redraw
    } else {
        MenuHoverEffect::None
    }
}

/// The row an arrow key moves the highlight to, `delta` rows on.
pub(super) fn menu_step(
    menu: &ContextMenuState,
    _pointer: &mut MenuPointer,
    delta: isize,
) -> usize {
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
