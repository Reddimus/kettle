//! Bounded, append-only text entry for title, palette, SSH, layout and Settings
//! path fields. The search bar uses its own [`crate::search_input::SearchEditor`]
//! and shares only [`accept_text`] to filter unhandled shortcut chords.

use winit::keyboard::ModifiersState;

/// Byte ceiling shared with search; both fields are measured on every frame.
pub(crate) const MAX_MODAL_INPUT_BYTES: usize = kettle_core::MAX_SEARCH_QUERY_BYTES;

/// Accept key-event text, excluding shortcuts and controls. Winit can supply
/// text for Command/Super chords, such as `⌘V` producing `"v"`.
/// Preserve Option composition (after `macos-option-as-alt` handling) and
/// Windows AltGr text reported with Ctrl+Alt.
///
/// [`KeyEvent::text`]: winit::event::KeyEvent::text
pub(crate) fn accept_text(text: Option<&str>, mods: ModifiersState) -> Option<&str> {
    // Ctrl+Alt can be AltGr; accept it only when the event supplies text.
    if mods.super_key() || (mods.control_key() && !mods.alt_key()) {
        return None;
    }
    let text = text?;
    (!text.is_empty() && !text.chars().any(char::is_control)).then_some(text)
}

/// Accept an IME commit independently of latched modifiers: a composition can
/// commit while switching input sources with `⌘Space`. [`push_text`] filters
/// any controls mixed with printable text.
pub(crate) fn accept_committed_text(text: &str) -> Option<&str> {
    (!text.is_empty() && !text.chars().all(char::is_control)).then_some(text)
}

/// Append `text` to a modal buffer, dropping control characters and stopping at
/// [`MAX_MODAL_INPUT_BYTES`]. Returns whether anything was added.
///
/// Truncation is silent because these fields have no overflow status line.
pub(crate) fn push_text(buf: &mut String, text: &str) -> bool {
    let mut changed = false;
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        if buf.len() + ch.len_utf8() > MAX_MODAL_INPUT_BYTES {
            break;
        }
        buf.push(ch);
        changed = true;
    }
    changed
}

/// Remove the last grapheme cluster, keeping combining marks and emoji intact.
/// Share search's boundary helper; return whether anything was removed.
pub(crate) fn backspace(buf: &mut String) -> bool {
    let boundary = crate::search_input::previous_grapheme_boundary(buf, buf.len());
    if boundary == buf.len() {
        return false;
    }
    buf.truncate(boundary);
    true
}

/// Whether this chord is the platform's Paste shortcut — `⌘V` on macOS,
/// `Ctrl+V` elsewhere. Mirrors the `shortcut` predicate in `App::search_key`.
pub(crate) fn is_paste_chord(text_key: Option<&str>, mods: ModifiersState) -> bool {
    let shortcut = if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
    };
    shortcut && text_key.is_some_and(|key| key.eq_ignore_ascii_case("v"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPER: ModifiersState = ModifiersState::SUPER;
    const CONTROL: ModifiersState = ModifiersState::CONTROL;
    const ALT: ModifiersState = ModifiersState::ALT;
    const SHIFT: ModifiersState = ModifiersState::SHIFT;

    #[test]
    fn command_chords_are_not_text_entry() {
        // winit hands `⌘V` to the app as text "v". Without the guard, that
        // becomes a tab named "v".
        assert_eq!(accept_text(Some("v"), SUPER), None, "cmd+v is not a 'v'");
        assert_eq!(accept_text(Some("a"), SUPER), None, "cmd+a is not an 'a'");
        assert_eq!(accept_text(Some("c"), CONTROL), None, "ctrl+c is not a 'c'");
    }

    #[test]
    fn ordinary_and_composed_typing_still_reaches_the_buffer() {
        assert_eq!(accept_text(Some("v"), ModifiersState::empty()), Some("v"));
        assert_eq!(accept_text(Some("V"), SHIFT), Some("V"), "shift types");
        // macOS Option is a compose key, not a command modifier: ⌥e → ´. If this
        // ever starts returning None, dead keys are broken for accented input.
        assert_eq!(accept_text(Some("´"), ALT), Some("´"), "option composes");
    }

    #[test]
    fn control_characters_and_empty_text_are_rejected() {
        assert_eq!(accept_text(Some("\u{1}"), ModifiersState::empty()), None);
        assert_eq!(accept_text(Some(""), ModifiersState::empty()), None);
        assert_eq!(accept_text(None, ModifiersState::empty()), None);
    }

    #[test]
    fn backspace_removes_a_whole_grapheme_cluster() {
        // One Backspace on "👩‍🚀" must remove it whole. String::pop takes only
        // the rocket and leaves "👩\u{200d}" behind.
        let mut buf = String::from("ab\u{1f469}\u{200d}\u{1f680}");
        assert!(backspace(&mut buf));
        assert_eq!(buf, "ab", "no dangling zero-width joiner");

        let mut accented = String::from("cafe\u{301}");
        assert!(backspace(&mut accented));
        assert_eq!(accented, "caf", "the accent leaves with its base letter");

        let mut empty = String::new();
        assert!(
            !backspace(&mut empty),
            "backspace on empty reports no change"
        );
    }

    #[test]
    fn push_text_filters_controls_and_honours_the_cap() {
        let mut buf = String::new();
        assert!(push_text(&mut buf, "a\tb\nc"));
        assert_eq!(buf, "abc", "control characters are dropped, not spaced");

        let mut full = "x".repeat(MAX_MODAL_INPUT_BYTES - 1);
        assert!(push_text(&mut full, "yz"));
        assert_eq!(
            full.len(),
            MAX_MODAL_INPUT_BYTES,
            "stops exactly at the cap"
        );
        assert!(
            !push_text(&mut full, "more"),
            "a full buffer accepts nothing"
        );
    }

    #[test]
    fn push_text_never_splits_a_multibyte_character_across_the_cap() {
        // A 4-byte emoji must not be half-written when only 3 bytes remain.
        let mut buf = "x".repeat(MAX_MODAL_INPUT_BYTES - 3);
        push_text(&mut buf, "\u{1f680}");
        assert!(buf.is_char_boundary(buf.len()));
        assert_eq!(
            buf.len(),
            MAX_MODAL_INPUT_BYTES - 3,
            "the emoji is refused whole"
        );
    }

    #[test]
    fn altgr_composed_characters_are_not_swallowed() {
        // Windows reports AltGr as Ctrl+Alt. Rejecting that combination
        // outright makes `@` untypeable in these fields on a German layout —
        // and there is no other way to enter it there.
        let altgr = CONTROL | ALT;
        assert_eq!(accept_text(Some("@"), altgr), Some("@"), "AltGr composes");
        assert_eq!(
            accept_text(Some("\u{20ac}"), altgr),
            Some("\u{20ac}"),
            "AltGr euro"
        );
        // A genuine Ctrl+Alt chord produces no text, so it is still not entry.
        assert_eq!(accept_text(None, altgr), None);
        // Control on its own remains a shortcut modifier.
        assert_eq!(accept_text(Some("c"), CONTROL), None);
    }

    #[test]
    fn committed_ime_text_ignores_whatever_modifier_is_latched() {
        // Committing a composition with ⌘Space must keep the whole phrase, even
        // though ws.mods still carries SUPER.
        assert_eq!(
            accept_committed_text("\u{4f60}\u{597d}"),
            Some("\u{4f60}\u{597d}")
        );
        assert_eq!(accept_committed_text(""), None);
        assert_eq!(
            accept_committed_text("\u{1}"),
            None,
            "control-only commit is nothing"
        );
    }

    #[test]
    fn paste_chord_is_platform_correct() {
        let native = if cfg!(target_os = "macos") {
            SUPER
        } else {
            CONTROL
        };
        let foreign = if cfg!(target_os = "macos") {
            CONTROL
        } else {
            SUPER
        };
        assert!(is_paste_chord(Some("v"), native));
        assert!(
            is_paste_chord(Some("V"), native),
            "shift+cmd+v still pastes"
        );
        assert!(
            !is_paste_chord(Some("v"), foreign),
            "the other platform's chord does not"
        );
        assert!(
            !is_paste_chord(Some("v"), ModifiersState::empty()),
            "bare v types"
        );
        assert!(!is_paste_chord(Some("b"), native));
    }
}
