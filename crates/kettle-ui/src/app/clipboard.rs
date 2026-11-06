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

fn osc52_is_primary(target: ClipboardType) -> bool {
    match osc52_clipboard_channel(target) {
        Osc52ClipboardChannel::Clipboard => false,
        #[cfg(target_os = "linux")]
        Osc52ClipboardChannel::Primary => true,
    }
}

fn set_osc52_clipboard(
    clipboard: &mut crate::clipboard::Clipboard,
    target: ClipboardType,
    text: String,
) -> Result<(), arboard::Error> {
    clipboard.set_selection(osc52_is_primary(target), text)
}

pub(super) fn get_clipboard_text_bounded(
    clipboard: &mut crate::clipboard::Clipboard,
    target: ClipboardType,
    max_bytes: usize,
) -> Result<String, arboard::Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    clipboard.get_selection_bounded(osc52_is_primary(target), max_bytes, deadline)
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

pub(super) trait Osc52Backend {
    fn set(&mut self, target: ClipboardType, text: String) -> Result<(), arboard::Error>;
    fn get(&mut self, target: ClipboardType) -> Result<String, arboard::Error>;
}

impl Osc52Backend for crate::clipboard::Clipboard {
    fn set(&mut self, target: ClipboardType, text: String) -> Result<(), arboard::Error> {
        set_osc52_clipboard(self, target, text)
    }

    fn get(&mut self, target: ClipboardType) -> Result<String, arboard::Error> {
        get_clipboard_text_bounded(self, target, OSC52_MAX + 1)
    }
}

pub(super) struct PaneClipboard<'a, C> {
    pub clipboard: Option<&'a mut C>,
    pub policy: Osc52,
    pub notify: bool,
    pub notices: &'a mut crate::mux::Osc52NoticeState,
    pub tr: &'a kettle_i18n::Translator,
}

impl crate::mux::Osc52NoticeState {
    fn show(&mut self, message: String) {
        self.message = Some(message);
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn copied(
        &mut self,
        now: std::time::Instant,
        enabled: bool,
        count: usize,
        tr: &kettle_i18n::Translator,
    ) {
        if enabled
            && self.last_copy.is_none_or(|last| {
                now.saturating_duration_since(last) >= std::time::Duration::from_secs(5)
            })
        {
            self.last_copy = Some(now);
            self.show(tr.osc52_copied(count as u64));
        }
    }

    pub(super) fn denied(&mut self, read: bool, tr: &kettle_i18n::Translator) {
        let seen = if read {
            &mut self.denied_read
        } else {
            &mut self.denied_write
        };
        if !*seen {
            *seen = true;
            self.show(
                tr.text(if read {
                    kettle_i18n::Text::Osc52DeniedRead
                } else {
                    kettle_i18n::Text::Osc52DeniedWrite
                })
                .to_string(),
            );
        }
    }
}

impl<C: Osc52Backend> Osc52Clipboard for PaneClipboard<'_, C> {
    fn store(&mut self, target: ClipboardType, text: String) {
        if !self.policy.can_copy() {
            self.notices.denied(false, self.tr);
            return;
        }
        if let Some(cb) = &mut self.clipboard {
            match prepare_osc52_copy(text, OSC52_MAX) {
                Ok(text) => {
                    let count = text.chars().count();
                    match cb.set(target, text) {
                        Ok(()) => self.notices.copied(
                            std::time::Instant::now(),
                            self.notify,
                            count,
                            self.tr,
                        ),
                        Err(error) => {
                            log::warn!("clipboard write failed (OSC 52 {target:?} target): {error}")
                        }
                    }
                }
                Err(error) => log::warn!("OSC 52: could not prepare text: {error}"),
            }
        }
    }

    fn load(&mut self, target: ClipboardType) -> String {
        // Denied reads still receive an empty, well-formed protocol reply.
        if self.policy.can_paste() {
            self.clipboard
                .as_mut()
                .map(|c| osc52_read_result(c.get(target)))
                .unwrap_or_default()
        } else {
            self.notices.denied(true, self.tr);
            String::new()
        }
    }
}

pub(super) fn osc52_read_result(result: Result<String, arboard::Error>) -> String {
    match result {
        Ok(text) => text,
        Err(error) => {
            log::warn!("clipboard read failed (OSC 52): {error}");
            String::new()
        }
    }
}
