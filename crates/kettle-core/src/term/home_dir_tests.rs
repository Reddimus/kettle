use super::{LazySessionLogWriter, home_dir_fallback};
use crate::persistence::AsyncFileWriter;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

fn from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |k| {
        pairs
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| OsString::from(*v))
    }
}

#[test]
fn prefers_home_then_userprofile_then_appdata() {
    // All three set (rare — a WSL user with both env worlds bleeding
    // through) → HOME wins. This is the Linux / macOS branch.
    assert_eq!(
        home_dir_fallback(from(&[
            ("HOME", "/h"),
            ("USERPROFILE", r"C:\u"),
            ("APPDATA", r"C:\a"),
        ])),
        Some(PathBuf::from("/h")),
    );
    // No HOME → USERPROFILE. This is the *Windows* branch.
    assert_eq!(
        home_dir_fallback(from(&[("USERPROFILE", r"C:\u"), ("APPDATA", r"C:\a"),])),
        Some(PathBuf::from(r"C:\u")),
    );
    // Only APPDATA set (very stripped Windows session) → APPDATA.
    assert_eq!(
        home_dir_fallback(from(&[("APPDATA", r"C:\a")])),
        Some(PathBuf::from(r"C:\a")),
    );
    // Nothing set (minimal Linux container without HOME) → None;
    // caller leaves cmd.cwd() untouched.
    assert_eq!(home_dir_fallback(from(&[])), None);
}

#[test]
fn empty_env_var_value_falls_through_to_next() {
    // `HOME=""` (a deliberately empty env var, seen in stripped-down CI
    // containers and after a misconfigured `unset HOME` / `export HOME=`
    // in a parent shell) is treated as unset, so the probe continues to
    // the next variable instead of handing CommandBuilder::cwd("") an
    // invalid empty path. Pinned at every level of the chain.
    //
    // HOME empty, USERPROFILE valid → USERPROFILE wins.
    assert_eq!(
        home_dir_fallback(from(&[("HOME", ""), ("USERPROFILE", r"C:\u")])),
        Some(PathBuf::from(r"C:\u")),
    );
    // HOME empty, USERPROFILE empty, APPDATA valid → APPDATA wins.
    assert_eq!(
        home_dir_fallback(from(&[
            ("HOME", ""),
            ("USERPROFILE", ""),
            ("APPDATA", r"C:\a"),
        ])),
        Some(PathBuf::from(r"C:\a")),
    );
    // All three empty → None. Caller leaves cmd.cwd() untouched
    // rather than handing an empty path to the OS spawn.
    assert_eq!(
        home_dir_fallback(from(&[("HOME", ""), ("USERPROFILE", ""), ("APPDATA", ""),])),
        None,
    );
}

/// Drift guard. `strip_ansi_bytes` is the pure ANSI-
/// strip helper behind `log_strip_ansi`. Verify:
///   - CSI sequences (SGR / cursor moves / etc.) are removed
///   - OSC sequences (title, hyperlink, OSC 7) are removed,
///     terminated by either BEL or ESC\
///   - Single-char ESC (ESC c full-reset) is removed
///   - Plain printable bytes + newlines pass through
#[test]
fn strip_ansi_bytes_removes_csi_osc_and_single_esc() {
    use super::strip_ansi_bytes;
    // CSI SGR around plain text: "hello world".
    let s = b"\x1b[31mhello\x1b[0m world";
    assert_eq!(strip_ansi_bytes(s), b"hello world");
    // OSC 0 (set title) terminated by BEL.
    let s = b"prefix \x1b]0;my-title\x07 suffix";
    assert_eq!(strip_ansi_bytes(s), b"prefix  suffix");
    // OSC 8 (hyperlink) terminated by ESC\\.
    let s = b"\x1b]8;;http://example/\x1b\\link text\x1b]8;;\x1b\\";
    assert_eq!(strip_ansi_bytes(s), b"link text");
    // Single-char ESC (full reset).
    let s = b"\x1bcclean";
    assert_eq!(strip_ansi_bytes(s), b"clean");
    // Newlines + tabs pass through.
    let s = b"line1\nline2\tindent\n";
    assert_eq!(strip_ansi_bytes(s), b"line1\nline2\tindent\n");
    // Bare ESC at the very end of buffer is dropped (matches
    // the documented split-across-reads limitation).
    let s = b"trail\x1b";
    assert_eq!(strip_ansi_bytes(s), b"trail");
    // Plain ASCII passes through unchanged.
    let s = b"no escapes here";
    assert_eq!(strip_ansi_bytes(s), b"no escapes here");
}

#[test]
fn session_log_raw_publisher_only_uses_bounded_worker_admission() {
    let source = super::production_source();
    let publisher = source
        .split("private_output_filter.feed(&buffer[..n], tap_raw)")
        .nth(1)
        .and_then(|body| body.split("extractor.set_raw_tap(false);").next())
        .expect("per-read raw-output publisher");
    assert!(
        publisher.contains("writer.try_write(filtered)"),
        "the PTY reader must hand log chunks to the shared persistence worker"
    );
    assert!(
        !publisher.contains("write_all") && !publisher.contains(".flush("),
        "filesystem write and flush calls must stay off the PTY reader"
    );
}

#[test]
fn session_log_target_open_is_deferred_to_the_persistence_worker() {
    let temp = kettle_test_support::private_tempdir("kettle-session-log-test-");
    let path = temp.path().join("deferred-session.log");
    let sink = LazySessionLogWriter::new(path.clone());
    assert!(
        !path.exists(),
        "constructing the handoff must not touch the filesystem"
    );

    let mut writer = AsyncFileWriter::spawn("session-log-open-test", Box::new(sink)).unwrap();
    assert!(
        !path.exists(),
        "an idle persistence worker must not create the target before data arrives"
    );
    writer
        .try_write(b"native parser output\n".to_vec())
        .unwrap();
    assert!(
        writer.finish_with_timeout(Duration::from_secs(2)),
        "deferred private target failed with {:?}",
        writer.status()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"native parser output\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

/// The log stripper must agree with the terminal about where a sequence
/// ends, or the log and the screen disagree about what happened.
///
/// Three places they can diverge:
///   * CAN/SUB cancel a CSI. Otherwise the next ordinary character is
///     eaten as the final byte, and `ESC [ 31 CAN hello` logs `ello` while
///     the terminal renders `hello`.
///   * A non-ST `ESC` inside a control string aborts it. Otherwise
///     `ESC ^ payload ESC c visible` leaves the stripper inside the string,
///     swallowing everything after.
///   * `0x9c` is both the 8-bit ST and a UTF-8 continuation byte, so a
///     payload containing `末` (`e6 9c ab`) must not end at the middle of
///     that character and leak its tail into the log.
#[test]
fn the_log_stripper_ends_sequences_where_the_terminal_does() {
    for (label, input, want) in [
        ("CAN cancels a CSI", &b"\x1b[31\x18hello"[..], "hello"),
        ("SUB cancels a CSI", &b"\x1b[31\x1ahello"[..], "hello"),
        (
            "an escape intermediate consumes its final",
            &b"before\x1b(Bafter"[..],
            "beforeafter",
        ),
        (
            "ESC from CSI starts a fresh OSC",
            &b"\x1b[31\x1b]0;secret\x07visible"[..],
            "visible",
        ),
        (
            "raw ST terminates OSC",
            &b"\x1b]0;title\x9cvisible"[..],
            "visible",
        ),
        (
            "a non-ST ESC aborts OSC",
            &b"\x1b]0;title\x1bcvisible"[..],
            "visible",
        ),
        (
            "a non-ST ESC aborts a control string",
            &b"\x1b^payload\x1bcvisible"[..],
            "visible",
        ),
        (
            "CAN after that ESC still cancels",
            &b"\x1b^payload\x1b\x18visible"[..],
            "visible",
        ),
        (
            "0x9c inside a UTF-8 character is not ST",
            "\x1b_G payload \u{672b} more\x1b\\after".as_bytes(),
            "after",
        ),
        (
            "and the same in an OSC",
            "\x1b]0;title \u{672b} more\x07after".as_bytes(),
            "after",
        ),
    ] {
        let mut stripper = super::AnsiStripper::new();
        let out = stripper.strip(input);
        assert_eq!(
            String::from_utf8_lossy(&out),
            want,
            "{label}: the stripper must end the sequence where the terminal does"
        );
    }
}

#[test]
fn log_stripping_keeps_osc_state_across_escape_controls() {
    for control in [0, b'\n', b'\r', b'\t', 0x07, 0x1f, 0x7f] {
        for prefix in [
            b"before\x1b".as_slice(),
            b"before\x1b]old\x1b",
            b"before\x1bPold\x1b",
        ] {
            let mut wire = prefix.to_vec();
            wire.push(control);
            wire.extend_from_slice(b"]0;hidden\x07after");
            let mut expected = b"before".to_vec();
            if control != 0x7f {
                expected.push(control);
            }
            expected.extend_from_slice(b"after");
            for split in 0..=wire.len() {
                let mut stripper = super::AnsiStripper::new();
                let mut output = stripper.strip(&wire[..split]);
                output.extend(stripper.strip(&wire[split..]));
                assert_eq!(output, expected, "control={control}, split={split}");
            }
        }
    }
}

#[test]
fn session_log_keeps_executable_controls_inside_escape_sequences() {
    for (wire, expected) in [
        (b"A\x1b[31\r\n\tmB".as_slice(), b"A\r\n\tB".as_slice()),
        (b"A\x1b[31 \r\n\tmB", b"A\r\n\tB"),
        (b"A\x1b( \r\n\tBB", b"A\r\n\tB"),
        (b"A\x1b[31\r\n\t\x18B", b"A\r\n\tB"),
        (b"A\x1b( \r\n\t\x1aB", b"A\r\n\tB"),
    ] {
        for split in 0..=wire.len() {
            let mut filter = super::SessionLogFilter::default();
            let mut output = filter.filter(&wire[..split], 1, true);
            output.extend(filter.filter(&wire[split..], 1, true));
            assert_eq!(output, expected, "wire={wire:?}, split={split}");
        }
    }
}

#[test]
fn escaped_utf8_leads_do_not_emit_orphaned_continuations() {
    for scalar in [
        &b"\xc3\xa9"[..],
        &b"\xe2\x82\xac"[..],
        &b"\xf0\x9f\x99\x82"[..],
    ] {
        for split in 0..=scalar.len() {
            let mut input = b"head\x1b".to_vec();
            let scalar_start = input.len();
            input.extend_from_slice(scalar);
            input.extend_from_slice(b"tail");
            let mut stripper = super::AnsiStripper::new();
            let boundary = scalar_start + split;
            let mut out = stripper.strip(&input[..boundary]);
            out.extend(stripper.strip(&input[boundary..]));
            assert_eq!(
                std::str::from_utf8(&out),
                Ok("headtail"),
                "scalar {scalar:?}, split {split}: {out:?}"
            );
        }
    }
}

#[test]
fn session_log_filter_resets_between_sessions_and_strip_modes() {
    let mut filter = super::SessionLogFilter::default();
    assert_eq!(filter.filter(b"before\x1b]0;partial", 1, true), b"before");

    // Logging is inactive while the terminal consumes the BEL. A new log
    // must not inherit the old log's OSC state.
    assert_eq!(filter.filter(b"visible", 2, true), b"visible");

    // Changing raw/stripped mode is another parser-session boundary even
    // when the writer generation itself has not changed.
    assert_eq!(filter.filter(b"\x1b]0;partial", 2, true), b"");
    assert_eq!(filter.filter(b"raw", 2, false), b"raw");
    assert_eq!(filter.filter(b"visible", 2, true), b"visible");
}

/// Ordinary UTF-8 text must survive the stripper untouched — the log is
/// meant to be readable.
#[test]
fn utf8_text_passes_through_the_log_stripper_unharmed() {
    for text in ["末端", "┌─┐ ‘quoted’ Ünicode └─┘", "🦀 crab", "Ûh"] {
        let mut stripper = super::AnsiStripper::new();
        let out = stripper.strip(text.as_bytes());
        assert_eq!(
            String::from_utf8(out).as_deref(),
            Ok(text),
            "plain text must reach the log byte for byte"
        );
    }
    // And across every chunk split, since the log is fed one PTY read at
    // a time.
    let text = "末端 ▐ ‘q’";
    let bytes = text.as_bytes();
    for split in 1..bytes.len() {
        let mut stripper = super::AnsiStripper::new();
        let mut out = stripper.strip(&bytes[..split]);
        out.extend_from_slice(&stripper.strip(&bytes[split..]));
        assert_eq!(
            String::from_utf8(out).as_deref(),
            Ok(text),
            "split at {split} corrupted the text"
        );
    }
}

/// A session log must not accumulate image payloads.
///
/// DCS (`ESC P`, Sixel) and APC (`ESC _`, Kitty graphics) bodies must be
/// dropped whole, not just their two introducer bytes. Kitty payloads carry
/// encoded file paths and shared-memory names, and Sixel carries raw pixel
/// data, so logging the body would fill a readable transcript with binary
/// and path-bearing payloads.
#[test]
fn session_log_stripping_drops_image_payloads_not_just_their_introducers() {
    for (label, input, want) in [
        (
            "Sixel DCS",
            &b"before\x1bPq#0;2;0;0;0#0~~@@vv@@~~@@~~$\x1b\\after"[..],
            "beforeafter",
        ),
        (
            "Kitty APC with a file path",
            &b"before\x1b_Ga=T,f=100,t=f;L3RtcC9zZWNyZXQucG5n\x1b\\after"[..],
            "beforeafter",
        ),
        ("PM", &b"before\x1b^private\x1b\\after"[..], "beforeafter"),
        ("SOS", &b"before\x1bXstring\x1b\\after"[..], "beforeafter"),
        (
            "DCS closed by 8-bit ST",
            &b"before\x1bPpayload\x9cafter"[..],
            "beforeafter",
        ),
    ] {
        let mut stripper = super::AnsiStripper::new();
        let out = stripper.strip(input);
        assert_eq!(
            String::from_utf8_lossy(&out),
            want,
            "{label}: the payload must be dropped, not logged"
        );
    }
}

/// A control string split across chunks must stay suppressed — the log is
/// fed one PTY read at a time, so a payload routinely straddles a boundary.
#[test]
fn a_control_string_split_across_log_chunks_stays_suppressed() {
    let input = b"before\x1b_Ga=T;payload-with-/tmp/path\x1b\\after";
    for split in 1..input.len() {
        let mut stripper = super::AnsiStripper::new();
        let mut out = stripper.strip(&input[..split]);
        out.extend_from_slice(&stripper.strip(&input[split..]));
        assert_eq!(
            String::from_utf8_lossy(&out),
            "beforeafter",
            "split at {split} leaked payload into the log"
        );
    }
}

/// CAN/SUB cancel a control string, so an unterminated one cannot swallow
/// the rest of the log.
#[test]
fn a_cancelled_control_string_does_not_swallow_the_rest_of_the_log() {
    for cancel in [0x18_u8, 0x1a] {
        for intro in [&b"\x1b]0;"[..], &b"\x1bP"[..], &b"\x1b_"[..]] {
            let mut input = b"before".to_vec();
            input.extend_from_slice(intro);
            input.extend_from_slice(b"payload");
            input.push(cancel);
            input.extend_from_slice(b"after");

            let mut stripper = super::AnsiStripper::new();
            let out = stripper.strip(&input);
            assert_eq!(
                String::from_utf8_lossy(&out),
                "beforeafter",
                "cancel {cancel:#04x} after {intro:?}: the log must resume"
            );
        }
    }
}

/// A control string that never terminates must not silence the log.
///
/// CAN, SUB and ST all end a sequence, and all three are the writer's
/// choice to send. A child that sends none of them must not hold the
/// stripper mid-sequence forever, silently dropping every later line of
/// the session log.
#[test]
fn an_unterminated_control_string_stops_swallowing_the_log() {
    for intro in [&b"\x1b]0;"[..], &b"\x1bP"[..], &b"\x1b_"[..], &b"\x1b["[..]] {
        let mut input = intro.to_vec();
        // `1` is a CSI parameter byte and ordinary payload everywhere
        // else, so no introducer sees it as its terminator.
        input.extend(std::iter::repeat_n(
            b'1',
            super::MAX_LOGGED_CONTROL_SEQUENCE_BYTES,
        ));
        input.extend_from_slice(b"after");

        let mut stripper = super::AnsiStripper::new();
        let out = stripper.strip(&input);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.ends_with("after"),
            "{intro:?}: the log must resume once the sequence outgrows its bound, got {text:?}"
        );
        // The introducer bytes are counted too, so the last few payload
        // bytes land in the log once the bound trips. What matters is that
        // the tail is a handful of bytes rather than the whole payload.
        assert!(
            out.len() < 16,
            "{intro:?}: {} bytes of the payload reached the log",
            out.len() - "after".len()
        );
    }
}

#[test]
fn utf8_continuations_count_toward_log_recovery_bound() {
    let limit = super::MAX_LOGGED_CONTROL_SEQUENCE_BYTES;
    for remaining in 0..4 {
        let mut input = b"\x1b]0;".to_vec();
        input.extend(std::iter::repeat_n(b'x', limit - 3 - remaining));
        let scalar_start = input.len();
        input.extend_from_slice(b"\xf0\x9c\x80\x80visible");
        for split in scalar_start..=scalar_start + 4 {
            let mut filter = super::SessionLogFilter::default();
            let mut out = filter.filter(&input[..split], 1, true);
            out.extend(filter.filter(&input[split..], 1, true));
            assert_eq!(
                std::str::from_utf8(&out),
                Ok("visible"),
                "remaining={remaining}, split={split}"
            );
        }
    }
}

/// The bound is per sequence, not per stream.
///
/// Two long-but-well-formed sequences must both be stripped whole. If the
/// counter carried over from the first, the second would trip the bound
/// part-way through and spill the rest of its payload into the log.
#[test]
fn each_control_sequence_gets_the_whole_bound() {
    let payload = "t".repeat(super::MAX_LOGGED_CONTROL_SEQUENCE_BYTES - 100);
    let one = format!("\x1b]0;{payload}\x07");

    let mut stripper = super::AnsiStripper::new();
    let mut out = stripper.strip(one.as_bytes());
    out.extend(stripper.strip(b"between"));
    out.extend(stripper.strip(one.as_bytes()));
    out.extend(stripper.strip(b"end"));

    assert_eq!(
        String::from_utf8_lossy(&out),
        "betweenend",
        "the second sequence must get its own budget"
    );
}

#[test]
fn restarted_osc_gets_a_fresh_log_stripping_budget() {
    let limit = super::MAX_LOGGED_CONTROL_SEQUENCE_BYTES;
    for payload_len in [limit - 6, limit - 3] {
        let mut input = b"\x1b]0;".to_vec();
        input.extend(std::iter::repeat_n(b'x', payload_len));
        let restart = input.len();
        input.extend_from_slice(b"\x1b]0;secret\x07visible");
        for split in [0, restart, restart + 1, restart + 2, input.len()] {
            let mut filter = super::SessionLogFilter::default();
            let mut out = filter.filter(&input[..split], 1, true);
            out.extend(filter.filter(&input[split..], 1, true));
            assert_eq!(out, b"visible", "payload_len={payload_len}, split={split}");
        }
    }
}

/// A CSI/OSC sequence whose terminator lands in the next PTY-read chunk
/// must still be recognized and fully removed when the same `AnsiStripper`
/// is reused across calls, as the reader thread's per-pane log path does.
/// Each case below splits a sequence at a different, deliberately awkward
/// byte boundary. A stateless stripper would leak the second chunk's raw
/// escape-sequence bytes into the log as literal text.
#[test]
fn ansi_stripper_persists_state_across_split_sequences() {
    use super::AnsiStripper;

    // CSI params split before the final byte: "\x1b[31mhello" split as
    // "\x1b[3" | "1mhello".
    let mut s = AnsiStripper::new();
    let mut out = s.strip(b"\x1b[3");
    out.extend(s.strip(b"1mhello"));
    assert_eq!(out, b"hello");

    // OSC body (title) split before the BEL terminator: split as
    // "\x1b]0;tit" | "le\x07after".
    let mut s = AnsiStripper::new();
    let mut out = s.strip(b"\x1b]0;tit");
    out.extend(s.strip(b"le\x07after"));
    assert_eq!(out, b"after");

    // OSC 8 hyperlink split exactly at the ST (`ESC \`) boundary — the
    // most awkward split, since the terminator itself straddles the
    // chunk boundary: "...\x1b" | "\\link".
    let mut s = AnsiStripper::new();
    let mut out = s.strip(b"\x1b]8;;http://example/\x1b");
    out.extend(s.strip(b"\\link"));
    assert_eq!(out, b"link");

    // Bare ESC at the very end of one chunk, CSI continues in the next:
    // "\x1b" | "[1mA".
    let mut s = AnsiStripper::new();
    let mut out = s.strip(b"\x1b");
    out.extend(s.strip(b"[1mA"));
    assert_eq!(out, b"A");

    // Sanity: a single instance also strips a sequence spread over
    // three chunks, and keeps passing plain bytes through between them.
    let mut s = AnsiStripper::new();
    let mut out = s.strip(b"before\x1b[");
    out.extend(s.strip(b"38;5;"));
    out.extend(s.strip(b"196mred"));
    assert_eq!(out, b"beforered");
}
