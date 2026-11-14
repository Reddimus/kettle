//! The in-window notification toast: each window's queue and the toast's
//! text.
//!
//! One toast shows at a time and at most [`WAITING`] wait behind it. The same
//! message from the same source counts on the toast already showing or
//! waiting with it ("×N") instead of queuing again. A full queue drops its
//! oldest *waiting* toast, never the one on screen (which the user may be
//! hovering or reading with assistive technology), and counts it on the
//! visible one ("N more notifications").

use std::collections::VecDeque;
use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::notification_router::{Delivery, Message, Source};
use crate::update_toast::ToastClock;

/// Toasts that wait behind the visible one.
pub(crate) const WAITING: usize = 3;

/// The toast painter's limit on its text (`kettle_render::update_toast_geometry`
/// shows nothing for more). The whole text, source, counter and overflow line
/// included, is clamped below it.
pub(crate) const MAX_TOAST_BYTES: usize = 1024;

/// Display columns a title or body keeps before it is ellipsized.
const FIELD_COLUMNS: usize = 160;

/// Display columns the source tab's title keeps.
const SOURCE_COLUMNS: usize = 24;

/// Wrapped rows the title and body share, above the overflow line.
const TEXT_ROWS: usize = 3;

pub(crate) struct NotificationToast {
    pub source: Source,
    pub label: Option<String>,
    pub message: Message,
    pub digest: u64,
    /// Times the source sent this message; 1 for once.
    pub repeats: u32,
    /// Notifications folded into this toast that it does not show.
    pub more: u32,
    pub clock: ToastClock,
}

impl NotificationToast {
    pub(crate) fn new(delivery: Delivery, now: Instant) -> Self {
        Self {
            source: delivery.source,
            label: delivery.label,
            message: delivery.message,
            digest: delivery.digest,
            repeats: 1,
            more: delivery.skipped,
            clock: ToastClock::new(now),
        }
    }
}

#[derive(Default)]
pub(crate) struct ToastQueue {
    visible: Option<NotificationToast>,
    waiting: VecDeque<NotificationToast>,
}

impl ToastQueue {
    pub(crate) fn push(&mut self, toast: NotificationToast) {
        if let Some(same) = self.find_mut(toast.source, toast.digest) {
            same.repeats = same.repeats.saturating_add(1);
            same.more = same.more.saturating_add(toast.more);
            return;
        }
        let Some(visible) = self.visible.as_mut() else {
            self.visible = Some(toast);
            return;
        };
        self.waiting.push_back(toast);
        if self.waiting.len() > WAITING
            && let Some(dropped) = self.waiting.pop_front()
        {
            visible.more = visible.more.saturating_add(dropped.more).saturating_add(1);
        }
    }

    /// Count a repeat on the toast showing or waiting with this message;
    /// `false` when none does.
    pub(crate) fn repeat(&mut self, source: Source, digest: u64) -> bool {
        self.find_mut(source, digest)
            .map(|toast| toast.repeats = toast.repeats.saturating_add(1))
            .is_some()
    }

    fn find_mut(&mut self, source: Source, digest: u64) -> Option<&mut NotificationToast> {
        self.visible
            .iter_mut()
            .chain(self.waiting.iter_mut())
            .find(|toast| toast.source == source && toast.digest == digest)
    }

    pub(crate) fn visible(&self) -> Option<&NotificationToast> {
        self.visible.as_ref()
    }

    pub(crate) fn visible_mut(&mut self) -> Option<&mut NotificationToast> {
        self.visible.as_mut()
    }

    /// Dismiss the toast on screen; the next waiting one takes its place.
    pub(crate) fn dismiss(&mut self) -> bool {
        let dismissed = self.visible.take().is_some();
        self.visible = self.waiting.pop_front();
        dismissed
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.visible.is_none()
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// `text` cut to `columns` display columns at a grapheme boundary, with `…`
/// when it was cut; empty for no columns.
pub(crate) fn ellipsize(text: &str, columns: usize) -> String {
    if text.width() <= columns {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let width = grapheme.width();
        if used + width + 1 > columns {
            break;
        }
        used += width;
        out.push_str(grapheme);
    }
    if columns > 0 {
        out.push('\u{2026}');
    }
    out
}

/// The toast's text, wrapped by the painter at `columns`: the source's title
/// and the message's title on the first line, the body after, the repeat
/// counter after the last of them, and the overflow line last. Title and body
/// share [`TEXT_ROWS`] rows and the whole stays within [`MAX_TOAST_BYTES`]:
/// the body is shortened first, then the title, at grapheme boundaries, and
/// the counter and the overflow line are never cut. `columns` of 0 (no
/// surface yet) fits by bytes alone.
pub(crate) fn toast_text(
    toast: &NotificationToast,
    tr: &kettle_i18n::Translator,
    columns: usize,
) -> String {
    let source = toast
        .label
        .as_deref()
        .filter(|label| !label.is_empty())
        .map(|label| ellipsize(label, SOURCE_COLUMNS));
    let title = ellipsize(&toast.message.title, FIELD_COLUMNS);
    let body = ellipsize(&toast.message.body.replace('\n', " "), FIELD_COLUMNS);
    let badge =
        (toast.repeats > 1).then(|| tr.notification_toast_repeats(u64::from(toast.repeats)));
    let more = (toast.more > 0).then(|| tr.notification_toast_more(u64::from(toast.more)));
    let compose = |title_columns: usize, body_columns: usize| {
        let title = ellipsize(&title, title_columns);
        let body = ellipsize(&body, body_columns);
        let mut text = match &source {
            Some(source) => tr.notification_toast_sourced(source, &title),
            None => title,
        };
        if !body.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&body);
        }
        if let Some(badge) = &badge {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(badge);
        }
        let core_rows = kettle_render::toast_text_rows(&text, columns);
        if let Some(more) = &more {
            text.push('\n');
            text.push_str(more);
        }
        (text, core_rows)
    };
    let fits = |(text, rows): &(String, Option<usize>)| {
        text.len() <= MAX_TOAST_BYTES
            && (columns == 0 || rows.is_some_and(|rows| rows <= TEXT_ROWS))
    };
    let (title_columns, body_columns) = (title.width(), body.width());
    // The widest field budget that still fits, by bisection: each is
    // monotonic in its budget.
    let widest = |range: std::ops::RangeInclusive<usize>,
                  text: &dyn Fn(usize) -> (String, Option<usize>)| {
        let (mut low, mut high) = (*range.start(), *range.end());
        if !fits(&text(low)) {
            return None;
        }
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if fits(&text(mid)) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        Some(low)
    };
    let full = compose(title_columns, body_columns);
    if fits(&full) {
        return full.0;
    }
    if let Some(body) = widest(0..=body_columns, &|budget| compose(title_columns, budget)) {
        return compose(title_columns, body).0;
    }
    if let Some(title) = widest(0..=title_columns, &|budget| compose(budget, 0)) {
        return compose(title, 0).0;
    }
    // Only a tiny surface gets here: keep the counter and overflow line and
    // still never exceed the painter's limit.
    let mut text = compose(0, 0).0;
    while text.len() > MAX_TOAST_BYTES {
        text.pop();
    }
    text
}

/// The toast's screen-reader label and description: who sent it and what it
/// says, with the repeat count and the overflow in the description so a
/// repeat changes no label and is not announced again.
pub(crate) fn toast_accessibility(
    toast: &NotificationToast,
    tr: &kettle_i18n::Translator,
) -> (String, Option<String>) {
    let title = ellipsize(&toast.message.title, FIELD_COLUMNS);
    let body = ellipsize(&toast.message.body, FIELD_COLUMNS);
    let message = match (title.is_empty(), body.is_empty()) {
        (false, false) => format!("{title}\n{body}"),
        (false, true) => title,
        _ => body,
    };
    let label = match toast.label.as_deref().filter(|label| !label.is_empty()) {
        Some(source) => tr.notification_toast_a11y(&ellipsize(source, SOURCE_COLUMNS), &message),
        None => tr.notification_toast_a11y_unsourced(&message),
    };
    let description: Vec<String> = [
        (toast.repeats > 1).then(|| tr.notification_toast_a11y_repeats(u64::from(toast.repeats))),
        (toast.more > 0).then(|| tr.notification_toast_more(u64::from(toast.more))),
    ]
    .into_iter()
    .flatten()
    .collect();
    (
        label,
        (!description.is_empty()).then(|| description.join("\n")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_i18n::{Language, Translator};

    fn toast(pane: u64, title: &str, body: &str) -> NotificationToast {
        let message = Message::new(title, body).unwrap();
        NotificationToast::new(
            Delivery {
                source: Source {
                    window: 1,
                    pane: Some(pane),
                },
                digest: {
                    use std::hash::{BuildHasher as _, BuildHasherDefault};
                    BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default()
                        .hash_one(&message)
                },
                message,
                skipped: 0,
                label: Some("fixture".into()),
            },
            Instant::now(),
        )
    }

    /// One visible, three waiting. Overflow drops the oldest waiting toast,
    /// never the visible one, and counts it there. The same message from
    /// the same source counts on its toast, visible or waiting, whatever
    /// waits behind it. Dismissing shows the next.
    #[test]
    fn the_queue_keeps_the_visible_toast_and_counts_what_it_drops() {
        let mut queue = ToastQueue::default();
        for i in 0..10 {
            queue.push(toast(1, &format!("n{i}"), ""));
        }
        assert_eq!(
            queue.visible().unwrap().message.title,
            "n0",
            "never evicted"
        );
        assert_eq!(queue.waiting(), WAITING);
        assert_eq!(queue.visible().unwrap().more, 6, "n1..n6 were dropped");
        // A repeat of a waiting toast and of the visible one.
        queue.push(toast(1, "n9", ""));
        queue.push(toast(1, "n0", ""));
        assert!(queue.repeat(
            Source {
                window: 1,
                pane: Some(1)
            },
            toast(1, "n0", "").digest
        ));
        assert_eq!(queue.visible().unwrap().repeats, 3);
        assert_eq!(queue.visible().unwrap().more, 6);
        assert_eq!(queue.waiting(), WAITING);
        // Another pane's identical text is its own toast.
        assert!(!queue.repeat(
            Source {
                window: 1,
                pane: Some(2)
            },
            toast(1, "n0", "").digest
        ));
        for expected in ["n7", "n8", "n9"] {
            assert!(queue.dismiss());
            let next = queue.visible().unwrap();
            assert_eq!(next.message.title, expected);
            assert_eq!(next.repeats, if expected == "n9" { 2 } else { 1 });
        }
        assert!(queue.dismiss());
        assert!(queue.is_empty());
        assert!(!queue.dismiss());
    }

    fn rows(text: &str, columns: usize) -> usize {
        kettle_render::toast_text_rows(text, columns).unwrap()
    }

    /// Long CJK, emoji, ZWJ and combining-mark text is cut at grapheme
    /// boundaries to fit three rows and 1024 bytes with the source, the
    /// repeat counter and the overflow line still whole. A triangle of a
    /// ZWJ family is 25 bytes in two columns, so columns alone would pass
    /// 2 KB to a painter that draws nothing over 1 KB.
    #[test]
    fn the_toast_text_fits_its_rows_and_the_painters_byte_limit() {
        let tr = Translator::new(Language::En);
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        let zalgo = format!("e{}", "\u{301}".repeat(30));
        for (title, body) in [
            (
                "\u{6f22}\u{5b57}".repeat(300),
                "\u{304b}\u{306a}".repeat(300),
            ),
            ("\u{1f514}".repeat(400), family.repeat(200)),
            (zalgo.repeat(40), zalgo.repeat(40)),
            ("short".into(), "w ".repeat(900)),
        ] {
            for columns in [0, 20, 56, 120] {
                let mut t = toast(1, &title, &body);
                t.repeats = 12;
                t.more = 3;
                let text = toast_text(&t, &tr, columns);
                let case = format!("{columns} columns, {:?}…", title.chars().next());
                assert!(
                    text.len() <= MAX_TOAST_BYTES,
                    "{case}: {} bytes",
                    text.len()
                );
                assert!(text.starts_with("fixture: "), "{case}");
                let (core, overflow) = text.rsplit_once('\n').unwrap();
                assert_eq!(overflow, "3 more notifications", "{case}");
                assert!(core.ends_with(" \u{d7}12"), "{case}: the counter is whole");
                if columns > 0 {
                    assert!(rows(core, columns) <= TEXT_ROWS, "{case}");
                }
                // Every cut lands on a grapheme boundary.
                let known: std::collections::HashSet<&str> = title
                    .graphemes(true)
                    .chain(body.graphemes(true))
                    .chain("fixture: \u{2026}\u{d7}0123456789\n".graphemes(true))
                    .collect();
                for grapheme in core.graphemes(true) {
                    assert!(
                        known.contains(grapheme),
                        "{case}: {grapheme:?} is a split grapheme"
                    );
                }
            }
        }
        // Short text is untouched.
        let t = toast(1, "Build finished", "");
        assert_eq!(toast_text(&t, &tr, 56), "fixture: Build finished");
        let t = toast(1, "Build done", "cargo test\npassed");
        assert_eq!(
            toast_text(&t, &tr, 56),
            "fixture: Build done\ncargo test passed"
        );
        let mut t = toast(1, "Build finished", "");
        t.label = None;
        t.repeats = 2;
        assert_eq!(toast_text(&t, &tr, 56), "Build finished \u{d7}2");
        // The source title has its own budget.
        t.label = Some("a".repeat(80));
        t.repeats = 1;
        assert_eq!(
            toast_text(&t, &tr, 56),
            format!("{}\u{2026}: Build finished", "a".repeat(23))
        );
    }

    /// The counter and the overflow line are translated with the text.
    #[test]
    fn the_toast_text_and_its_screen_reader_label_are_translated() {
        let mut t = toast(1, "Build finished", "");
        t.repeats = 3;
        t.more = 1;
        let en = Translator::new(Language::En);
        let es = Translator::new(Language::Es);
        assert_eq!(
            toast_text(&t, &en, 56),
            "fixture: Build finished \u{d7}3\n1 more notification"
        );
        assert_eq!(
            toast_text(&t, &es, 56),
            "fixture: Build finished \u{d7}3\n1 notificaci\u{f3}n m\u{e1}s"
        );
        assert_eq!(
            toast_accessibility(&t, &en),
            (
                "Notification from fixture: Build finished".to_string(),
                Some("Sent 3 times\n1 more notification".to_string())
            )
        );
        assert_eq!(
            toast_accessibility(&t, &es),
            (
                "Notificaci\u{f3}n de fixture: Build finished".to_string(),
                Some("Enviada 3 veces\n1 notificaci\u{f3}n m\u{e1}s".to_string())
            )
        );
        t.label = None;
        t.repeats = 1;
        t.more = 0;
        assert_eq!(
            toast_accessibility(&t, &en),
            ("Notification: Build finished".to_string(), None)
        );
        assert_eq!(
            toast_accessibility(&t, &es),
            ("Notificaci\u{f3}n: Build finished".to_string(), None)
        );
    }

    #[test]
    fn ellipsizing_cuts_whole_graphemes_by_display_width() {
        assert_eq!(ellipsize("abcdef", 6), "abcdef");
        assert_eq!(ellipsize("abcdef", 4), "abc\u{2026}");
        assert_eq!(ellipsize("\u{6f22}\u{5b57}\u{6f22}", 4), "\u{6f22}\u{2026}");
        assert_eq!(ellipsize("e\u{301}e\u{301}e\u{301}", 2), "e\u{301}\u{2026}");
        assert_eq!(ellipsize("abc", 0), "");
        // A ZWJ sequence is kept whole or dropped whole, never cut at a
        // joiner.
        let couple = "\u{1f468}\u{200d}\u{1f469}";
        let cut = ellipsize(&format!("{couple}abc"), 3);
        assert!(
            cut == "\u{2026}" || cut == format!("{couple}\u{2026}"),
            "{cut:?}"
        );
    }
}
