//! Translate winit keyboard and mouse events into PTY byte sequences
//! (xterm-compatible, honoring application-cursor-key and mouse modes).

use kettle_core::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Which mouse-tracking mode the application has requested.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseTracking {
    /// No tracking — kettle does local selection/scroll.
    Off,
    /// Report press/release only.
    Click,
    /// Report press/release + drag (button held).
    Drag,
    /// Report all motion.
    Motion,
}

pub fn mouse_tracking(mode: TermMode) -> (MouseTracking, bool) {
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let t = if mode.contains(TermMode::MOUSE_MOTION) {
        MouseTracking::Motion
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        MouseTracking::Drag
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        MouseTracking::Click
    } else {
        MouseTracking::Off
    };
    (t, sgr)
}

/// Encode a mouse event. `btn`: 0=left,1=middle,2=right,64=wheel-up,
/// 65=wheel-down. `col`/`row` are 0-based grid coordinates.
#[allow(clippy::too_many_arguments)]
pub fn mouse_encode(
    sgr: bool,
    btn: u8,
    pressed: bool,
    motion: bool,
    col: usize,
    row: usize,
    mods: ModifiersState,
) -> Vec<u8> {
    let mut cb = btn as u32;
    if motion {
        cb += 32;
    }
    if mods.shift_key() {
        cb += 4;
    }
    if mods.alt_key() {
        cb += 8;
    }
    if mods.control_key() {
        cb += 16;
    }
    let x = col + 1;
    let y = row + 1;
    if sgr {
        let kind = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{cb};{x};{y}{kind}").into_bytes()
    } else {
        // Legacy X10: clamp to the 1..223 representable range.
        let enc = |v: usize| (v.min(223) as u8).wrapping_add(32);
        let b = (cb.min(223) as u8).wrapping_add(32);
        vec![0x1b, b'[', b'M', b, enc(x), enc(y)]
    }
}

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

/// Build the bytes for a clipboard paste. Newlines are normalized to CR (so a
/// trailing newline can't auto-run a shell command unexpectedly), and when the
/// app enabled bracketed paste the content is wrapped and any embedded end
/// marker stripped (paste-injection guard).
pub fn paste_payload(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let safe = body.replace("\x1b[201~", "");
        let mut v = Vec::with_capacity(safe.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(safe.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        body.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_normalizes_and_brackets() {
        assert_eq!(paste_payload("a\r\nb\n", false), b"a\rb\r");
        let p = paste_payload("x\n", true);
        assert!(p.starts_with(b"\x1b[200~") && p.ends_with(b"\x1b[201~"));
    }

    #[test]
    fn paste_strips_injected_end_marker() {
        let p = paste_payload("evil\x1b[201~rm -rf /", true);
        // Only the wrapper's own terminator may remain.
        assert_eq!(
            p.windows(6).filter(|w| *w == b"\x1b[201~").count(),
            1,
            "embedded bracketed-paste end marker must be stripped"
        );
    }
}
