use super::*;

pub(super) fn osc52_copy_is_available(policy: Osc52, clipboard_available: bool) -> bool {
    policy.can_copy() && clipboard_available
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Osc52ClipboardChannel {
    Clipboard,
    #[cfg(target_os = "linux")]
    Primary,
}

pub(super) fn osc52_clipboard_channel(target: ClipboardType) -> Osc52ClipboardChannel {
    #[cfg(target_os = "linux")]
    if target == ClipboardType::Selection {
        return Osc52ClipboardChannel::Primary;
    }

    let _ = target;
    Osc52ClipboardChannel::Clipboard
}

pub(super) fn prepare_osc52_copy(
    text: String,
    cap: usize,
) -> Result<String, std::collections::TryReserveError> {
    let mut text = kettle_core::scrub_card_markers(text)?;
    let keep = clamp_osc52(&text, cap).len();
    text.truncate(keep);
    Ok(text)
}

fn set_osc52_clipboard(
    clipboard: &mut arboard::Clipboard,
    target: ClipboardType,
    text: String,
) -> Result<(), arboard::Error> {
    match osc52_clipboard_channel(target) {
        Osc52ClipboardChannel::Clipboard => clipboard.set_text(text),
        #[cfg(target_os = "linux")]
        Osc52ClipboardChannel::Primary => {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text)
        }
    }
}

fn get_osc52_clipboard(
    clipboard: &mut arboard::Clipboard,
    target: ClipboardType,
) -> Result<String, arboard::Error> {
    match osc52_clipboard_channel(target) {
        Osc52ClipboardChannel::Clipboard => clipboard.get_text(),
        #[cfg(target_os = "linux")]
        Osc52ClipboardChannel::Primary => {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
        }
    }
}

/// Cap an OSC 52 clipboard payload so a hostile program can't make the
/// terminal allocate/set an unbounded clipboard. Truncates on a UTF-8
/// char boundary at or below `max` bytes (xterm/kitty also bound this).
pub(super) fn clamp_osc52(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 1 MiB — generous for real copies, bounded against abuse.
const OSC52_MAX: usize = 1 << 20;

pub(super) trait Osc52Clipboard {
    fn store(&mut self, target: ClipboardType, text: String);
    fn load(&mut self, target: ClipboardType) -> String;
}

pub(super) fn apply_osc52_event(
    clipboard: &mut impl Osc52Clipboard,
    event: TermEvent,
) -> Option<String> {
    match event {
        TermEvent::ClipboardStore(target, text) => {
            clipboard.store(target, text);
            None
        }
        TermEvent::ClipboardLoad(target, fmt) => {
            let text = clipboard.load(target);
            Some(fmt(clamp_osc52(&text, OSC52_MAX)))
        }
        _ => unreachable!(),
    }
}

impl Osc52Clipboard for App {
    fn store(&mut self, target: ClipboardType, text: String) {
        self.store_osc52_writes(vec![(target, text)]);
    }

    fn load(&mut self, target: ClipboardType) -> String {
        // OSC 52 read lets a (remote) program exfiltrate the
        // clipboard; denied by default — reply empty so the
        // protocol stays well-formed without leaking.
        if self.cfg.osc52.can_paste() {
            self.clipboard
                .as_mut()
                .and_then(|c| match get_osc52_clipboard(c, target) {
                    Ok(text) => Some(text),
                    Err(error) => {
                        log::warn!(
                            "clipboard read failed (OSC 52 {target:?} target): \
                             {error}"
                        );
                        None
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        }
    }
}

impl App {
    /// Apply OSC 52 stores when the config allows programs to copy.
    fn store_osc52_writes(&mut self, writes: Vec<(ClipboardType, String)>) {
        for (target, s) in writes {
            // Normalize complete card clusters before clipping so a
            // byte limit cannot leave a partial placeholder behind.
            if self.cfg.osc52.can_copy()
                && let Some(cb) = &mut self.clipboard
            {
                match prepare_osc52_copy(s, OSC52_MAX) {
                    Ok(text) => {
                        if let Err(error) = set_osc52_clipboard(cb, target, text) {
                            log::warn!(
                                "clipboard write failed (OSC 52 {target:?} target): {error}"
                            );
                        }
                    }
                    Err(error) => log::warn!("OSC 52: could not prepare text: {error}"),
                }
            }
        }
    }
}
