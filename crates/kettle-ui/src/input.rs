//! Translate winit keyboard and mouse events into PTY byte sequences
//! (xterm-compatible, honoring application-cursor-key and mouse modes).

use kettle_core::TermMode;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

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

/// Whether a pointer-motion event produces a report under `track`.
///
/// Stated per mode rather than as a negated special case, because the negated
/// form ("not 1003, and no button held") let 1000 report drags whenever a
/// button happened to be down — a mode that is defined as press-and-release
/// only. `vim` with `ttymouse=xterm` enables 1000 alone and reached exactly
/// that.
pub fn motion_is_reported(track: MouseTracking, button_held: bool) -> bool {
    match track {
        // 1003 — all motion, button or not.
        MouseTracking::Motion => true,
        // 1002 — motion only while a button is down.
        MouseTracking::Drag => button_held,
        // 1000 — press and release only.
        MouseTracking::Click | MouseTracking::Off => false,
    }
}

/// xterm's "no button" code.
///
/// A release reports it in place of the real button, and a motion report with
/// no button held reports it plus the motion bit — `3 + 32 = 35`, the
/// `CSI < 35 ; x ; y M` that DEC 1003 delivers while the pointer merely
/// hovers.
pub const MOUSE_NO_BUTTON: u8 = 3;

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

/// Scrollback lines per physical wheel detent at `scroll-multiplier = 1.0`.
const LINES_PER_NOTCH: f32 = 3.0;

/// Physical pixels per detent for backends that report `PixelDelta` (macOS
/// trackpads, Wayland/libinput). 60 px ÷ 3 lines reproduces the historical
/// `p.y / 20.0` lines-per-pixel ratio exactly, so scroll feel is unchanged.
const PIXELS_PER_NOTCH: f32 = 60.0;

/// Ceiling on retained residue. A device streaming deltas faster than they are
/// drained — or a hostile synthetic feed over the ctl socket — cannot grow the
/// accumulator without bound. 10k notches is orders of magnitude more motion
/// than any real gesture.
const MAX_WHEEL_RESIDUAL: f32 = 10_000.0;

/// Whole steps drained out of a [`WheelAccum`] by one wheel event.
///
/// Two quantities, because the wheel drives two different kinds of consumer:
/// *discrete* ones that must move exactly one step per physical detent (tab
/// cycling, Ctrl+wheel font zoom, context-menu rows) and *continuous* ones that
/// scale with `scroll-multiplier` (scrollback, search viewport, mouse reports).
/// Deriving both from a single number would cycle three tabs per detent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WheelSteps {
    /// Whole detents, ignoring `scroll-multiplier`. Positive = wheel up.
    pub notches: i32,
    /// Scrollback lines, scaled by `scroll-multiplier`. Positive = scroll back
    /// toward older output (matches `Scroll::Delta`'s sign convention).
    pub lines: i32,
    /// Whole horizontal detents. Positive = wheel/swipe right.
    pub cols: i32,
}

impl WheelSteps {
    pub fn is_zero(self) -> bool {
        self.notches == 0 && self.lines == 0 && self.cols == 0
    }
}

/// Sub-detent wheel residue.
///
/// Windows Precision Touchpads and high-resolution wheels deliver
/// `WM_MOUSEWHEEL` deltas far smaller than `WHEEL_DELTA` (120) — MSDN requires
/// that applications "accumulate the delta values until `WHEEL_DELTA` is
/// reached". winit divides by 120 and, on Windows, *always* reports
/// `LineDelta` (never `PixelDelta`), so one touchpad gesture arrives as a
/// stream of ~0.07–0.3 notch events.
///
/// Rounding each event in isolation — the pre-v2.41.0 behavior — rounded every
/// one of them to zero, so touchpad scrolling did not merely feel slow, it was
/// *completely dead*. The same dead-zone killed the mouse wheel outright at
/// `scroll-multiplier = 0.1` and swallowed slow macOS/Wayland trackpad motion.
/// Carrying the fraction forward is what makes sub-detent input work at all.
#[derive(Clone, Copy, Debug, Default)]
pub struct WheelAccum {
    notches: f32,
    lines: f32,
    cols: f32,
}

impl WheelAccum {
    /// Drop all residue. Called on `TouchPhase::Ended`/`Cancelled` so momentum
    /// scrolling on macOS doesn't leak a partial step into the next gesture.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Fold one winit delta into the residue and return whatever whole steps
    /// that made available.
    pub fn feed(&mut self, delta: &winit::event::MouseScrollDelta, multiplier: f32) -> WheelSteps {
        use winit::event::MouseScrollDelta;
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (*x, *y),
            MouseScrollDelta::PixelDelta(p) => {
                (p.x as f32 / PIXELS_PER_NOTCH, p.y as f32 / PIXELS_PER_NOTCH)
            }
        };
        // A NaN or infinite delta would poison the residue permanently: every
        // later `trunc()` would yield NaN and the wheel would stay dead for the
        // life of the window. Drop the event instead of persisting the damage.
        if !dx.is_finite() || !dy.is_finite() {
            return WheelSteps::default();
        }
        // Reversing direction abandons the residue rather than spending it
        // against the new direction, so an up-then-down flick doesn't lose its
        // first step to leftovers from the previous one. The axes are
        // independent — a diagonal gesture reversing horizontally must not
        // discard vertical progress.
        if wheel_reverses(dy, self.notches) {
            self.notches = 0.0;
            self.lines = 0.0;
        }
        if wheel_reverses(dx, self.cols) {
            self.cols = 0.0;
        }
        self.notches = clamp_residual(self.notches + dy);
        self.lines = clamp_residual(self.lines + dy * LINES_PER_NOTCH * multiplier.max(0.0));
        self.cols = clamp_residual(self.cols + dx);
        WheelSteps {
            notches: drain_residual(&mut self.notches),
            lines: drain_residual(&mut self.lines),
            cols: drain_residual(&mut self.cols),
        }
    }
}

/// True when `delta` pushes against non-zero residue of the opposite sign.
fn wheel_reverses(delta: f32, residual: f32) -> bool {
    delta != 0.0 && residual != 0.0 && (delta < 0.0) != (residual < 0.0)
}

fn clamp_residual(v: f32) -> f32 {
    v.clamp(-MAX_WHEEL_RESIDUAL, MAX_WHEEL_RESIDUAL)
}

/// Take the whole part, leave the fraction behind for the next event.
fn drain_residual(residual: &mut f32) -> i32 {
    let whole = residual.trunc();
    *residual -= whole;
    whole as i32
}

/// xterm alternate-scroll behavior (DEC private mode 1007): when the focused
/// app is on the alternate screen and has NOT enabled mouse tracking, wheel
/// notches are delivered as Up/Down cursor keys instead of scrolling terminal
/// history. This is what makes `less`, `man`, and vim scroll with a wheel before
/// they opt into mouse reports.
///
/// Gated on `ALTERNATE_SCROLL` as well as `ALT_SCREEN`, matching upstream
/// Alacritty and xterm. The flag is *set by default*, so the common case is
/// unchanged — but an app that opts out with `CSI ?1007 l` (because it wants
/// the wheel to reach kettle's own scrollback, or handles scrolling some other
/// way) is now honored instead of being force-fed synthetic arrow keys.
pub fn alternate_scroll_key(lines: i32, mode: TermMode) -> Option<Vec<u8>> {
    if lines == 0
        || !mode.contains(TermMode::ALT_SCREEN)
        || !mode.contains(TermMode::ALTERNATE_SCROLL)
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

/// Whether the legacy xterm/DEC encodings can represent this chord's
/// modifiers at all.
///
/// Legacy parameterized sequences carry `1 + shift + 2*alt + 4*ctrl`. Bit 8 is
/// xterm's *Meta*, a distinct X11 modifier Kettle has no key for — it is not
/// macOS Command or the Windows/Linux Super key. Reporting Super as bit 8
/// produced `CSI 1;9D` / `CSI 1;11A` parameters that no line editor decodes, so
/// an unbound Command chord left literal `1D` / `1A` on the user's command line
/// instead of doing nothing. Super reaches applications only through the Kitty
/// keyboard protocol, which defines a real super bit (see `kitty_modifier_bits`).
pub fn legacy_encodes_modifiers(mods: ModifiersState) -> bool {
    !mods.super_key()
}

/// Legacy xterm "modifyOtherKeys" / "modifyCursorKeys" modifier code: `1` for
/// no modifiers, otherwise `1 + shift + 2*alt + 4*ctrl`. This is the value apps
/// see in `CSI 1;<m>A`-style cursor reports, `CSI 5;<m>~` page-up, and
/// `CSI 1;<m>P` modified F1. [`legacy_encodes_modifiers`] filters Super before
/// this legacy-only helper is called.
pub(crate) fn legacy_xterm_modifier(mods: ModifiersState) -> u32 {
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
    m
}

/// Encode xterm's `CSI 27` form when the active level covers this exact chord.
///
/// Level one cannot be represented by a blanket gate: it preserves aliases
/// such as Ctrl+I and Shift+Return while encoding Alt+Return and Ctrl+Tab.
fn modify_other_keys_sequence(
    code: u8,
    mods: ModifiersState,
    mode: TermMode,
    level_one_encodes: bool,
    level_two_encodes: bool,
) -> Option<Vec<u8>> {
    let modifier = legacy_xterm_modifier(mods);
    if modifier == 1 {
        return None;
    }

    let encodes = if mode.contains(TermMode::MODIFY_OTHER_KEYS_2) {
        level_two_encodes
    } else if mode.contains(TermMode::MODIFY_OTHER_KEYS_1) {
        level_one_encodes
    } else {
        false
    };

    encodes.then(|| format!("\x1b[27;{modifier};{code}~").into_bytes())
}

fn modify_other_keys_was_negotiated(mode: TermMode) -> bool {
    mode.contains(TermMode::MODIFY_OTHER_KEYS_NEGOTIATED)
        || mode.intersects(TermMode::MODIFY_OTHER_KEYS)
}

fn legacy_control_code(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => Some((c as u8) - b'a' + 1),
        '@' | '`' | '2' | ' ' => Some(0x00),
        '[' | '{' | '3' => Some(0x1b),
        '\\' | '|' | '4' => Some(0x1c),
        ']' | '}' | '5' => Some(0x1d),
        '^' | '~' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn level_one_encodes_ascii(c: char, mods: ModifiersState) -> bool {
    if mods.alt_key() {
        return true;
    }
    if !mods.control_key() {
        return false;
    }

    let code = c as u8;
    // Level one keeps Control/Shift behavior for the printable control-input
    // range and for aliases outside it; replacing either would reintroduce
    // collisions that this compatibility level intentionally retains.
    !(64..=127).contains(&code) && legacy_control_code(c).is_none()
}

/// Application-keypad (DECKPAM) encoding for a **numpad** key, or `None` when it
/// doesn't apply (mode off, not a numpad key, or an unsupported modifier is
/// held).
///
/// `TermMode::APP_KEYPAD` is set/cleared by DECKPAM (`ESC =`)
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
        || !legacy_encodes_modifiers(mods)
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
    if !legacy_encodes_modifiers(mods) {
        return None;
    }

    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let m = legacy_xterm_modifier(mods);
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
            NamedKey::Enter => {
                let level_one_encodes = alt || ctrl;
                if let Some(sequence) =
                    modify_other_keys_sequence(13, mods, mode, level_one_encodes, true)
                {
                    return Some(sequence);
                }

                // The fallback preserves Kettle's shipped multiline chord for
                // clients which negotiate nothing, but cannot raise or imitate
                // the level reported to applications.
                let fallback = mode.contains(TermMode::UNNEGOTIATED_MODIFIED_ENTER)
                    && !modify_other_keys_was_negotiated(mode);
                return Some(if modded && fallback {
                    format!("\x1b[27;{m};13~").into_bytes()
                } else {
                    vec![b'\r']
                });
            }
            NamedKey::Backspace => {
                if let Some(sequence) = modify_other_keys_sequence(
                    8,
                    mods,
                    mode,
                    false,
                    mods != ModifiersState::CONTROL,
                ) {
                    return Some(sequence);
                }
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
                // XKB exposes Shift+Tab as ISO_Left_Tab, an edit key outside
                // modifyOtherKeys, so its established CSI Z form must win.
                if !shift
                    && let Some(sequence) = modify_other_keys_sequence(9, mods, mode, true, true)
                {
                    return Some(sequence);
                }
                return Some(if shift {
                    b"\x1b[Z".to_vec()
                } else {
                    vec![b'\t']
                });
            }
            NamedKey::Escape => {
                let level_one_encodes = alt;
                if let Some(sequence) =
                    modify_other_keys_sequence(27, mods, mode, level_one_encodes, true)
                {
                    return Some(sequence);
                }
                return Some(vec![0x1b]);
            }
            // The space bar arrives as NamedKey::Space, which
            // returned a literal space BEFORE any modifier was inspected — so
            // Ctrl+Space emitted 0x20 instead of NUL (0x00), silently breaking
            // emacs/readline set-mark and tmux/vim C-SPC bindings. (The
            // `' ' => 0x00` entry in the Ctrl table below is in the
            // Key::Character arm, which the space key never reaches.) xterm
            // emits NUL for Ctrl+Space and ESC+space for Alt+Space.
            NamedKey::Space => {
                let level_one_encodes = alt;
                if let Some(sequence) =
                    modify_other_keys_sequence(32, mods, mode, level_one_encodes, true)
                {
                    return Some(sequence);
                }
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
        if c.is_ascii()
            && s.len() == 1
            && let Some(sequence) = modify_other_keys_sequence(
                c as u8,
                mods,
                mode,
                level_one_encodes_ascii(c, mods),
                true,
            )
        {
            // The sequence carries the key's ASCII identity before Control
            // collapses it, which is why Ctrl+I reports 105 rather than 9.
            return Some(sequence);
        }
        if ctrl && let Some(code) = legacy_control_code(c) {
            // Seven-bit C0 aliases for Ctrl+punctuation:
            //   Ctrl+@ / Ctrl+Space = NUL (0x00)
            //   Ctrl+[              = ESC (0x1B)
            //   Ctrl+\              = FS  (0x1C, SIGQUIT in cooked tty)
            //   Ctrl+]              = GS  (0x1D, telnet/screen escape)
            //   Ctrl+^              = RS  (0x1E, vim alt-buffer, tmux)
            //   Ctrl+_ / Ctrl+/     = US  (0x1F, tmux/nano "undo")
            // Shifted partners keep the same aliases. Alt adds an ESC prefix,
            // except when the platform text differs from the logical key:
            // that signals an AltGr composition which must reach the PTY as
            // text. On X11 and Wayland AltGr is Mod5, not Alt.
            let composed_a_character = alt
                && text.is_some_and(|t| {
                    !t.is_empty()
                        && !t.eq_ignore_ascii_case(s.as_str())
                        && !t.chars().any(char::is_control)
                });
            if !composed_a_character {
                return Some(if alt { vec![0x1b, code] } else { vec![code] });
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

/// Encode a complete winit key event, including Kitty keyboard protocol modes.
///
/// The terminal engine owns negotiation and exposes the active progressive-
/// enhancement flags through [`TermMode`]. With no negotiated flags this is a
/// strict compatibility wrapper around Kettle's legacy xterm encoder. Once an
/// application opts into Kitty keyboard reporting, press/repeat/release events
/// are encoded according to the negotiated flags instead of being guessed from
/// modifiers alone.
pub fn encode_key_event(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    let kitty = mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL);
    if !kitty {
        if event.state == ElementState::Released {
            return None;
        }
        return encode_app_keypad(&event.logical_key, event.location, mods, mode)
            .or_else(|| encode(&event.logical_key, event.text.as_deref(), mods, mode));
    }

    let event = KittyKeyEvent::from(event);
    encode_kitty_key_event(&event, mods, mode)
}

/// Encode a synthetic key press, such as an agent-server `send_keys` token.
///
/// Synthetic input has no platform [`KeyEvent`], but it must still honor the
/// focused application's Kitty keyboard negotiation. In particular, Neovim
/// requests disambiguated Escape (`CSI 27 u`); sending a legacy bare Escape
/// after that negotiation leaves agent-driven editor commands stuck.
pub fn encode_key_press(key: &Key, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        return encode(key, None, mods, mode);
    }

    encode_kitty_key_event(&synthetic_key_event(key, mods), mods, mode)
}

/// Whether [`encode_key_press`] uses a Kitty sequence instead of the legacy
/// xterm encoding. Callers use this to avoid applying legacy Backspace/Delete
/// byte remaps to an already encoded CSI-u sequence.
pub fn key_press_uses_kitty_sequence(key: &Key, mods: ModifiersState, mode: TermMode) -> bool {
    kitty_event_uses_sequence(&synthetic_key_event(key, mods), mods, mode)
}

/// Whether this event is represented by Kitty CSI-u rather than Kettle's
/// legacy xterm encoder. Pure enhancement flags and legacy-compatible keys can
/// keep downstream compatibility behavior such as Backspace/Delete remaps.
pub fn uses_kitty_sequence(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> bool {
    kitty_event_uses_sequence(&KittyKeyEvent::from(event), mods, mode)
}

fn kitty_event_uses_sequence(event: &KittyKeyEvent, mods: ModifiersState, mode: TermMode) -> bool {
    if !mode.intersects(
        TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES
            | TermMode::REPORT_ALL_KEYS_AS_ESC,
    ) {
        return false;
    }

    should_build_kitty_sequence(event, mods, mode)
}

fn encode_kitty_key_event(
    event: &KittyKeyEvent,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !kitty_event_uses_sequence(event, mods, mode) {
        if event.state == ElementState::Released {
            return None;
        }
        return encode_app_keypad(&event.logical_key, event.location, mods, mode)
            .or_else(|| encode(&event.logical_key, event.text.as_deref(), mods, mode));
    }

    if event.state == ElementState::Released {
        if !mode.contains(TermMode::REPORT_EVENT_TYPES) {
            return None;
        }
        if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
            && matches!(
                event.logical_key,
                Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace)
            )
        {
            return None;
        }
    }

    build_kitty_sequence(event, mods, mode)
}

struct KittyKeyEvent {
    logical_key: Key,
    key_without_modifiers: Key,
    text: Option<String>,
    text_with_all_modifiers: String,
    location: KeyLocation,
    state: ElementState,
    repeat: bool,
}

impl From<&KeyEvent> for KittyKeyEvent {
    fn from(event: &KeyEvent) -> Self {
        Self {
            logical_key: event.logical_key.clone(),
            key_without_modifiers: event.key_without_modifiers(),
            text: event.text.as_deref().map(str::to_owned),
            text_with_all_modifiers: event
                .text_with_all_modifiers()
                .unwrap_or_default()
                .to_owned(),
            location: event.location,
            state: event.state,
            repeat: event.repeat,
        }
    }
}

fn synthetic_key_event(key: &Key, mods: ModifiersState) -> KittyKeyEvent {
    let text = match key {
        Key::Character(text) => Some(text.to_string()),
        Key::Named(NamedKey::Space) => Some(" ".to_owned()),
        Key::Named(NamedKey::Enter) => Some("\r".to_owned()),
        Key::Named(NamedKey::Tab) => Some("\t".to_owned()),
        Key::Named(NamedKey::Backspace) => Some("\u{8}".to_owned()),
        Key::Named(NamedKey::Escape) => Some("\u{1b}".to_owned()),
        _ => None,
    };
    // A synthetic token has no platform text event. Control-modified key text
    // is either a forbidden C0 code (Ctrl+C, Ctrl+Space, Ctrl+Enter) or
    // layout-dependent AltGr text. Omitting associated text is the only honest
    // portable representation; the CSI-u key code + modifier field still
    // carries the complete chord. Unmodified/Shift/Alt tokens have the literal
    // text supplied by the token itself.
    let text_with_all_modifiers = if mods.control_key() {
        String::new()
    } else {
        text.clone().unwrap_or_default()
    };

    KittyKeyEvent {
        logical_key: key.clone(),
        // Agent tokens describe logical keys rather than a physical keyboard
        // position, so no layout-derived alternate key is available.
        key_without_modifiers: key.clone(),
        text,
        text_with_all_modifiers,
        location: KeyLocation::Standard,
        state: ElementState::Pressed,
        repeat: false,
    }
}

fn should_build_kitty_sequence(
    event: &KittyKeyEvent,
    mods: ModifiersState,
    mode: TermMode,
) -> bool {
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) || event.state == ElementState::Released {
        return true;
    }

    let disambiguate = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES)
        && (event.logical_key == Key::Named(NamedKey::Escape)
            || event.location == KeyLocation::Numpad
            || (!mods.is_empty()
                && (mods != ModifiersState::SHIFT
                    || matches!(
                        event.logical_key,
                        Key::Named(NamedKey::Tab | NamedKey::Enter | NamedKey::Backspace)
                    ))));

    if disambiguate {
        return true;
    }

    match event.logical_key {
        Key::Named(named) => named.to_text().is_none(),
        _ => event.text_with_all_modifiers.is_empty(),
    }
}

fn build_kitty_sequence(
    event: &KittyKeyEvent,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let encode_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let event_type = mode.contains(TermMode::REPORT_EVENT_TYPES)
        && (event.repeat || event.state == ElementState::Released);
    let associated_text = mode
        .contains(TermMode::REPORT_ASSOCIATED_TEXT)
        .then_some(event.text_with_all_modifiers.as_str())
        .filter(|text| {
            event.state != ElementState::Released && !text.is_empty() && !is_control_character(text)
        });

    let mut modifier_bits = kitty_modifier_bits(mods);
    let (base, terminator) = kitty_numpad_base(event)
        .or_else(|| kitty_extended_named_base(event))
        .or_else(|| {
            kitty_functional_base(event, modifier_bits, event_type, associated_text.is_some())
        })
        .or_else(|| {
            kitty_control_or_modifier_base(event, encode_all, &mut modifier_bits)
                .map(|base| (base, 'u'))
        })
        .or_else(|| {
            kitty_textual_base(event, mods, mode, associated_text).map(|base| (base, 'u'))
        })?;

    let mut payload = format!("\x1b[{base}");
    if event_type || modifier_bits != 0 || associated_text.is_some() {
        payload.push(';');
        payload.push_str(&(modifier_bits + 1).to_string());
    }
    if event_type {
        payload.push(':');
        payload.push(match event.state {
            _ if event.repeat => '2',
            ElementState::Pressed => '1',
            ElementState::Released => '3',
        });
    }
    if let Some(text) = associated_text {
        payload.push(';');
        let mut codepoints = text.chars().map(u32::from);
        payload.push_str(&codepoints.next()?.to_string());
        for codepoint in codepoints {
            payload.push(':');
            payload.push_str(&codepoint.to_string());
        }
    }
    payload.push(terminator);
    Some(payload.into_bytes())
}

fn kitty_modifier_bits(mods: ModifiersState) -> u8 {
    u8::from(mods.shift_key())
        | (u8::from(mods.alt_key()) << 1)
        | (u8::from(mods.control_key()) << 2)
        | (u8::from(mods.super_key()) << 3)
}

fn kitty_numpad_base(event: &KittyKeyEvent) -> Option<(String, char)> {
    if event.location != KeyLocation::Numpad {
        return None;
    }
    let code = match event.logical_key.as_ref() {
        Key::Character("0") => 57399,
        Key::Character("1") => 57400,
        Key::Character("2") => 57401,
        Key::Character("3") => 57402,
        Key::Character("4") => 57403,
        Key::Character("5") => 57404,
        Key::Character("6") => 57405,
        Key::Character("7") => 57406,
        Key::Character("8") => 57407,
        Key::Character("9") => 57408,
        Key::Character(".") => 57409,
        Key::Character("/") => 57410,
        Key::Character("*") => 57411,
        Key::Character("-") => 57412,
        Key::Character("+") => 57413,
        Key::Character("=") => 57415,
        Key::Named(NamedKey::Enter) => 57414,
        Key::Named(NamedKey::ArrowLeft) => 57417,
        Key::Named(NamedKey::ArrowRight) => 57418,
        Key::Named(NamedKey::ArrowUp) => 57419,
        Key::Named(NamedKey::ArrowDown) => 57420,
        Key::Named(NamedKey::PageUp) => 57421,
        Key::Named(NamedKey::PageDown) => 57422,
        Key::Named(NamedKey::Home) => 57423,
        Key::Named(NamedKey::End) => 57424,
        Key::Named(NamedKey::Insert) => 57425,
        Key::Named(NamedKey::Delete) => 57426,
        _ => return None,
    };
    Some((code.to_string(), 'u'))
}

fn kitty_extended_named_base(event: &KittyKeyEvent) -> Option<(String, char)> {
    let named = match event.logical_key {
        Key::Named(named) => named,
        _ => return None,
    };
    let code = match named {
        NamedKey::F13 => 57376,
        NamedKey::F14 => 57377,
        NamedKey::F15 => 57378,
        NamedKey::F16 => 57379,
        NamedKey::F17 => 57380,
        NamedKey::F18 => 57381,
        NamedKey::F19 => 57382,
        NamedKey::F20 => 57383,
        NamedKey::F21 => 57384,
        NamedKey::F22 => 57385,
        NamedKey::F23 => 57386,
        NamedKey::F24 => 57387,
        NamedKey::F25 => 57388,
        NamedKey::F26 => 57389,
        NamedKey::F27 => 57390,
        NamedKey::F28 => 57391,
        NamedKey::F29 => 57392,
        NamedKey::F30 => 57393,
        NamedKey::F31 => 57394,
        NamedKey::F32 => 57395,
        NamedKey::F33 => 57396,
        NamedKey::F34 => 57397,
        NamedKey::F35 => 57398,
        NamedKey::ScrollLock => 57359,
        NamedKey::PrintScreen => 57361,
        NamedKey::Pause => 57362,
        NamedKey::ContextMenu => 57363,
        NamedKey::MediaPlay => 57428,
        NamedKey::MediaPause => 57429,
        NamedKey::MediaPlayPause => 57430,
        NamedKey::MediaStop => 57432,
        NamedKey::MediaFastForward => 57433,
        NamedKey::MediaRewind => 57434,
        NamedKey::MediaTrackNext => 57435,
        NamedKey::MediaTrackPrevious => 57436,
        NamedKey::MediaRecord => 57437,
        NamedKey::AudioVolumeDown => 57438,
        NamedKey::AudioVolumeUp => 57439,
        NamedKey::AudioVolumeMute => 57440,
        _ => return None,
    };
    Some((code.to_string(), 'u'))
}

fn kitty_functional_base(
    event: &KittyKeyEvent,
    modifier_bits: u8,
    event_type: bool,
    associated_text: bool,
) -> Option<(String, char)> {
    let named = match event.logical_key {
        Key::Named(named) => named,
        _ => return None,
    };
    let one = if modifier_bits == 0 && !event_type && !associated_text {
        ""
    } else {
        "1"
    };
    let (base, terminator) = match named {
        NamedKey::PageUp => ("5", '~'),
        NamedKey::PageDown => ("6", '~'),
        NamedKey::Insert => ("2", '~'),
        NamedKey::Delete => ("3", '~'),
        NamedKey::Home => (one, 'H'),
        NamedKey::End => (one, 'F'),
        NamedKey::ArrowLeft => (one, 'D'),
        NamedKey::ArrowRight => (one, 'C'),
        NamedKey::ArrowUp => (one, 'A'),
        NamedKey::ArrowDown => (one, 'B'),
        NamedKey::F1 => (one, 'P'),
        NamedKey::F2 => (one, 'Q'),
        // Kitty reserves CSI 13~ for F3 while legacy xterm uses CSI R.
        NamedKey::F3 => ("13", '~'),
        NamedKey::F4 => (one, 'S'),
        NamedKey::F5 => ("15", '~'),
        NamedKey::F6 => ("17", '~'),
        NamedKey::F7 => ("18", '~'),
        NamedKey::F8 => ("19", '~'),
        NamedKey::F9 => ("20", '~'),
        NamedKey::F10 => ("21", '~'),
        NamedKey::F11 => ("23", '~'),
        NamedKey::F12 => ("24", '~'),
        _ => return None,
    };
    Some((base.to_string(), terminator))
}

fn kitty_control_or_modifier_base(
    event: &KittyKeyEvent,
    encode_all: bool,
    modifier_bits: &mut u8,
) -> Option<String> {
    let named = match event.logical_key {
        Key::Named(named) => named,
        _ => return None,
    };
    let control = match named {
        NamedKey::Tab => "9",
        NamedKey::Enter => "13",
        NamedKey::Escape => "27",
        NamedKey::Space => "32",
        NamedKey::Backspace => "127",
        _ => "",
    };
    if !encode_all && control.is_empty() {
        return None;
    }

    let base = match (named, event.location) {
        (NamedKey::Shift, KeyLocation::Left) => "57441",
        (NamedKey::Control, KeyLocation::Left) => "57442",
        (NamedKey::Alt, KeyLocation::Left) => "57443",
        (NamedKey::Super, KeyLocation::Left) => "57444",
        (NamedKey::Hyper, KeyLocation::Left) => "57445",
        (NamedKey::Meta, KeyLocation::Left) => "57446",
        (NamedKey::Shift, _) => "57447",
        (NamedKey::Control, _) => "57448",
        (NamedKey::Alt, _) => "57449",
        (NamedKey::Super, _) => "57450",
        (NamedKey::Hyper, _) => "57451",
        (NamedKey::Meta, _) => "57452",
        (NamedKey::CapsLock, _) => "57358",
        (NamedKey::NumLock, _) => "57360",
        _ => control,
    };

    let pressed = event.state == ElementState::Pressed;
    let bit = match named {
        NamedKey::Shift => Some(0),
        NamedKey::Alt => Some(1),
        NamedKey::Control => Some(2),
        NamedKey::Super => Some(3),
        _ => None,
    };
    if let Some(bit) = bit {
        if pressed {
            *modifier_bits |= 1 << bit;
        } else {
            *modifier_bits &= !(1 << bit);
        }
    }

    (!base.is_empty()).then(|| base.to_string())
}

fn kitty_textual_base(
    event: &KittyKeyEvent,
    mods: ModifiersState,
    mode: TermMode,
    associated_text: Option<&str>,
) -> Option<String> {
    let character = match event.logical_key.as_ref() {
        Key::Character(character) => character,
        _ => return None,
    };
    if character.chars().count() == 1 {
        let shifted = character.chars().next()?;
        let mut unshifted = if mods.shift_key() {
            shifted.to_lowercase().next().unwrap_or(shifted)
        } else {
            shifted
        };
        if mods.shift_key()
            && unshifted == shifted
            && let Key::Character(without_modifiers) = event.key_without_modifiers.as_ref()
        {
            unshifted = without_modifiers.chars().next().unwrap_or(unshifted);
        }
        let primary = u32::from(unshifted);
        let alternate = u32::from(shifted);
        if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) && primary != alternate {
            Some(format!("{primary}:{alternate}"))
        } else {
            Some(primary.to_string())
        }
    } else if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) && associated_text.is_some() {
        Some("0".to_string())
    } else {
        None
    }
}

fn is_control_character(text: &str) -> bool {
    let Some(codepoint) = text.chars().next() else {
        return false;
    };
    text.chars().count() == 1
        && (codepoint <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&codepoint))
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
    // A stack-style single pass reaches the same fixpoint without repeatedly
    // rescanning and reallocating the entire clipboard. Truncation can expose a
    // marker across a splice seam, so recheck the bounded six-byte suffix; each
    // successful check removes six bytes and total work remains linear.
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    let mut safe = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 2;
            if bracketed { b'\n' } else { b'\r' }
        } else {
            let byte = bytes[index];
            index += 1;
            if !bracketed && byte == b'\n' {
                b'\r'
            } else {
                byte
            }
        };
        safe.push(byte);
        while safe.ends_with(START) || safe.ends_with(END) {
            safe.truncate(safe.len() - START.len());
        }
    }
    if bracketed {
        // Preserve `\n` line endings; only normalize CRLF->LF for consistency.
        let mut v = Vec::with_capacity(safe.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(&safe);
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        // Normalize every newline to CR so a trailing/interior newline can't
        // auto-run a command via the shell's line discipline.
        safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production source of this file, excluding test-only items.
    fn production_source() -> String {
        let production = kettle_test_support::production_source(include_str!("input.rs"));
        assert!(
            !production.contains("fn production_source()"),
            "the production slice retained its own helper"
        );
        assert!(
            !production.contains("#[test]"),
            "the production slice retained a test function"
        );
        assert!(
            !production.contains("#[cfg(test)]"),
            "the production slice retained a test-only item"
        );
        production
    }

    fn negotiated_modify_other_keys(level: u8) -> TermMode {
        TermMode::MODIFY_OTHER_KEYS_NEGOTIATED
            | match level {
                0 => TermMode::empty(),
                1 => TermMode::MODIFY_OTHER_KEYS_1,
                2 => TermMode::MODIFY_OTHER_KEYS_2,
                _ => panic!("unsupported test level {level}"),
            }
    }

    fn protocol_event(
        logical_key: Key,
        key_without_modifiers: Key,
        text: Option<&str>,
        text_with_all_modifiers: &str,
        location: KeyLocation,
        state: ElementState,
        repeat: bool,
    ) -> KittyKeyEvent {
        KittyKeyEvent {
            logical_key,
            key_without_modifiers,
            text: text.map(str::to_owned),
            text_with_all_modifiers: text_with_all_modifiers.to_owned(),
            location,
            state,
            repeat,
        }
    }

    fn character_event(
        logical: &str,
        unmodified: &str,
        text: &str,
        state: ElementState,
        repeat: bool,
    ) -> KittyKeyEvent {
        protocol_event(
            Key::Character(logical.into()),
            Key::Character(unmodified.into()),
            Some(text),
            text,
            KeyLocation::Standard,
            state,
            repeat,
        )
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Payload {
        Text,
        Meta,
        Ss3,
        Csi,
    }

    fn classify(bytes: &[u8]) -> Result<Payload, String> {
        if !bytes.contains(&0x1b) {
            return std::str::from_utf8(bytes)
                .map(|_| Payload::Text)
                .map_err(|error| format!("non-UTF-8 text payload: {error}"));
        }
        if bytes.first() != Some(&0x1b) {
            return Err("escape byte appears outside the allowed prefix".into());
        }
        if bytes.len() <= 2 {
            return std::str::from_utf8(&bytes[1..])
                .map(|_| Payload::Meta)
                .map_err(|error| format!("non-UTF-8 Meta payload: {error}"));
        }
        if bytes[1..].contains(&0x1b) {
            return Err("escape byte appears outside the allowed Meta payload".into());
        }
        match bytes[1] {
            b'O' => {
                if bytes.len() == 3 && (0x40..=0x7e).contains(&bytes[2]) {
                    Ok(Payload::Ss3)
                } else {
                    Err("malformed SS3 payload".into())
                }
            }
            b'[' => {
                let Some((&final_byte, params)) = bytes[2..].split_last() else {
                    return Err("CSI payload has no final byte".into());
                };
                if !(0x40..=0x7e).contains(&final_byte) {
                    return Err(format!("CSI final byte {final_byte:#04x} is out of range"));
                }
                if !params
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
                {
                    return Err("CSI parameter bytes contain a non-digit separator".into());
                }
                Ok(Payload::Csi)
            }
            _ => std::str::from_utf8(&bytes[1..])
                .map(|_| Payload::Meta)
                .map_err(|error| format!("non-UTF-8 Meta payload: {error}")),
        }
    }

    fn legacy_modifier_param(bytes: &[u8]) -> Option<u32> {
        if classify(bytes).ok()? != Payload::Csi {
            return None;
        }
        let (&final_byte, params) = bytes[2..].split_last()?;
        let params = std::str::from_utf8(params).ok()?;
        let fields: Vec<&str> = params.split(';').collect();
        match final_byte {
            b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'P' | b'Q' | b'R' | b'S'
                if fields.len() == 2 && fields[0] == "1" =>
            {
                fields[1].parse().ok()
            }
            b'~' if fields.len() == 2 && fields[0] != "27" => fields[1].parse().ok(),
            b'~' if fields.len() == 3 && fields[0] == "27" => fields[1].parse().ok(),
            _ => None,
        }
    }

    fn kitty_modifier_param(bytes: &[u8]) -> Option<u32> {
        if classify(bytes).ok()? != Payload::Csi {
            return None;
        }
        let (_, params) = bytes[2..].split_last()?;
        let params = std::str::from_utf8(params).ok()?;
        let Some((_, modifier_and_rest)) = params.split_once(';') else {
            return Some(1);
        };
        modifier_and_rest.split([';', ':']).next()?.parse().ok()
    }

    fn escaped_bytes(bytes: Option<&[u8]>) -> String {
        match bytes {
            None => "None".into(),
            Some(bytes) => {
                let escaped: String = bytes
                    .iter()
                    .flat_map(|byte| std::ascii::escape_default(*byte).map(char::from))
                    .collect();
                format!("b\"{escaped}\"")
            }
        }
    }

    fn modifier_chord(mods: ModifiersState, key_name: &str) -> String {
        let mut parts = Vec::new();
        if mods.control_key() {
            parts.push("Ctrl");
        }
        if mods.alt_key() {
            parts.push("Alt");
        }
        if mods.shift_key() {
            parts.push("Shift");
        }
        if mods.super_key() {
            parts.push("Super");
        }
        parts.push(key_name);
        parts.join("+")
    }

    fn assert_well_formed(
        entry_point: &str,
        bytes: Option<&[u8]>,
        chord: &str,
        key_name: &str,
        mode: TermMode,
    ) {
        if let Some(bytes) = bytes
            && let Err(reason) = classify(bytes)
        {
            panic!(
                "{entry_point} emitted malformed bytes: {reason}; chord={chord}, key={key_name}, mode_bits={:#010x}, bytes={}",
                mode.bits(),
                escaped_bytes(Some(bytes))
            );
        }
    }

    #[test]
    fn every_legacy_entry_point_consults_the_super_guard() {
        let src = production_source();
        for signature in ["pub fn encode(", "pub fn encode_app_keypad("] {
            let body = src
                .split(signature)
                .nth(1)
                .and_then(|rest| rest.split("\n}").next())
                .unwrap_or_else(|| panic!("missing production body for {signature}"));
            assert!(
                body.contains("legacy_encodes_modifiers(mods)"),
                "{signature} must reject Super before emitting a legacy sequence"
            );
        }

        let kitty_body = src
            .split("fn kitty_modifier_bits(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("kitty_modifier_bits production body");
        assert!(
            kitty_body.contains("mods.super_key()"),
            "Kitty CSI-u must retain its real Super modifier bit"
        );
    }

    #[test]
    fn legacy_encoding_is_well_formed_for_every_modifier_combination() {
        #[derive(Clone)]
        struct SweepKey {
            name: &'static str,
            key: Key,
            location: KeyLocation,
        }

        let named = |name, key| SweepKey {
            name,
            key: Key::Named(key),
            location: KeyLocation::Standard,
        };
        let character = |name, value: &'static str| SweepKey {
            name,
            key: Key::Character(value.into()),
            location: KeyLocation::Standard,
        };
        let numpad_character = |name, value: &'static str| SweepKey {
            name,
            key: Key::Character(value.into()),
            location: KeyLocation::Numpad,
        };
        let numpad_named = |name, key| SweepKey {
            name,
            key: Key::Named(key),
            location: KeyLocation::Numpad,
        };

        let keys = vec![
            named("ArrowUp", NamedKey::ArrowUp),
            named("ArrowDown", NamedKey::ArrowDown),
            named("ArrowLeft", NamedKey::ArrowLeft),
            named("ArrowRight", NamedKey::ArrowRight),
            named("Home", NamedKey::Home),
            named("End", NamedKey::End),
            named("PageUp", NamedKey::PageUp),
            named("PageDown", NamedKey::PageDown),
            named("Insert", NamedKey::Insert),
            named("Delete", NamedKey::Delete),
            named("F1", NamedKey::F1),
            named("F2", NamedKey::F2),
            named("F3", NamedKey::F3),
            named("F4", NamedKey::F4),
            named("F5", NamedKey::F5),
            named("F6", NamedKey::F6),
            named("F7", NamedKey::F7),
            named("F8", NamedKey::F8),
            named("F9", NamedKey::F9),
            named("F10", NamedKey::F10),
            named("F11", NamedKey::F11),
            named("F12", NamedKey::F12),
            named("Tab", NamedKey::Tab),
            named("Enter", NamedKey::Enter),
            named("Backspace", NamedKey::Backspace),
            named("Escape", NamedKey::Escape),
            named("Space", NamedKey::Space),
            character("a", "a"),
            character("i", "i"),
            character("z", "z"),
            character("G", "G"),
            character("1", "1"),
            character("2", "2"),
            character("8", "8"),
            character("0", "0"),
            character("[", "["),
            character("backslash", "\\"),
            character("]", "]"),
            character("^", "^"),
            character("_", "_"),
            character("/", "/"),
            character("?", "?"),
            character("@", "@"),
            character(",", ","),
            character("=", "="),
            character("é", "é"),
            numpad_character("Numpad0", "0"),
            numpad_character("Numpad1", "1"),
            numpad_character("Numpad2", "2"),
            numpad_character("Numpad3", "3"),
            numpad_character("Numpad4", "4"),
            numpad_character("Numpad5", "5"),
            numpad_character("Numpad6", "6"),
            numpad_character("Numpad7", "7"),
            numpad_character("Numpad8", "8"),
            numpad_character("Numpad9", "9"),
            numpad_character("Numpad+", "+"),
            numpad_character("Numpad-", "-"),
            numpad_character("Numpad*", "*"),
            numpad_character("Numpad/", "/"),
            numpad_character("Numpad.", "."),
            numpad_character("Numpad=", "="),
            numpad_named("NumpadEnter", NamedKey::Enter),
            numpad_named("NumpadArrowLeft", NamedKey::ArrowLeft),
            numpad_named("NumpadHome", NamedKey::Home),
            numpad_named("NumpadDelete", NamedKey::Delete),
        ];
        let legacy_modes = [
            TermMode::empty(),
            TermMode::APP_CURSOR,
            TermMode::APP_KEYPAD,
            TermMode::APP_CURSOR | TermMode::APP_KEYPAD,
            negotiated_modify_other_keys(0),
            negotiated_modify_other_keys(1),
            negotiated_modify_other_keys(2),
            TermMode::UNNEGOTIATED_MODIFIED_ENTER,
            TermMode::UNNEGOTIATED_MODIFIED_ENTER | negotiated_modify_other_keys(0),
        ];
        let kitty_modes = [
            TermMode::DISAMBIGUATE_ESC_CODES,
            TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES,
            TermMode::REPORT_ALL_KEYS_AS_ESC,
            TermMode::REPORT_ALL_KEYS_AS_ESC
                | TermMode::REPORT_ALTERNATE_KEYS
                | TermMode::REPORT_ASSOCIATED_TEXT,
            TermMode::REPORT_EVENT_TYPES,
        ];

        for mask in 0..16 {
            let mut mods = ModifiersState::empty();
            if mask & 1 != 0 {
                mods |= ModifiersState::SHIFT;
            }
            if mask & 2 != 0 {
                mods |= ModifiersState::ALT;
            }
            if mask & 4 != 0 {
                mods |= ModifiersState::CONTROL;
            }
            if mask & 8 != 0 {
                mods |= ModifiersState::SUPER;
            }
            let expected_legacy_modifier = 1
                + u32::from(mods.shift_key())
                + 2 * u32::from(mods.alt_key())
                + 4 * u32::from(mods.control_key());

            for key in &keys {
                let chord = modifier_chord(mods, key.name);
                for mode in legacy_modes {
                    let outputs = [
                        ("encode", encode(&key.key, key.key.to_text(), mods, mode)),
                        (
                            "encode_app_keypad",
                            encode_app_keypad(&key.key, key.location, mods, mode),
                        ),
                        ("encode_key_press", encode_key_press(&key.key, mods, mode)),
                    ];

                    for (entry_point, output) in &outputs {
                        let bytes = output.as_deref();
                        assert_well_formed(entry_point, bytes, &chord, key.name, mode);
                        if mods.super_key() {
                            assert!(
                                output.is_none(),
                                "Super reached legacy {entry_point}; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                                key.name,
                                mode.bits(),
                                escaped_bytes(bytes)
                            );
                        }
                        if let Some(param) = bytes.and_then(legacy_modifier_param) {
                            assert_eq!(
                                param,
                                expected_legacy_modifier,
                                "wrong legacy modifier parameter from {entry_point}; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                                key.name,
                                mode.bits(),
                                escaped_bytes(bytes)
                            );
                            assert!(
                                (1..=8).contains(&param),
                                "legacy modifier parameter out of range from {entry_point}; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                                key.name,
                                mode.bits(),
                                escaped_bytes(bytes)
                            );
                            assert_eq!(
                                (param - 1) & 8,
                                0,
                                "legacy modifier offset set xterm's Meta bit from {entry_point}; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                                key.name,
                                mode.bits(),
                                escaped_bytes(bytes)
                            );
                        }
                    }
                }

                for mode in kitty_modes {
                    let event = match &key.key {
                        Key::Character(text) if key.location == KeyLocation::Standard => {
                            character_event(
                                text.as_str(),
                                text.as_str(),
                                text.as_str(),
                                ElementState::Pressed,
                                false,
                            )
                        }
                        _ => {
                            let text = key.key.to_text().map(str::to_owned);
                            protocol_event(
                                key.key.clone(),
                                key.key.clone(),
                                text.as_deref(),
                                text.as_deref().unwrap_or_default(),
                                key.location,
                                ElementState::Pressed,
                                false,
                            )
                        }
                    };
                    let uses_kitty = kitty_event_uses_sequence(&event, mods, mode);
                    let output = encode_kitty_key_event(&event, mods, mode);
                    let bytes = output.as_deref();
                    assert_well_formed("encode_kitty_key_event", bytes, &chord, key.name, mode);

                    if uses_kitty {
                        let Some(bytes) = bytes else {
                            panic!(
                                "Kitty selected CSI-u but emitted nothing; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                                key.name,
                                mode.bits(),
                                escaped_bytes(None)
                            );
                        };
                        let Some(param) = kitty_modifier_param(bytes) else {
                            panic!(
                                "Kitty sequence has no effective modifier parameter; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                                key.name,
                                mode.bits(),
                                escaped_bytes(Some(bytes))
                            );
                        };
                        assert_eq!(
                            param - 1,
                            u32::from(kitty_modifier_bits(mods)),
                            "wrong Kitty modifier parameter; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                            key.name,
                            mode.bits(),
                            escaped_bytes(Some(bytes))
                        );
                    } else if mods.super_key() {
                        assert!(
                            output.is_none(),
                            "Kitty fallback leaked Super into legacy encoding; chord={chord}, key={}, mode_bits={:#010x}, bytes={}",
                            key.name,
                            mode.bits(),
                            escaped_bytes(bytes)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn kitty_disambiguates_control_and_escape_without_changing_plain_text() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES;
        let plain = character_event("a", "a", "a", ElementState::Pressed, false);
        assert_eq!(
            encode_kitty_key_event(&plain, ModifiersState::empty(), mode),
            Some(b"a".to_vec())
        );

        let control = protocol_event(
            Key::Character("a".into()),
            Key::Character("a".into()),
            Some("a"),
            "\u{1}",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(&control, ModifiersState::CONTROL, mode),
            Some(b"\x1b[97;5u".to_vec())
        );

        let escape = protocol_event(
            Key::Named(NamedKey::Escape),
            Key::Named(NamedKey::Escape),
            None,
            "\u{1b}",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(&escape, ModifiersState::empty(), mode),
            Some(b"\x1b[27u".to_vec())
        );
    }

    #[test]
    fn kitty_reports_repeat_release_and_modifier_sides() {
        let events = TermMode::REPORT_EVENT_TYPES;
        let repeat = character_event("a", "a", "a", ElementState::Pressed, true);
        assert_eq!(
            encode_kitty_key_event(&repeat, ModifiersState::empty(), events),
            Some(b"a".to_vec()),
            "text repeats stay text until report-all is requested"
        );
        let all_events = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_EVENT_TYPES;
        assert_eq!(
            encode_kitty_key_event(&repeat, ModifiersState::empty(), all_events),
            Some(b"\x1b[97;1:2u".to_vec())
        );
        let release = character_event("a", "a", "", ElementState::Released, false);
        assert_eq!(
            encode_kitty_key_event(&release, ModifiersState::empty(), events),
            Some(b"\x1b[97;1:3u".to_vec())
        );

        let enter_release = protocol_event(
            Key::Named(NamedKey::Enter),
            Key::Named(NamedKey::Enter),
            None,
            "",
            KeyLocation::Standard,
            ElementState::Released,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(&enter_release, ModifiersState::empty(), events),
            None,
            "legacy Enter has no unambiguous release representation"
        );

        let left_shift_press = protocol_event(
            Key::Named(NamedKey::Shift),
            Key::Named(NamedKey::Shift),
            None,
            "",
            KeyLocation::Left,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(&left_shift_press, ModifiersState::SHIFT, all_events),
            Some(b"\x1b[57441;2u".to_vec())
        );
        let left_shift_release = KittyKeyEvent {
            state: ElementState::Released,
            ..left_shift_press
        };
        assert_eq!(
            encode_kitty_key_event(&left_shift_release, ModifiersState::empty(), all_events),
            Some(b"\x1b[57441;1:3u".to_vec())
        );
    }

    #[test]
    fn kitty_reports_alternate_keypad_function_and_associated_text_codes() {
        let alternate = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ALTERNATE_KEYS;
        let shifted = character_event("A", "a", "A", ElementState::Pressed, false);
        assert_eq!(
            encode_kitty_key_event(&shifted, ModifiersState::SHIFT, alternate),
            Some(b"\x1b[97:65;2u".to_vec())
        );

        let caps_like = character_event("A", "a", "A", ElementState::Pressed, false);
        assert_eq!(
            encode_kitty_key_event(&caps_like, ModifiersState::empty(), alternate),
            Some(b"\x1b[65u".to_vec()),
            "an uppercase logical key without Shift must not be lowercased"
        );

        let numpad = protocol_event(
            Key::Character("1".into()),
            Key::Character("1".into()),
            Some("1"),
            "1",
            KeyLocation::Numpad,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(
                &numpad,
                ModifiersState::empty(),
                TermMode::REPORT_ALL_KEYS_AS_ESC
            ),
            Some(b"\x1b[57400u".to_vec())
        );

        let f3 = protocol_event(
            Key::Named(NamedKey::F3),
            Key::Named(NamedKey::F3),
            None,
            "",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(
                &f3,
                ModifiersState::empty(),
                TermMode::DISAMBIGUATE_ESC_CODES
            ),
            Some(b"\x1b[13~".to_vec())
        );

        let associated = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ASSOCIATED_TEXT;
        let accented = character_event("é", "é", "é", ElementState::Pressed, false);
        assert_eq!(
            encode_kitty_key_event(&accented, ModifiersState::empty(), associated),
            Some(b"\x1b[233;1;233u".to_vec())
        );
        let grapheme = character_event("👩‍💻", "👩‍💻", "👩‍💻", ElementState::Pressed, false);
        assert_eq!(
            encode_kitty_key_event(&grapheme, ModifiersState::empty(), associated),
            Some(b"\x1b[0;1;128105:8205:128187u".to_vec())
        );
    }

    #[test]
    fn synthetic_control_chords_match_gui_protocol_events() {
        let mode = TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::REPORT_ALTERNATE_KEYS
            | TermMode::REPORT_ASSOCIATED_TEXT;

        let shifted_mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let gui_shifted = protocol_event(
            Key::Character("C".into()),
            Key::Character("c".into()),
            Some("\u{3}"),
            "\u{3}",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_key_press(&Key::Character("C".into()), shifted_mods, mode),
            encode_kitty_key_event(&gui_shifted, shifted_mods, mode),
            "Ctrl+Shift+C must preserve Shift and its alternate key"
        );

        let gui_control_space = protocol_event(
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Space),
            Some("\0"),
            "\0",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_key_press(&Key::Named(NamedKey::Space), ModifiersState::CONTROL, mode),
            encode_kitty_key_event(&gui_control_space, ModifiersState::CONTROL, mode),
            "Ctrl+Space must omit C0 associated text in both paths"
        );
    }

    #[test]
    fn kitty_pure_enhancement_flags_do_not_change_legacy_encoding() {
        let f3 = protocol_event(
            Key::Named(NamedKey::F3),
            Key::Named(NamedKey::F3),
            None,
            "",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert_eq!(
            encode_kitty_key_event(
                &f3,
                ModifiersState::empty(),
                TermMode::REPORT_ALTERNATE_KEYS | TermMode::REPORT_ASSOCIATED_TEXT
            ),
            Some(b"\x1bOR".to_vec())
        );

        let plain = character_event("a", "a", "a", ElementState::Pressed, false);
        assert_eq!(
            encode_kitty_key_event(
                &plain,
                ModifiersState::empty(),
                TermMode::REPORT_ASSOCIATED_TEXT
            ),
            Some(b"a".to_vec())
        );

        let backspace = protocol_event(
            Key::Named(NamedKey::Backspace),
            Key::Named(NamedKey::Backspace),
            Some("\u{8}"),
            "\u{8}",
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        assert!(!kitty_event_uses_sequence(
            &backspace,
            ModifiersState::empty(),
            TermMode::REPORT_EVENT_TYPES
        ));
        assert!(kitty_event_uses_sequence(
            &backspace,
            ModifiersState::CONTROL,
            TermMode::DISAMBIGUATE_ESC_CODES
        ));
        assert!(kitty_event_uses_sequence(
            &backspace,
            ModifiersState::empty(),
            TermMode::REPORT_ALL_KEYS_AS_ESC
        ));
    }

    #[test]
    fn paste_normalizes_and_brackets() {
        // Non-bracketed: every newline (CRLF or LF) collapses to a single CR so
        // the shell's line discipline can't auto-run interior/trailing lines.
        assert_eq!(paste_payload("a\r\nb\n", false), b"a\rb\r");
        let p = paste_payload("x\n", true);
        assert!(p.starts_with(b"\x1b[200~") && p.ends_with(b"\x1b[201~"));
    }

    #[test]
    fn legacy_xterm_v_control_and_meta_encodings_are_preserved() {
        let key = Key::Character("v".into());
        assert_eq!(
            encode(&key, Some("v"), ModifiersState::CONTROL, TermMode::empty()),
            Some(vec![0x16]),
            "legacy Ctrl+V must encode as C-v"
        );
        assert_eq!(
            encode(&key, Some("v"), ModifiersState::ALT, TermMode::empty()),
            Some(vec![0x1b, b'v']),
            "legacy Alt+V must encode as M-v"
        );
        assert_eq!(
            encode(
                &key,
                Some("v"),
                ModifiersState::CONTROL | ModifiersState::ALT,
                TermMode::empty()
            ),
            Some(vec![0x1b, 0x16]),
            "legacy Ctrl+Alt+V must encode as M-C-v"
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
        // A single left-to-right `.replace` pass
        // leaves a marker that re-forms across the splice seam.
        // `\x1b[20\x1b[201~1~` -> (strip inner `\x1b[201~`) -> `\x1b[201~`. The
        // sanitizer must leave exactly ONE closer (the wrapper's). The old
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
    fn paste_marker_sanitizer_is_work_bounded_for_deep_nesting() {
        // Each inner removal reveals exactly one outer marker. A repeated
        // whole-string `.replace` implementation needs 100,000 passes over a
        // roughly 600 KiB clipboard; the stack sanitizer consumes it once.
        let depth = 100_000;
        let mut nested = String::with_capacity(depth * 6 + 6);
        for _ in 0..depth {
            nested.push_str("\x1b[20");
        }
        nested.push_str("\x1b[201~");
        for _ in 0..depth {
            nested.push_str("1~");
        }

        assert_eq!(
            paste_payload(&nested, false),
            b"",
            "deeply nested reconstructed markers must be removed"
        );
        assert_eq!(
            paste_payload(&nested, true),
            b"\x1b[200~\x1b[201~",
            "only Kettle's bracketed-paste wrapper may remain"
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

    /// Application-keypad mode (DECKPAM) makes unmodified
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

        // Not applicable: mode off, not on the numpad, or an unsupported
        // modifier.
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
        assert_eq!(
            encode_app_keypad(&ch("5"), np, ModifiersState::SUPER, app),
            None
        );
        // The plain number row (Standard location) still goes through `encode`.
        let mode = TermMode::empty();
        assert_eq!(encode(&ch("5"), Some("5"), none, mode), Some(b"5".to_vec()));
    }

    /// The space bar comes through as NamedKey::Space, so the
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

    /// `Ctrl+Alt+<char>` is xterm's Meta+Control form: ESC then the C0 code.
    ///
    /// It used to be special-cased for `C-M-v` alone, so every other chord
    /// fell through to the printable-Meta path and lost Control — and for the
    /// four characters whose C0 codes are sequence introducers it wrote a bare
    /// CSI / OSC / APC / DCS opener into the PTY, after which the terminal
    /// consumed whatever the user typed next as parameters.
    #[test]
    fn ctrl_alt_characters_keep_control_and_never_emit_a_bare_introducer() {
        use winit::keyboard::Key;

        let mode = TermMode::empty();
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        let encode_char = |c: char, mods: ModifiersState| -> Vec<u8> {
            let key = Key::Character(c.to_string().into());
            encode(&key, None, mods, mode).unwrap_or_else(|| panic!("{c} encodes"))
        };

        for (c, code) in [
            ('a', 0x01_u8),
            ('f', 0x06),
            ('b', 0x02),
            ('k', 0x0b),
            ('v', 0x16),
            // The four that used to escape as introducers.
            ('[', 0x1b), // CSI
            (']', 0x1d), // OSC
            ('_', 0x1f), // APC
            ('p', 0x10), // DCS, reached as Ctrl+Alt+Shift+P
        ] {
            // Precondition: plain Ctrl already produces this C0 code, so the
            // Alt case below is asserting a real relationship rather than a
            // restated constant.
            assert_eq!(
                encode_char(c, ModifiersState::CONTROL),
                vec![code],
                "Ctrl+{c} must be its C0 code"
            );
            assert_eq!(
                encode_char(c, ctrl_alt),
                vec![0x1b, code],
                "Ctrl+Alt+{c} must be ESC followed by the C0 code, not ESC {c}"
            );
        }

        // The specific regression: never the literal character, which is what
        // turned these four into sequence openers.
        for introducer in ['[', ']', '_'] {
            assert_ne!(
                encode_char(introducer, ctrl_alt),
                vec![0x1b, introducer as u8],
                "Ctrl+Alt+{introducer} must not write a bare introducer"
            );
        }

        // A character with no C0 code keeps its printable-Meta behaviour, so
        // an AltGr-produced glyph on an international layout is untouched.
        assert_eq!(
            encode_char('é', ctrl_alt),
            {
                let mut expected = vec![0x1b];
                expected.extend_from_slice("é".as_bytes());
                expected
            },
            "a character outside the C0 table stays printable Meta input"
        );

        // The AltGr substitute. winit only neutralizes AltGr for the RIGHT
        // Alt, but Windows documents left-Ctrl + left-Alt as a substitute, so
        // a German `Ctrl+Alt+Q` — how you type `@` — reaches this branch as
        // plain CONTROL|ALT with `@` as the committed text. The C0 table must
        // not claim it: `q` is in the table, and answering DC1/XON to a
        // request for `@` would be worse than the wrong character the old
        // code gave.
        let composed = encode(
            &Key::Character("q".to_string().into()),
            Some("@"),
            ctrl_alt,
            mode,
        )
        .expect("composed key encodes");
        assert_ne!(
            composed,
            vec![0x1b, 0x11],
            "a press that committed a printable character must not be \
             encoded as that key's control code"
        );

        // And a chord that committed no printable text still takes the table,
        // which is what keeps the fix above from disabling itself.
        assert_eq!(
            encode(
                &Key::Character("a".to_string().into()),
                None,
                ctrl_alt,
                mode
            ),
            Some(vec![0x1b, 0x01])
        );
        // Nor is the platform echoing the base character a composition: the
        // committed text has to DIFFER from the logical key. Printability
        // alone would have flattened every `Ctrl+Alt+<letter>` on Windows,
        // where the text is usually the letter itself.
        assert_eq!(
            encode(
                &Key::Character("a".to_string().into()),
                Some("a"),
                ctrl_alt,
                mode
            ),
            Some(vec![0x1b, 0x01])
        );
        assert_eq!(
            encode(
                &Key::Character("A".to_string().into()),
                Some("a"),
                ctrl_alt,
                mode
            ),
            Some(vec![0x1b, 0x01]),
            "a case-only difference is not a composition either"
        );
        // A committed CONTROL character is not composition — that is just the
        // platform echoing the chord back, and the table still applies.
        assert_eq!(
            encode(
                &Key::Character("a".to_string().into()),
                Some("\u{1}"),
                ctrl_alt,
                mode
            ),
            Some(vec![0x1b, 0x01])
        );
    }

    #[test]
    fn xterm_modifier_table() {
        // xterm's legacy modifier parameter is 1 = none, +1 shift, +2 alt,
        // +4 ctrl. Its +8 Meta bit is not the Super/Command key.
        assert_eq!(legacy_xterm_modifier(ModifiersState::empty()), 1);
        assert_eq!(legacy_xterm_modifier(ModifiersState::SHIFT), 2);
        assert_eq!(legacy_xterm_modifier(ModifiersState::ALT), 3);
        assert_eq!(legacy_xterm_modifier(ModifiersState::CONTROL), 5);
        assert_eq!(
            legacy_xterm_modifier(ModifiersState::CONTROL | ModifiersState::SHIFT),
            6
        );
        assert_eq!(
            legacy_xterm_modifier(ModifiersState::CONTROL | ModifiersState::ALT),
            7
        );
        assert_eq!(legacy_xterm_modifier(ModifiersState::SUPER), 1);
        assert_eq!(
            legacy_xterm_modifier(
                ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::ALT
            ),
            7
        );
    }

    #[test]
    fn encode_modifies_named_keys_per_xterm() {
        use winit::keyboard::{Key, NamedKey};
        let no = ModifiersState::empty();
        let ctrl = ModifiersState::CONTROL;
        let alt = ModifiersState::ALT;
        let shift = ModifiersState::SHIFT;
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let super_key = ModifiersState::SUPER;
        let super_alt = ModifiersState::SUPER | ModifiersState::ALT;
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
            encode(&Key::Named(NamedKey::ArrowUp), None, alt, mode),
            Some(b"\x1b[1;3A".to_vec()),
            "an edge-fallen-through Alt+Up must retain the xterm Alt modifier"
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowLeft), None, ctrl_shift, mode),
            Some(b"\x1b[1;6D".to_vec()),
            "Ctrl+Shift+ArrowLeft must be CSI 1;6D"
        );
        for (name, key, mods) in [
            ("Cmd+Up", NamedKey::ArrowUp, super_key),
            ("Cmd+Option+Up", NamedKey::ArrowUp, super_alt),
            ("Cmd+Left", NamedKey::ArrowLeft, super_key),
            ("Cmd+Right", NamedKey::ArrowRight, super_key),
            ("Cmd+F1", NamedKey::F1, super_key),
            ("Cmd+Delete", NamedKey::Delete, super_key),
            ("Cmd+PageUp", NamedKey::PageUp, super_key),
        ] {
            for mode in [TermMode::empty(), TermMode::APP_CURSOR] {
                assert_eq!(
                    encode(&Key::Named(key), None, mods, mode),
                    None,
                    "{name} must not use a legacy xterm sequence in {mode:?}"
                );
            }
        }

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
    fn legacy_encoding_has_no_super_representation() {
        struct Chord {
            name: &'static str,
            key: Key,
            location: KeyLocation,
            required_mode: TermMode,
        }

        let standard = KeyLocation::Standard;
        let chords = [
            Chord {
                name: "Cmd+Option+Up",
                key: Key::Named(NamedKey::ArrowUp),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Left",
                key: Key::Named(NamedKey::ArrowLeft),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Right",
                key: Key::Named(NamedKey::ArrowRight),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+E",
                key: Key::Character("e".into()),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Enter",
                key: Key::Named(NamedKey::Enter),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Escape",
                key: Key::Named(NamedKey::Escape),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Space",
                key: Key::Named(NamedKey::Space),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Backspace",
                key: Key::Named(NamedKey::Backspace),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Tab",
                key: Key::Named(NamedKey::Tab),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+F5",
                key: Key::Named(NamedKey::F5),
                location: standard,
                required_mode: TermMode::empty(),
            },
            Chord {
                name: "Cmd+Numpad5",
                key: Key::Character("5".into()),
                location: KeyLocation::Numpad,
                required_mode: TermMode::APP_KEYPAD,
            },
        ];

        for base_mode in [
            TermMode::empty(),
            negotiated_modify_other_keys(1),
            negotiated_modify_other_keys(2),
        ] {
            for chord in &chords {
                let mode = base_mode | chord.required_mode;
                let mods = if chord.name == "Cmd+Option+Up" {
                    ModifiersState::SUPER | ModifiersState::ALT
                } else {
                    ModifiersState::SUPER
                };
                let encoded = encode_app_keypad(&chord.key, chord.location, mods, mode)
                    .or_else(|| encode(&chord.key, None, mods, mode));
                assert_eq!(
                    encoded,
                    None,
                    "{} must be silent in legacy mode bits {:#010x}",
                    chord.name,
                    mode.bits()
                );
            }
        }
    }

    #[test]
    fn super_reaches_applications_only_through_kitty_csi_u() {
        let super_alt = ModifiersState::SUPER | ModifiersState::ALT;
        let up = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            encode_key_press(&up, super_alt, TermMode::empty()),
            None,
            "Cmd+Option+Up has no legacy representation"
        );
        assert_eq!(
            encode_key_press(&up, super_alt, TermMode::DISAMBIGUATE_ESC_CODES),
            Some(b"\x1b[1;11A".to_vec())
        );
        assert_eq!(
            encode_key_press(
                &Key::Character("e".into()),
                ModifiersState::SUPER,
                TermMode::DISAMBIGUATE_ESC_CODES,
            ),
            Some(b"\x1b[101;9u".to_vec())
        );
    }

    #[test]
    fn enter_quartet_stays_pairwise_distinct_where_the_shipped_table_promises_it() {
        let enter = Key::Named(NamedKey::Enter);
        let rows = [
            (
                "negotiated xterm level 2",
                negotiated_modify_other_keys(2),
                [
                    b"\r".as_slice(),
                    b"\x1b[27;2;13~".as_slice(),
                    b"\x1b[27;5;13~".as_slice(),
                    b"\x1b[27;3;13~".as_slice(),
                ],
            ),
            (
                "Kitty disambiguation",
                TermMode::DISAMBIGUATE_ESC_CODES,
                [
                    b"\r".as_slice(),
                    b"\x1b[13;2u".as_slice(),
                    b"\x1b[13;5u".as_slice(),
                    b"\x1b[13;3u".as_slice(),
                ],
            ),
        ];
        let modifiers = [
            ModifiersState::empty(),
            ModifiersState::SHIFT,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
        ];

        for (row, mode, expected) in rows {
            let outputs = modifiers.map(|mods| {
                encode_key_press(&enter, mods, mode).expect("Enter quartet must encode")
            });
            assert_eq!(
                outputs[0], b"\r",
                "plain Enter must remain CR in the {row} row"
            );
            for (index, expected) in expected.into_iter().enumerate() {
                assert_eq!(
                    outputs[index], expected,
                    "Enter table mismatch in the {row} row at modifier index {index}"
                );
            }
            for left in 0..outputs.len() {
                for right in left + 1..outputs.len() {
                    assert_ne!(
                        outputs[left], outputs[right],
                        "Enter outputs collided in the {row} row at indexes {left} and {right}"
                    );
                }
            }
        }
    }

    #[test]
    fn modify_other_keys_return_matrix_matches_xterm_at_every_level() {
        struct ReturnRow {
            name: &'static str,
            mods: ModifiersState,
            expected: [&'static [u8]; 3],
        }

        let enter = Key::Named(NamedKey::Enter);
        let rows = [
            ReturnRow {
                name: "Shift",
                mods: ModifiersState::SHIFT,
                expected: [b"\r", b"\r", b"\x1b[27;2;13~"],
            },
            ReturnRow {
                name: "Alt",
                mods: ModifiersState::ALT,
                expected: [b"\r", b"\x1b[27;3;13~", b"\x1b[27;3;13~"],
            },
            ReturnRow {
                name: "Shift+Alt",
                mods: ModifiersState::SHIFT | ModifiersState::ALT,
                expected: [b"\r", b"\x1b[27;4;13~", b"\x1b[27;4;13~"],
            },
            ReturnRow {
                name: "Control",
                mods: ModifiersState::CONTROL,
                expected: [b"\r", b"\x1b[27;5;13~", b"\x1b[27;5;13~"],
            },
        ];

        for row in rows {
            for (level, expected) in row.expected.into_iter().enumerate() {
                assert_eq!(
                    encode_key_press(&enter, row.mods, negotiated_modify_other_keys(level as u8),),
                    Some(expected.to_vec()),
                    "{}+Return at modifyOtherKeys level {level}",
                    row.name,
                );
            }
        }
    }

    #[test]
    fn modify_other_keys_tab_and_ctrl_i_matrix() {
        let tab = Key::Named(NamedKey::Tab);
        let ctrl_i = Key::Character("i".into());

        for level in 0..=2 {
            let mode = negotiated_modify_other_keys(level);
            let encoded_tab = encode_key_press(&tab, ModifiersState::empty(), mode);
            let encoded_ctrl_i = encode_key_press(&ctrl_i, ModifiersState::CONTROL, mode);
            let expected_ctrl_i = if level == 2 {
                b"\x1b[27;5;105~".as_slice()
            } else {
                b"\t".as_slice()
            };

            assert_eq!(encoded_tab, Some(b"\t".to_vec()));
            assert_eq!(encoded_ctrl_i, Some(expected_ctrl_i.to_vec()));
            assert_eq!(encoded_ctrl_i == encoded_tab, level < 2);
            assert_eq!(
                encode_key_press(&tab, ModifiersState::SHIFT, mode),
                Some(b"\x1b[Z".to_vec()),
                "Shift+Tab stays the edit-key sequence at level {level}",
            );

            let expected_ctrl_tab = if level == 0 {
                b"\t".as_slice()
            } else {
                b"\x1b[27;5;9~".as_slice()
            };
            assert_eq!(
                encode_key_press(&tab, ModifiersState::CONTROL, mode),
                Some(expected_ctrl_tab.to_vec()),
            );
        }
    }

    #[test]
    fn plain_modify_other_keys_inputs_stay_legacy_at_every_level() {
        let keys = [
            (Key::Named(NamedKey::Enter), b"\r".as_slice()),
            (Key::Named(NamedKey::Tab), b"\t".as_slice()),
            (Key::Named(NamedKey::Backspace), b"\x7f".as_slice()),
            (Key::Named(NamedKey::Escape), b"\x1b".as_slice()),
            (Key::Named(NamedKey::Space), b" ".as_slice()),
            (Key::Character("a".into()), b"a".as_slice()),
        ];

        for level in 0..=2 {
            let mode = negotiated_modify_other_keys(level);
            for (key, expected) in &keys {
                assert_eq!(
                    encode_key_press(key, ModifiersState::empty(), mode),
                    Some(expected.to_vec()),
                    "plain {key:?} changed at level {level}",
                );
            }
        }
    }

    #[test]
    fn modify_other_keys_covers_named_and_ascii_legacy_inputs() {
        let level_zero = negotiated_modify_other_keys(0);
        let level_one = negotiated_modify_other_keys(1);
        let level_two = negotiated_modify_other_keys(2);

        let backspace = Key::Named(NamedKey::Backspace);
        assert_eq!(
            encode_key_press(&backspace, ModifiersState::ALT, level_zero),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(
            encode_key_press(&backspace, ModifiersState::ALT, level_one),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(
            encode_key_press(&backspace, ModifiersState::ALT, level_two),
            Some(b"\x1b[27;3;8~".to_vec())
        );
        assert_eq!(
            encode_key_press(&backspace, ModifiersState::CONTROL, level_two),
            Some(b"\x08".to_vec()),
            "xterm retains the exact Ctrl+Backspace alias at level two",
        );

        let escape = Key::Named(NamedKey::Escape);
        assert_eq!(
            encode_key_press(&escape, ModifiersState::SHIFT, level_one),
            Some(b"\x1b".to_vec())
        );
        assert_eq!(
            encode_key_press(&escape, ModifiersState::ALT, level_one),
            Some(b"\x1b[27;3;27~".to_vec())
        );
        assert_eq!(
            encode_key_press(&escape, ModifiersState::SHIFT, level_two),
            Some(b"\x1b[27;2;27~".to_vec())
        );

        let space = Key::Named(NamedKey::Space);
        assert_eq!(
            encode_key_press(&space, ModifiersState::CONTROL, level_one),
            Some(b"\0".to_vec())
        );
        assert_eq!(
            encode_key_press(&space, ModifiersState::CONTROL, level_two),
            Some(b"\x1b[27;5;32~".to_vec())
        );
        assert_eq!(
            encode_key_press(&space, ModifiersState::ALT, level_one),
            Some(b"\x1b[27;3;32~".to_vec())
        );

        let a = Key::Character("a".into());
        assert_eq!(
            encode_key_press(&a, ModifiersState::CONTROL, level_one),
            Some(b"\x01".to_vec())
        );
        assert_eq!(
            encode_key_press(&a, ModifiersState::CONTROL, level_two),
            Some(b"\x1b[27;5;97~".to_vec())
        );
        assert_eq!(
            encode_key_press(&a, ModifiersState::ALT, level_one),
            Some(b"\x1b[27;3;97~".to_vec())
        );

        let comma = Key::Character(",".into());
        assert_eq!(
            encode_key_press(&comma, ModifiersState::CONTROL, level_one),
            Some(b"\x1b[27;5;44~".to_vec())
        );
        let two = Key::Character("2".into());
        assert_eq!(
            encode_key_press(&two, ModifiersState::CONTROL, level_one),
            Some(b"\0".to_vec())
        );
        assert_eq!(
            encode_key_press(&two, ModifiersState::CONTROL, level_two),
            Some(b"\x1b[27;5;50~".to_vec())
        );

        let shifted_bracket = Key::Character("}".into());
        let shifted_control = ModifiersState::SHIFT | ModifiersState::CONTROL;
        assert_eq!(
            encode_key_press(&shifted_bracket, shifted_control, level_zero),
            Some(b"\x1d".to_vec())
        );
        assert_eq!(
            encode_key_press(&shifted_bracket, shifted_control, level_one),
            Some(b"\x1d".to_vec())
        );
        assert_eq!(
            encode_key_press(&shifted_bracket, shifted_control, level_two),
            Some(b"\x1b[27;6;125~".to_vec())
        );

        let upper_a = Key::Character("A".into());
        assert_eq!(
            encode_key_press(&upper_a, ModifiersState::SHIFT, level_one),
            Some(b"A".to_vec())
        );
        assert_eq!(
            encode_key_press(&upper_a, ModifiersState::SHIFT, level_two),
            Some(b"\x1b[27;2;65~".to_vec())
        );
    }

    #[test]
    fn modified_enter_fallback_applies_only_before_negotiation() {
        let enter = Key::Named(NamedKey::Enter);
        let fallback = TermMode::UNNEGOTIATED_MODIFIED_ENTER;

        for (mods, expected) in [
            (ModifiersState::SHIFT, b"\x1b[27;2;13~".as_slice()),
            (ModifiersState::CONTROL, b"\x1b[27;5;13~".as_slice()),
            (ModifiersState::ALT, b"\x1b[27;3;13~".as_slice()),
        ] {
            assert_eq!(
                encode_key_press(&enter, mods, fallback),
                Some(expected.to_vec())
            );
            assert_eq!(
                encode_key_press(
                    &enter,
                    mods,
                    fallback | TermMode::MODIFY_OTHER_KEYS_NEGOTIATED,
                ),
                Some(b"\r".to_vec())
            );
        }
        assert_eq!(
            encode_key_press(&enter, ModifiersState::SHIFT, TermMode::empty()),
            Some(b"\r".to_vec())
        );
    }

    #[test]
    fn kitty_enter_encoding_wins_over_every_modify_other_keys_level() {
        let enter = Key::Named(NamedKey::Enter);
        for level in 0..=2 {
            let mode = negotiated_modify_other_keys(level)
                | TermMode::UNNEGOTIATED_MODIFIED_ENTER
                | TermMode::DISAMBIGUATE_ESC_CODES;
            let outputs = [
                encode_key_press(&enter, ModifiersState::empty(), mode).unwrap(),
                encode_key_press(&enter, ModifiersState::SHIFT, mode).unwrap(),
                encode_key_press(&enter, ModifiersState::CONTROL, mode).unwrap(),
                encode_key_press(&enter, ModifiersState::ALT, mode).unwrap(),
            ];

            assert_eq!(outputs[0], b"\r");
            assert_eq!(outputs[1], b"\x1b[13;2u");
            assert_eq!(outputs[2], b"\x1b[13;5u");
            assert_eq!(outputs[3], b"\x1b[13;3u");
            for left in 0..outputs.len() {
                for right in left + 1..outputs.len() {
                    assert_ne!(
                        outputs[left], outputs[right],
                        "Kitty Enter outputs collided at level {level}",
                    );
                }
            }
        }
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

    /// Each DEC mouse mode reports exactly the motion it is defined to report.
    ///
    /// Driven through the real classifier and the real rule, one row per mode,
    /// with the button held and not held — so a mode that silently behaves
    /// like its neighbour shows up as a row that disagrees. Two did: 1003
    /// reported nothing without a button (its whole purpose), and 1000
    /// reported drags whenever one happened to be down.
    #[test]
    fn each_mouse_mode_reports_exactly_the_motion_it_promises() {
        use kettle_core::TermMode;

        // (DEC mode, TermMode, motion with a button, motion without one)
        let table = [
            (
                "1000 click-only",
                TermMode::MOUSE_REPORT_CLICK,
                false,
                false,
            ),
            ("1002 drag", TermMode::MOUSE_DRAG, true, false),
            ("1003 all motion", TermMode::MOUSE_MOTION, true, true),
            ("off", TermMode::empty(), false, false),
        ];
        for (name, mode, held, hovering) in table {
            let (track, _) = mouse_tracking(mode);
            assert_eq!(
                motion_is_reported(track, true),
                held,
                "{name}: motion with a button held"
            );
            assert_eq!(
                motion_is_reported(track, false),
                hovering,
                "{name}: motion while only hovering"
            );
        }

        // The three tracking modes must not be interchangeable — if any two
        // rows above agreed on both columns, this table could not tell them
        // apart and would pass with the modes confused.
        let signature = |mode: TermMode| {
            let (track, _) = mouse_tracking(mode);
            (
                motion_is_reported(track, true),
                motion_is_reported(track, false),
            )
        };
        let click = signature(TermMode::MOUSE_REPORT_CLICK);
        let drag = signature(TermMode::MOUSE_DRAG);
        let all = signature(TermMode::MOUSE_MOTION);
        assert!(
            click != drag && drag != all && click != all,
            "1000/1002/1003 must be distinguishable: {click:?} {drag:?} {all:?}"
        );
    }

    /// A hovering motion report carries xterm's no-button code, so 1003's
    /// report is the `CSI < 35 ; x ; y M` applications match on.
    #[test]
    fn hovering_motion_reports_the_no_button_code() {
        let seq = mouse_encode(
            true,
            MOUSE_NO_BUTTON,
            true,
            true,
            9,
            4,
            ModifiersState::empty(),
        );
        assert_eq!(
            String::from_utf8(seq).expect("utf8"),
            "\x1b[<35;10;5M",
            "1003 hover must report button 3 plus the motion bit"
        );
    }

    #[test]
    fn alternate_scroll_emits_cursor_keys_only_without_mouse_tracking() {
        // `ALTERNATE_SCROLL` (DEC 1007) is in the terminal's default mode set,
        // so the realistic alt-screen state carries both flags.
        let alt = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            alternate_scroll_key(3, alt),
            Some(b"\x1b[A\x1b[A\x1b[A".to_vec())
        );
        assert_eq!(
            alternate_scroll_key(-3, alt),
            Some(b"\x1b[B\x1b[B\x1b[B".to_vec())
        );

        let app_cursor = alt | TermMode::APP_CURSOR;
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
            alternate_scroll_key(3, alt | TermMode::MOUSE_REPORT_CLICK),
            None,
            "mouse-tracking apps must receive wheel reports, not synthesized arrows"
        );
        // An app that turns 1007 off wants the wheel to reach kettle's own
        // scrollback instead of being fed synthetic arrow keys.
        assert_eq!(
            alternate_scroll_key(3, TermMode::ALT_SCREEN),
            None,
            "CSI ?1007 l must opt out of alternate scroll"
        );
    }

    #[test]
    fn wheel_accum_carries_sub_notch_residue() {
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseScrollDelta;

        // A Windows Precision Touchpad gesture: WM_MOUSEWHEEL deltas well under
        // WHEEL_DELTA(120), which winit hands us as fractional LineDelta. Before
        // v2.41.0 each of these rounded to zero independently and the terminal
        // never scrolled at all.
        let mut accum = WheelAccum::default();
        let mut lines = 0;
        let mut notches = 0;
        for _ in 0..20 {
            let steps = accum.feed(&MouseScrollDelta::LineDelta(0.0, 0.1), 1.0);
            lines += steps.lines;
            notches += steps.notches;
        }
        // 20 x 0.1 notches = 2 notches = 6 lines. Allow one step of float slack.
        assert!(
            (5..=6).contains(&lines),
            "sub-notch deltas must accumulate into real scroll, got {lines}"
        );
        assert!(
            (1..=2).contains(&notches),
            "sub-notch deltas must accumulate into whole detents, got {notches}"
        );

        // Pin the defect itself: the pre-fix formula (`y.round() * 3.0 * mult`,
        // rounded) yields exactly nothing for this identical input, which is why
        // touchpad scrolling was dead rather than merely slow.
        let old_formula_total: i32 = (0..20).map(|_| (0.1f32.round() * 3.0) as i32).sum();
        assert_eq!(
            old_formula_total, 0,
            "regression guard: the old per-event rounding dropped the whole gesture"
        );

        // Whole detents keep their historical feel exactly: 3 lines per notch,
        // scaled by the multiplier.
        let one = MouseScrollDelta::LineDelta(0.0, 1.0);
        assert_eq!(
            WheelAccum::default().feed(&one, 1.0),
            WheelSteps {
                notches: 1,
                lines: 3,
                cols: 0
            }
        );
        assert_eq!(WheelAccum::default().feed(&one, 2.0).lines, 6);
        assert_eq!(
            WheelAccum::default()
                .feed(&MouseScrollDelta::LineDelta(0.0, -2.0), 1.0)
                .lines,
            -6
        );
        // PixelDelta parity with the historical `p.y / 20.0`: 60 px = 3 lines.
        assert_eq!(
            WheelAccum::default()
                .feed(
                    &MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 60.0)),
                    1.0
                )
                .lines,
            3
        );
        // A sub-threshold PixelDelta (macOS/Wayland trackpad) also accumulates
        // instead of vanishing.
        let mut pixel = WheelAccum::default();
        let small = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0));
        let total: i32 = (0..8).map(|_| pixel.feed(&small, 1.0).lines).sum();
        assert!(
            total >= 1,
            "small pixel deltas must accumulate, got {total}"
        );
    }

    #[test]
    fn wheel_accum_reversal_reset_and_hostile_input() {
        use winit::event::MouseScrollDelta;

        // Reversing direction abandons leftover residue instead of spending it
        // against the new direction.
        let mut accum = WheelAccum::default();
        accum.feed(&MouseScrollDelta::LineDelta(0.0, 0.9), 1.0);
        let back = accum.feed(&MouseScrollDelta::LineDelta(0.0, -0.9), 1.0);
        assert_eq!(
            back.notches, 0,
            "a reversal must not immediately fire a notch off stale residue"
        );

        // `reset` clears everything (TouchPhase::Ended / momentum end).
        let mut accum = WheelAccum::default();
        accum.feed(&MouseScrollDelta::LineDelta(0.0, 0.9), 1.0);
        accum.reset();
        assert_eq!(
            accum
                .feed(&MouseScrollDelta::LineDelta(0.0, 0.2), 1.0)
                .lines,
            0
        );

        // NaN/infinite deltas are dropped, not folded in — otherwise the residue
        // becomes NaN and the wheel is dead for the life of the window.
        let mut accum = WheelAccum::default();
        assert!(
            accum
                .feed(&MouseScrollDelta::LineDelta(0.0, f32::NAN), 1.0)
                .is_zero()
        );
        assert!(
            accum
                .feed(&MouseScrollDelta::LineDelta(0.0, f32::INFINITY), 1.0)
                .is_zero()
        );
        assert_eq!(
            accum
                .feed(&MouseScrollDelta::LineDelta(0.0, 1.0), 1.0)
                .lines,
            3,
            "a poisoned residue would make every later event yield nothing"
        );

        // Residue stays bounded under a flood, so a runaway device or a hostile
        // ctl feed can't accumulate an unbounded scroll.
        let mut accum = WheelAccum::default();
        for _ in 0..64 {
            accum.feed(&MouseScrollDelta::LineDelta(0.0, f32::MAX), 1.0);
        }
        let after = accum.feed(&MouseScrollDelta::LineDelta(0.0, 1.0), 1.0);
        assert!(
            after.lines.abs() <= 30_001,
            "residue must stay clamped, got {}",
            after.lines
        );

        // Horizontal motion accumulates on its own axis and does not disturb
        // vertical progress.
        let mut accum = WheelAccum::default();
        let diag = accum.feed(&MouseScrollDelta::LineDelta(1.0, 1.0), 1.0);
        assert_eq!((diag.cols, diag.notches, diag.lines), (1, 1, 3));
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
        // Side buttons. Back = SGR 128, Forward = 129 — press 'M'
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
