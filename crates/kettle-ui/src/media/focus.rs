//! A preview lane holding the keyboard (`focus_preview`). While it does,
//! every key is the lane's: a few move, zoom, fit, page or copy what it
//! shows, Esc gives the keyboard back to the terminal, and every other key
//! and chord, Enter, digits and the application's shortcuts included, does
//! nothing, so none reaches the program in the pane by accident.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Which lane holds the keyboard: the pane's, while it shows `item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreviewFocus {
    pub pane: u64,
    pub item: u64,
}

/// What a key does while a lane holds the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewKey {
    /// Give the keyboard back to the terminal.
    Exit,
    /// Move toward a side, as an arrow names it: `(-1, 0)` is left.
    Move(i32, i32),
    ZoomIn,
    ZoomOut,
    /// Fit the picture again, or show a source from its start.
    Fit,
    /// A gallery's page before (`-1`) or after (`1`) the one shown, as
    /// Page Up and Page Down.
    Page(i32),
    Copy,
    /// Nothing: the key is the lane's all the same.
    Swallow,
}

impl PreviewKey {
    /// Whether the key acts once a press: holding it does not repeat.
    pub(crate) fn once(self) -> bool {
        matches!(self, Self::Exit | Self::Copy | Self::Fit)
    }
}

/// What `key` with `mods` held does while a lane holds the keyboard. Shift
/// counts only where it makes `+`; with Ctrl, Alt or the Command key held,
/// every key is swallowed.
pub(crate) fn preview_key(key: &Key, mods: ModifiersState) -> PreviewKey {
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return PreviewKey::Swallow;
    }
    let shift = mods.shift_key();
    match key {
        Key::Named(NamedKey::Escape) => PreviewKey::Exit,
        Key::Named(arrow) if !shift => match arrow {
            NamedKey::ArrowLeft => PreviewKey::Move(-1, 0),
            NamedKey::ArrowRight => PreviewKey::Move(1, 0),
            NamedKey::ArrowUp => PreviewKey::Move(0, -1),
            NamedKey::ArrowDown => PreviewKey::Move(0, 1),
            NamedKey::PageUp => PreviewKey::Page(-1),
            NamedKey::PageDown => PreviewKey::Page(1),
            _ => PreviewKey::Swallow,
        },
        Key::Character(text) => match (text.as_str(), shift) {
            ("+", _) | ("=", false) => PreviewKey::ZoomIn,
            ("-", false) => PreviewKey::ZoomOut,
            ("0", false) => PreviewKey::Fit,
            ("c", false) => PreviewKey::Copy,
            _ => PreviewKey::Swallow,
        },
        _ => PreviewKey::Swallow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    /// Esc, the arrows, Page Up and Page Down, `+`, `-`, `0` and `c` act;
    /// Shift only makes `+`,
    /// and with Ctrl, Alt or Command held nothing acts. Enter, digits, y and
    /// n, Tab and every other key are swallowed.
    #[test]
    fn a_focused_lane_takes_a_few_keys_and_swallows_the_rest() {
        let none = ModifiersState::empty();
        let shift = ModifiersState::SHIFT;
        assert_eq!(
            preview_key(&Key::Named(NamedKey::Escape), none),
            PreviewKey::Exit
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::Escape), shift),
            PreviewKey::Exit
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::ArrowLeft), none),
            PreviewKey::Move(-1, 0)
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::ArrowDown), none),
            PreviewKey::Move(0, 1)
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::ArrowUp), shift),
            PreviewKey::Swallow
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::PageUp), none),
            PreviewKey::Page(-1)
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::PageDown), none),
            PreviewKey::Page(1)
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::PageDown), shift),
            PreviewKey::Swallow
        );
        assert_eq!(preview_key(&character("+"), shift), PreviewKey::ZoomIn);
        assert_eq!(preview_key(&character("="), none), PreviewKey::ZoomIn);
        assert_eq!(preview_key(&character("="), shift), PreviewKey::Swallow);
        assert_eq!(preview_key(&character("-"), none), PreviewKey::ZoomOut);
        assert_eq!(preview_key(&character("0"), none), PreviewKey::Fit);
        assert_eq!(preview_key(&character("c"), none), PreviewKey::Copy);
        assert_eq!(preview_key(&character("C"), shift), PreviewKey::Swallow);
        for key in [
            Key::Named(NamedKey::Enter),
            Key::Named(NamedKey::Tab),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Backspace),
            character("1"),
            character("9"),
            character("y"),
            character("n"),
            character("q"),
        ] {
            assert_eq!(preview_key(&key, none), PreviewKey::Swallow, "{key:?}");
        }
        for mods in [
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SUPER,
        ] {
            for key in [
                Key::Named(NamedKey::Escape),
                character("c"),
                character("+"),
                Key::Named(NamedKey::ArrowLeft),
            ] {
                assert_eq!(
                    preview_key(&key, mods),
                    PreviewKey::Swallow,
                    "{mods:?} {key:?}"
                );
            }
        }
        assert!(PreviewKey::Exit.once() && PreviewKey::Copy.once() && PreviewKey::Fit.once());
        assert!(!PreviewKey::Move(1, 0).once() && !PreviewKey::ZoomIn.once());
    }
}
