use super::*;

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_effective_modifiers(
    mut raw: ModifiersState,
    policy: MacosOptionAsAlt,
    left_option_pressed: bool,
    right_option_pressed: bool,
) -> ModifiersState {
    if !raw.alt_key() || raw.control_key() || raw.super_key() {
        return raw;
    }
    let option_is_alt = match policy {
        MacosOptionAsAlt::None => false,
        MacosOptionAsAlt::Left => left_option_pressed,
        MacosOptionAsAlt::Right => right_option_pressed,
        MacosOptionAsAlt::Both => true,
    };
    if !option_is_alt {
        raw.remove(ModifiersState::ALT);
    }
    raw
}

/// Whether `macos-option-as-alt` has any business masking Option for this key.
///
/// The policy answers exactly one question: does Option compose text (`⌥e` →
/// `´`) or act as Meta? That question does not arise for a key macOS composes
/// no character from. Answering it anyway lets `macos_effective_modifiers` drop
/// the ALT bit, so the encoder's `ESC DEL` arm (`input::encode`, the
/// `(false, true)` Backspace case) never sees it and `⌥⌫` deletes a single
/// character instead of a word.
///
/// Backspace is the one entry that cannot be derived. winit reports the legacy
/// C0 `\x08` from `NamedKey::Backspace.to_text()`, so a plain
/// `to_text().is_none()` test excludes the very key this exists for.
#[cfg(any(target_os = "macos", test))]
pub(super) fn option_is_meta_for(key: &Key) -> bool {
    match key {
        Key::Named(NamedKey::Backspace) => true,
        // Return composes nothing either, though winit gives it the text
        // "\r". Option+Return is Alt+Return, the newline chord in Codex,
        // Claude Code, zsh and fish; masked, it would submit instead.
        Key::Named(NamedKey::Enter) => true,
        Key::Named(named) => named.to_text().is_none(),
        // Exactly what the policy protects. Restoring ALT for a character key
        // would prefix ESC to a glyph macOS has already composed, so `⌥e` would
        // send `ESC ´` instead of `´`.
        _ => false,
    }
}

/// The modifiers the PTY encoder should see, as against the ones the rest of
/// the UI sees. Identical to `masked` except on the keys `option_is_meta_for`
/// exempts, where a physically-held Option is restored.
///
/// The `left`/`right` policy sides are deliberately not consulted: a side that
/// cannot compose text has nothing to choose between. kitty draws the same
/// line — `macos_option_as_alt = no` still sends `ESC DEL` for `⌥⌫`, because
/// that setting governs only the text path.
#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_pty_modifiers(
    masked: ModifiersState,
    raw: ModifiersState,
    key: &Key,
) -> ModifiersState {
    if raw.alt_key() && option_is_meta_for(key) {
        return masked | ModifiersState::ALT;
    }
    masked
}

/// `ws.mods` as the PTY key encoder should read it. Every other consumer —
/// keybind matching above all — keeps the masked value, so no chord starts
/// firing an action it did not fire before.
///
/// Cross-platform on purpose, with the `cfg` inside the body: splitting at the
/// call site would leave a `ws.mods` in `write_terminal_key_event` for the
/// source census to find on every target.
pub(super) fn pty_key_modifiers(ws: &WindowState, key: &Key) -> ModifiersState {
    #[cfg(target_os = "macos")]
    {
        macos_pty_modifiers(ws.mods, ws.macos_raw_mods, key)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = key;
        ws.mods
    }
}

/// Whether Option is physically down, whatever `macos-option-as-alt` says.
///
/// On macOS the search and modal editors bind word-wise motions to Option on
/// Backspace, Delete and the horizontal arrows, all keys `option_is_meta_for`
/// exempts. So the composition policy has no say here either. Reading the
/// masked `ws.mods` would leave `delete_word_backward` and `move_left(by_word)`
/// unreachable.
pub(super) fn option_physically_held(ws: &WindowState) -> bool {
    #[cfg(target_os = "macos")]
    {
        ws.macos_raw_mods.alt_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ws.mods.alt_key()
    }
}

#[cfg(target_os = "macos")]
pub(super) fn set_macos_option_as_alt(window: &Window, policy: MacosOptionAsAlt) {
    use winit::platform::macos::{OptionAsAlt, WindowExtMacOS as _};

    let policy = match policy {
        MacosOptionAsAlt::None => OptionAsAlt::None,
        MacosOptionAsAlt::Left => OptionAsAlt::OnlyLeft,
        MacosOptionAsAlt::Right => OptionAsAlt::OnlyRight,
        MacosOptionAsAlt::Both => OptionAsAlt::Both,
    };
    window.set_option_as_alt(policy);
}

#[cfg(target_os = "macos")]
pub(super) fn refresh_macos_modifiers(ws: &mut WindowState, policy: MacosOptionAsAlt) {
    ws.mods = macos_effective_modifiers(
        ws.macos_raw_mods,
        policy,
        ws.macos_left_option_pressed,
        ws.macos_right_option_pressed,
    );
}

pub(super) fn stamp_accepted_input(
    last_typed: &mut Option<std::time::Instant>,
    delivery: PaneInputDelivery,
    input_time: std::time::Instant,
) {
    if delivery.accepted {
        *last_typed = Some(input_time);
    }
}

/// The modifiers physically held. On macOS, Option counts whether or not it
/// composes text (`macos-option-as-alt`), so an Option chord is a chord.
pub(super) fn held_modifiers(ws: &WindowState) -> winit::keyboard::ModifiersState {
    #[cfg(target_os = "macos")]
    {
        ws.macos_raw_mods
    }
    #[cfg(not(target_os = "macos"))]
    {
        ws.mods
    }
}

const MAX_TERMINAL_OWNED_KEYS: usize = 256;

pub(super) fn key_release_bytes(
    cfg: &Config,
    event: &winit::event::KeyEvent,
    mods: ModifiersState,
    mode: kettle_core::TermMode,
) -> Option<Arc<[u8]>> {
    if !mode.contains(kettle_core::TermMode::REPORT_EVENT_TYPES) {
        return None;
    }
    let mut release = event.clone();
    release.state = ElementState::Released;
    release.repeat = false;
    encode_key_event_for_mode(cfg, &release, mods, mode)
        .filter(|bytes| !bytes.is_empty())
        .map(Arc::from)
}

/// A release shares admission state with its deferred press, even when it
/// waits on a later paste or remains held after replay. Any accepted repeat
/// makes the release necessary; later rejected repeats must not clear it.
#[derive(Clone)]
pub(crate) struct TerminalKeyRelease {
    pub(super) bytes: Arc<[u8]>,
    pub(super) press_accepted: Arc<std::sync::atomic::AtomicBool>,
}

impl TerminalKeyRelease {
    pub(super) fn new(bytes: Arc<[u8]>, accepted: bool) -> Self {
        Self {
            bytes,
            press_accepted: Arc::new(std::sync::atomic::AtomicBool::new(accepted)),
        }
    }

    pub(super) fn mark_accepted(&self) {
        self.press_accepted
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(super) fn release_terminal_owned_keys(
    ws: &mut WindowState,
    mut send: impl FnMut(u64, PhysicalKey, TerminalKeyRelease),
) {
    for ((key, pane), bytes) in std::mem::take(&mut ws.terminal_key_release_bytes) {
        send(pane, key, bytes);
    }
    ws.suppressed_key_releases
        .extend(ws.terminal_owned_key_releases.drain());
    ws.terminal_key_recipients.clear();
}

impl App {}

/// Terminator parity: remap encoded Backspace/Delete bytes per the user's
/// `backspace-binding`/`delete-binding`. `send_keys` uses it too, so it honors
/// the same remap as GUI keystrokes (the "same path as a human key press"
/// contract).
///
/// The binding only ever replaces the UNMODIFIED encoding, and the test for
/// that is the encoded bytes themselves, not the modifier state. Delete's
/// `CSI 3 ~` form carries a modifier parameter for *shift, alt and control
/// alike* (Super has no legacy representation and is rejected before this
/// point). With `delete-binding` defaulting to `EscapeSequence`, remapping a
/// modified Delete would rewrite it to the plain `CSI 3 ~`, so `Ctrl+Delete`
/// would equal `Delete` and readline's `kill-word` and every `<C-Del>` /
/// `<S-Del>` / `<M-Del>` editor mapping would delete one character instead.
///
/// Comparing against the plain form cannot drift from the encoder, and it is
/// right for the cases modifier logic keeps missing: a `modifyOtherKeys`
/// level-1 `Shift+Backspace` (`CSI 27;2;8~`) and any kitty-protocol encoding
/// are left alone, because an application that negotiated a precise encoding
/// must not have it overwritten by a legacy remap.
pub(super) fn apply_bs_del_binding(
    cfg: &Config,
    key: &Key,
    mode: kettle_core::TermMode,
    bytes: Vec<u8>,
) -> Vec<u8> {
    let Key::Named(named) = key else {
        return bytes;
    };
    // Explicit DECBKM overrides the configured startup encoding until reset.
    if *named == NamedKey::Backspace && mode.contains(kettle_core::TermMode::BACKARROW_NEGOTIATED) {
        return bytes;
    }
    use kettle_config::{BackspaceBinding, DeleteBinding};
    /// What `input::encode` emits for an unmodified Backspace.
    const PLAIN_BACKSPACE: &[u8] = &[0x7f];
    /// What `input::encode` emits for an unmodified Delete.
    const PLAIN_DELETE: &[u8] = b"\x1b[3~";
    if *named == NamedKey::Backspace && bytes == PLAIN_BACKSPACE {
        match cfg.backspace_binding {
            BackspaceBinding::AsciiDel => vec![0x7f],
            BackspaceBinding::ControlH => vec![0x08],
            BackspaceBinding::EscapeSequence => b"\x1b[3~".to_vec(),
            BackspaceBinding::Automatic => bytes,
        }
    } else if *named == NamedKey::Delete && bytes == PLAIN_DELETE {
        match cfg.delete_binding {
            DeleteBinding::AsciiDel => vec![0x7f],
            DeleteBinding::ControlH => vec![0x08],
            DeleteBinding::EscapeSequence => b"\x1b[3~".to_vec(),
            DeleteBinding::Automatic => bytes,
        }
    } else {
        bytes
    }
}

pub(super) fn encode_key_event_for_mode(
    cfg: &Config,
    event: &winit::event::KeyEvent,
    mods: ModifiersState,
    mode: kettle_core::TermMode,
) -> Option<Vec<u8>> {
    let mut bytes = input::encode_key_event(event, mods, mode)?;
    if !input::uses_kitty_sequence(event, mods, mode) {
        bytes = apply_bs_del_binding(cfg, &event.logical_key, mode, bytes);
    }
    Some(bytes)
}

pub(super) fn key_is_modified_enter(key: &Key, mods: ModifiersState) -> bool {
    matches!(key, Key::Named(NamedKey::Enter))
        && (mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key())
}

pub(super) fn parse_ctl_send_key_batch(
    keys: &[serde_json::Value],
) -> std::result::Result<Vec<(String, ModifiersState, Key)>, String> {
    if keys.is_empty() || keys.len() > MAX_CTL_SEND_KEYS {
        return Err(format!(
            "'keys' must contain 1..={MAX_CTL_SEND_KEYS} entries"
        ));
    }
    let mut parsed = Vec::with_capacity(keys.len());
    for value in keys {
        let Some(token) = value.as_str() else {
            return Err("non-string entry in 'keys'".into());
        };
        if token.is_empty() || token.len() > MAX_CTL_SEND_KEY_TOKEN_BYTES {
            return Err(format!(
                "key tokens must be 1..={MAX_CTL_SEND_KEY_TOKEN_BYTES} bytes"
            ));
        }
        let Some((mods, key)) = parse_send_key(token) else {
            return Err(format!("unrecognized key token '{token}'"));
        };
        parsed.push((token.to_owned(), mods, key));
    }
    Ok(parsed)
}

pub(super) fn extend_ctl_send_key_bytes(
    bytes: &mut Vec<u8>,
    encoded: &[u8],
) -> std::result::Result<(), String> {
    if encoded.len() > MAX_CTL_SEND_KEY_BYTES.saturating_sub(bytes.len()) {
        return Err(format!(
            "encoded key sequence exceeds {MAX_CTL_SEND_KEY_BYTES} bytes"
        ));
    }
    bytes.extend_from_slice(encoded);
    Ok(())
}

/// Parse one `send_keys` token (`"escape"`, `"ctrl+c"`, `"shift+tab"`,
/// `"f5"`, `"alt+enter"`, a bare character like `"G"`) into the
/// `(mods, key)` pair the GUI's PTY encoder (`input::encode_key_press`)
/// consumes. Same `+`-separated grammar and modifier aliases as config
/// keybind triggers (`parse_trigger`), plus names the keybind grammar lacks
/// (escape, insert, comma). Character case is preserved (`"G"` sends `G`,
/// like a human holding Shift). `None` on an unrecognized token, so the
/// caller can name the bad token instead of sending wrong bytes. Pure
/// (unit-tested).
pub(super) fn parse_send_key(token: &str) -> Option<(ModifiersState, Key)> {
    parse_key_token(token)
}

/// Parse a Kettle-owned UI key. Application chrome can consume Super/Command
/// character chords directly (for example Command+A on macOS).
pub(super) fn parse_ui_key(token: &str) -> Option<(ModifiersState, Key)> {
    let (mods, key) = parse_key_token(token)?;
    let key = if key == Key::Named(NamedKey::Space) {
        Key::Character(" ".into())
    } else if matches!(
        &key,
        Key::Named(
            NamedKey::Escape
                | NamedKey::Enter
                | NamedKey::Tab
                | NamedKey::Backspace
                | NamedKey::Delete
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::F3
        ) | Key::Character(_)
    ) {
        key
    } else {
        return None;
    };
    Some((mods, key))
}

fn parse_key_token(token: &str) -> Option<(ModifiersState, Key)> {
    let parts: Vec<&str> = token.split('+').collect();
    let last = parts.len().checked_sub(1)?;
    let mut mods = ModifiersState::empty();
    let mut key: Option<Key> = None;
    for (i, part) in parts.iter().enumerate() {
        let raw = part.trim();
        let lower = raw.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "alt" | "opt" | "option" => mods |= ModifiersState::ALT,
            "shift" => mods |= ModifiersState::SHIFT,
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => {
                mods |= ModifiersState::SUPER;
            }
            _ => {
                // Only the LAST slot may be a key (same rule as parse_trigger:
                // a typo'd modifier must fail loudly, not bind a plain key).
                if i != last {
                    return None;
                }
                // Named characters first, since a literal `,` can't survive the
                // CLI's comma-split `--keys` and `+` is every client's chord
                // separator. Mirrors `parse_key`'s plus/minus/equal aliases.
                match lower.as_str() {
                    "plus" => {
                        key = Some(Key::Character("+".into()));
                        continue;
                    }
                    "comma" => {
                        key = Some(Key::Character(",".into()));
                        continue;
                    }
                    "minus" => {
                        key = Some(Key::Character("-".into()));
                        continue;
                    }
                    "equal" => {
                        key = Some(Key::Character("=".into()));
                        continue;
                    }
                    _ => {}
                }
                let named = match lower.as_str() {
                    "escape" | "esc" => Some(NamedKey::Escape),
                    "enter" | "return" => Some(NamedKey::Enter),
                    "tab" => Some(NamedKey::Tab),
                    "backspace" | "bs" => Some(NamedKey::Backspace),
                    "delete" | "del" => Some(NamedKey::Delete),
                    "insert" | "ins" => Some(NamedKey::Insert),
                    "space" => Some(NamedKey::Space),
                    "up" => Some(NamedKey::ArrowUp),
                    "down" => Some(NamedKey::ArrowDown),
                    "left" => Some(NamedKey::ArrowLeft),
                    "right" => Some(NamedKey::ArrowRight),
                    "home" => Some(NamedKey::Home),
                    "end" => Some(NamedKey::End),
                    "page_up" | "pageup" | "prior" => Some(NamedKey::PageUp),
                    "page_down" | "pagedown" | "next" => Some(NamedKey::PageDown),
                    "f1" => Some(NamedKey::F1),
                    "f2" => Some(NamedKey::F2),
                    "f3" => Some(NamedKey::F3),
                    "f4" => Some(NamedKey::F4),
                    "f5" => Some(NamedKey::F5),
                    "f6" => Some(NamedKey::F6),
                    "f7" => Some(NamedKey::F7),
                    "f8" => Some(NamedKey::F8),
                    "f9" => Some(NamedKey::F9),
                    "f10" => Some(NamedKey::F10),
                    "f11" => Some(NamedKey::F11),
                    "f12" => Some(NamedKey::F12),
                    _ => None,
                };
                key = Some(match named {
                    Some(n) => Key::Named(n),
                    None => {
                        // A single character, case preserved.
                        let mut ch = raw.chars();
                        let mut c = ch.next()?;
                        if ch.next().is_some() {
                            return None;
                        }
                        // A bare uppercase ASCII token represents the same
                        // logical action as a GUI Shift+letter press. Preserve
                        // that inferred modifier so negotiated Kitty CSI-u can
                        // report the unshifted primary key and shifted
                        // alternate key instead of misreporting a Caps-Lock
                        // style unmodified uppercase key.
                        if c.is_ascii_uppercase() {
                            mods |= ModifiersState::SHIFT;
                        }
                        // Keep SHIFT on alphabetic chords while normalizing
                        // their logical character to uppercase: the legacy
                        // encoder still emits the expected byte, while Kitty
                        // CSI-u needs the live modifier bit to distinguish
                        // Ctrl+C from Ctrl+Shift+C.
                        if mods.contains(ModifiersState::SHIFT) && c.is_ascii_alphabetic() {
                            c = c.to_ascii_uppercase();
                        }
                        Key::Character(c.to_string().into())
                    }
                });
            }
        }
    }
    key.map(|k| (mods, k))
}

fn push_unique_trigger(out: &mut Vec<Trigger>, trigger: Trigger) {
    if !out.contains(&trigger) {
        out.push(trigger);
    }
}

fn physical_keybind_chars(physical: &PhysicalKey, mods: ModifiersState) -> &'static [char] {
    match physical {
        PhysicalKey::Code(KeyCode::Equal) => {
            if mods.shift_key() {
                &['=', '+']
            } else {
                &['=']
            }
        }
        PhysicalKey::Code(KeyCode::Minus) => {
            if mods.shift_key() {
                &['-', '_']
            } else {
                &['-']
            }
        }
        // US punctuation keys. A binding written with the unshifted
        // character, as imported Terminator accelerators are
        // (`<Control><Shift>comma`), must match the shifted character the
        // keyboard produces with Shift held.
        PhysicalKey::Code(KeyCode::Comma) => shifted(mods, &[','], &[',', '<']),
        PhysicalKey::Code(KeyCode::Period) => shifted(mods, &['.'], &['.', '>']),
        PhysicalKey::Code(KeyCode::Slash) => shifted(mods, &['/'], &['/', '?']),
        PhysicalKey::Code(KeyCode::Semicolon) => shifted(mods, &[';'], &[';', ':']),
        PhysicalKey::Code(KeyCode::Quote) => shifted(mods, &['\''], &['\'', '"']),
        PhysicalKey::Code(KeyCode::BracketLeft) => shifted(mods, &['['], &['[', '{']),
        PhysicalKey::Code(KeyCode::BracketRight) => shifted(mods, &[']'], &[']', '}']),
        PhysicalKey::Code(KeyCode::Backslash) => shifted(mods, &['\\'], &['\\', '|']),
        PhysicalKey::Code(KeyCode::Backquote) => shifted(mods, &['`'], &['`', '~']),
        PhysicalKey::Code(KeyCode::NumpadAdd) => &['+'],
        PhysicalKey::Code(KeyCode::NumpadSubtract) => &['-'],
        PhysicalKey::Code(KeyCode::NumpadEqual) => &['='],
        PhysicalKey::Code(KeyCode::Digit0) => shifted(mods, &['0'], &['0', ')']),
        PhysicalKey::Code(KeyCode::Numpad0) => &['0'],
        _ => &[],
    }
}

fn shifted(
    mods: ModifiersState,
    plain: &'static [char],
    with_shift: &'static [char],
) -> &'static [char] {
    if mods.shift_key() { with_shift } else { plain }
}

pub(super) fn keybind_candidates(
    logical_key: Option<&Key>,
    physical_key: Option<&PhysicalKey>,
    mods: ModifiersState,
    unshifted_key: Option<&Key>,
) -> Vec<Trigger> {
    let mut out = Vec::new();
    let kmods = to_mods(mods);
    if let Some(k) = logical_key.and_then(to_kkey) {
        push_unique_trigger(&mut out, Trigger::new(kmods, k));
    }
    if let Some(physical) = physical_key {
        let aliases = physical_keybind_chars(physical, mods);
        if physical_aliases_apply(logical_key, aliases, unshifted_key) {
            for c in aliases {
                push_unique_trigger(&mut out, Trigger::new(kmods, KKey::Char(*c)));
            }
        }
    }
    out
}

/// Whether a key's US characters may stand in for what the layout produced.
/// Only where the layout gives the key no ASCII character of its own (a
/// non-Latin layout, or AZERTY's `à` on the 0 key) or produced one of those
/// same characters at another shift level. Prefer the layout's unshifted key
/// so German Shift+comma (`;`) keeps its comma alias. On Dvorak, Ctrl+W is
/// the US comma key: aliasing it to Ctrl+, would open Settings instead of reaching the
/// shell.
fn physical_aliases_apply(
    logical_key: Option<&Key>,
    aliases: &[char],
    unshifted_key: Option<&Key>,
) -> bool {
    let Some(Key::Character(text)) = unshifted_key.or(logical_key) else {
        return true;
    };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii() => aliases.contains(&c),
        _ => true,
    }
}

/// Drop the repeats of a key that closed a Kettle modal. They belong to the
/// press the modal consumed, not to the terminal it uncovered; its release is
/// already suppressed with that press. The key's release, or a fresh press of
/// it, ends the suppression. Returns whether to drop this event.
pub(super) fn drop_closing_key_repeat(
    closing: &mut HashSet<PhysicalKey>,
    physical_key: PhysicalKey,
    state: ElementState,
    repeat: bool,
) -> bool {
    if !closing.contains(&physical_key) {
        return false;
    }
    if state == ElementState::Pressed && repeat {
        return true;
    }
    closing.remove(&physical_key);
    false
}

/// The Kettle modal that owns the keyboard, in the keyboard path's dispatch
/// order (the first open one gets the key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyModal {
    ConfirmDialog,
    ContextMenu,
    ViMode,
    Hint,
    Palette,
    ThemePicker,
    SettingsText,
    SettingsCapture,
    Settings,
    LayoutPicker,
    Ssh,
    TitleEdit,
    Search,
    /// A preview lane holding the keyboard (`focus_preview`).
    Preview,
}

pub(super) fn top_modal(ws: &WindowState) -> Option<KeyModal> {
    Some(if ws.confirm_dialog.is_some() {
        KeyModal::ConfirmDialog
    } else if ws.context_menu.is_some() {
        KeyModal::ContextMenu
    } else if ws.vi_mode.is_some() {
        KeyModal::ViMode
    } else if ws.hint_state.is_some() {
        KeyModal::Hint
    } else if ws.palette_input.is_some() {
        KeyModal::Palette
    } else if ws.theme_picker_input.is_some() {
        KeyModal::ThemePicker
    } else if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
        KeyModal::SettingsText
    } else if settings_capture_active(ws) {
        KeyModal::SettingsCapture
    } else if ws.settings_nav.is_some() {
        KeyModal::Settings
    } else if ws.layout_picker_input.is_some() {
        KeyModal::LayoutPicker
    } else if ws.ssh_input.is_some() {
        KeyModal::Ssh
    } else if ws.editing_title.is_some() {
        KeyModal::TitleEdit
    } else if ws.search.open {
        KeyModal::Search
    } else if ws.preview_focus.is_some() {
        KeyModal::Preview
    } else {
        return None;
    })
}

/// Record `key` as the one that closed or replaced the modal it was pressed
/// in, so its auto-repeats are dropped (`drop_closing_key_repeat`) instead of
/// reaching whatever the modal uncovered: the terminal, the parent Settings
/// under a closed text field, or the layout picker the palette just opened.
/// The modal's input-method composition ends with it, for the same reason.
pub(super) fn remember_closing_key(
    ws: &mut WindowState,
    key_modal: Option<KeyModal>,
    key: PhysicalKey,
) {
    if top_modal(ws) != key_modal {
        ws.closing_keys.insert(key);
        end_modal_composition(ws);
    }
}

/// A Kettle modal whose keys do not all repeat (see `modal_repeat_is_inert`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModalRepeat {
    ConfirmDialog,
    /// `vim_nav`: `vim-menu-nav` is on, so `j`/`k` and Ctrl+D/U move.
    ContextMenu {
        vim_nav: bool,
    },
    ViMode,
    Hint,
    Palette,
    /// `steps`: the focused row is a number, which Left/Right step. On a
    /// toggle or a choice they flip or cycle it and persist each step.
    Settings {
        steps: bool,
    },
}

/// Whether an auto-repeat of `key` must do nothing in `modal`. Keys that
/// activate, toggle or pick act once per press: holding the Enter that opened
/// a submenu would otherwise activate its first row, and holding Space on a
/// Settings toggle would flip it at the repeat rate. Keys that move keep
/// repeating.
pub(super) fn modal_repeat_is_inert(
    modal: ModalRepeat,
    repeat: bool,
    key: &Key,
    ctrl: bool,
) -> bool {
    if !repeat {
        return false;
    }
    let moves = |key: &Key| {
        matches!(
            key,
            Key::Named(
                NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::PageUp
                    | NamedKey::PageDown
                    | NamedKey::Tab
            )
        )
    };
    let letter = |key: &Key, letters: &[&str]| matches!(key, Key::Character(c) if letters.iter().any(|l| c.eq_ignore_ascii_case(l)));
    match modal {
        // Rows are also picked by mnemonic letters and vim `l`; only the
        // highlight moves repeat: arrows, and with vim-menu-nav `j`/`k` and
        // Ctrl+D/U half pages. Without it, `j` and `k` are mnemonics.
        ModalRepeat::ContextMenu { vim_nav } => {
            let vim_move = vim_nav
                && if ctrl {
                    letter(key, &["d", "u"])
                } else {
                    letter(key, &["j", "k"])
                };
            !(moves(key) || vim_move)
        }
        // A dialog's letters and Enter/Space answer it; focus moves repeat.
        ModalRepeat::ConfirmDialog => !moves(key),
        // The query is typed text; only Enter runs the selection.
        ModalRepeat::Palette => matches!(key, Key::Named(NamedKey::Enter)),
        // Enter and Space activate or toggle the focused row. Left/Right (and
        // vim `h`/`l`) step a number and repeat; on a toggle or a choice they
        // flip or cycle it, persisting each step, so act once per press.
        ModalRepeat::Settings { steps } => {
            matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space))
                || (!steps
                    && (matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight))
                        || letter(key, &["h", "l"])))
        }
        // `v` toggles visual mode; motions repeat.
        ModalRepeat::ViMode => letter(key, &["v"]),
        // Label letters only pick a target; nothing in hint mode moves.
        ModalRepeat::Hint => true,
    }
}

/// Whether the Settings overlay is waiting for a keybind chord.
pub(super) fn settings_capture_active(ws: &WindowState) -> bool {
    ws.settings_nav.as_ref().is_some_and(|nav| nav.capturing)
}

/// Whether an action flips a state, so auto-repeat of its held chord must not
/// run it again: the chord would otherwise flip the state back and forth for
/// as long as it is held, and leave it wherever the last repeat fell. Such a
/// repeat is still consumed, so it never reaches the program.
pub(super) fn toggles_once_per_press(action: &Action) -> bool {
    matches!(
        action,
        Action::ToggleViMode
            | Action::ToggleZoom
            | Action::ScaledZoom
            | Action::ToggleFullscreen
            | Action::ToggleBroadcastAll
            | Action::ToggleBroadcastGroup
            | Action::ToggleBroadcastOff
            | Action::ToggleBroadcastWindow
            | Action::ToggleCopyOnSelect
            | Action::ToggleCursorBlink
            | Action::ToggleMouseHide
            | Action::TogglePaneReadOnly
            | Action::ToggleScrollbar
            | Action::ToggleLightDark
            | Action::ToggleSessionLog
            | Action::ToggleGroupAll
            | Action::ToggleGroupTab
            | Action::ToggleGroupWindow
            | Action::ToggleWindowVisibility
            | Action::OpenMediaShelf
    )
}

/// Whether a key press is bound to toggling vi mode. Vi mode takes every key
/// before the keybind table is consulted, so without this check the chord
/// that entered it (`Ctrl+Shift+Space` by default) could not leave it.
pub(super) fn is_vi_mode_toggle_chord(
    bindings: &Bindings,
    logical_key: &Key,
    physical_key: &PhysicalKey,
    mods: ModifiersState,
    unshifted_key: Option<&Key>,
) -> bool {
    matches!(
        resolve_keybind_action(
            bindings,
            Some(logical_key),
            Some(physical_key),
            mods,
            unshifted_key
        ),
        Some((_, Action::ToggleViMode))
    )
}

pub(super) fn resolve_keybind_action(
    bindings: &Bindings,
    logical_key: Option<&Key>,
    physical_key: Option<&PhysicalKey>,
    mods: ModifiersState,
    unshifted_key: Option<&Key>,
) -> Option<(Trigger, Action)> {
    keybind_candidates(logical_key, physical_key, mods, unshifted_key)
        .into_iter()
        .find_map(|trigger| {
            bindings
                .get(&trigger)
                .cloned()
                .map(|action| (trigger, action))
        })
}

/// The non-macOS default `Alt+Arrow` focus chords are adaptive at the edge of
/// the visible split tree. When a visible neighbour exists, the keybind owns
/// the chord and moves focus. When no visible pane exists in that direction,
/// the original key event falls through to the PTY, preserving application
/// bindings such as Codex's `Alt+Left`/`Alt+Right` word motion and `Alt+Up`
/// previous-message editor.
///
/// "Visible" is the operative word: a zoomed pane (`Ctrl+Shift+X`, or the
/// `scaled_zoom` action) hides its siblings, so directional focus has nowhere to go
/// and the chord belongs to the program until the zoom is released. A Kettle
/// no-op there would swallow the press and its release for nothing.
///
/// Match the trigger and action as a pair. A user who deliberately binds
/// `Alt+Up` to some other action must get that action, not an implicit terminal
/// fallback, and actions invoked from menus or automation have no physical
/// chord to reinterpret. macOS keeps its explicit focus chords
/// (`Cmd+Opt+Arrow` and `Ctrl+Cmd+Arrow`) and leaves Option+Arrow entirely to
/// the terminal.
pub(super) fn adaptive_alt_focus_direction(
    trigger: Trigger,
    action: &Action,
    enabled: bool,
) -> Option<(i32, i32)> {
    if !enabled || trigger.mods != Mods::ALT {
        return None;
    }
    match (trigger.key, action) {
        (KKey::Up, Action::FocusUp) => Some((0, -1)),
        (KKey::Down, Action::FocusDown) => Some((0, 1)),
        (KKey::Left, Action::FocusLeft) => Some((-1, 0)),
        (KKey::Right, Action::FocusRight) => Some((1, 0)),
        _ => None,
    }
}

/// Whether an adaptive focus chord belongs to the terminal rather than to
/// Kettle. `visible_neighbor_exists` is the mux's answer for the chord's
/// direction; it is already `false` while zoom collapses the layout to the
/// focused pane, so no separate zoom flag is consulted.
pub(super) fn adaptive_alt_focus_falls_through(
    trigger: Trigger,
    action: &Action,
    enabled: bool,
    visible_neighbor_exists: bool,
) -> bool {
    !visible_neighbor_exists && adaptive_alt_focus_direction(trigger, action, enabled).is_some()
}

/// How a default chord shares its key with a program that owns the keyboard
/// (see `chord_goes_to_program`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProgramKeyRule {
    /// The program always gets it: Shift+Arrow split resize, which Codex
    /// (Shift+Left answers a queued question), Claude Code, Neovim, fish and
    /// every text field use too.
    ProgramFirst,
    /// The program gets it only when Kettle's action would do nothing (no
    /// prompt to jump to, nothing to scroll, one tab, no selection to extend)
    /// and the view is not scrolled into history. Copy has no rule: Codex
    /// reads Ctrl+Shift+C as Ctrl+C, which would discard a draft.
    WhenKettleIdle,
    /// As `WhenKettleIdle`, for pane-focus chords that have nowhere to go (a
    /// zoomed or single-pane tab, or no pane on that side), and only while the
    /// pane's negotiated keyboard mode tells the chord apart. Sent in plain
    /// legacy mode, Ctrl+Shift+N would arrive as Ctrl+N, a different key, and
    /// a Command chord would arrive as nothing at all.
    WhenKettleIdleAndDistinct,
}

/// The rule for this chord under the user's config: none with
/// `keybind-yield = off`, and none for a trigger the config binds itself, even
/// to its default action, so a chord the user bound is always Kettle's.
pub(super) fn configured_program_key_rule(
    cfg: &kettle_config::Config,
    trigger: Trigger,
    action: &Action,
    macos: bool,
) -> Option<ProgramKeyRule> {
    if cfg.keybind_yield == kettle_config::KeybindYield::Off
        || cfg.keybinds_declared.contains(&trigger)
    {
        return None;
    }
    program_key_rule(trigger, action, macos)
}

/// The default chords a program that owns the keyboard can have, matched as
/// exact (trigger, action) pairs, as `adaptive_alt_focus_direction` does: a
/// chord the user binds to anything, or an action the user moves to another
/// chord, is always Kettle's.
pub(super) fn program_key_rule(
    trigger: Trigger,
    action: &Action,
    macos: bool,
) -> Option<ProgramKeyRule> {
    use KKey::*;
    use ProgramKeyRule::*;

    let shift = Mods::SHIFT;
    let ctrl = Mods::CTRL;
    let ctrl_shift = Mods::CTRL | Mods::SHIFT;
    let rule = match (trigger.mods, trigger.key, action) {
        (m, Up, Action::ResizeUp)
        | (m, Down, Action::ResizeDown)
        | (m, Left, Action::ResizeLeft)
        | (m, Right, Action::ResizeRight)
            if m == shift =>
        {
            ProgramFirst
        }
        (m, Up, Action::JumpPrevPrompt) | (m, Down, Action::JumpNextPrompt)
            if m == ctrl || (macos && m == Mods::SUPER) =>
        {
            WhenKettleIdle
        }
        (m, PageUp, Action::ScrollPageUp)
        | (m, PageDown, Action::ScrollPageDown)
        | (m, Home, Action::SelectToTop)
        | (m, End, Action::SelectToBottom)
            if m == shift =>
        {
            WhenKettleIdle
        }
        (m, Up, Action::ScrollLineUp)
        | (m, Down, Action::ScrollLineDown)
        | (m, PageDown, Action::MoveTabRight)
        | (m, PageUp, Action::MoveTabLeft)
            if m == ctrl_shift =>
        {
            WhenKettleIdle
        }
        (m, Home, Action::ScrollToTop)
        | (m, End, Action::ScrollToBottom)
        | (m, PageDown, Action::NextTab)
        | (m, PageUp, Action::PrevTab)
            if m == ctrl =>
        {
            WhenKettleIdle
        }
        (m, Char('n'), Action::FocusNext) | (m, Char('p'), Action::FocusPrev)
            if m == ctrl_shift =>
        {
            WhenKettleIdleAndDistinct
        }
        // macOS's two directional focus chords. Alt+Arrow elsewhere already
        // falls through when it has nowhere to go
        // (`adaptive_focus_chord_falls_through`).
        (m, Up, Action::FocusUp)
        | (m, Down, Action::FocusDown)
        | (m, Left, Action::FocusLeft)
        | (m, Right, Action::FocusRight)
            if macos && (m == Mods::ALT | Mods::SUPER || m == Mods::CTRL | Mods::SUPER) =>
        {
            WhenKettleIdleAndDistinct
        }
        // Alt+1-9 everywhere, and Cmd+1-9 on macOS.
        (m, Char(digit @ '1'..='9'), Action::GotoTab(index))
            if (m == Mods::ALT || (macos && m == Mods::SUPER))
                && u32::from(*index) + 1 == digit.to_digit(10).unwrap_or(0) =>
        {
            WhenKettleIdle
        }
        _ => return None,
    };
    Some(rule)
}

/// The window state that decides whether a Kettle action would do anything.
pub(super) struct KettleActionState {
    pub(super) tabs: usize,
    pub(super) current_tab: usize,
    /// The pane has a Kettle selection (even an empty one a click left), which
    /// `Shift+Home/End` would extend.
    pub(super) selection_exists: bool,
    /// A focus action has a visible pane to move to.
    pub(super) focus_target_exists: bool,
    /// The pane's keyboard mode encodes the chord as itself (see
    /// [`chord_reaches_program_distinctly`]).
    pub(super) chord_is_distinct: bool,
}

/// Whether Kettle's action would change nothing if it ran now, with the view
/// at the bottom (`program_gets_chord` never asks otherwise).
fn kettle_action_is_idle(
    action: &Action,
    claims: &kettle_core::KeyboardClaims,
    kettle: &KettleActionState,
) -> bool {
    let no_history = claims.alt_screen || claims.history_size == 0;
    match action {
        Action::JumpPrevPrompt => !claims.prompt_above,
        Action::JumpNextPrompt => !claims.prompt_below,
        Action::ScrollToTop | Action::ScrollPageUp | Action::ScrollLineUp => no_history,
        Action::ScrollToBottom | Action::ScrollPageDown | Action::ScrollLineDown => true,
        // With nothing selected, Kettle would start a selection at the
        // program's cursor; a program that owns the keyboard wants the key.
        Action::SelectToTop | Action::SelectToBottom => !kettle.selection_exists,
        Action::NextTab | Action::PrevTab | Action::MoveTabLeft | Action::MoveTabRight => {
            kettle.tabs < 2
        }
        Action::GotoTab(index) => {
            let index = usize::from(*index);
            index >= kettle.tabs || index == kettle.current_tab
        }
        Action::FocusUp
        | Action::FocusDown
        | Action::FocusLeft
        | Action::FocusRight
        | Action::FocusNext
        | Action::FocusPrev => !kettle.focus_target_exists,
        _ => false,
    }
}

/// Whether a program that owns the keyboard gets a chord with this rule.
///
/// A view scrolled into history keeps every `WhenKettleIdle` chord Kettle's:
/// a key that reached the program would snap the view back to the bottom
/// (`scroll-on-keystroke`), so the user reading scrollback would lose their
/// place at its top, and a held key would bounce between the two.
pub(super) fn program_gets_chord(
    rule: ProgramKeyRule,
    action: &Action,
    claims: &kettle_core::KeyboardClaims,
    kettle: &KettleActionState,
) -> bool {
    if !claims.program_owns_keyboard() {
        return false;
    }
    match rule {
        ProgramKeyRule::ProgramFirst => true,
        ProgramKeyRule::WhenKettleIdle => {
            claims.display_offset == 0 && kettle_action_is_idle(action, claims, kettle)
        }
        ProgramKeyRule::WhenKettleIdleAndDistinct => {
            kettle.chord_is_distinct
                && claims.display_offset == 0
                && kettle_action_is_idle(action, claims, kettle)
        }
    }
}

/// Whether the pane's keyboard mode sends this chord as a key of its own: it
/// encodes to bytes, and those bytes differ from the same key with any one of
/// its modifiers released. Plain legacy mode sends Ctrl+Shift+N exactly as
/// Ctrl+N and a Command chord as nothing, so a program could only mistake the
/// one for another key or never see the other.
pub(super) fn chord_reaches_program_distinctly(
    trigger: Trigger,
    mode: kettle_core::TermMode,
) -> bool {
    // The press below carries no text. A real Ctrl+Shift+N does, its Control
    // character, and with kitty flags that neither disambiguate nor report all
    // keys (event types alone, say) that text sends it down the legacy path as
    // Ctrl+N. Only those two flags build the kitty sequence for a character
    // key whatever its text, so require one of them.
    if matches!(trigger.key, KKey::Char(_))
        && mode.intersects(kettle_core::TermMode::KITTY_KEYBOARD_PROTOCOL)
        && !mode.intersects(
            kettle_core::TermMode::DISAMBIGUATE_ESC_CODES
                | kettle_core::TermMode::REPORT_ALL_KEYS_AS_ESC,
        )
    {
        return false;
    }
    let Some(key) = chord_key(trigger.key) else {
        return false;
    };
    let mods = chord_modifiers(trigger.mods);
    let Some(bytes) = crate::input::encode_key_press(&key, mods, mode) else {
        return false;
    };
    [
        ModifiersState::SHIFT,
        ModifiersState::CONTROL,
        ModifiersState::ALT,
        ModifiersState::SUPER,
    ]
    .into_iter()
    .filter(|m| mods.contains(*m))
    .all(|m| crate::input::encode_key_press(&key, mods - m, mode).as_deref() != Some(&bytes[..]))
}

/// The winit key for the keys a [`ProgramKeyRule::WhenKettleIdleAndDistinct`]
/// chord uses. A shifted letter is the lowercase character with Shift held, as
/// winit reports it once the modifier is applied by the keybind lookup.
fn chord_key(key: KKey) -> Option<Key> {
    Some(match key {
        KKey::Up => Key::Named(NamedKey::ArrowUp),
        KKey::Down => Key::Named(NamedKey::ArrowDown),
        KKey::Left => Key::Named(NamedKey::ArrowLeft),
        KKey::Right => Key::Named(NamedKey::ArrowRight),
        KKey::Char(c) => Key::Character(c.to_string().into()),
        _ => return None,
    })
}

fn chord_modifiers(m: Mods) -> ModifiersState {
    let mut out = ModifiersState::empty();
    out.set(ModifiersState::SHIFT, m.contains(Mods::SHIFT));
    out.set(ModifiersState::CONTROL, m.contains(Mods::CTRL));
    out.set(ModifiersState::ALT, m.contains(Mods::ALT));
    out.set(ModifiersState::SUPER, m.contains(Mods::SUPER));
    out
}

pub(super) fn parse_ctl_mods(
    value: Option<&serde_json::Value>,
) -> std::result::Result<ModifiersState, String> {
    let mut mods = ModifiersState::empty();
    let Some(value) = value else {
        return Ok(mods);
    };
    let parts: Vec<String> = if let Some(s) = value.as_str() {
        if s.trim().is_empty() {
            Vec::new()
        } else {
            s.split('+').map(|p| p.trim().to_string()).collect()
        }
    } else if let Some(arr) = value.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(s) = item.as_str() else {
                return Err("'mods' array entries must be strings".to_string());
            };
            out.push(s.to_string());
        }
        out
    } else {
        return Err("'mods' must be a string like 'ctrl+shift' or an array".to_string());
    };
    for part in parts {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "alt" | "opt" | "option" => mods |= ModifiersState::ALT,
            "shift" => mods |= ModifiersState::SHIFT,
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => {
                mods |= ModifiersState::SUPER;
            }
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(mods)
}

pub(super) fn parse_ctl_logical_key(
    value: Option<&serde_json::Value>,
) -> std::result::Result<Option<Key>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(token) = value.as_str() else {
        return Err("'logical' must be a key token string".to_string());
    };
    if matches!(
        token.trim().to_ascii_lowercase().as_str(),
        "" | "none" | "unidentified"
    ) {
        return Ok(None);
    }
    let Some((mods, key)) = parse_send_key(token) else {
        return Err(format!("unrecognized logical key token '{token}'"));
    };
    if !mods.is_empty() {
        return Err("'logical' must be one key token without modifiers".to_string());
    }
    Ok(Some(key))
}

pub(super) fn parse_ctl_physical_key(
    value: Option<&serde_json::Value>,
) -> std::result::Result<Option<PhysicalKey>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return Err("'physical' must be a key-code string".to_string());
    };
    let normalized: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if matches!(normalized.as_str(), "" | "none" | "unidentified") {
        return Ok(None);
    }
    let code = match normalized.as_str() {
        "equal" | "equals" => KeyCode::Equal,
        "minus" => KeyCode::Minus,
        "digit0" | "key0" | "0" => KeyCode::Digit0,
        "numpadadd" | "numpadplus" => KeyCode::NumpadAdd,
        "numpadsubtract" | "numpadminus" => KeyCode::NumpadSubtract,
        "numpadequal" | "numpadequals" => KeyCode::NumpadEqual,
        "numpad0" => KeyCode::Numpad0,
        _ => return Err(format!("unsupported physical key code '{raw}'")),
    };
    Ok(Some(PhysicalKey::Code(code)))
}

pub(super) fn to_kkey(key: &Key) -> Option<KKey> {
    Some(match key {
        Key::Character(s) => KKey::Char(s.chars().next()?.to_ascii_lowercase()),
        Key::Named(n) => match n {
            NamedKey::ArrowUp => KKey::Up,
            NamedKey::ArrowDown => KKey::Down,
            NamedKey::ArrowLeft => KKey::Left,
            NamedKey::ArrowRight => KKey::Right,
            NamedKey::PageUp => KKey::PageUp,
            NamedKey::PageDown => KKey::PageDown,
            NamedKey::Home => KKey::Home,
            NamedKey::End => KKey::End,
            NamedKey::Enter => KKey::Enter,
            NamedKey::Tab => KKey::Tab,
            // winit reports the space bar as a named key, never as the
            // character " ", so without this the `ctrl+shift+space` vi-mode
            // default, and any `space` chord in a config, never matched.
            NamedKey::Space => KKey::Char(' '),
            // Without these two the `cmd+backspace` default parses out of the
            // config and then never matches a real key press.
            NamedKey::Backspace => KKey::Backspace,
            NamedKey::Delete => KKey::Delete,
            NamedKey::F1 => KKey::F(1),
            NamedKey::F2 => KKey::F(2),
            NamedKey::F3 => KKey::F(3),
            NamedKey::F4 => KKey::F(4),
            NamedKey::F5 => KKey::F(5),
            NamedKey::F6 => KKey::F(6),
            NamedKey::F7 => KKey::F(7),
            NamedKey::F8 => KKey::F(8),
            NamedKey::F9 => KKey::F(9),
            NamedKey::F10 => KKey::F(10),
            NamedKey::F11 => KKey::F(11),
            NamedKey::F12 => KKey::F(12),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether a captured `(mods, key)` chord is safe to bind from the settings
/// keybind-capture overlay.
///
/// A modifier-LESS chord is rejected unless the key is an F-key. The global
/// key path matches keybinds before text encoding, so binding a bare `a` (a
/// mis-press during capture) would make every future `a` fire the action
/// instead of typing, across all panes, persisted across restarts, with no
/// in-overlay unbind. Enter/Tab/arrows are likewise essential unmodified, so
/// only F-keys (which produce no text) may be bound without a modifier.
pub(super) fn keybind_chord_is_safe(mods: Mods, key: KKey) -> bool {
    !mods.is_empty() || matches!(key, KKey::F(_))
}

pub(super) fn to_mods(m: ModifiersState) -> Mods {
    let mut out = Mods::empty();
    if m.shift_key() {
        out |= Mods::SHIFT;
    }
    if m.control_key() {
        out |= Mods::CTRL;
    }
    if m.alt_key() {
        out |= Mods::ALT;
    }
    if m.super_key() {
        out |= Mods::SUPER;
    }
    out
}

impl App {
    /// Deliver user-authored bytes with the same selection, cursor, broadcast,
    /// read-only, and scroll behavior for keyboard and IME commit paths.
    pub(super) fn write_terminal_input(
        &mut self,
        ws: &mut WindowState,
        bytes: &[u8],
    ) -> PaneInputResult {
        self.sync_pane_focus_reports(ws);
        if bytes.is_empty() {
            return PaneInputResult::Queued;
        }
        self.clear_selection_on_input(ws);
        self.reset_blink_phase(ws);
        self.hide_mouse_cursor(ws);
        // Queued behind a paste still being prepared, it is delivered in
        // order when that paste is.
        if self.defer_terminal_input(ws, None, bytes, true, false) {
            return PaneInputResult::Queued;
        }
        let input_time = std::time::Instant::now();
        let result = if ws.mux.is_broadcast_on() {
            self.broadcast_input(ws, bytes, self.cfg.scroll_on_keystroke, input_time)
        } else if let Some(pane_id) = ws.mux.active_focus() {
            let Some(pane) = ws.mux.panes.get(&pane_id) else {
                return PaneInputResult::Queued;
            };
            let result = pane.feed_input(bytes);
            if result.is_queued() {
                ws.last_typed = Some(input_time);
            }
            if result.is_queued()
                && self.cfg.scroll_on_keystroke
                && let Ok(mut term) = pane.term.term.lock()
            {
                term.scroll_display(Scroll::Bottom);
            }
            Self::dismiss_media_paste_receipt_after_input(ws, pane_id, result);
            result
        } else {
            PaneInputResult::Queued
        };
        self.report_input_result(result);
        result
    }

    pub(super) fn send_or_defer_key_releases(
        &mut self,
        ws: &mut WindowState,
        releases: Vec<(u64, PhysicalKey, TerminalKeyRelease)>,
    ) {
        let mut releases = releases;
        if !paste::defer_key_releases(&mut self.queued_pastes, &mut releases) {
            self.send_key_releases(Some(ws), &releases);
        }
    }

    pub(super) fn send_key_releases(
        &mut self,
        ws: Option<&WindowState>,
        releases: &[(u64, PhysicalKey, TerminalKeyRelease)],
    ) {
        let mut result = PaneInputResult::Queued;
        for mux in ws
            .map(|ws| &ws.mux)
            .into_iter()
            .chain(self.windows.values().map(|ws| &ws.mux))
        {
            result = result.merge(Self::release_keys_to(mux, releases));
        }
        self.report_input_result(result);
    }

    pub(super) fn release_keys_to(
        mux: &Mux,
        releases: &[(u64, PhysicalKey, TerminalKeyRelease)],
    ) -> PaneInputResult {
        let mut result = PaneInputResult::Queued;
        for (id, key, release) in releases {
            if release
                .press_accepted
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                result = result.merge(mux.release_key_to(&[(*id, *key, release.bytes.clone())]));
            }
        }
        result
    }

    pub(super) fn write_terminal_key_event(
        &mut self,
        ws: &mut WindowState,
        event: &winit::event::KeyEvent,
        prepare_input: bool,
    ) {
        if !prepare_input {
            let key = event.physical_key;
            ws.terminal_key_recipients.remove(&key);
            let mut releases = Vec::new();
            ws.terminal_key_release_bytes.retain(|(kept, pane), bytes| {
                if *kept == key {
                    releases.push((*pane, key, bytes.clone()));
                    false
                } else {
                    true
                }
            });
            self.send_or_defer_key_releases(ws, releases);
            return;
        }
        if ws.terminal_key_recipients.len() >= MAX_TERMINAL_OWNED_KEYS
            && !ws.terminal_key_recipients.contains_key(&event.physical_key)
        {
            self.report_input_result(PaneInputResult::Backpressured);
            return;
        }
        if self.defer_terminal_input(ws, Some(event), &[], prepare_input, false) {
            if prepare_input {
                self.clear_selection_on_input(ws);
                self.reset_blink_phase(ws);
                self.hide_mouse_cursor(ws);
            }
            return;
        }
        self.sync_pane_focus_reports(ws);
        let mods = pty_key_modifiers(ws, &event.logical_key);
        let modified_enter = key_is_modified_enter(&event.logical_key, mods);
        if !ws.mux.is_broadcast_on() {
            let mode = ws
                .mux
                .focused()
                .map(|pane| pane.effective_key_mode(self.cfg.modify_other_keys, modified_enter))
                .unwrap_or_else(kettle_core::TermMode::empty);
            let Some(bytes) = encode_key_event_for_mode(&self.cfg, event, mods, mode) else {
                return;
            };
            let result = self.write_terminal_input(ws, &bytes);
            if result.is_queued()
                && !bytes.is_empty()
                && let Some(pane_id) = ws.mux.active_focus()
            {
                if let Some(release) = key_release_bytes(&self.cfg, event, mods, mode) {
                    ws.terminal_key_release_bytes
                        .entry((event.physical_key, pane_id))
                        .and_modify(|release| release.mark_accepted())
                        .or_insert_with(|| TerminalKeyRelease::new(release, true));
                }
                ws.terminal_key_recipients
                    .entry(event.physical_key)
                    .or_default()
                    .insert(pane_id);
            }
            return;
        }

        if prepare_input {
            self.clear_selection_on_input(ws);
            self.reset_blink_phase(ws);
            self.hide_mouse_cursor(ws);
        }
        let input_time = prepare_input.then(std::time::Instant::now);
        let scroll_to_bottom = prepare_input && self.cfg.scroll_on_keystroke;
        let scope = ws.mux.broadcast.clone();
        let cfg = &self.cfg;
        let receipt_pane = if prepare_input {
            Self::media_paste_receipt_owners(ws)
        } else {
            Default::default()
        };
        let mut release_bytes = std::collections::HashMap::new();
        let (mut delivery, recipients) = ws.mux.broadcast_encoded(
            self.cfg.modify_other_keys,
            modified_enter,
            scroll_to_bottom,
            receipt_pane,
            |id, mode| {
                if let Some(release) = key_release_bytes(cfg, event, mods, mode) {
                    release_bytes.insert(id, release);
                }
                encode_key_event_for_mode(cfg, event, mods, mode)
            },
        );
        for id in &recipients {
            if let Some(bytes) = release_bytes.remove(id) {
                ws.terminal_key_release_bytes
                    .entry((event.physical_key, *id))
                    .and_modify(|release| release.mark_accepted())
                    .or_insert_with(|| TerminalKeyRelease::new(bytes, true));
            }
        }
        ws.terminal_key_recipients
            .entry(event.physical_key)
            .or_default()
            .extend(recipients);
        Self::dismiss_media_paste_receipt_after_delivery(ws, delivery);
        if let Some(input_time) = input_time {
            stamp_accepted_input(&mut ws.last_typed, delivery, input_time);
        }
        if Mux::scope_crosses_windows(&scope) {
            for other in self.windows.values_mut() {
                let receipt_pane = if prepare_input {
                    Self::media_paste_receipt_owners(other)
                } else {
                    Default::default()
                };
                let (foreign, recipients) = other.mux.broadcast_encoded_foreign(
                    &scope,
                    self.cfg.modify_other_keys,
                    modified_enter,
                    scroll_to_bottom,
                    receipt_pane,
                    |id, mode| {
                        if let Some(release) = key_release_bytes(cfg, event, mods, mode) {
                            release_bytes.insert(id, release);
                        }
                        encode_key_event_for_mode(cfg, event, mods, mode)
                    },
                );
                for id in &recipients {
                    if let Some(bytes) = release_bytes.remove(id) {
                        ws.terminal_key_release_bytes
                            .entry((event.physical_key, *id))
                            .and_modify(|release| release.mark_accepted())
                            .or_insert_with(|| TerminalKeyRelease::new(bytes, true));
                    }
                }
                ws.terminal_key_recipients
                    .entry(event.physical_key)
                    .or_default()
                    .extend(recipients);
                if scroll_to_bottom
                    && foreign.accepted
                    && let Some(window) = &other.window
                {
                    window.request_redraw();
                }
                Self::dismiss_media_paste_receipt_after_delivery(other, foreign);
                if let Some(input_time) = input_time {
                    stamp_accepted_input(&mut other.last_typed, foreign, input_time);
                }
                delivery.result = delivery.result.merge(foreign.result);
                delivery.accepted |= foreign.accepted;
            }
        }
        self.report_input_result(delivery.result);
    }

    /// The one routing decision shared by real keyboard input and the
    /// `dispatch_keybind` control route: does this resolved chord belong to
    /// the terminal instead of the matched action? Only the non-macOS default
    /// `Alt+Arrow` focus chords are adaptive; they fall through when the mux
    /// has no visible pane in that direction, which includes every zoomed
    /// multi-pane tab because zoom collapses the layout to the focused pane.
    pub(super) fn adaptive_focus_chord_falls_through(
        &self,
        ws: &WindowState,
        trigger: Trigger,
        action: &Action,
    ) -> bool {
        // A trigger the config binds itself, even to its default action, is
        // always Kettle's, as for every program key rule.
        let enabled = !cfg!(target_os = "macos") && !self.cfg.keybinds_declared.contains(&trigger);
        adaptive_alt_focus_direction(trigger, action, enabled).is_some_and(|(dx, dy)| {
            adaptive_alt_focus_falls_through(
                trigger,
                action,
                true,
                ws.mux.pane_in_direction(self.area(ws), dx, dy).is_some(),
            )
        })
    }

    /// The routing decision the real keyboard and `dispatch_keybind` share: a
    /// matched chord goes to the terminal when it is an adaptive focus chord
    /// with nowhere to go, or when a program that owns the keyboard also uses
    /// it (see [`program_key_rule`]). The search bar keeps only the first
    /// rule: while it has the keyboard, the program cannot get the key, so a
    /// Kettle action is still better than nothing.
    pub(super) fn chord_falls_through(
        &self,
        ws: &WindowState,
        trigger: Trigger,
        action: &Action,
    ) -> bool {
        self.adaptive_focus_chord_falls_through(ws, trigger, action)
            || self.chord_goes_to_program(ws, trigger, action)
    }

    /// Whether a default chord that the focused pane's program also uses goes
    /// to that program: only default (trigger, action) pairs with a
    /// [`ProgramKeyRule`], only while `keybind-yield` is on, and only while a
    /// program, not an idle shell prompt, owns the pane's keyboard. Reads the
    /// pane's terminal only after the cheap pair match succeeds.
    fn chord_goes_to_program(&self, ws: &WindowState, trigger: Trigger, action: &Action) -> bool {
        let Some(rule) =
            configured_program_key_rule(&self.cfg, trigger, action, cfg!(target_os = "macos"))
        else {
            return false;
        };
        // Broadcast writes the key to every target in that target's own
        // keyboard mode, but only the focused pane is asked here, so a
        // broadcast chord stays Kettle's.
        if ws.mux.is_broadcast_on() {
            return false;
        }
        let Some(pane) = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id)) else {
            return false;
        };
        // A pane that cannot take input would drop the key.
        if pane.read_only || pane.pty_input_failed() {
            return false;
        }
        let claims = pane.term.keyboard_claims();
        if !claims.program_owns_keyboard() {
            return false;
        }
        let kettle = KettleActionState {
            tabs: ws.mux.tabs.len(),
            current_tab: ws.mux.active,
            selection_exists: matches!(action, Action::SelectToTop | Action::SelectToBottom)
                && pane
                    .term
                    .term
                    .lock()
                    .ok()
                    .is_some_and(|t| t.selection.is_some()),
            focus_target_exists: self.focus_target_exists(ws, action),
            chord_is_distinct: rule == ProgramKeyRule::WhenKettleIdleAndDistinct
                && pane
                    .term
                    .term
                    .lock()
                    .ok()
                    .is_some_and(|t| chord_reaches_program_distinctly(trigger, *t.mode())),
        };
        program_gets_chord(rule, action, &claims, &kettle)
    }

    /// Whether a focus action has a visible pane to move to. A zoomed tab
    /// shows only its focused pane, so it has none in any direction, as a
    /// single-pane tab has none.
    pub(super) fn focus_target_exists(&self, ws: &WindowState, action: &Action) -> bool {
        let direction = match action {
            Action::FocusUp => (0, -1),
            Action::FocusDown => (0, 1),
            Action::FocusLeft => (-1, 0),
            Action::FocusRight => (1, 0),
            Action::FocusNext | Action::FocusPrev => {
                return ws.mux.leaves(ws.mux.active, self.area(ws)).len() > 1;
            }
            _ => return false,
        };
        ws.mux
            .pane_in_direction(self.area(ws), direction.0, direction.1)
            .is_some()
    }
}
