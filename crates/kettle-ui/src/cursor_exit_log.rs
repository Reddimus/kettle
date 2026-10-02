//! Opt-in HC cursor-exit records. This observer never requests a frame.

use serde::Deserialize;
use winit::event::ElementState;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

const CONTRACT: &str = "cursor_exit_v1";
const MAX_LINE: usize = 4096;
const FIRST_KEY_SEQ: u64 = 7;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
struct Context {
    contract: String,
    launch_id: String,
    calibration_keys: u64,
    warmup: u64,
    keys: u64,
}

#[cfg(any(target_os = "macos", test))]
fn parse_context(bytes: &[u8]) -> Option<Context> {
    if bytes.len() > MAX_LINE || bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    let context: Context = serde_json::from_slice(bytes).ok()?;
    if context.contract != CONTRACT
        || context.launch_id.len() != 32
        || !context.launch_id.bytes().all(|b| b.is_ascii_hexdigit())
        || context.calibration_keys != 6
        || context.keys == 0
        || context
            .calibration_keys
            .checked_add(context.warmup)?
            .checked_add(context.keys)
            .is_none()
    {
        return None;
    }
    Some(context)
}

fn bounded_line(json: String) -> Option<String> {
    let line = format!("{CONTRACT} {json}\n");
    (line.len() <= MAX_LINE).then_some(line)
}

impl Context {
    fn capability(&self, pane_id: u64, window_id: u64) -> Option<String> {
        bounded_line(format!(
            "{{\"event\": \"capability\", \"launch_id\": \"{}\", \"pane_id\": {}, \"window_id\": {}, \"clock\": \"CLOCK_UPTIME_RAW\", \"first_key_seq\": {}}}",
            self.launch_id, pane_id, window_id, FIRST_KEY_SEQ,
        ))
    }

    /// A counted key that ended no layer blink. Without it, extra input
    /// would be invisible to the harness.
    fn input(&self, key: ExitKey) -> Option<String> {
        bounded_line(format!(
            "{{\"event\": \"input\", \"launch_id\": \"{}\", \"pane_id\": {}, \"key_seq\": {}}}",
            self.launch_id, key.pane_id, key.key_seq,
        ))
    }

    fn exit(&self, frame: ExitFrame, t_end_ns: u64) -> Option<String> {
        let elapsed = t_end_ns.checked_sub(frame.t_start_ns)?;
        // Integer div_ceil also handles durations at the u64 boundary.
        let total_frame_us = elapsed.div_ceil(1000);
        bounded_line(format!(
            "{{\"event\": \"exit\", \"launch_id\": \"{}\", \"pane_id\": {}, \"key_seq\": {}, \"t_start_ns\": {}, \"t_end_ns\": {}, \"total_frame_us\": {}, \"layer_active\": true}}",
            self.launch_id,
            frame.key.pane_id,
            frame.key.key_seq,
            frame.t_start_ns,
            t_end_ns,
            total_frame_us,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExitKey {
    pane_id: u64,
    key_seq: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExitFrame {
    key: ExitKey,
    t_start_ns: u64,
}

#[derive(Default)]
pub(crate) struct CursorExitLog {
    /// Cached once. Callers skip all observer work when false.
    pub(crate) enabled: bool,
    context: Option<Context>,
    target_seq: u64,
    bound: bool,
    key_seq: u64,
    pending: Option<ExitKey>,
    /// The last key that could end the layer blink and has not yet asked
    /// for its exit frame.
    unrequested: Option<ExitKey>,
    #[cfg(test)]
    emitted: Vec<String>,
}

impl CursorExitLog {
    pub(crate) fn from_env(target_seq: u64) -> Self {
        #[cfg(target_os = "macos")]
        {
            if let Some(path) = std::env::var_os("KETTLE_CURSOR_EXIT_CONTEXT")
                && log::log_enabled!(target: "kettle::cursor_blink", log::Level::Info)
                && let Some(context) = read_context(std::path::Path::new(&path))
            {
                return Self::with_context(context, target_seq);
            }
        }
        let _ = target_seq;
        Self::default()
    }

    #[cfg(any(target_os = "macos", test))]
    fn with_context(context: Context, target_seq: u64) -> Self {
        Self {
            enabled: true,
            context: Some(context),
            target_seq,
            ..Self::default()
        }
    }

    /// Bind once, after the launch pane exists and before event dispatch.
    pub(crate) fn bind(&mut self, seq: u64, pane_id: u64, window: &winit::window::Window) {
        if !self.bound
            && seq == self.target_seq
            && let Some(window_id) = native_window_number(window)
            && let Some(line) = self
                .context
                .as_ref()
                .and_then(|c| c.capability(pane_id, window_id))
        {
            // A short/failed write leaves incomplete evidence, never a new capability.
            self.bound = true;
            emit_line(&line);
        }
    }

    pub(crate) fn accepted_key(
        &mut self,
        seq: u64,
        pane_id: Option<u64>,
        state: ElementState,
        logical: &Key,
        physical: PhysicalKey,
        layer_active: bool,
    ) -> Option<ExitKey> {
        if !self.bound
            || seq != self.target_seq
            || state != ElementState::Pressed
            || is_modifier(logical, physical)
        {
            return None;
        }
        self.flush_unrequested();
        // Never cap counting at W+N: extra accepted input must remain visible.
        let Some(next) = self.key_seq.checked_add(1) else {
            self.pending = None;
            return None;
        };
        self.key_seq = next;
        // Pane 0 is no pane, so an unknown pane cannot match the capability.
        let key = ExitKey {
            pane_id: pane_id.unwrap_or(0),
            key_seq: next,
        };
        if !layer_active || next < FIRST_KEY_SEQ || pane_id.is_none() {
            // This key cannot end a layer blink: record it now.
            self.emit_input(key);
            return None;
        }
        self.unrequested = Some(key);
        Some(key)
    }

    pub(crate) fn request_exit(&mut self, key: Option<ExitKey>) {
        let Some(key) = key else {
            return;
        };
        if self.unrequested == Some(key) {
            self.unrequested = None;
        }
        // First requester owns a coalesced frame. A later key still consumes
        // its sequence; it cannot steal this exit or fabricate a second one,
        // so it is recorded as input.
        match self.pending {
            None => self.pending = Some(key),
            Some(owner) if owner == key => {}
            Some(_) => self.emit_input(key),
        }
    }

    /// Record the key that could have ended the layer blink but asked for
    /// no frame. Called before each new key and before the event loop waits.
    pub(crate) fn flush_unrequested(&mut self) {
        if let Some(key) = self.unrequested.take() {
            self.emit_input(key);
        }
    }

    fn emit_input(&mut self, key: ExitKey) {
        if let Some(line) = self.context.as_ref().and_then(|c| c.input(key)) {
            self.emit(line);
        }
    }

    fn emit(&mut self, line: String) {
        #[cfg(test)]
        self.emitted.push(line);
        #[cfg(not(test))]
        emit_line(&line);
    }

    pub(crate) fn begin_frame(
        &self,
        layer_active: bool,
        t_start_ns: Option<u64>,
    ) -> Option<ExitFrame> {
        if !layer_active {
            return None;
        }
        Some(ExitFrame {
            key: self.pending?,
            t_start_ns: t_start_ns?,
        })
    }

    pub(crate) fn finish_frame(
        &mut self,
        frame: Option<ExitFrame>,
        presented: bool,
        t_end_ns: Option<u64>,
    ) -> Option<String> {
        if !presented {
            return None;
        }
        let key = self.pending.take();
        // No deduplication by key or timestamp. If a caller submits the same
        // exit twice, keep both records so the consumer can refuse the stream.
        let line = frame
            .zip(t_end_ns)
            .and_then(|(frame, end)| self.context.as_ref()?.exit(frame, end));
        // A frame that cannot be timed still consumed the key: keep it visible.
        if line.is_none()
            && let Some(key) = key
        {
            self.emit_input(key);
        }
        line
    }

    /// The layer was hidden without a frame: a key that owned the exit, or
    /// that still could have asked for one, ended no layer blink.
    pub(crate) fn hidden(&mut self) {
        if let Some(key) = self.pending.take() {
            self.emit_input(key);
        }
        self.flush_unrequested();
    }
}

fn is_modifier(logical: &Key, physical: PhysicalKey) -> bool {
    matches!(
        logical,
        Key::Named(
            NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::Control
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Shift
                | NamedKey::Symbol
                | NamedKey::SymbolLock
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::Super
        )
    ) || matches!(
        physical,
        PhysicalKey::Code(
            KeyCode::AltLeft
                | KeyCode::AltRight
                | KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::SuperLeft
                | KeyCode::SuperRight
                | KeyCode::CapsLock
                | KeyCode::Fn
                | KeyCode::FnLock
                | KeyCode::NumLock
                | KeyCode::ScrollLock
                | KeyCode::Meta
                | KeyCode::Hyper
        )
    )
}

#[cfg(target_os = "macos")]
fn read_context(path: &std::path::Path) -> Option<Context> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    // Validate the opened descriptor, not a racy stat followed by open.
    if !private_context(
        metadata.is_file(),
        metadata.uid(),
        unsafe { libc::geteuid() },
        metadata.mode(),
        metadata.nlink(),
        metadata.len(),
    ) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take((MAX_LINE + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_context(&bytes)
}

#[cfg(any(target_os = "macos", test))]
fn private_context(regular: bool, owner: u32, euid: u32, mode: u32, links: u64, len: u64) -> bool {
    regular && owner == euid && mode & 0o077 == 0 && links == 1 && len <= MAX_LINE as u64
}

#[cfg(target_os = "macos")]
pub(crate) fn raw_now_ns() -> Option<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_UPTIME_RAW excludes sleep, matching HC's keyblock and clock checks.
    if unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut time) } != 0 {
        return None;
    }
    let secs = u64::try_from(time.tv_sec).ok()?;
    let nanos = u64::try_from(time.tv_nsec).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn raw_now_ns() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn native_window_number(window: &winit::window::Window) -> Option<u64> {
    use objc2_app_kit::NSView;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // Called only on winit's main thread; winit owns this view and its window.
    let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    let number = unsafe { view.window()?.windowNumber() };
    u64::try_from(number).ok()
}

#[cfg(not(target_os = "macos"))]
fn native_window_number(_window: &winit::window::Window) -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
pub(crate) fn emit_line(line: &str) {
    // stderr's shared lock prevents logger threads splitting this line. One
    // write also preserves pipe atomicity: the fixed schema is below 512 bytes.
    // Never write a short-write suffix as a second, apparently complete line.
    let stderr = std::io::stderr();
    let _lock = stderr.lock();
    loop {
        let written = unsafe { libc::write(libc::STDERR_FILENO, line.as_ptr().cast(), line.len()) };
        if written >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            break;
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn emit_line(_line: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: &str = r#"{"contract":"cursor_exit_v1","launch_id":"0123456789abcdef0123456789abcdef","calibration_keys":6,"warmup":1,"keys":3}"#;
    const CAPABILITY: &str = "cursor_exit_v1 {\"event\": \"capability\", \"launch_id\": \"0123456789abcdef0123456789abcdef\", \"pane_id\": 2, \"window_id\": 9, \"clock\": \"CLOCK_UPTIME_RAW\", \"first_key_seq\": 7}\n";
    const EXIT: &str = "cursor_exit_v1 {\"event\": \"exit\", \"launch_id\": \"0123456789abcdef0123456789abcdef\", \"pane_id\": 2, \"key_seq\": 7, \"t_start_ns\": 1000003000, \"t_end_ns\": 1009003000, \"total_frame_us\": 9000, \"layer_active\": true}\n";

    fn context() -> Context {
        parse_context(CONTEXT.as_bytes()).unwrap()
    }

    fn bound() -> CursorExitLog {
        let mut log = CursorExitLog::with_context(context(), 1);
        // The native bind emits once in production. Pure tests need no window.
        log.bound = true;
        log
    }

    fn key(log: &mut CursorExitLog, pane: u64, active: bool, requests_frame: bool) {
        let ticket = log.accepted_key(
            1,
            Some(pane),
            ElementState::Pressed,
            &Key::Character("j".into()),
            PhysicalKey::Code(KeyCode::KeyJ),
            active,
        );
        if requests_frame {
            log.request_exit(ticket);
        }
    }

    fn calibrated() -> CursorExitLog {
        let mut log = bound();
        for _ in 0..6 {
            key(&mut log, 2, false, true);
        }
        log
    }

    #[test]
    fn context_accepts_exact_fields() {
        let parsed = context();
        assert_eq!(parsed.contract, CONTRACT);
        assert_eq!(parsed.launch_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(
            (parsed.calibration_keys, parsed.warmup, parsed.keys),
            (6, 1, 3)
        );
        assert!(parse_context(CONTEXT.replace("abcdef", "ABCDEF").as_bytes()).is_some());
        assert!(
            parse_context(CONTEXT.replace("\"warmup\":1", "\"warmup\":0").as_bytes()).is_some()
        );
    }

    #[test]
    fn context_refuses_other_fields_types_and_values() {
        let mut bad = vec![
            CONTEXT.replace("\"keys\":3", "\"keys\":3,\"extra\":0"),
            CONTEXT.replace("\"keys\":3", "\"keys\":3,\"keys\":3"),
            CONTEXT.replace("cursor_exit_v1", "cursor_exit_v2"),
            CONTEXT.replace(
                "0123456789abcdef0123456789abcdef",
                "g123456789abcdef0123456789abcdef",
            ),
            CONTEXT.replace("0123456789abcdef0123456789abcdef", "1234"),
            CONTEXT.replace("\"calibration_keys\":6", "\"calibration_keys\":5"),
            CONTEXT.replace("\"keys\":3", "\"keys\":0"),
            CONTEXT.replace("\"warmup\":1", "\"warmup\":18446744073709551615"),
            format!("{CONTEXT} {{}}"),
            r#"["cursor_exit_v1","0123456789abcdef0123456789abcdef",6,1,3]"#.into(),
            "[]".into(),
            "null".into(),
            "{bad}".into(),
        ];
        for field in ["contract", "launch_id"] {
            let original = if field == "contract" {
                CONTRACT
            } else {
                "0123456789abcdef0123456789abcdef"
            };
            for value in ["true", "null", "32", "[]", "{}"] {
                bad.push(CONTEXT.replace(
                    &format!("\"{field}\":\"{original}\""),
                    &format!("\"{field}\":{value}"),
                ));
            }
        }
        for field in ["calibration_keys", "warmup", "keys"] {
            let original = match field {
                "calibration_keys" => 6,
                "warmup" => 1,
                _ => 3,
            };
            for value in [
                "true",
                "false",
                "null",
                "-1",
                "1.0",
                "\"1\"",
                "18446744073709551616",
            ] {
                bad.push(CONTEXT.replace(
                    &format!("\"{field}\":{original}"),
                    &format!("\"{field}\":{value}"),
                ));
            }
        }
        for field in [
            "contract",
            "launch_id",
            "calibration_keys",
            "warmup",
            "keys",
        ] {
            let mut value: serde_json::Value = serde_json::from_str(CONTEXT).unwrap();
            value.as_object_mut().unwrap().remove(field);
            bad.push(value.to_string());
        }
        bad.push(format!("{CONTEXT}{}", " ".repeat(MAX_LINE)));
        for bytes in bad {
            assert!(parse_context(bytes.as_bytes()).is_none(), "{bytes}");
        }
        assert!(parse_context(&[0xff]).is_none());
    }

    #[test]
    fn private_context_refuses_public_foreign_linked_or_unbounded_files() {
        assert!(private_context(true, 501, 501, 0o600, 1, 100));
        assert!(private_context(true, 501, 501, 0o400, 1, 100));
        for args in [
            (false, 501, 501, 0o600, 1, 100),
            (true, 502, 501, 0o600, 1, 100),
            (true, 501, 501, 0o640, 1, 100),
            (true, 501, 501, 0o604, 1, 100),
            (true, 501, 501, 0o600, 2, 100),
            (true, 501, 501, 0o600, 1, 4097),
        ] {
            assert!(!private_context(
                args.0, args.1, args.2, args.3, args.4, args.5
            ));
        }
    }

    #[test]
    fn capability_line_matches_hc_fixture_bytes() {
        assert_eq!(
            context().capability(2, 9).unwrap().as_bytes(),
            CAPABILITY.as_bytes()
        );
    }

    #[test]
    fn exit_line_matches_hc_fixture_bytes_and_ceiling() {
        let frame = ExitFrame {
            key: ExitKey {
                pane_id: 2,
                key_seq: 7,
            },
            t_start_ns: 1_000_003_000,
        };
        assert_eq!(
            context().exit(frame, 1_009_003_000).unwrap().as_bytes(),
            EXIT.as_bytes()
        );
        for (delta, expected) in [
            (0, 0),
            (1, 1),
            (999, 1),
            (1000, 1),
            (1001, 2),
            (u64::MAX, u64::MAX / 1000 + 1),
        ] {
            let line = context()
                .exit(
                    ExitFrame {
                        t_start_ns: 0,
                        ..frame
                    },
                    delta,
                )
                .unwrap();
            let json: serde_json::Value =
                serde_json::from_str(line.strip_prefix("cursor_exit_v1 ").unwrap()).unwrap();
            assert_eq!(json["total_frame_us"].as_u64(), Some(expected));
        }
        assert!(context().exit(frame, frame.t_start_ns - 1).is_none());
    }

    #[test]
    fn line_cap_includes_prefix_and_lf() {
        let payload_size = 4096 - CONTRACT.len() - 2;
        assert_eq!(bounded_line("x".repeat(payload_size)).unwrap().len(), 4096);
        assert!(bounded_line("x".repeat(payload_size + 1)).is_none());
        let frame = ExitFrame {
            key: ExitKey {
                pane_id: u64::MAX,
                key_seq: u64::MAX,
            },
            t_start_ns: 0,
        };
        assert!(context().exit(frame, u64::MAX).unwrap().len() <= 512);
        assert!(context().capability(u64::MAX, u64::MAX).unwrap().len() <= 512);
    }

    #[test]
    fn accepted_key_counts_calibration_repeats_extra_keys_and_actual_panes() {
        let mut log = bound();
        for n in 1..=6 {
            key(&mut log, 2, true, true);
            assert_eq!(log.key_seq, n);
            assert!(log.begin_frame(true, Some(10)).is_none());
        }
        key(&mut log, 2, true, true);
        let first = log.begin_frame(true, Some(10)).unwrap();
        assert_eq!(
            first.key,
            ExitKey {
                pane_id: 2,
                key_seq: 7
            }
        );
        // Two accepted events, including repeats, consume two sequences.
        key(&mut log, 4, true, true);
        assert_eq!(log.key_seq, 8);
        assert_eq!(log.begin_frame(true, Some(10)).unwrap().key, first.key);
        log.finish_frame(Some(first), true, Some(20)).unwrap();
        for _ in 0..3 {
            key(&mut log, 4, false, true);
        }
        key(&mut log, 4, true, true);
        assert_eq!(
            log.begin_frame(true, Some(30)).unwrap().key,
            ExitKey {
                pane_id: 4,
                key_seq: 12
            }
        );
    }

    #[test]
    fn releases_modifiers_and_other_windows_do_not_count() {
        let mut log = calibrated();
        let j = Key::Character("j".into());
        let physical = PhysicalKey::Code(KeyCode::KeyJ);
        assert!(
            log.accepted_key(1, Some(2), ElementState::Released, &j, physical, true)
                .is_none()
        );
        assert!(
            log.accepted_key(2, Some(2), ElementState::Pressed, &j, physical, true)
                .is_none()
        );
        for named in [
            NamedKey::Alt,
            NamedKey::AltGraph,
            NamedKey::CapsLock,
            NamedKey::Control,
            NamedKey::Fn,
            NamedKey::FnLock,
            NamedKey::NumLock,
            NamedKey::ScrollLock,
            NamedKey::Shift,
            NamedKey::Symbol,
            NamedKey::SymbolLock,
            NamedKey::Meta,
            NamedKey::Hyper,
            NamedKey::Super,
        ] {
            assert!(
                log.accepted_key(
                    1,
                    Some(2),
                    ElementState::Pressed,
                    &Key::Named(named),
                    physical,
                    true
                )
                .is_none()
            );
        }
        for code in [
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
            KeyCode::SuperLeft,
            KeyCode::SuperRight,
            KeyCode::CapsLock,
            KeyCode::Fn,
            KeyCode::FnLock,
            KeyCode::NumLock,
            KeyCode::ScrollLock,
            KeyCode::Meta,
            KeyCode::Hyper,
        ] {
            assert!(
                log.accepted_key(
                    1,
                    Some(2),
                    ElementState::Pressed,
                    &j,
                    PhysicalKey::Code(code),
                    true
                )
                .is_none()
            );
        }
        assert_eq!(log.key_seq, 6);
        assert!(log.pending.is_none());
        // An actual key with a control encoding still counts. PTY/control-server
        // bytes have no accepted_key call, pinned by the routing source guard.
        let control = Key::Character("\u{3}".into());
        assert_eq!(
            log.accepted_key(1, Some(2), ElementState::Pressed, &control, physical, true)
                .unwrap()
                .key_seq,
            7
        );
    }

    fn input_line(pane: u64, seq: u64) -> String {
        format!(
            "cursor_exit_v1 {{\"event\": \"input\", \"launch_id\": \"0123456789abcdef0123456789abcdef\", \"pane_id\": {pane}, \"key_seq\": {seq}}}\n"
        )
    }

    #[test]
    fn calibration_keys_are_input_records_with_hc_bytes() {
        let log = calibrated();
        let expected: Vec<String> = (1..=6).map(|seq| input_line(2, seq)).collect();
        assert_eq!(log.emitted, expected);
    }

    #[test]
    fn a_key_that_ends_no_layer_blink_is_an_input_record() {
        let mut log = calibrated();
        key(&mut log, 2, false, true);
        assert_eq!(log.emitted.last(), Some(&input_line(2, 7)));
        // A later handoff cannot turn inactive input into a key-owned exit.
        assert!(log.begin_frame(true, Some(10)).is_none());
        assert!(
            log.finish_frame(log.begin_frame(false, Some(10)), true, Some(20))
                .is_none()
        );
        // An eligible key that asks for no frame is recorded at the next flush.
        key(&mut log, 2, true, false);
        assert_eq!(log.emitted.len(), 7);
        assert!(
            log.finish_frame(log.begin_frame(true, Some(10)), true, Some(20))
                .is_none()
        );
        log.flush_unrequested();
        assert_eq!(log.emitted.last(), Some(&input_line(2, 8)));
        assert_eq!(log.key_seq, 8);
        // The next key also flushes an unrequested one.
        key(&mut log, 2, true, false);
        key(&mut log, 2, false, false);
        assert_eq!(log.emitted[8..], [input_line(2, 9), input_line(2, 10)]);
    }

    #[test]
    fn extra_input_after_the_final_exit_stays_visible() {
        let mut log = calibrated();
        key(&mut log, 2, true, true);
        let line = log.finish_frame(log.begin_frame(true, Some(1_000)), true, Some(3_000));
        assert!(line.is_some_and(|line| line.contains("\"key_seq\": 7")));
        // Cmd+C with no selection after the last measured exit: no layer, no
        // exit, but the key is counted and recorded.
        let ticket = log.accepted_key(
            1,
            Some(2),
            ElementState::Pressed,
            &Key::Character("c".into()),
            PhysicalKey::Code(KeyCode::KeyC),
            false,
        );
        assert!(ticket.is_none());
        assert_eq!(log.emitted.last(), Some(&input_line(2, 8)));
        // Six calibration inputs and this one: key 7's exit is not repeated
        // as an input.
        assert_eq!(log.emitted.len(), 7);
    }

    #[test]
    fn coalesced_cancelled_and_untimed_keys_are_input_records() {
        let mut log = calibrated();
        key(&mut log, 2, true, true);
        let owner = log.pending;
        // A second key coalesced into the same frame cannot own the exit.
        key(&mut log, 2, true, true);
        assert_eq!(log.emitted.last(), Some(&input_line(2, 8)));
        // Asking again for the owner's exit adds nothing.
        log.request_exit(owner);
        assert_eq!(log.emitted.len(), 7);
        // A hide without a frame cancels the owner's exit.
        log.hidden();
        assert_eq!(log.emitted.last(), Some(&input_line(2, 7)));
        assert!(log.pending.is_none());
        // A presented frame that cannot be timed still consumed its key.
        key(&mut log, 2, true, true);
        assert!(
            log.finish_frame(log.begin_frame(true, None), true, Some(5))
                .is_none()
        );
        assert_eq!(log.emitted.last(), Some(&input_line(2, 9)));
    }

    #[test]
    fn failed_frames_retry_the_same_key_and_hide_cancels_the_join() {
        let mut log = calibrated();
        key(&mut log, 2, true, true);
        let first = log.begin_frame(true, Some(10)).unwrap();
        assert!(log.finish_frame(Some(first), false, Some(20)).is_none());
        let retry = log.begin_frame(true, Some(30)).unwrap();
        assert_eq!(retry.key, first.key);
        let line = log.finish_frame(Some(retry), true, Some(40)).unwrap();
        assert!(line.contains("\"t_start_ns\": 30"));
        assert!(log.begin_frame(true, Some(50)).is_none());
        key(&mut log, 2, true, true);
        log.hidden();
        assert!(log.begin_frame(true, Some(60)).is_none());
    }

    #[test]
    fn duplicate_exit_submissions_are_preserved() {
        let mut log = calibrated();
        key(&mut log, 2, true, true);
        let frame = log.begin_frame(true, Some(10));
        let first = log.finish_frame(frame, true, Some(20)).unwrap();
        let duplicate = log.finish_frame(frame, true, Some(20)).unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(format!("{first}{duplicate}").lines().count(), 2);
    }

    fn app_source() -> &'static str {
        include_str!("app.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap()
    }

    #[test]
    fn source_guard_brackets_the_complete_exit_frame() {
        let redraw = app_source()
            .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
            .unwrap()
            .1
            .split_once("fn build_status_bar(")
            .unwrap()
            .0;
        let start = redraw.find("crate::cursor_exit_log::raw_now_ns()").unwrap();
        let materialize = redraw.find("Self::materialize_layer_blink(").unwrap();
        let snapshots = redraw.find("let panes: Vec<PaneView>").unwrap();
        let begin = redraw.find("layer.begin_exit_frame();").unwrap();
        let render = redraw
            .find("renderer.render_frame_with_status_and_pre_present(")
            .unwrap();
        let hide = redraw.find("layer.hide();").unwrap();
        let commit = redraw.find("layer.end_exit_frame();").unwrap();
        let end = redraw
            .rfind("crate::cursor_exit_log::raw_now_ns()")
            .unwrap();
        let format = redraw.find("ws.cursor_exit_log.finish_frame(").unwrap();
        let write = redraw
            .find("crate::cursor_exit_log::emit_line(&line);")
            .unwrap();
        assert!(start < materialize && materialize < snapshots && snapshots < begin);
        assert!(begin < render && render < hide && hide < commit && commit < end);
        assert!(end < format && format < write);
        assert_eq!(
            redraw
                .matches("crate::cursor_exit_log::raw_now_ns()")
                .count(),
            2
        );
        assert!(redraw.contains("cursor_exit_frame.is_some() && layer_exit_presented"));
        let layer = include_str!("macos_cursor_layer.rs");
        let close = layer
            .split_once("pub(super) fn end_exit_frame(&self) {")
            .unwrap()
            .1
            .split_once("\n        }")
            .unwrap()
            .0;
        assert!(
            close.find("CATransaction::commit();").unwrap()
                < close.find("CATransaction::flush();").unwrap()
        );
        let clock = include_str!("cursor_exit_log.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        assert!(clock.contains("libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut time)"));
    }

    #[test]
    fn source_guard_flushes_unrequested_keys_before_the_loop_waits() {
        let app = app_source();
        let body = app
            .split_once("fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {")
            .expect("about_to_wait")
            .1;
        let flush = body
            .find("ws.cursor_exit_log.flush_unrequested();")
            .expect("about_to_wait flushes unrequested keys");
        assert!(
            flush
                < body
                    .find("let torn_tick_wait")
                    .expect("rest of about_to_wait")
        );
    }

    #[test]
    fn source_guard_counts_only_routed_keys_and_binds_before_the_first_frame() {
        let source = app_source();
        assert_eq!(source.matches(".accepted_key(").count(), 1);
        assert_eq!(source.matches("CursorExitLog::from_env(1)").count(), 1);
        let keyboard = source
            .split_once("WindowEvent::KeyboardInput { event, .. } => {")
            .unwrap()
            .1
            .split_once("WindowEvent::RedrawRequested =>")
            .unwrap()
            .0;
        let count = keyboard.find("ws.cursor_exit_log.accepted_key(").unwrap();
        assert!(
            keyboard
                .find("event.state == ElementState::Released")
                .unwrap()
                < count
        );
        assert!(keyboard.find("if ws.ime_preedit.is_some()").unwrap() < count);
        assert!(count < keyboard.find("dev_record_key(").unwrap());
        assert!(count < keyboard.find("self.reset_blink_phase(ws);").unwrap());
        assert!(keyboard[..count].contains("let cursor_exit_key = if ws.cursor_exit_log.enabled"));
        assert_eq!(
            keyboard.matches(".request_exit(cursor_exit_key);").count(),
            3
        );
        let resumed = source
            .split_once("fn resumed_inner(")
            .unwrap()
            .1
            .split_once("fn handle_accessibility_action(")
            .unwrap()
            .0;
        let bind = resumed
            .find("ws.cursor_exit_log.bind(ws.seq, pane_id, window);")
            .unwrap();
        assert!(bind < resumed.find("self.redraw(ws);").unwrap());
        assert!(resumed[..bind].contains("ws.mux.tabs.get(ws.mux.active).map(|tab| tab.focus)"));
        let module = include_str!("cursor_exit_log.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        assert!(
            module
                .contains("log::log_enabled!(target: \"kettle::cursor_blink\", log::Level::Info)")
        );
        assert!(module.contains("libc::O_NOFOLLOW | libc::O_NONBLOCK"));
        assert!(
            module.contains("libc::write(libc::STDERR_FILENO, line.as_ptr().cast(), line.len())")
        );
        assert!(module.contains("if !self.bound"));
    }
}
