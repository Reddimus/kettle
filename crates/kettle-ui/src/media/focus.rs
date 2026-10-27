//! Keyboard handling for focused previews. Navigation, playback and viewer
//! controls stay in the preview; Esc returns focus to the terminal. Other
//! keys, including modified application shortcuts, are swallowed.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Which lane holds the keyboard: the pane's, while it shows `item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreviewFocus {
    pub pane: u64,
    pub item: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewerKey {
    Focus(bool),
    Activate,
    Pin,
}

pub(crate) fn viewer_key(key: &Key, mods: ModifiersState) -> Option<ViewerKey> {
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return None;
    }
    match key {
        Key::Named(NamedKey::Tab) => Some(ViewerKey::Focus(!mods.shift_key())),
        Key::Named(NamedKey::Enter | NamedKey::Space) if !mods.shift_key() => {
            Some(ViewerKey::Activate)
        }
        Key::Character(text) if text == "p" && !mods.shift_key() => Some(ViewerKey::Pin),
        _ => None,
    }
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
    /// Another of a gallery's pages: the one before or after the one shown,
    /// as Page Up and Page Down, or its first or last, as Home and End.
    Page(PageStep),
    Copy,
    /// Play or pause a video, or stop its silent preview, as Space.
    Play,
    /// Mute a playing video's sound, or let it be heard, as M.
    Mute,
    Theater,
    Fullscreen,
    Menu,
    /// Nothing: the key is the lane's all the same.
    Swallow,
}

impl PreviewKey {
    /// Whether the key acts once a press: holding it does not repeat.
    pub(crate) fn once(self) -> bool {
        matches!(
            self,
            Self::Exit
                | Self::Copy
                | Self::Fit
                | Self::Play
                | Self::Mute
                | Self::Theater
                | Self::Fullscreen
                | Self::Menu
                | Self::Page(PageStep::First | PageStep::Last)
        )
    }
}

/// Which of a gallery's pages to show next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageStep {
    /// So many pages on from the one shown: `-1` is the one before.
    By(i32),
    First,
    Last,
}

/// Map preview keys; Ctrl, Alt and Command chords are swallowed.
pub(crate) fn preview_key(key: &Key, mods: ModifiersState) -> PreviewKey {
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return PreviewKey::Swallow;
    }
    let shift = mods.shift_key();
    match key {
        Key::Named(NamedKey::Escape) => PreviewKey::Exit,
        Key::Named(NamedKey::ContextMenu) if !shift => PreviewKey::Menu,
        Key::Named(NamedKey::F10) if shift => PreviewKey::Menu,
        Key::Named(NamedKey::Space) if !shift => PreviewKey::Play,
        Key::Named(arrow) if !shift => match arrow {
            NamedKey::ArrowLeft => PreviewKey::Move(-1, 0),
            NamedKey::ArrowRight => PreviewKey::Move(1, 0),
            NamedKey::ArrowUp => PreviewKey::Move(0, -1),
            NamedKey::ArrowDown => PreviewKey::Move(0, 1),
            NamedKey::PageUp => PreviewKey::Page(PageStep::By(-1)),
            NamedKey::PageDown => PreviewKey::Page(PageStep::By(1)),
            NamedKey::Home => PreviewKey::Page(PageStep::First),
            NamedKey::End => PreviewKey::Page(PageStep::Last),
            _ => PreviewKey::Swallow,
        },
        Key::Character(text) => match (text.as_str(), shift) {
            ("+", _) | ("=", false) => PreviewKey::ZoomIn,
            ("-", false) => PreviewKey::ZoomOut,
            ("0", false) => PreviewKey::Fit,
            ("c", false) => PreviewKey::Copy,
            ("m", false) => PreviewKey::Mute,
            ("k", false) => PreviewKey::Play,
            ("t", false) => PreviewKey::Theater,
            ("f", false) => PreviewKey::Fullscreen,
            _ => PreviewKey::Swallow,
        },
        _ => PreviewKey::Swallow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_previews_offer_standard_context_menu_keys_once() {
        assert_eq!(
            preview_key(&Key::Named(NamedKey::ContextMenu), ModifiersState::empty()),
            PreviewKey::Menu
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::F10), ModifiersState::SHIFT),
            PreviewKey::Menu
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::F10), ModifiersState::empty()),
            PreviewKey::Swallow
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::ContextMenu), ModifiersState::CONTROL),
            PreviewKey::Swallow
        );
        assert!(PreviewKey::Menu.once());
    }

    #[test]
    fn video_shortcuts_toggle_once_and_never_accept_modified_chords() {
        for (key, action) in [
            ("k", PreviewKey::Play),
            ("t", PreviewKey::Theater),
            ("f", PreviewKey::Fullscreen),
        ] {
            assert_eq!(
                preview_key(&character(key), ModifiersState::empty()),
                action
            );
            assert!(action.once());
            for mods in [
                ModifiersState::SHIFT,
                ModifiersState::ALT,
                ModifiersState::CONTROL,
                ModifiersState::SUPER,
            ] {
                assert_eq!(preview_key(&character(key), mods), PreviewKey::Swallow);
            }
        }
    }

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    #[test]
    fn centered_viewer_uses_standard_focus_and_activation_keys() {
        let none = ModifiersState::empty();
        assert_eq!(
            viewer_key(&Key::Named(NamedKey::Tab), none),
            Some(ViewerKey::Focus(true))
        );
        assert_eq!(
            viewer_key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT),
            Some(ViewerKey::Focus(false))
        );
        assert_eq!(
            viewer_key(&Key::Named(NamedKey::Enter), none),
            Some(ViewerKey::Activate)
        );
        assert_eq!(viewer_key(&character("p"), none), Some(ViewerKey::Pin));
        assert_eq!(viewer_key(&character("p"), ModifiersState::CONTROL), None);
    }

    /// Esc, the arrows, Page Up and Page Down, `+`, `-`, `0`, `c` and Space
    /// act; Shift only makes `+`, and with Ctrl, Alt or Command held nothing
    /// acts. Enter, digits, y and n, Tab and every other key are swallowed.
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
            PreviewKey::Page(PageStep::By(-1))
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::PageDown), none),
            PreviewKey::Page(PageStep::By(1))
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::PageDown), shift),
            PreviewKey::Swallow
        );
        assert!(!PreviewKey::Page(PageStep::By(1)).once());
        assert_eq!(
            preview_key(&Key::Named(NamedKey::Home), none),
            PreviewKey::Page(PageStep::First)
        );
        assert_eq!(
            preview_key(&Key::Named(NamedKey::End), none),
            PreviewKey::Page(PageStep::Last)
        );
        assert!(PreviewKey::Page(PageStep::Last).once());
        assert_eq!(
            preview_key(&Key::Named(NamedKey::End), shift),
            PreviewKey::Swallow
        );
        assert_eq!(preview_key(&character("+"), shift), PreviewKey::ZoomIn);
        assert_eq!(preview_key(&character("="), none), PreviewKey::ZoomIn);
        assert_eq!(preview_key(&character("="), shift), PreviewKey::Swallow);
        assert_eq!(preview_key(&character("-"), none), PreviewKey::ZoomOut);
        assert_eq!(preview_key(&character("0"), none), PreviewKey::Fit);
        assert_eq!(preview_key(&character("c"), none), PreviewKey::Copy);
        assert_eq!(preview_key(&character("C"), shift), PreviewKey::Swallow);
        assert_eq!(preview_key(&character("m"), none), PreviewKey::Mute);
        assert_eq!(preview_key(&character("M"), shift), PreviewKey::Swallow);
        assert!(PreviewKey::Mute.once());
        assert_eq!(
            preview_key(&Key::Named(NamedKey::Space), none),
            PreviewKey::Play
        );
        assert!(PreviewKey::Play.once());
        assert_eq!(
            preview_key(&Key::Named(NamedKey::Space), shift),
            PreviewKey::Swallow
        );
        for key in [
            Key::Named(NamedKey::Enter),
            Key::Named(NamedKey::Tab),
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
