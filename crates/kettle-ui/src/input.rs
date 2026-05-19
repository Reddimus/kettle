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

/// xterm "modifyOtherKeys" / "modifyCursorKeys" modifier code: `1` for no
/// modifiers, otherwise `1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0) +
/// (super?8:0)`. This is the value apps see in `CSI 1;<m>A`-style cursor
/// reports, `CSI 5;<m>~` page-up, `CSI 1;<m>P` modified F1, etc. — the
/// shared encoding xterm/Alacritty/WezTerm/kitty all emit.
pub fn xterm_modifier(mods: ModifiersState) -> u32 {
    let mut m = 1;
    if mods.shift_key() {
        m += 1;
    }
    if mods.alt_key() {
        m += 2;
    }
    if mods.control_key() {
        m += 4;
    }
    if mods.super_key() {
        m += 8;
    }
    m
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
    let shift = mods.shift_key();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let m = xterm_modifier(mods);
    let modded = m > 1;

    // Cursor / navigation keys. Unmodified honors `app-cursor` mode (vim,
    // less, readline all rely on this so arrow keys produce `\x1bOA` after
    // they request DECCKM); modified always uses CSI with a modifier
    // parameter (`CSI 1;<m>A`), which xterm/Alacritty/WezTerm all do —
    // there is no `SS3`-style modified form.
    let csi = |c: char| {
        if modded {
            return Some(format!("\x1b[1;{m}{c}").into_bytes());
        }
        let intro = if app_cursor { b"\x1bO" } else { b"\x1b[" };
        let mut v = intro.to_vec();
        v.push(c as u8);
        Some(v)
    };

    // `~`-terminated function/nav keys: unmodified is `\x1b[<n>~`, modified
    // is `\x1b[<n>;<m>~` (Insert, Delete, PageUp, PageDown, F5..F12).
    let tilde = |n: u32| {
        Some(if modded {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        })
    };

    // F1..F4: unmodified is the legacy `\x1bOP..S` (SS3); modified switches
    // to `CSI 1;<m>P..S` per xterm. F5..F12 reuse the tilde form above.
    let fkey_ss3 = |c: char| {
        Some(if modded {
            format!("\x1b[1;{m}{c}").into_bytes()
        } else {
            format!("\x1bO{c}").into_bytes()
        })
    };

    if let Key::Named(n) = key {
        match n {
            NamedKey::Enter => return Some(vec![b'\r']),
            NamedKey::Backspace => {
                // The three flavors every modern terminal emits:
                //   plain Backspace  → DEL (0x7F)  — xterm convention,
                //     what readline's `backward-delete-char` reads.
                //   Alt+Backspace    → ESC+DEL    — readline's standard
                //     `backward-kill-word` (a.k.a. M-DEL).
                //   Ctrl+Backspace   → BS  (0x08) — alacritty/xterm
                //     convention; users coming from VS Code / browsers
                //     expect this to be "delete word back," and bash can
                //     be told so with `bind '"\C-h":backward-kill-word'`.
                //     Without distinguishing it, Ctrl+Backspace was a
                //     plain Backspace, breaking the muscle memory.
                return Some(match (ctrl, alt) {
                    (true, _) => vec![0x08],
                    (false, true) => vec![0x1b, 0x7f],
                    (false, false) => vec![0x7f],
                });
            }
            // Shift+Tab is the standard "back-tab" (`CSI Z`) used by
            // readline, fzf, and every TUI form for reverse field nav.
            NamedKey::Tab => {
                return Some(if shift {
                    b"\x1b[Z".to_vec()
                } else {
                    vec![b'\t']
                });
            }
            NamedKey::Escape => return Some(vec![0x1b]),
            NamedKey::Space => return Some(vec![b' ']),
            NamedKey::ArrowUp => return csi('A'),
            NamedKey::ArrowDown => return csi('B'),
            NamedKey::ArrowRight => return csi('C'),
            NamedKey::ArrowLeft => return csi('D'),
            NamedKey::Home => return csi('H'),
            NamedKey::End => return csi('F'),
            NamedKey::Delete => return tilde(3),
            NamedKey::Insert => return tilde(2),
            NamedKey::PageUp => return tilde(5),
            NamedKey::PageDown => return tilde(6),
            NamedKey::F1 => return fkey_ss3('P'),
            NamedKey::F2 => return fkey_ss3('Q'),
            NamedKey::F3 => return fkey_ss3('R'),
            NamedKey::F4 => return fkey_ss3('S'),
            NamedKey::F5 => return tilde(15),
            NamedKey::F6 => return tilde(17),
            NamedKey::F7 => return tilde(18),
            NamedKey::F8 => return tilde(19),
            NamedKey::F9 => return tilde(20),
            NamedKey::F10 => return tilde(21),
            NamedKey::F11 => return tilde(23),
            NamedKey::F12 => return tilde(24),
            _ => {}
        }
    }

    // Character keys.
    if let Key::Character(s) = key {
        let c = s.chars().next()?;
        if ctrl && !alt {
            // Full xterm control-code table for Ctrl+<punctuation> — the
            // letters are the obvious A→0x01..Z→0x1A range, the rest is
            // the seven-bit C0 row terminals have produced since VT100:
            //   Ctrl+@ / Ctrl+Space = NUL (0x00)
            //   Ctrl+[              = ESC (0x1B)
            //   Ctrl+\              = FS  (0x1C, SIGQUIT in cooked tty)
            //   Ctrl+]              = GS  (0x1D, telnet/screen escape)
            //   Ctrl+^              = RS  (0x1E, vim alt-buffer, tmux)
            //   Ctrl+_ / Ctrl+/     = US  (0x1F, tmux/nano "undo")
            // Adding `@`, `^`, `_`, `/` to the existing table; they were
            // previously falling through and inserting the literal char
            // — which is at best harmless, at worst breaks editor
            // shortcuts in tmux/vim/nano.
            let b = c.to_ascii_lowercase();
            let code = match b {
                'a'..='z' => Some((b as u8) - b'a' + 1),
                '@' | ' ' => Some(0x00),
                '[' => Some(0x1B),
                '\\' => Some(0x1C),
                ']' => Some(0x1D),
                '^' => Some(0x1E),
                '_' | '/' => Some(0x1F),
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
        // Strip *both* bracketed-paste markers from the body. The closing
        // marker is the well-known injection target (close the bracket
        // early to make the shell auto-run the remainder); the opening
        // marker is the same class of bug going the other way — a paste
        // containing `\x1b[200~` can confuse some shells into treating our
        // genuine closer as "still pasted text" and never leaving paste
        // mode, swallowing further input. Alacritty/iTerm2/WezTerm all
        // strip both.
        let safe = body.replace("\x1b[200~", "").replace("\x1b[201~", "");
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

    #[test]
    fn paste_strips_injected_start_marker() {
        // Embedded `\x1b[200~` is the other half of the bracketed-paste
        // injection family: it can confuse shells into thinking they're
        // entering paste mode mid-way, so our real `\x1b[201~` at the end
        // doesn't actually exit paste mode. Defense in depth — Alacritty /
        // iTerm2 / WezTerm all strip both. Same shape as the close-marker
        // test above so the contract is documented in pairs.
        let p = paste_payload("evil\x1b[200~rm -rf /", true);
        assert_eq!(
            p.windows(6).filter(|w| *w == b"\x1b[200~").count(),
            1,
            "embedded bracketed-paste start marker must be stripped"
        );
        // Closing marker should still be exactly one (the wrapper's).
        assert_eq!(p.windows(6).filter(|w| *w == b"\x1b[201~").count(), 1);
        // Body between wrappers is the original text minus the marker.
        assert!(
            std::str::from_utf8(&p).unwrap().contains("evilrm -rf /"),
            "body after strip: {}",
            String::from_utf8_lossy(&p)
        );
    }

    #[test]
    fn backspace_three_flavors() {
        use winit::keyboard::{Key, NamedKey};
        let no = ModifiersState::empty();
        let alt = ModifiersState::ALT;
        let ctrl = ModifiersState::CONTROL;
        let mode = TermMode::empty();
        // Plain → DEL (xterm; readline `backward-delete-char`).
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), None, no, mode),
            Some(vec![0x7f])
        );
        // Alt+Backspace → ESC+DEL (readline `backward-kill-word`, M-DEL).
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), None, alt, mode),
            Some(vec![0x1b, 0x7f])
        );
        // Ctrl+Backspace → BS (alacritty/xterm; VS Code-style delete-word
        // muscle memory works once the shell is told `\C-h` = kill-word).
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), None, ctrl, mode),
            Some(vec![0x08])
        );
        // Ctrl+Alt+Backspace currently follows the ctrl path (BS) — the
        // combo is rarely bound and going through ctrl matches alacritty.
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), None, ctrl_alt, mode),
            Some(vec![0x08])
        );
    }

    #[test]
    fn ctrl_punctuation_emits_the_full_c0_row() {
        use winit::keyboard::{Key, SmolStr};
        let ctrl = ModifiersState::CONTROL;
        let mode = TermMode::empty();
        // Helper: encode a single-character key with Ctrl held.
        let enc = |c: &str| encode(&Key::Character(SmolStr::new(c)), None, ctrl, mode);
        // Letters: A → 0x01, M → 0x0D (carriage return), Z → 0x1A.
        assert_eq!(enc("a"), Some(vec![0x01]));
        assert_eq!(enc("m"), Some(vec![0x0D]));
        assert_eq!(enc("z"), Some(vec![0x1A]));
        // Punctuation row — each one was either already mapped (`[`, `\\`,
        // `]`, ` `) or newly added (`@`, `^`, `_`, `/`).
        assert_eq!(enc("@"), Some(vec![0x00]), "Ctrl+@ = NUL");
        assert_eq!(enc("["), Some(vec![0x1B]), "Ctrl+[ = ESC");
        assert_eq!(enc("\\"), Some(vec![0x1C]), "Ctrl+\\ = FS / SIGQUIT");
        assert_eq!(enc("]"), Some(vec![0x1D]), "Ctrl+] = GS");
        assert_eq!(
            enc("^"),
            Some(vec![0x1E]),
            "Ctrl+^ = RS (vim alt-buf, tmux)"
        );
        assert_eq!(enc("_"), Some(vec![0x1F]), "Ctrl+_ = US");
        assert_eq!(enc("/"), Some(vec![0x1F]), "Ctrl+/ = US (tmux/nano undo)");
    }

    #[test]
    fn xterm_modifier_table() {
        // xterm "modifyCursorKeys" encoding: 1 = none, +1 shift, +2 alt,
        // +4 ctrl, +8 super (cmd/win). The standard table every modern
        // terminal honors — bash/readline/vim/less/fzf all read it.
        assert_eq!(xterm_modifier(ModifiersState::empty()), 1);
        assert_eq!(xterm_modifier(ModifiersState::SHIFT), 2);
        assert_eq!(xterm_modifier(ModifiersState::ALT), 3);
        assert_eq!(xterm_modifier(ModifiersState::CONTROL), 5);
        assert_eq!(
            xterm_modifier(ModifiersState::CONTROL | ModifiersState::SHIFT),
            6
        );
        assert_eq!(
            xterm_modifier(ModifiersState::CONTROL | ModifiersState::ALT),
            7
        );
        assert_eq!(xterm_modifier(ModifiersState::SUPER), 9);
    }

    #[test]
    fn encode_modifies_named_keys_per_xterm() {
        use winit::keyboard::{Key, NamedKey};
        let no = ModifiersState::empty();
        let ctrl = ModifiersState::CONTROL;
        let shift = ModifiersState::SHIFT;
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let mode = TermMode::empty();

        // Unmodified arrows keep the legacy `CSI A..D`; modified switch to
        // `CSI 1;<m><letter>`. `Ctrl+Right` is "skip word" in bash/zsh/vim.
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowRight), None, no, mode),
            Some(b"\x1b[C".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowRight), None, ctrl, mode),
            Some(b"\x1b[1;5C".to_vec()),
            "Ctrl+ArrowRight must be CSI 1;5C (xterm modifyCursorKeys)"
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowLeft), None, ctrl_shift, mode),
            Some(b"\x1b[1;6D".to_vec()),
            "Ctrl+Shift+ArrowLeft must be CSI 1;6D"
        );

        // App-cursor mode (DECCKM) only changes the *unmodified* form;
        // modified still uses CSI so vim's arrows-in-insert work.
        let app = TermMode::APP_CURSOR;
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowUp), None, no, app),
            Some(b"\x1bOA".to_vec()),
            "DECCKM: bare arrows use SS3"
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowUp), None, ctrl, app),
            Some(b"\x1b[1;5A".to_vec()),
            "DECCKM: modified arrows stay CSI"
        );

        // Tilde-form nav: Delete / Insert / PageUp / PageDown, modified
        // inserts `;<m>` before `~`. `Ctrl+Delete` = delete-word.
        assert_eq!(
            encode(&Key::Named(NamedKey::Delete), None, no, mode),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Delete), None, ctrl, mode),
            Some(b"\x1b[3;5~".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::PageUp), None, shift, mode),
            Some(b"\x1b[5;2~".to_vec())
        );

        // F1..F4 switch SS3 → CSI when modified; F5..F12 stay tilde.
        assert_eq!(
            encode(&Key::Named(NamedKey::F1), None, no, mode),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::F1), None, ctrl, mode),
            Some(b"\x1b[1;5P".to_vec()),
            "Ctrl+F1 must be CSI 1;5P (not SS3)"
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::F5), None, ctrl, mode),
            Some(b"\x1b[15;5~".to_vec())
        );

        // Shift+Tab = `CSI Z` back-tab (readline reverse-field nav, fzf).
        assert_eq!(
            encode(&Key::Named(NamedKey::Tab), None, no, mode),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Tab), None, shift, mode),
            Some(b"\x1b[Z".to_vec()),
            "Shift+Tab must be back-tab CSI Z"
        );
    }

    #[test]
    fn mouse_tracking_detection() {
        use kettle_core::TermMode;
        assert!(matches!(
            mouse_tracking(TermMode::empty()),
            (MouseTracking::Off, false)
        ));
        assert!(matches!(
            mouse_tracking(TermMode::MOUSE_REPORT_CLICK),
            (MouseTracking::Click, false)
        ));
        assert!(matches!(
            mouse_tracking(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE),
            (MouseTracking::Click, true)
        ));
        assert!(matches!(
            mouse_tracking(TermMode::MOUSE_DRAG),
            (MouseTracking::Drag, false)
        ));
        assert!(matches!(
            mouse_tracking(TermMode::MOUSE_MOTION),
            (MouseTracking::Motion, false)
        ));
    }

    #[test]
    fn mouse_encode_sgr_and_legacy() {
        let none = ModifiersState::empty();
        // SGR: left press at grid (0,0) -> 1-based coords, 'M' = press.
        assert_eq!(
            mouse_encode(true, 0, true, false, 0, 0, none),
            b"\x1b[<0;1;1M"
        );
        // Release uses 'm'.
        assert_eq!(
            mouse_encode(true, 0, false, false, 2, 3, none),
            b"\x1b[<0;3;4m"
        );
        // Wheel-up (btn 64) is always a press.
        assert_eq!(
            mouse_encode(true, 64, true, false, 0, 0, none),
            b"\x1b[<64;1;1M"
        );
        // Legacy X10: ESC [ M then (32+btn)(32+col+1)(32+row+1).
        assert_eq!(
            mouse_encode(false, 0, true, false, 0, 0, none),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
    }

    #[test]
    fn mouse_encode_modifiers_and_motion() {
        // Ctrl adds 16, motion adds 32 to the SGR button code.
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            mouse_encode(true, 0, true, true, 0, 0, ctrl),
            b"\x1b[<48;1;1M" // 0 + 32 (motion) + 16 (ctrl)
        );
        let shift = ModifiersState::SHIFT;
        assert_eq!(
            mouse_encode(true, 0, true, false, 0, 0, shift),
            b"\x1b[<4;1;1M" // 0 + 4 (shift)
        );
    }
}
