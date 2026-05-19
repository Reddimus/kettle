//! Translate winit keyboard events into PTY byte sequences (xterm-compatible,
//! honoring application-cursor-key mode).

use kettle_core::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Encode a key press to the bytes that should be written to the PTY.
/// Returns `None` if the key produces no output.
pub fn encode(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    // Cursor / navigation keys.
    let csi = |c: char| {
        let intro = if app_cursor { b"\x1bO" } else { b"\x1b[" };
        let mut v = intro.to_vec();
        v.push(c as u8);
        Some(v)
    };

    if let Key::Named(n) = key {
        match n {
            NamedKey::Enter => return Some(vec![b'\r']),
            NamedKey::Backspace => return Some(if alt { vec![0x1b, 0x7f] } else { vec![0x7f] }),
            NamedKey::Tab => return Some(vec![b'\t']),
            NamedKey::Escape => return Some(vec![0x1b]),
            NamedKey::Space => return Some(vec![b' ']),
            NamedKey::ArrowUp => return csi('A'),
            NamedKey::ArrowDown => return csi('B'),
            NamedKey::ArrowRight => return csi('C'),
            NamedKey::ArrowLeft => return csi('D'),
            NamedKey::Home => return csi('H'),
            NamedKey::End => return csi('F'),
            NamedKey::Delete => return Some(b"\x1b[3~".to_vec()),
            NamedKey::Insert => return Some(b"\x1b[2~".to_vec()),
            NamedKey::PageUp => return Some(b"\x1b[5~".to_vec()),
            NamedKey::PageDown => return Some(b"\x1b[6~".to_vec()),
            NamedKey::F1 => return Some(b"\x1bOP".to_vec()),
            NamedKey::F2 => return Some(b"\x1bOQ".to_vec()),
            NamedKey::F3 => return Some(b"\x1bOR".to_vec()),
            NamedKey::F4 => return Some(b"\x1bOS".to_vec()),
            NamedKey::F5 => return Some(b"\x1b[15~".to_vec()),
            NamedKey::F6 => return Some(b"\x1b[17~".to_vec()),
            NamedKey::F7 => return Some(b"\x1b[18~".to_vec()),
            NamedKey::F8 => return Some(b"\x1b[19~".to_vec()),
            NamedKey::F9 => return Some(b"\x1b[20~".to_vec()),
            NamedKey::F10 => return Some(b"\x1b[21~".to_vec()),
            NamedKey::F11 => return Some(b"\x1b[23~".to_vec()),
            NamedKey::F12 => return Some(b"\x1b[24~".to_vec()),
            _ => {}
        }
    }

    // Character keys.
    if let Key::Character(s) = key {
        let c = s.chars().next()?;
        if ctrl && !alt {
            // Control codes for letters and a few punctuation keys.
            let b = c.to_ascii_lowercase();
            let code = match b {
                'a'..='z' => Some((b as u8) - b'a' + 1),
                '[' => Some(27),
                '\\' => Some(28),
                ']' => Some(29),
                ' ' => Some(0),
                _ => None,
            };
            if let Some(code) = code {
                return Some(vec![code]);
            }
        }
        let mut out = Vec::new();
        if alt {
            out.push(0x1b);
        }
        out.extend_from_slice(s.as_bytes());
        return Some(out);
    }

    // Fallback to committed text (handles IME / dead keys).
    if let Some(t) = text
        && !t.is_empty()
    {
        let mut out = Vec::new();
        if alt {
            out.push(0x1b);
        }
        out.extend_from_slice(t.as_bytes());
        return Some(out);
    }
    None
}
