//! A preview lane holding the keyboard (`focus_preview`). While it does,
//! every key is the lane's: a few move, zoom, fit, page, copy what it shows
//! or play or pause a video (left and right then step it), Esc gives the
//! keyboard back to the terminal, and every other key
//! and chord, Enter, digits and the application's shortcuts included, does
//! nothing, so none reaches the program in the pane by accident.

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
