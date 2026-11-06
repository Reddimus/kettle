//! Global hotkeys with scoped native registrations; no accessibility permission.
use crate::app::UserEvent;
use kettle_config::keybinds::Trigger;
#[cfg(any(unix, test))]
use kettle_config::keybinds::{Key, Mods};
use winit::event_loop::EventLoopProxy;

// Carbon virtual key codes are physical US positions. X11 uses keysyms and
// resolves them against the server's current keyboard mapping at registration.
#[cfg(any(target_os = "macos", test))]
fn carbon_trigger(mut trigger: Trigger) -> Trigger {
    if let Key::Char(symbol) = trigger.key
        && let Some(base) = "~!@#$%^&*()_+{}|:\"<>?"
            .chars()
            .zip("`1234567890-=[]\\;',./".chars())
            .find_map(|(shifted, base)| (symbol == shifted).then_some(base))
    {
        trigger.key = Key::Char(base);
        trigger.mods |= Mods::SHIFT;
    }
    trigger
}

#[cfg(any(target_os = "macos", test))]
fn carbon_key(key: Key) -> Option<u32> {
    Some(match key {
        Key::Escape => 53,
        Key::Char('`') => 50,
        Key::Char(c) => {
            [
                ('a', 0),
                ('s', 1),
                ('d', 2),
                ('f', 3),
                ('h', 4),
                ('g', 5),
                ('z', 6),
                ('x', 7),
                ('c', 8),
                ('v', 9),
                ('b', 11),
                ('q', 12),
                ('w', 13),
                ('e', 14),
                ('r', 15),
                ('y', 16),
                ('t', 17),
                ('1', 18),
                ('2', 19),
                ('3', 20),
                ('4', 21),
                ('6', 22),
                ('5', 23),
                ('=', 24),
                ('9', 25),
                ('7', 26),
                ('-', 27),
                ('8', 28),
                ('0', 29),
                (']', 30),
                ('o', 31),
                ('u', 32),
                ('[', 33),
                ('i', 34),
                ('p', 35),
                ('l', 37),
                ('j', 38),
                ('\'', 39),
                ('k', 40),
                (';', 41),
                ('\\', 42),
                (',', 43),
                ('/', 44),
                ('n', 45),
                ('m', 46),
                ('.', 47),
                (' ', 49),
            ]
            .into_iter()
            .find(|(ch, _)| *ch == c)?
            .1
        }
        Key::Enter => 36,
        Key::Tab => 48,
        Key::Backspace => 51,
        Key::Delete => 117,
        Key::Left => 123,
        Key::Right => 124,
        Key::Down => 125,
        Key::Up => 126,
        Key::Home => 115,
        Key::End => 119,
        Key::PageUp => 116,
        Key::PageDown => 121,
        Key::F(n) => *[122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111]
            .get(usize::from(n).checked_sub(1)?)?,
    })
}

#[cfg(any(target_os = "macos", test))]
fn carbon_mods(mods: Mods) -> u32 {
    let mut value = 0;
    for (modifier, bit) in [
        (Mods::SUPER, 1 << 8),
        (Mods::SHIFT, 1 << 9),
        (Mods::ALT, 1 << 11),
        (Mods::CTRL, 1 << 12),
    ] {
        if mods.contains(modifier) {
            value |= bit;
        }
    }
    value
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn x11_keysym(key: Key) -> Option<u32> {
    Some(match key {
        Key::Char(c) if c.is_ascii() => c as u32,
        Key::Escape => 0xff1b,
        Key::Enter => 0xff0d,
        Key::Tab => 0xff09,
        Key::Backspace => 0xff08,
        Key::Delete => 0xffff,
        Key::Home => 0xff50,
        Key::Left => 0xff51,
        Key::Up => 0xff52,
        Key::Right => 0xff53,
        Key::Down => 0xff54,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::End => 0xff57,
        Key::F(n @ 1..=12) => 0xffbd + u32::from(n),
        _ => return None,
    })
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn x11_keycode(
    first: u8,
    per_code: u8,
    symbols: &[u32],
    key: Key,
    level3: Option<u16>,
    mode_switch: Option<u16>,
) -> Option<(u8, u16)> {
    if per_code == 0 {
        return None;
    }
    let symbol = x11_keysym(key)?;
    for (offset, keys) in symbols.chunks_exact(usize::from(per_code)).enumerate() {
        for (column, &candidate) in keys.iter().enumerate() {
            if candidate != symbol {
                continue;
            }
            // Core X11 exposes two groups' base/Shift pairs first. XKB
            // appends their Level3/Level4 pairs after those four columns.
            // Legacy four-column maps use Mode_switch (or ISO_Level3) alone.
            let selector = match column / 2 {
                0 => Some(0),
                1 => mode_switch.or(level3),
                2 => level3,
                3 => mode_switch.zip(level3).map(|(group, level)| group | level),
                _ => None,
            };
            if let Some(selector) = selector {
                let code = first.checked_add(u8::try_from(offset).ok()?)?;
                return Some((code, selector | (column as u16 & 1)));
            }
        }
    }
    None
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn x11_modifier_mask(
    first: u8,
    per_code: u8,
    symbols: &[u32],
    per_modifier: u8,
    modifiers: &[u8],
    symbol: u32,
) -> Option<u16> {
    if per_code == 0 || per_modifier == 0 {
        return None;
    }
    modifiers
        .chunks_exact(usize::from(per_modifier))
        .take(8)
        .enumerate()
        .find_map(|(index, codes)| {
            codes
                .iter()
                .any(|code| {
                    *code != 0
                        && code.checked_sub(first).is_some_and(|offset| {
                            symbols
                                .chunks_exact(usize::from(per_code))
                                .nth(usize::from(offset))
                                .is_some_and(|keys| keys.contains(&symbol))
                        })
                })
                .then_some(1 << index)
        })
}

#[cfg(target_os = "macos")]
pub(crate) use macos::Registration;
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) use x11::Registration;
#[cfg(not(any(unix, target_os = "macos")))]
pub(crate) struct Registration;

pub(crate) fn register(
    trigger: Trigger,
    proxy: EventLoopProxy<UserEvent>,
    wayland: bool,
) -> Result<Option<Registration>, String> {
    if wayland || cfg!(windows) {
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log::info!(
                "quick-terminal-hotkey is not registered on Wayland or Windows; bind `kettle quick-terminal` in desktop shortcut settings"
            );
        }
        return Ok(None);
    }
    #[cfg(any(target_os = "macos", unix))]
    {
        Registration::new(trigger, proxy).map(Some)
    }
    #[cfg(not(any(target_os = "macos", unix)))]
    {
        let _ = (trigger, proxy);
        Ok(None)
    }
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
#[derive(Default)]
struct PressState {
    held: bool,
    released_at: Option<u32>,
}
#[cfg(any(all(unix, not(target_os = "macos")), test))]
impl PressState {
    fn press(&mut self, time: u32) -> bool {
        let repeat = self.held && self.released_at.is_none_or(|released| released == time);
        self.held = true;
        self.released_at = None;
        !repeat
    }
    fn release(&mut self, time: u32) {
        self.released_at = Some(time);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::cell::Cell;
    use std::ffi::c_void;
    type Ref = *mut c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct HotKeyId {
        signature: u32,
        id: u32,
    }
    #[repr(C)]
    struct EventType {
        class: u32,
        kind: u32,
    }
    const SIGNATURE: u32 = u32::from_be_bytes(*b"Kttl");
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> Ref;
        fn GetEventKind(event: Ref) -> u32;
        fn InstallEventHandler(
            target: Ref,
            callback: unsafe extern "C" fn(Ref, Ref, Ref) -> i32,
            count: u32,
            types: *const EventType,
            data: Ref,
            handler: *mut Ref,
        ) -> i32;
        fn RemoveEventHandler(handler: Ref) -> i32;
        fn RegisterEventHotKey(
            key: u32,
            mods: u32,
            id: HotKeyId,
            target: Ref,
            options: u32,
            result: *mut Ref,
        ) -> i32;
        fn UnregisterEventHotKey(hotkey: Ref) -> i32;
        fn GetEventParameter(
            event: Ref,
            name: u32,
            kind: u32,
            actual_kind: *mut u32,
            size: u32,
            actual_size: *mut u32,
            data: Ref,
        ) -> i32;
    }
    struct Callback {
        proxy: EventLoopProxy<UserEvent>,
        held: Cell<bool>,
    }
    unsafe extern "C" fn pressed(_: Ref, event: Ref, data: Ref) -> i32 {
        let mut id = HotKeyId {
            signature: 0,
            id: 0,
        };
        // SAFETY: Carbon supplies a live event; the output buffer matches typeEventHotKeyID.
        let status = unsafe {
            GetEventParameter(
                event,
                u32::from_be_bytes(*b"----"),
                u32::from_be_bytes(*b"hkid"),
                std::ptr::null_mut(),
                std::mem::size_of::<HotKeyId>() as u32,
                std::ptr::null_mut(),
                (&mut id as *mut HotKeyId).cast(),
            )
        };
        if status != 0 || id.signature != SIGNATURE || id.id != 1 {
            return -9874;
        }
        // SAFETY: Registration owns this stable Box until the handler is removed.
        let callback = unsafe { &*data.cast::<Callback>() };
        // SAFETY: Carbon supplied a live EventRef to this callback.
        if unsafe { GetEventKind(event) } == 9 {
            callback.held.set(false);
        } else if !callback.held.replace(true) {
            let _ = callback.proxy.send_event(UserEvent::ToggleQuickTerminal);
        }
        0
    }
    pub(crate) struct Registration {
        key: Ref,
        handler: Ref,
        _proxy: Box<Callback>,
    }
    impl Registration {
        pub fn new(trigger: Trigger, proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
            let trigger = carbon_trigger(trigger);
            let code = carbon_key(trigger.key).ok_or("unsupported Carbon hotkey")?;
            let mut proxy = Box::new(Callback {
                proxy,
                held: Cell::new(false),
            });
            let mut handler = std::ptr::null_mut();
            let mut key = std::ptr::null_mut();
            let events = [
                EventType {
                    class: u32::from_be_bytes(*b"keyb"),
                    kind: 6,
                },
                EventType {
                    class: u32::from_be_bytes(*b"keyb"),
                    kind: 9,
                },
            ];
            // SAFETY: called on the UI thread, with stable callback data and correctly sized output pointers.
            let status = unsafe {
                InstallEventHandler(
                    GetApplicationEventTarget(),
                    pressed,
                    2,
                    events.as_ptr(),
                    (&mut *proxy as *mut Callback).cast(),
                    &mut handler,
                )
            };
            if status != 0 {
                return Err(format!("InstallEventHandler: {status}"));
            }
            // SAFETY: valid key/modifier codes and application target; output is held for RAII cleanup.
            let status = unsafe {
                RegisterEventHotKey(
                    code,
                    carbon_mods(trigger.mods),
                    HotKeyId {
                        signature: SIGNATURE,
                        id: 1,
                    },
                    GetApplicationEventTarget(),
                    0,
                    &mut key,
                )
            };
            if status != 0 {
                // SAFETY: installation succeeded; no callback can run after removal on this thread.
                unsafe {
                    RemoveEventHandler(handler);
                }
                return Err(format!("RegisterEventHotKey: {status}"));
            }
            Ok(Self {
                key,
                handler,
                _proxy: proxy,
            })
        }
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: these handles belong to this registration, dropped on the same UI thread.
            unsafe {
                UnregisterEventHotKey(self.key);
                RemoveEventHandler(self.handler);
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use super::*;
    use std::io::Write as _;
    use std::os::{fd::AsRawFd as _, unix::net::UnixStream};
    use x11rb::{
        connection::Connection as _,
        protocol::{Event, xproto::*},
    };
    pub(crate) struct Registration {
        shutdown: UnixStream,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Registration {
        pub fn new(trigger: Trigger, proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
            let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
            let setup = conn.setup();
            let first = setup.min_keycode;
            let mapping = conn
                .get_keyboard_mapping(
                    first,
                    setup
                        .max_keycode
                        .checked_sub(first)
                        .and_then(|range| range.checked_add(1))
                        .ok_or("invalid X11 keyboard range")?,
                )
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?;
            if mapping.keysyms_per_keycode == 0 {
                return Err("empty X11 keyboard mapping".into());
            }
            let modifier_map = conn
                .get_modifier_mapping()
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?;
            if modifier_map.keycodes_per_modifier() == 0 {
                return Err("empty X11 modifier mapping".into());
            }
            let modifier_mask = |symbol| {
                x11_modifier_mask(
                    first,
                    mapping.keysyms_per_keycode,
                    &mapping.keysyms,
                    modifier_map.keycodes_per_modifier(),
                    &modifier_map.keycodes,
                    symbol,
                )
            };
            let (code, required_mods) = x11_keycode(
                first,
                mapping.keysyms_per_keycode,
                &mapping.keysyms,
                trigger.key,
                modifier_mask(0xfe03), // ISO_Level3_Shift
                modifier_mask(0xff7e), // Mode_switch
            )
            .ok_or("key is absent from the X11 keyboard map")?;
            let mut mods = ModMask::from(required_mods);
            for (modifier, mask) in [
                (Mods::CTRL, ModMask::CONTROL),
                (Mods::SHIFT, ModMask::SHIFT),
                (Mods::ALT, ModMask::M1),
                (Mods::SUPER, ModMask::M4),
            ] {
                if trigger.mods.contains(modifier) {
                    mods |= mask;
                }
            }
            // NumLock is not fixed to Mod2 on every keyboard map.
            let num_codes: Vec<u8> = mapping
                .keysyms
                .chunks(usize::from(mapping.keysyms_per_keycode))
                .enumerate()
                .filter(|(_, syms)| syms.contains(&0xff7f))
                .map(|(i, _)| first + i as u8)
                .collect();
            let mut ignored = u16::from(ModMask::LOCK);
            for (i, codes) in modifier_map
                .keycodes
                .chunks(usize::from(modifier_map.keycodes_per_modifier()))
                .enumerate()
            {
                if codes.iter().any(|code| num_codes.contains(code)) {
                    ignored |= 1 << i;
                }
            }
            for root in &setup.roots {
                for locks in 0..=255u16 {
                    if locks & !ignored == 0 {
                        conn.grab_key(
                            false,
                            root.root,
                            mods | ModMask::from(locks),
                            code,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                        )
                        .map_err(|e| e.to_string())?
                        .check()
                        .map_err(|e| e.to_string())?;
                    }
                }
            }
            conn.flush().map_err(|e| e.to_string())?;
            let (shutdown, listener) = UnixStream::pair().map_err(|e| e.to_string())?;
            let worker = std::thread::Builder::new()
                .name("quick-hotkey".into())
                .spawn(move || {
                    let mut state = PressState::default();
                    loop {
                        match conn.poll_for_event() {
                            Ok(Some(Event::KeyPress(event))) if event.detail == code => {
                                if state.press(event.time)
                                    && proxy.send_event(UserEvent::ToggleQuickTerminal).is_err()
                                {
                                    break;
                                }
                            }
                            Ok(Some(Event::KeyRelease(event))) if event.detail == code => {
                                state.release(event.time)
                            }
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                let mut fds = [
                                    libc::pollfd {
                                        fd: conn.stream().as_raw_fd(),
                                        events: libc::POLLIN,
                                        revents: 0,
                                    },
                                    libc::pollfd {
                                        fd: listener.as_raw_fd(),
                                        events: libc::POLLIN,
                                        revents: 0,
                                    },
                                ];
                                // SAFETY: both fds remain owned for this loop; poll writes only these two pollfd entries.
                                let ready = unsafe {
                                    libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1)
                                };
                                if ready < 0 {
                                    if std::io::Error::last_os_error().kind()
                                        == std::io::ErrorKind::Interrupted
                                    {
                                        continue;
                                    }
                                    break;
                                }
                                if fds[1].revents != 0
                                    || fds[0].revents
                                        & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                                        != 0
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    // Closing this connection releases every passive grab, including partial registrations.
                })
                .map_err(|e| e.to_string())?;
            Ok(Self {
                shutdown,
                worker: Some(worker),
            })
        }
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            let _ = self.shutdown.write_all(&[1]);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_s4_x11_german_unshifted_plus() {
        let trigger = kettle_config::parse_quick_terminal_hotkey("ctrl+plus").unwrap();
        assert_eq!(trigger.key, Key::Char('+'));
        assert_eq!(trigger.mods, Mods::CTRL);
        assert_eq!(
            x11_keycode(20, 4, &[43, 42, 126, 0], trigger.key, Some(1 << 7), None),
            Some((20, 0))
        );
    }

    #[test]
    fn review_s4_x11_us_shifted_tilde() {
        assert_eq!(
            x11_keycode(49, 2, &[96, 126], Key::Char('~'), None, None),
            Some((49, 1))
        );
    }

    #[test]
    fn review_s4_x11_level3_uses_live_modifier_mapping() {
        // Level3 is Mod3 here, rather than the commonly assumed Mod5.
        let symbols = [43, 42, 126, 124, 0xfe03, 0xfe03, 0, 0];
        let modifiers = [0, 0, 0, 0, 0, 21, 0, 0];
        let level3 = x11_modifier_mask(20, 4, &symbols, 1, &modifiers, 0xfe03);
        assert_eq!(level3, Some(1 << 5));
        assert_eq!(
            x11_keycode(20, 4, &symbols, Key::Char('~'), level3, None),
            Some((20, 1 << 5))
        );
        assert_eq!(
            x11_keycode(20, 4, &symbols, Key::Char('|'), level3, None),
            Some((20, (1 << 5) | 1))
        );
        assert_eq!(
            x11_keycode(20, 4, &symbols, Key::Char('~'), None, None),
            None
        );
        assert_eq!(
            x11_keycode(20, 0, &symbols, Key::Char('~'), level3, None),
            None
        );
        assert_eq!(x11_modifier_mask(20, 4, &symbols, 0, &[], 0xfe03), None);

        // XKB core replies also include duplicate base/Shift columns before
        // Level3/Level4. Both groups and every column must be considered.
        let symbols = [43, 42, 43, 42, 126, 124, 64, 35];
        for (symbol, expected) in [
            ('~', 1 << 5),
            ('|', (1 << 5) | 1),
            ('@', (1 << 5) | (1 << 7)),
            ('#', (1 << 5) | (1 << 7) | 1),
        ] {
            assert_eq!(
                x11_keycode(20, 8, &symbols, Key::Char(symbol), level3, Some(1 << 7)),
                Some((20, expected))
            );
        }
    }

    #[test]
    fn review_quick_captured_shifted_punctuation_maps_to_both_native_backends() {
        use crate::settings::{HotkeyCapture, capture_hotkey};
        for (symbol, base) in "~!@#$%^&*()_+{}|:\"<>?"
            .chars()
            .zip("`1234567890-=[]\\;',./".chars())
        {
            let HotkeyCapture::Value(Some(trigger)) =
                capture_hotkey(Some(Key::Char(symbol)), Mods::CTRL | Mods::SHIFT)
            else {
                panic!("capture failed")
            };
            assert_eq!(trigger.key, Key::Char(symbol), "captured {symbol}");
            assert_eq!(trigger.mods, Mods::CTRL | Mods::SHIFT);
            let carbon = carbon_trigger(trigger);
            assert_eq!(carbon.key, Key::Char(base));
            assert_eq!(carbon.mods, Mods::CTRL | Mods::SHIFT);
            assert_eq!(carbon_key(carbon.key), carbon_key(Key::Char(base)));
            assert!(carbon_key(carbon.key).is_some());
            let mapping = [base as u32, symbol as u32];
            assert_eq!(
                x11_keycode(49, 2, &mapping, trigger.key, None, None),
                Some((49, 1))
            );
            let parsed = kettle_config::parse_quick_terminal_hotkey(&trigger.label()).unwrap();
            assert_eq!(parsed, trigger);
        }
    }
    #[test]
    fn x11_repeat_pairs_do_not_retoggle_but_distinct_presses_do() {
        let mut state = PressState::default();
        assert!(state.press(100));
        assert!(!state.press(101));
        state.release(200);
        assert!(!state.press(200));
        state.release(250);
        assert!(state.press(251));
    }

    #[test]
    fn documented_hotkeys_map_to_native_codes() {
        let ctrl_grave = kettle_config::parse_quick_terminal_hotkey("ctrl+grave").unwrap();
        let cmd_escape = kettle_config::parse_quick_terminal_hotkey("cmd+escape").unwrap();
        assert_eq!(carbon_key(ctrl_grave.key), Some(50));
        assert_eq!(carbon_mods(ctrl_grave.mods), 1 << 12);
        assert_eq!(carbon_key(cmd_escape.key), Some(53));
        assert_eq!(carbon_mods(cmd_escape.mods), 1 << 8);
        assert_eq!(x11_keysym(ctrl_grave.key), Some(0x60));
        assert_eq!(x11_keysym(cmd_escape.key), Some(0xff1b));
        assert_eq!(x11_keysym(Key::F(12)), Some(0xffc9));
        let option_a = kettle_config::parse_quick_terminal_hotkey("alt+a").unwrap();
        assert_eq!(carbon_key(carbon_trigger(option_a).key), Some(0));
        assert_eq!(carbon_mods(option_a.mods), 1 << 11);
    }
}
