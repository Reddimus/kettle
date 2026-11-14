//! Where a program's notification is shown, and how often.
//!
//! OSC 9, OSC 777 and `kettle.notify` all come here. While a Kettle window
//! has focus a notification is a toast in that window and never a desktop
//! notification (Warp's rule, per app rather than per window); with none
//! focused it goes to the desktop. A per-source cooldown and identical-message
//! suppression apply before either route, so a flood from an unfocused pane
//! cannot fill the 64-slot desktop queue any more than a focused one can fill
//! the window. The text is terminal output: it is sanitized on the way in,
//! bounded, held only in memory while it is shown or in flight, and never
//! logged.

use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasher;
use std::time::{Duration, Instant};

/// A source's next notification waits this long after its last one was
/// shown. Messages in between fold into one, which is shown when the
/// cooldown ends with a count of the ones it replaced.
pub(crate) const COOLDOWN: Duration = Duration::from_secs(2);

/// The same message from the same source again within this long is a repeat:
/// it is counted on the toast showing it, never shown or sent again.
pub(crate) const IDENTICAL: Duration = Duration::from_secs(5);

/// Longest title and body kept, in characters, after sanitizing. Display
/// clamps further; this bounds what the queues and the pending map hold.
pub(crate) const MAX_TITLE_CHARS: usize = 256;
pub(crate) const MAX_BODY_CHARS: usize = 1024;

/// Longest source label kept, in characters.
pub(crate) const MAX_SOURCE_CHARS: usize = 128;

/// Sources the router remembers at once.
const MAX_SOURCES: usize = 256;

/// Desktop notifications in flight that a failure could still bring back
/// into the window, and how long one is remembered.
pub(crate) const PENDING_LIMIT: usize = 64;
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(60);

/// `raw` as notification text: controls and the Unicode bidi formatting
/// characters become whitespace, every whitespace run collapses to one space
/// (or, where `newlines` allows, to one newline when the run held one), the
/// ends are trimmed and at most `max_chars` characters are kept, cut at a
/// grapheme boundary. Applied to every field that enters: OSC fields,
/// `kettle.notify` arguments and the source tab's title.
pub(crate) fn sanitize_text(raw: &str, newlines: bool, max_chars: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;
    let mut out = String::with_capacity(raw.len().min(max_chars.saturating_mul(4)));
    let mut chars = 0;
    // The whitespace run being collapsed: `Some(true)` once it held a newline.
    let mut run: Option<bool> = None;
    for ch in raw.chars() {
        if ch.is_whitespace()
            || ch.is_control()
            || kettle_config::ssh_config::is_bidi_format_char(ch)
        {
            run = Some(run.unwrap_or(false) || (newlines && ch == '\n'));
            continue;
        }
        if let Some(newline) = run.take()
            && !out.is_empty()
        {
            out.push(if newline { '\n' } else { ' ' });
            chars += 1;
        }
        out.push(ch);
        chars += 1;
        // One character past the cap shows whether the cap splits a grapheme.
        if chars > max_chars {
            break;
        }
    }
    if chars > max_chars {
        let (mut kept, mut end) = (0, 0);
        for (at, grapheme) in out.grapheme_indices(true) {
            let count = grapheme.chars().count();
            if kept + count > max_chars {
                break;
            }
            kept += count;
            end = at + grapheme.len();
        }
        out.truncate(end);
        out.truncate(out.trim_end().len());
    }
    out
}

/// A notification's text, sanitized.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Message {
    pub title: String,
    pub body: String,
}

impl Message {
    /// `None` when nothing is left to show. A message with a body and no
    /// title shows its body as the title.
    pub(crate) fn new(title: &str, body: &str) -> Option<Self> {
        let title = sanitize_text(title, false, MAX_TITLE_CHARS);
        let body = sanitize_text(body, true, MAX_BODY_CHARS);
        if title.is_empty() {
            let title = sanitize_text(&body, false, MAX_TITLE_CHARS);
            return (!title.is_empty()).then_some(Self {
                title,
                body: String::new(),
            });
        }
        Some(Self { title, body })
    }
}

/// Where a notification came from: a window, and the pane that printed it
/// (`None` for a Lua script's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Source {
    pub window: u64,
    pub pane: Option<u64>,
}

/// A notification to show now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Delivery {
    pub source: Source,
    pub message: Message,
    /// Identifies the message for repeats; equal text, equal digest.
    pub digest: u64,
    /// Messages from the source its cooldown folded into this one.
    pub skipped: u32,
    /// The source tab's sanitized title when it was delivered, filled in by
    /// the window that delivers it.
    pub label: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Show it now, on whichever route [`route`] picks.
    Deliver(Delivery),
    /// The source's last message again within [`IDENTICAL`]: count it on the
    /// toast that shows it, if one still does, and send nothing.
    Repeat { source: Source, digest: u64 },
    /// Inside the source's cooldown: kept as its newest message, shown by
    /// [`Router::take_due`] when the cooldown ends.
    Held,
}

struct Held {
    message: Message,
    digest: u64,
    skipped: u32,
}

struct SourceState {
    delivered_at: Instant,
    digest: u64,
    held: Option<Held>,
}

/// The per-source cooldown and repeat suppression, before routing.
#[derive(Default)]
pub(crate) struct Router {
    hasher: std::collections::hash_map::RandomState,
    sources: HashMap<Source, SourceState>,
}

impl Router {
    fn digest(&self, message: &Message) -> u64 {
        self.hasher.hash_one(message)
    }

    pub(crate) fn admit(&mut self, source: Source, message: Message, now: Instant) -> Admission {
        let digest = self.digest(&message);
        if let Some(state) = self.sources.get_mut(&source) {
            let since = now.saturating_duration_since(state.delivered_at);
            if digest == state.digest && since < IDENTICAL {
                return Admission::Repeat { source, digest };
            }
            if since < COOLDOWN {
                match &mut state.held {
                    Some(held) if held.digest == digest => {}
                    Some(held) => {
                        held.skipped = held.skipped.saturating_add(1);
                        held.message = message;
                        held.digest = digest;
                    }
                    None => {
                        state.held = Some(Held {
                            message,
                            digest,
                            skipped: 0,
                        });
                    }
                }
                return Admission::Held;
            }
            // A held message whose cooldown ended before `take_due` ran is
            // older than this one: it folds in.
            let skipped = state
                .held
                .take()
                .map_or(0, |held| held.skipped.saturating_add(1));
            state.delivered_at = now;
            state.digest = digest;
            return Admission::Deliver(Delivery {
                source,
                message,
                digest,
                skipped,
                label: None,
            });
        }
        self.make_room(now);
        self.sources.insert(
            source,
            SourceState {
                delivered_at: now,
                digest,
                held: None,
            },
        );
        Admission::Deliver(Delivery {
            source,
            message,
            digest,
            skipped: 0,
            label: None,
        })
    }

    /// Held messages whose source's cooldown has ended, to show now.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<Delivery> {
        let mut due = Vec::new();
        for (source, state) in &mut self.sources {
            if now.saturating_duration_since(state.delivered_at) >= COOLDOWN
                && let Some(held) = state.held.take()
            {
                state.delivered_at = now;
                state.digest = held.digest;
                due.push(Delivery {
                    source: *source,
                    message: held.message,
                    digest: held.digest,
                    skipped: held.skipped,
                    label: None,
                });
            }
        }
        due.sort_by_key(|delivery| delivery.source);
        self.prune(now);
        due
    }

    /// How long until [`Router::take_due`] has something, if anything is held.
    pub(crate) fn next_due(&self, now: Instant) -> Option<Duration> {
        self.sources
            .values()
            .filter(|state| state.held.is_some())
            .map(|state| COOLDOWN.saturating_sub(now.saturating_duration_since(state.delivered_at)))
            .min()
    }

    /// A closed window's sources, held messages included, are forgotten.
    pub(crate) fn forget_window(&mut self, window: u64) {
        self.sources.retain(|source, _| source.window != window);
    }

    fn prune(&mut self, now: Instant) {
        self.sources.retain(|_, state| {
            state.held.is_some() || now.saturating_duration_since(state.delivered_at) < IDENTICAL
        });
    }

    fn make_room(&mut self, now: Instant) {
        if self.sources.len() < MAX_SOURCES {
            return;
        }
        self.prune(now);
        while self.sources.len() >= MAX_SOURCES {
            let oldest = self
                .sources
                .iter()
                .min_by_key(|(_, state)| (state.held.is_some(), state.delivered_at))
                .map(|(source, _)| *source);
            match oldest {
                Some(source) => self.sources.remove(&source),
                None => break,
            };
        }
    }

    #[cfg(test)]
    fn sources(&self) -> usize {
        self.sources.len()
    }
}

/// Where a notification goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// A toast in this window: the one with focus.
    Toast(u64),
    /// A desktop notification; no Kettle window has focus.
    Desktop,
}

/// While any Kettle window has focus the notification is a toast there, and
/// never also a desktop notification: the user is looking at Kettle, macOS
/// may hide a banner from the frontmost app anyway, and GNOME would banner
/// over the window. Only with no Kettle window focused does it go to the
/// desktop.
pub(crate) fn route(focused_window: Option<u64>) -> Route {
    focused_window.map_or(Route::Desktop, Route::Toast)
}

/// Desktop notifications in flight, by the id the worker reports a failure
/// with: at most [`PENDING_LIMIT`], each for at most [`PENDING_TTL`]. A failed
/// one comes back here to be shown in its window instead.
#[derive(Default)]
pub(crate) struct PendingDesktop {
    last_id: u64,
    entries: VecDeque<(u64, Instant, Delivery)>,
}

impl PendingDesktop {
    /// Remember `delivery` and return its id. The oldest entry makes room.
    pub(crate) fn insert(&mut self, delivery: Delivery, now: Instant) -> u64 {
        self.prune(now);
        self.last_id = self.last_id.wrapping_add(1).max(1);
        while self.entries.len() >= PENDING_LIMIT {
            self.entries.pop_front();
        }
        self.entries.push_back((self.last_id, now, delivery));
        self.last_id
    }

    /// The delivery `id` was, if it is still remembered.
    pub(crate) fn take(&mut self, id: u64, now: Instant) -> Option<Delivery> {
        self.prune(now);
        let at = self.entries.iter().position(|(entry, _, _)| *entry == id)?;
        self.entries.remove(at).map(|(_, _, delivery)| delivery)
    }

    pub(crate) fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|(_, at, _)| now.saturating_duration_since(*at) >= PENDING_TTL)
        {
            self.entries.pop_front();
        }
    }

    pub(crate) fn forget_window(&mut self, window: u64) {
        self.entries
            .retain(|(_, _, delivery)| delivery.source.window != window);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(title: &str) -> Message {
        Message::new(title, "").unwrap()
    }

    fn pane(id: u64) -> Source {
        Source {
            window: 1,
            pane: Some(id),
        }
    }

    /// Every field that enters loses its controls and bidi formatting
    /// characters, has its whitespace runs collapsed and its ends trimmed,
    /// and is capped. Bodies keep one newline where a run held one.
    #[test]
    fn notification_text_is_sanitized_at_entry() {
        assert_eq!(
            sanitize_text(
                " a\u{202E}b\u{2066}\tc\u{7}\u{1b}[31md \u{200F} ",
                false,
                64
            ),
            "a b c [31md"
        );
        assert_eq!(
            sanitize_text("one \n\n  two\r\nthree", true, 64),
            "one\ntwo\nthree"
        );
        assert_eq!(sanitize_text("one \n\n  two", false, 64), "one two");
        assert_eq!(
            sanitize_text("\u{2028}x\u{3000}\u{a0}y\u{85}", false, 64),
            "x y"
        );
        assert_eq!(sanitize_text("abcdef", false, 4), "abcd");
        assert_eq!(sanitize_text("ab cd", false, 3), "ab");
        assert_eq!(sanitize_text("\u{202E}\u{2069}\n", true, 64), "");
        // A ZWJ emoji and combining marks are text, not formatting.
        assert_eq!(
            sanitize_text("\u{1f469}\u{200d}\u{1f680} e\u{301}", false, 64),
            "\u{1f469}\u{200d}\u{1f680} e\u{301}"
        );
        // Every bidi formatting character kettle-config knows is removed.
        let bidi: String = (0..=0x10FFFF_u32)
            .filter_map(char::from_u32)
            .filter(|c| kettle_config::ssh_config::is_bidi_format_char(*c))
            .collect();
        assert!(bidi.chars().count() >= 12);
        assert_eq!(sanitize_text(&format!("a{bidi}b"), false, 64), "a b");
        let message = Message::new(&"t".repeat(10_000), &"b".repeat(10_000)).unwrap();
        assert_eq!(message.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(message.body.chars().count(), MAX_BODY_CHARS);
        assert_eq!(Message::new(" \u{202E} ", "\n\t"), None);
        assert_eq!(
            Message::new("", "line one\nline two"),
            Some(Message {
                title: "line one line two".into(),
                body: String::new()
            })
        );
        // The cap never splits a grapheme: a ZWJ sequence or a base with its
        // marks is kept whole or dropped whole.
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(sanitize_text(&format!("ab{family}"), false, 4), "ab");
        assert_eq!(
            sanitize_text(&format!("ab{family}"), false, 7),
            format!("ab{family}")
        );
        assert_eq!(sanitize_text("abcde\u{301}f", false, 5), "abcd");
        assert_eq!(sanitize_text("abcde\u{301}f", false, 6), "abcde\u{301}");
    }

    /// kettle-vt cleans OSC fields with its own copy of the bidi list (it
    /// does not depend on kettle-config); the two name the same characters.
    #[test]
    fn osc_fields_and_other_sources_strip_the_same_bidi_characters() {
        fn listed(source: &str, function: &str) -> Vec<String> {
            let body = source
                .split(function)
                .nth(1)
                .and_then(|rest| rest.split("\n}\n").next())
                .expect("the bidi list");
            let mut points: Vec<String> = body
                .split("'\\u{")
                .skip(1)
                .filter_map(|rest| rest.split('}').next())
                .map(str::to_ascii_uppercase)
                .collect();
            points.sort();
            points
        }
        let vt = listed(
            include_str!("../../kettle-vt/src/extract.rs"),
            "fn is_bidi_format(c: char) -> bool {",
        );
        let config = listed(
            include_str!("../../kettle-config/src/ssh_config.rs"),
            "pub fn is_bidi_format_char(c: char) -> bool {",
        );
        assert!(vt.len() >= 7, "{vt:?}");
        assert_eq!(vt, config);
    }

    /// The cooldown and the repeat suppression run before routing. A burst
    /// of 100 distinct messages from one pane shows the first now and the
    /// newest when the cooldown ends, counting the 98 in between; an
    /// identical message within 5 s is a repeat, never a second delivery;
    /// another pane is not held back.
    #[test]
    fn a_burst_from_one_source_is_one_delivery_now_and_one_when_its_cooldown_ends() {
        let mut router = Router::default();
        let start = Instant::now();
        let mut delivered = Vec::new();
        for i in 0..100 {
            match router.admit(pane(1), message(&format!("step {i}")), start) {
                Admission::Deliver(delivery) => delivered.push(delivery),
                Admission::Held => {}
                other => panic!("step {i}: {other:?}"),
            }
        }
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].message.title, "step 0");
        assert!(matches!(
            router.admit(pane(2), message("other pane"), start),
            Admission::Deliver(_)
        ));
        assert_eq!(router.next_due(start), Some(COOLDOWN));
        assert!(router.take_due(start + COOLDOWN / 2).is_empty());
        let due = router.take_due(start + COOLDOWN);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].message.title, "step 99");
        assert_eq!(due[0].skipped, 98, "the messages it replaced are counted");
        assert_eq!(router.next_due(start + COOLDOWN), None);

        // The same text again is a repeat while the window lasts, from the
        // last delivery, even past the cooldown.
        let later = start + COOLDOWN;
        for after in [
            Duration::ZERO,
            COOLDOWN,
            IDENTICAL - Duration::from_millis(1),
        ] {
            assert_eq!(
                router.admit(pane(1), message("step 99"), later + after),
                Admission::Repeat {
                    source: pane(1),
                    digest: due[0].digest
                }
            );
        }
        assert!(matches!(
            router.admit(pane(1), message("step 99"), later + IDENTICAL),
            Admission::Deliver(_)
        ));
        // A held message whose cooldown passed before anyone took it folds
        // into the next delivery instead of being lost.
        let t = later + IDENTICAL;
        assert_eq!(router.admit(pane(1), message("held"), t), Admission::Held);
        match router.admit(pane(1), message("newer"), t + COOLDOWN) {
            Admission::Deliver(delivery) => {
                assert_eq!(delivery.message.title, "newer");
                assert_eq!(delivery.skipped, 1);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The router forgets idle sources, keeps at most 256 and forgets a
    /// closed window's.
    #[test]
    fn the_router_stays_bounded() {
        let mut router = Router::default();
        let start = Instant::now();
        for id in 0..1000 {
            let _ = router.admit(pane(id), message("x"), start);
        }
        assert!(router.sources() <= MAX_SOURCES);
        let _ = router.admit(pane(5000), message("y"), start + IDENTICAL);
        let _ = router.take_due(start + IDENTICAL);
        assert_eq!(router.sources(), 1, "idle sources are pruned");
        let _ = router.admit(pane(5000), message("z"), start + IDENTICAL);
        router.forget_window(1);
        assert_eq!(router.sources(), 0);
        assert_eq!(router.next_due(start), None);
    }

    /// Focused anywhere in Kettle: a toast there, never the desktop. Focused
    /// nowhere: the desktop.
    #[test]
    fn notifications_route_by_app_focus() {
        assert_eq!(route(Some(1)), Route::Toast(1));
        assert_eq!(route(Some(7)), Route::Toast(7));
        assert_eq!(route(None), Route::Desktop);
    }

    /// The pending map keeps at most 64 deliveries for at most a minute and
    /// gives each back once, by id.
    #[test]
    fn pending_desktop_notifications_are_bounded_and_expire() {
        let mut pending = PendingDesktop::default();
        let start = Instant::now();
        let delivery = |title: &str| Delivery {
            source: pane(1),
            message: message(title),
            digest: 0,
            skipped: 0,
            label: None,
        };
        let ids: Vec<u64> = (0..100)
            .map(|i| pending.insert(delivery(&format!("n{i}")), start))
            .collect();
        assert_eq!(pending.len(), PENDING_LIMIT);
        assert_eq!(pending.take(ids[0], start), None, "the oldest made room");
        let kept = pending.take(ids[99], start).expect("the newest is kept");
        assert_eq!(kept.message.title, "n99");
        assert_eq!(pending.take(ids[99], start), None, "given back once");
        assert!(pending.take(ids[60], start + PENDING_TTL).is_none());
        assert_eq!(pending.len(), 0, "expired entries are pruned");
        let id = pending.insert(delivery("closing"), start);
        pending.forget_window(1);
        assert_eq!(pending.take(id, start), None);
        assert_ne!(pending.insert(delivery("a"), start), 0);
    }
}
