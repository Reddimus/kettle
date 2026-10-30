//! Shared keyboard filtering and bounded entry for modal text editors.

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
/// commit while switching input sources with `⌘Space`. The editor normalizes
/// committed text to one line.
pub(crate) fn accept_committed_text(text: &str) -> Option<&str> {
    (!text.is_empty() && !text.chars().all(char::is_control)).then_some(text)
}

/// Insert typed text at the caret, replacing the selection. The shared editor
/// normalizes single-line input and rejects over-limit inserts atomically.
pub(crate) fn push_text(buf: &mut crate::text_editor::TextEditor, text: &str) -> bool {
    buf.type_text(text, MAX_MODAL_INPUT_BYTES).changed
}

pub(crate) fn backspace(buf: &mut crate::text_editor::TextEditor) -> bool {
    buf.backspace()
}

/// Whether this chord is the platform's Paste shortcut — `⌘V` on macOS,
/// `Ctrl+V` elsewhere. Mirrors the `shortcut` predicate in `App::search_key`.
pub(crate) fn is_paste_chord(text_key: Option<&str>, mods: ModifiersState) -> bool {
    let shortcut = if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
    };
    shortcut && !mods.alt_key() && text_key.is_some_and(|key| key.eq_ignore_ascii_case("v"))
}

/// Handle editing commands before the field's submit, cancel and list navigation.
/// Clipboard failures leave a cut selection intact.
pub(crate) fn edit_key(
    editor: &mut crate::text_editor::TextEditor,
    key: &winit::keyboard::Key,
    mods: ModifiersState,
    mut read: impl FnMut() -> Option<String>,
    mut write: impl FnMut(&str) -> bool,
) -> bool {
    use winit::keyboard::{Key, NamedKey};
    let macos = cfg!(target_os = "macos");
    let word = if macos {
        mods.alt_key()
    } else {
        mods.control_key()
    };
    let shortcut = if macos {
        mods.super_key()
    } else {
        mods.control_key()
    };
    let shift = mods.shift_key();
    match key {
        Key::Named(NamedKey::ArrowLeft) => {
            if macos && shortcut {
                editor.move_home(shift);
            } else {
                editor.move_left(shift, word);
            }
        }
        Key::Named(NamedKey::ArrowRight) => {
            if macos && shortcut {
                editor.move_end(shift);
            } else {
                editor.move_right(shift, word);
            }
        }
        Key::Named(NamedKey::Home) => editor.move_home(shift),
        Key::Named(NamedKey::End) => editor.move_end(shift),
        Key::Named(NamedKey::Backspace) => {
            if macos && shortcut {
                editor.delete_to_start();
            } else if word {
                editor.delete_word_backward();
            } else {
                editor.backspace();
            }
        }
        Key::Named(NamedKey::Delete) => {
            if macos && shortcut {
                editor.delete_to_end();
            } else if word {
                editor.delete_word_forward();
            } else {
                editor.delete();
            }
        }
        Key::Character(s) if shortcut && !mods.alt_key() => match s.to_ascii_lowercase().as_str() {
            "a" => editor.select_all(),
            "c" => {
                if let Some(text) = editor.selected_text() {
                    write(text);
                }
            }
            "x" => {
                editor.cut_selection(MAX_MODAL_INPUT_BYTES, |text| write(text));
            }
            "v" => {
                if let Some(text) = read() {
                    editor.insert(&text, MAX_MODAL_INPUT_BYTES);
                }
            }
            "z" if shift => {
                editor.redo();
            }
            "z" => {
                editor.undo();
            }
            _ => return false,
        },
        _ => return false,
    }
    true
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
        let mut buf = crate::text_editor::TextEditor::from("ab\u{1f469}\u{200d}\u{1f680}");
        assert!(backspace(&mut buf));
        assert_eq!(buf.text(), "ab", "no dangling zero-width joiner");

        let mut accented = crate::text_editor::TextEditor::from("cafe\u{301}");
        assert!(backspace(&mut accented));
        assert_eq!(
            accented.text(),
            "caf",
            "the accent leaves with its base letter"
        );

        let mut empty = crate::text_editor::TextEditor::default();
        assert!(
            !backspace(&mut empty),
            "backspace on empty reports no change"
        );
    }

    #[test]
    fn push_text_bounds_control_only_source_before_filtering() {
        let mut buf = crate::text_editor::TextEditor::default();
        let oversized = format!("{}x", "\t".repeat(MAX_MODAL_INPUT_BYTES));
        assert!(
            !push_text(&mut buf, &oversized),
            "text beyond the source budget must not be scanned"
        );
        assert!(buf.is_empty());
        let mut full = crate::text_editor::TextEditor::from("x".repeat(MAX_MODAL_INPUT_BYTES));
        assert!(!push_text(&mut full, &oversized));
    }

    #[test]
    fn push_text_filters_controls_and_honours_the_cap() {
        let mut buf = crate::text_editor::TextEditor::default();
        assert!(push_text(&mut buf, "a\tb\nc"));
        assert_eq!(
            buf.text(),
            "a b c",
            "single-line normalization matches search"
        );

        let mut full = crate::text_editor::TextEditor::from("x".repeat(MAX_MODAL_INPUT_BYTES - 1));
        assert!(!push_text(&mut full, "yz"));
        assert_eq!(
            full.len(),
            MAX_MODAL_INPUT_BYTES - 1,
            "oversized insert is atomic"
        );
        assert!(
            !push_text(&mut full, "more"),
            "a full buffer accepts nothing"
        );
    }

    #[test]
    fn push_text_never_splits_a_multibyte_character_across_the_cap() {
        // A 4-byte emoji must not be half-written when only 3 bytes remain.
        let mut buf = crate::text_editor::TextEditor::from("x".repeat(MAX_MODAL_INPUT_BYTES - 3));
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
        assert!(!is_paste_chord(Some("v"), CONTROL | ALT), "AltGr types");
    }
}
