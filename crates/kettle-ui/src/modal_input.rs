//! Shared text-entry rules for the modal overlays that own a plain `String`
//! buffer: the title editors, the command palette, the SSH launcher, the layout
//! picker, and the Settings path prompt.
//!
//! The search bar is deliberately *not* a caller. It owns a real
//! [`crate::search_input::SearchEditor`] with a cursor, a selection, and its own
//! clipboard chords, so it needs none of this. These helpers exist to give the
//! remaining append-only fields the same *correctness* guarantees search already
//! has — modifier filtering, grapheme-correct deletion, a byte cap, and a
//! working paste — without pretending they are full editors.
//!
//! Each rule below is here because a live probe against the real application
//! caught its absence; see the module tests for the reproductions.

use winit::keyboard::ModifiersState;

/// Byte ceiling for a modal text buffer.
///
/// Matches `kettle_core::MAX_SEARCH_QUERY_BYTES` on purpose: both are
/// single-line fields backed by a `String` that the renderer measures every
/// frame, and having one bound for "how long may a modal field get" is easier to
/// reason about than two. Before this cap the fields were unbounded — a live
/// probe pushed 3000 characters into a tab title and the tab bar dutifully tried
/// to lay all of them out.
pub(crate) const MAX_MODAL_INPUT_BYTES: usize = kettle_core::MAX_SEARCH_QUERY_BYTES;

/// The text a modal field should accept from a key event, or `None` when the
/// keystroke is not text entry.
///
/// `winit`'s [`KeyEvent::text`] is **not** filtered by Command/Super: on macOS a
/// `⌘V` arrives as `Some("v")`. Every modal that appended `text` blindly
/// therefore typed a literal character instead of running the shortcut —
/// `⌘V` in "Edit tab title" produced a tab named `v`, and in the command palette
/// it rewrote the query to `v`, which re-ranks the list so the *next* Enter runs
/// whichever command that query happens to surface.
///
/// The confirm dialog already applied exactly this rule inline (its
/// `Key::Character` arm requires no ctrl/alt/super before reading `y`/`n`/`h`/`l`).
/// This is that rule, factored out so every modal shares it.
///
/// Alt is deliberately **not** rejected. On macOS, Option is a legitimate text
/// producer — `⌥e` composes `´` — and `macos-option-as-alt` has already decided
/// upstream whether Option means "compose" or "meta". Rejecting it here would
/// break dead keys for everyone who types accented characters.
///
/// [`KeyEvent::text`]: winit::event::KeyEvent::text
pub(crate) fn accept_text(text: Option<&str>, mods: ModifiersState) -> Option<&str> {
    if mods.control_key() || mods.super_key() {
        return None;
    }
    let text = text?;
    (!text.is_empty() && !text.chars().any(char::is_control)).then_some(text)
}

/// Append `text` to a modal buffer, dropping control characters and stopping at
/// [`MAX_MODAL_INPUT_BYTES`]. Returns whether anything was added.
///
/// Truncation is silent by design: unlike the search bar there is no status line
/// to carry a `TooLong` badge, and refusing the whole insert would make a long
/// paste look like a dead keystroke.
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

/// Remove the last **grapheme cluster** from a modal buffer. Returns whether
/// anything was removed.
///
/// `String::pop` removes one `char`, which is not what a person means by
/// "delete the thing I just typed". A live probe typed `👩‍🚀` (woman + ZWJ +
/// rocket) into a tab title and pressed Backspace once: the rocket vanished and
/// the buffer kept a dangling zero-width joiner, because `U+200D` is a format
/// character rather than a control character and so survived every filter.
/// Combining accents fail the same way — one Backspace after `é` typed as
/// `e` + `U+0301` leaves a bare `e` and looks like nothing happened.
///
/// [`crate::search_input::SearchEditor::backspace`] has always been grapheme-
/// correct; this shares its boundary helper so the two cannot drift.
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
        // The reproduction: winit hands `⌘V` to the app as text "v". Before the
        // guard this became a tab literally named "v".
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
        // The exact live reproduction: one Backspace on "👩‍🚀" used to leave
        // "👩\u{200d}" behind, because String::pop took only the rocket.
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
