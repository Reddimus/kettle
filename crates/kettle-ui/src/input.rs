//! Translate winit keyboard and mouse events into PTY byte sequences
//! (xterm-compatible, honoring application-cursor-key and mouse modes).

use kettle_core::TermMode;
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};

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

/// xterm alternate-scroll behavior (DEC private mode 1007): when the focused
/// app is on the alternate screen and has NOT enabled mouse tracking, wheel
/// notches are delivered as Up/Down cursor keys instead of scrolling terminal
/// history. This is what makes `less`, `man`, and vim scroll with a wheel before
/// they opt into mouse reports.
pub fn alternate_scroll_key(lines: i32, mode: TermMode) -> Option<Vec<u8>> {
    if lines == 0
        || !mode.contains(TermMode::ALT_SCREEN)
        || mouse_tracking(mode).0 != MouseTracking::Off
    {
        return None;
    }

    let key = if lines > 0 {
        Key::Named(NamedKey::ArrowUp)
    } else {
        Key::Named(NamedKey::ArrowDown)
    };
    let bytes = encode(&key, None, ModifiersState::empty(), mode)?;
    let repeat = usize::try_from(lines.unsigned_abs().min(8)).unwrap_or(8);
    Some(bytes.repeat(repeat))
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
    let x = col + 1;
    let y = row + 1;
    // Build the modifier/motion bitfield onto a per-mode button base. SGR
    // always reports the real button (and signals press/release with the M/m
    // final byte), so its base is `btn`. Legacy X10 has no separate release
    // final byte: a release is encoded by substituting the "button-release"
    // sentinel `3` for the button code on the `!pressed` event. Wheel/extended
    // buttons (`btn >= 64`) are press-only motion notches with no release at
    // all, so they keep their real code.
    let base = |sentinel: bool| -> u32 {
        if sentinel && !pressed && btn < 64 {
            3
        } else {
            btn as u32
        }
    };
    let bits = |b: u32| -> u32 {
        let mut cb = b;
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
        cb
    };
    if sgr {
        let cb = bits(base(false));
        let kind = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{cb};{x};{y}{kind}").into_bytes()
    } else {
        // Legacy X10: clamp to the 1..223 representable range.
        let cb = bits(base(true));
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

/// Application-keypad (DECKPAM) encoding for a **numpad** key, or `None` when it
/// doesn't apply (mode off, not a numpad key, or a Ctrl/Alt modifier is held).
///
/// Cycle 828 (audit): `TermMode::APP_KEYPAD` is set/cleared by DECKPAM (`ESC =`)
/// / DECKPNM (`ESC >`) in the engine, but the key encoder only ever consulted
/// `APP_CURSOR` — so under application-keypad mode the numpad still sent plain
/// ASCII instead of the xterm SS3 keypad sequences (`ESC O p`..`ESC O y` for
/// 0–9, `ESC O M` for keypad-Enter, `k`/`m`/`j`/`o`/`n`/`X` for `+ - * / . =`).
/// curses apps, gnuplot, BBS/serial clients, and TUI calculators rely on these.
/// `event.location` is what distinguishes the numpad from the main number row;
/// the main encoder is location-agnostic, so this runs first.
pub fn encode_app_keypad(
    key: &Key,
    location: KeyLocation,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !mode.contains(TermMode::APP_KEYPAD)
        || location != KeyLocation::Numpad
        || mods.control_key()
        || mods.alt_key()
    {
        return None;
    }
    let c = match key {
        Key::Named(NamedKey::Enter) => b'M',
        Key::Character(s) => match s.chars().next()? {
            '0' => b'p',
            '1' => b'q',
            '2' => b'r',
            '3' => b's',
            '4' => b't',
            '5' => b'u',
            '6' => b'v',
            '7' => b'w',
            '8' => b'x',
            '9' => b'y',
            '.' => b'n',
            '+' => b'k',
            '-' => b'm',
            '*' => b'j',
            '/' => b'o',
            '=' => b'X',
            _ => return None,
        },
        _ => return None,
    };
    Some(vec![0x1b, b'O', c])
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
            // Cycle 818 (audit): the space bar arrives as NamedKey::Space, which
            // returned a literal space BEFORE any modifier was inspected — so
            // Ctrl+Space emitted 0x20 instead of NUL (0x00), silently breaking
            // emacs/readline set-mark and tmux/vim C-SPC bindings. (The
            // `' ' => 0x00` entry in the Ctrl table below is in the
            // Key::Character arm, which the space key never reaches.) xterm
            // emits NUL for Ctrl+Space and ESC+space for Alt+Space.
            NamedKey::Space => {
                return Some(if ctrl && !alt {
                    vec![0x00]
                } else if alt {
                    vec![0x1b, b' ']
                } else {
                    vec![b' ']
                });
            }
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
        // Preserve xterm's Meta+Control form for the WSL image-paste chord.
        // The generic Ctrl+Alt path historically treated every character as
        // printable Meta input (ESC + `v`), losing Control. Keep this scoped to
        // C-M-v so AltGr-produced characters on international layouts retain
        // their existing text behavior.
        if ctrl && alt && c.eq_ignore_ascii_case(&'v') {
            return Some(vec![0x1b, 0x16]);
        }
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

/// Build the bytes for a clipboard paste.
///
/// In **bracketed-paste** mode the receiving application (vim, IPython, node,
/// $EDITOR, …) is explicitly opting in to handle multi-line content itself, so
/// line endings must be preserved as `\n` — rewriting them to CR garbles a
/// multi-line paste (the app sees a bare carriage return between lines instead
/// of a newline, collapsing or mangling rows). We only collapse `\r\n`→`\n` for
/// consistency and wrap the body in the `\x1b[200~` … `\x1b[201~` markers, with
/// any embedded markers stripped (paste-injection guard).
///
/// In the **non-bracketed** path the bytes go straight to the shell's line
/// discipline, so every newline is normalized to CR — a trailing newline would
/// otherwise auto-run the pasted command unexpectedly (and each interior `\n`
/// would submit a line). This CR normalization is correct ONLY here; it must
/// never touch the bracketed body above.
pub fn paste_payload(text: &str, bracketed: bool) -> Vec<u8> {
    // Strip *both* bracketed-paste markers from a body. The closing marker is
    // the well-known injection target (close the bracket early to make the
    // shell auto-run the remainder); the opening marker is the same class of
    // bug going the other way — a paste containing `\x1b[200~` can confuse some
    // shells into treating our genuine closer as "still pasted text" and never
    // leaving paste mode, swallowing further input. Alacritty/iTerm2/WezTerm all
    // strip both.
    // Strip in a FIXPOINT loop, not a single left-to-right pass: a crafted body
    // like `\x1b[20\x1b[201~1~` re-forms an intact `\x1b[201~` across the splice
    // seam after one `.replace`, leaving a live closer that ends bracketed paste
    // early and auto-runs the tail. Loop until no marker survives (cycle 916,
    // file-by-file audit). The guard runs in BOTH arms: even a non-bracketed
    // paste can carry a stray marker that the receiving app would misread.
    let strip_markers = |s: String| -> String {
        let mut safe = s;
        while safe.contains("\x1b[200~") || safe.contains("\x1b[201~") {
            safe = safe.replace("\x1b[200~", "").replace("\x1b[201~", "");
        }
        safe
    };
    if bracketed {
        // Preserve `\n` line endings; only normalize CRLF→LF for consistency.
        let safe = strip_markers(text.replace("\r\n", "\n"));
        let mut v = Vec::with_capacity(safe.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(safe.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        // Normalize every newline to CR so a trailing/interior newline can't
        // auto-run a command via the shell's line discipline.
        let body = strip_markers(text.replace("\r\n", "\r").replace('\n', "\r"));
        body.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_normalizes_and_brackets() {
        // Non-bracketed: every newline (CRLF or LF) collapses to a single CR so
        // the shell's line discipline can't auto-run interior/trailing lines.
        assert_eq!(paste_payload("a\r\nb\n", false), b"a\rb\r");
        let p = paste_payload("x\n", true);
        assert!(p.starts_with(b"\x1b[200~") && p.ends_with(b"\x1b[201~"));
    }

    #[test]
    fn client_image_paste_chords_pass_through_to_the_pty() {
        let key = Key::Character("v".into());
        assert_eq!(
            encode(&key, Some("v"), ModifiersState::CONTROL, TermMode::empty()),
            Some(vec![0x16]),
            "Ctrl+V must reach Codex and Linux/WSL Claude as C-v"
        );
        assert_eq!(
            encode(&key, Some("v"), ModifiersState::ALT, TermMode::empty()),
            Some(vec![0x1b, b'v']),
            "Alt+V must reach native-Windows Claude as M-v"
        );
        assert_eq!(
            encode(
                &key,
                Some("v"),
                ModifiersState::CONTROL | ModifiersState::ALT,
                TermMode::empty()
            ),
            Some(vec![0x1b, 0x16]),
            "Ctrl+Alt+V must reach Codex under WSL as M-C-v"
        );
    }

    #[test]
    fn paste_bracketed_preserves_newlines() {
        // P0 data-corruption regression: a multi-line bracketed paste must reach
        // the application (vim/IPython/node) with `\n` between lines — NOT `\r`.
        // The old code ran `.replace('\n', "\r")` unconditionally, garbling every
        // multi-line paste into an editor. The CR normalization belongs to the
        // non-bracketed path only.
        let p = paste_payload("line1\nline2\nline3", true);
        assert_eq!(p, b"\x1b[200~line1\nline2\nline3\x1b[201~");
        // CRLF input is collapsed to LF (consistency), never to CR.
        let q = paste_payload("a\r\nb\n", true);
        assert_eq!(q, b"\x1b[200~a\nb\n\x1b[201~");
        // No carriage returns leak into a bracketed body.
        assert!(
            !q[6..q.len() - 6].contains(&b'\r'),
            "bracketed body must not contain CR: {}",
            String::from_utf8_lossy(&q)
        );
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
    fn paste_strips_overlap_reconstructed_marker() {
        // Cycle 916 (file-by-file audit): a single left-to-right `.replace` pass
        // leaves a marker that re-forms across the splice seam.
        // `\x1b[20\x1b[201~1~` -> (strip inner `\x1b[201~`) -> `\x1b[201~`. The
        // fixpoint loop must leave exactly ONE closer (the wrapper's). The old
        // single-pass code left two (the reconstructed one auto-runs the tail).
        let p = paste_payload("a\x1b[20\x1b[201~1~b", true);
        assert_eq!(
            p.windows(6).filter(|w| *w == b"\x1b[201~").count(),
            1,
            "overlap-reconstructed end marker must be stripped to the fixpoint"
        );
        let q = paste_payload("a\x1b[20\x1b[200~0~b", true);
        assert_eq!(
            q.windows(6).filter(|w| *w == b"\x1b[200~").count(),
            1,
            "overlap-reconstructed start marker must be stripped to the fixpoint"
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

    /// Cycle 828 (audit): application-keypad mode (DECKPAM) makes unmodified
    /// numpad keys emit SS3 sequences; without it (and off the numpad) they send
    /// plain ASCII via the normal encoder.
    #[test]
    fn app_keypad_emits_ss3_for_numpad() {
        use winit::keyboard::{Key, KeyLocation, NamedKey, SmolStr};
        let app = TermMode::APP_KEYPAD;
        let none = ModifiersState::empty();
        let np = KeyLocation::Numpad;
        let ch = |c: &str| Key::Character(SmolStr::new(c));

        // Digits 0–9 → ESC O p..y; operators/decimal/enter → their SS3 letters.
        assert_eq!(
            encode_app_keypad(&ch("0"), np, none, app),
            Some(b"\x1bOp".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&ch("9"), np, none, app),
            Some(b"\x1bOy".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&ch("."), np, none, app),
            Some(b"\x1bOn".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&ch("+"), np, none, app),
            Some(b"\x1bOk".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&ch("-"), np, none, app),
            Some(b"\x1bOm".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&ch("*"), np, none, app),
            Some(b"\x1bOj".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&ch("/"), np, none, app),
            Some(b"\x1bOo".to_vec())
        );
        assert_eq!(
            encode_app_keypad(&Key::Named(NamedKey::Enter), np, none, app),
            Some(b"\x1bOM".to_vec())
        );

        // Not applicable: mode off, not on the numpad, or a Ctrl/Alt modifier.
        assert_eq!(
            encode_app_keypad(&ch("5"), np, none, TermMode::empty()),
            None
        );
        assert_eq!(
            encode_app_keypad(&ch("5"), KeyLocation::Standard, none, app),
            None
        );
        assert_eq!(
            encode_app_keypad(&ch("5"), np, ModifiersState::CONTROL, app),
            None
        );
        // The plain number row (Standard location) still goes through `encode`.
        let mode = TermMode::empty();
        assert_eq!(encode(&ch("5"), Some("5"), none, mode), Some(b"5".to_vec()));
    }

    /// Cycle 818 (audit): the space bar comes through as NamedKey::Space, so the
    /// Ctrl+@/Ctrl+Space → NUL rule has to be handled there, not only in the
    /// Ctrl-punctuation table (which Space never reaches).
    #[test]
    fn ctrl_space_emits_nul() {
        use winit::keyboard::{Key, NamedKey};
        let mode = TermMode::empty();
        let sp = || Key::Named(NamedKey::Space);
        // Plain space → 0x20.
        assert_eq!(
            encode(&sp(), None, ModifiersState::empty(), mode),
            Some(vec![b' '])
        );
        // Ctrl+Space → NUL (emacs/readline set-mark, tmux/vim C-SPC).
        assert_eq!(
            encode(&sp(), None, ModifiersState::CONTROL, mode),
            Some(vec![0x00])
        );
        // Alt+Space → ESC + space (xterm meta convention).
        assert_eq!(
            encode(&sp(), None, ModifiersState::ALT, mode),
            Some(vec![0x1b, b' '])
        );
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
    fn alternate_scroll_emits_cursor_keys_only_without_mouse_tracking() {
        let alt = TermMode::ALT_SCREEN;
        assert_eq!(
            alternate_scroll_key(3, alt),
            Some(b"\x1b[A\x1b[A\x1b[A".to_vec())
        );
        assert_eq!(
            alternate_scroll_key(-3, alt),
            Some(b"\x1b[B\x1b[B\x1b[B".to_vec())
        );

        let app_cursor = TermMode::ALT_SCREEN | TermMode::APP_CURSOR;
        assert_eq!(
            alternate_scroll_key(3, app_cursor),
            Some(b"\x1bOA\x1bOA\x1bOA".to_vec())
        );
        assert_eq!(
            alternate_scroll_key(-3, app_cursor),
            Some(b"\x1bOB\x1bOB\x1bOB".to_vec())
        );
        assert_eq!(
            alternate_scroll_key(i32::MIN, alt),
            Some(b"\x1b[B".repeat(8))
        );

        assert_eq!(alternate_scroll_key(0, alt), None);
        assert_eq!(alternate_scroll_key(3, TermMode::empty()), None);
        assert_eq!(
            alternate_scroll_key(3, TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK),
            None,
            "mouse-tracking apps must receive wheel reports, not synthesized arrows"
        );
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
        // Cycle 810: side buttons. Back = SGR 128, Forward = 129 — press 'M'
        // at grid (0,0), release 'm'. Pins the xterm 8–11 button encoding the
        // app forwards for XBUTTON1/2.
        assert_eq!(
            mouse_encode(true, 128, true, false, 0, 0, none),
            b"\x1b[<128;1;1M"
        );
        assert_eq!(
            mouse_encode(true, 129, false, false, 0, 0, none),
            b"\x1b[<129;1;1m"
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

    #[test]
    fn mouse_encode_legacy_release_uses_sentinel() {
        // Legacy X10/normal mode has no separate release final byte (it always
        // sends `ESC [ M`), so a button release must encode the "button-release"
        // sentinel `3` instead of the pressed button's code. The old code
        // re-encoded the original button on release, so an app could never tell
        // which button (if any) came up — and a left release looked identical to
        // a left press, breaking drag-select / click-up handling in legacy apps.
        let none = ModifiersState::empty();
        // Left (btn 0) release at grid (0,0): ESC [ M (32+3) (32+1) (32+1).
        assert_eq!(
            mouse_encode(false, 0, false, false, 0, 0, none),
            vec![0x1b, 0x5b, 0x4d, 0x23, 0x21, 0x21] // 0x23 = 32+3
        );
        // Middle (1) and right (2) releases ALSO collapse to the `3` sentinel —
        // legacy mode cannot distinguish which normal button was released.
        assert_eq!(
            mouse_encode(false, 1, false, false, 0, 0, none),
            vec![0x1b, 0x5b, 0x4d, 0x23, 0x21, 0x21]
        );
        assert_eq!(
            mouse_encode(false, 2, false, false, 0, 0, none),
            vec![0x1b, 0x5b, 0x4d, 0x23, 0x21, 0x21]
        );
        // A legacy PRESS still reports the real button (unchanged).
        assert_eq!(
            mouse_encode(false, 2, true, false, 0, 0, none),
            vec![0x1b, 0x5b, 0x4d, 32 + 2, 0x21, 0x21]
        );
        // Modifier/motion bits still ride on top of the `3` sentinel on release
        // (the sentinel replaces only the button base, before the +32/+bits).
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            mouse_encode(false, 0, false, false, 0, 0, ctrl),
            vec![0x1b, 0x5b, 0x4d, (3 + 16 + 32) as u8, 0x21, 0x21] // 3 + ctrl(16) + 32
        );
        // Wheel/extended buttons (btn >= 64) are press-only notches with no
        // release semantics, so they keep their real code even when !pressed.
        assert_eq!(
            mouse_encode(false, 64, false, false, 0, 0, none),
            vec![0x1b, 0x5b, 0x4d, 32 + 64, 0x21, 0x21]
        );
    }

    #[test]
    fn mouse_encode_sgr_release_keeps_real_button() {
        // SGR mode signals release with the trailing `m` final byte and reports
        // the REAL button number — it must NOT be rewritten to the `3` sentinel.
        let none = ModifiersState::empty();
        // Right-button (2) release: button 2, trailing 'm', not '3'.
        let p = mouse_encode(true, 2, false, false, 0, 0, none);
        assert_eq!(p, b"\x1b[<2;1;1m");
        assert!(p.ends_with(b"m"), "SGR release must use the 'm' final byte");
        assert!(
            !p.starts_with(b"\x1b[<3;"),
            "SGR release must carry the real button, not the legacy `3` sentinel"
        );
        // Middle (1) release likewise keeps button 1.
        assert_eq!(
            mouse_encode(true, 1, false, false, 0, 0, none),
            b"\x1b[<1;1;1m"
        );
    }
}
