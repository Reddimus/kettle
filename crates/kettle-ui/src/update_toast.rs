//! Foreground-only update delivery and its persisted, bounded receipts.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::update_notice::{Notice, NoticeKind};

const LIFETIME: Duration = Duration::from_secs(7);
/// The fade over a toast's last moments, when motion is allowed.
const FADE: Duration = Duration::from_millis(150);
/// The least time a toast has left after a pause ends: enough to read it
/// again, then its whole fade.
const AFTER_PAUSE: Duration = Duration::from_secs(3);
const LEASE_SECONDS: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub(crate) notice: Notice,
    pub owner: u32,
    pub started: Option<u64>,
    pub until: u64,
    #[serde(default)]
    pub created: u64,
}

impl Claim {
    pub(crate) fn live_at(&self, now: u64) -> bool {
        self.created <= now && now < self.until
    }
}

#[derive(Clone, Debug)]
pub enum ClaimReply {
    Acquired(Claim),
    Seen,
    Busy(u64),
    Unavailable,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Ledger {
    #[serde(default)]
    seen: BTreeMap<NoticeKind, String>,
    #[serde(default)]
    claim: Option<Claim>,
}

impl Ledger {
    pub fn claim(
        &mut self,
        notice: Notice,
        now: u64,
        owner: u32,
        started: Option<u64>,
    ) -> Option<Claim> {
        let version = kettle_update::Version::parse(notice.tag.trim_start_matches('v')).ok()?;
        if self.seen.get(&notice.kind).is_some_and(|tag| {
            kettle_update::Version::parse(tag.trim_start_matches('v'))
                .is_ok_and(|seen| seen >= version)
        }) || self.claim.as_ref().is_some_and(|claim| claim.live_at(now))
        {
            return None;
        }
        let claim = Claim {
            notice,
            owner,
            started,
            until: now.saturating_add(LEASE_SECONDS),
            created: now,
        };
        self.claim = Some(claim.clone());
        Some(claim)
    }

    pub fn busy_until(&self, now: u64) -> Option<u64> {
        self.claim
            .as_ref()
            .filter(|c| c.live_at(now))
            .map(|c| c.until)
    }

    pub fn finish(&mut self, claim: &Claim, seen: bool) -> bool {
        if seen {
            let replace = self
                .seen
                .get(&claim.notice.kind)
                .is_none_or(|previous| version(&claim.notice.tag) > version(previous));
            if replace {
                self.seen
                    .insert(claim.notice.kind, claim.notice.tag.clone());
            }
        }
        let owns = self.claim.as_ref() == Some(claim);
        if owns {
            self.claim = None;
        }
        seen || owns
    }
}

fn version(tag: &str) -> Option<kettle_update::Version> {
    kettle_update::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// At most one pending high watermark for each of the three notice states.
pub(crate) struct ReceiptWriter {
    pending: std::sync::Arc<std::sync::Mutex<BTreeMap<NoticeKind, Claim>>>,
    wake: crossbeam_channel::Sender<()>,
}

impl ReceiptWriter {
    pub fn start() -> std::io::Result<Self> {
        Self::with_retry(Duration::from_secs(5), |claim| {
            crate::update_check::finish_toast(claim, true)
        })
    }

    fn with_retry(
        delay: Duration,
        persist: impl Fn(&Claim) -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let pending =
            std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::<NoticeKind, Claim>::new()));
        let work = pending.clone();
        let (wake, receiver) = crossbeam_channel::bounded(1);
        std::thread::Builder::new()
            .name("kettle-toast-receipts".into())
            .spawn(move || {
                let mut retry = false;
                loop {
                    let next = if retry {
                        receiver.recv_timeout(delay)
                    } else {
                        receiver
                            .recv()
                            .map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected)
                    };
                    let stopping =
                        matches!(next, Err(crossbeam_channel::RecvTimeoutError::Disconnected));
                    let claims: Vec<_> = work
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .values()
                        .cloned()
                        .collect();
                    for claim in claims {
                        if persist(&claim) {
                            let mut pending = work
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if pending.get(&claim.notice.kind) == Some(&claim) {
                                pending.remove(&claim.notice.kind);
                            }
                        }
                    }
                    if stopping {
                        break;
                    }
                    retry = !work
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_empty();
                }
            })?;
        Ok(Self { pending, wake })
    }

    pub fn note(&self, claim: Claim) {
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending
                .get(&claim.notice.kind)
                .is_none_or(|previous| version(&claim.notice.tag) >= version(&previous.notice.tag))
            {
                pending.insert(claim.notice.kind, claim);
            }
        }
        let _ = self.wake.try_send(());
    }

    pub fn suppresses(&self, notice: &Notice) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&notice.kind)
            .is_some_and(|claim| version(&claim.notice.tag) >= version(&notice.tag))
    }

    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// A transient toast's lifetime: `LIFETIME` of time it was presented,
/// visible and not paused (hovered or under assistive focus). Time in the
/// background, behind a dialog or under the pointer does not count. Shared
/// by the update toast and the notification toast.
///
/// A paused toast is drawn whole, even if the pause caught it fading, and
/// when the pause ends it has at least `AFTER_PAUSE` left, so its fade
/// starts again from the beginning.
pub(crate) struct ToastClock {
    elapsed: Duration,
    last_tick: Instant,
    counting: bool,
    paused: bool,
    /// A frame showing the toast reached the screen.
    pub presented: bool,
}

impl ToastClock {
    pub fn new(now: Instant) -> Self {
        Self {
            elapsed: Duration::ZERO,
            last_tick: now,
            counting: false,
            paused: false,
            presented: false,
        }
    }

    pub fn tick(&mut self, now: Instant, visible: bool, paused: bool) {
        if self.counting {
            self.elapsed = self
                .elapsed
                .saturating_add(now.saturating_duration_since(self.last_tick));
        }
        self.last_tick = now;
        if self.paused && !paused {
            self.elapsed = self.elapsed.min(LIFETIME.saturating_sub(AFTER_PAUSE));
        }
        self.paused = paused;
        self.counting = self.presented && visible && !paused;
    }

    pub fn remaining(&self) -> Duration {
        LIFETIME.saturating_sub(self.elapsed)
    }

    pub fn opacity(&self, motion: bool) -> f32 {
        if motion && !self.paused {
            (self.remaining().as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Whether the next frames redraw a fade in progress.
    pub fn fading(&self, motion: bool) -> bool {
        motion && self.counting && self.remaining() <= FADE
    }

    /// Whether a toast drawn at opacity `drawn` before the last tick needs
    /// another frame: its fade is running, or a pause starting or ending
    /// changed how it is drawn.
    pub fn needs_frame(&self, drawn: f32, motion: bool) -> bool {
        self.fading(motion) || self.opacity(motion) != drawn
    }

    pub fn wake_after(&self, motion: bool) -> Option<Duration> {
        self.counting.then(|| {
            if self.fading(motion) {
                Duration::from_millis(16)
            } else {
                self.remaining()
                    .saturating_sub(if motion { FADE } else { Duration::ZERO })
                    .max(Duration::from_millis(1))
            }
        })
    }
}

pub(crate) struct Toast {
    pub claim: Claim,
    pub window: u64,
    pub acknowledged: bool,
    pub clock: ToastClock,
}

impl Toast {
    pub fn new(claim: Claim, window: u64, now: Instant) -> Self {
        Self {
            claim,
            window,
            acknowledged: false,
            clock: ToastClock::new(now),
        }
    }

    pub fn tick(&mut self, now: Instant, visible: bool, paused: bool) {
        self.clock.tick(now, visible, paused);
    }

    pub fn remaining(&self) -> Duration {
        self.clock.remaining()
    }

    pub fn opacity(&self, motion: bool) -> f32 {
        self.clock.opacity(motion)
    }

    pub fn wake_after(&self, motion: bool) -> Option<Duration> {
        self.clock.wake_after(motion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: NoticeKind, tag: &str) -> Notice {
        Notice {
            kind,
            tag: tag.into(),
        }
    }

    #[test]
    fn delivery_is_once_per_state_across_restart_and_processes() {
        let mut ledger = Ledger::default();
        let available = notice(NoticeKind::Available, "v6.0.0");
        let claim = ledger.claim(available.clone(), 1, 10, Some(100)).unwrap();
        assert!(ledger.claim(available.clone(), 2, 11, Some(101)).is_none());
        assert!(ledger.finish(&claim, true));
        let mut restored: Ledger =
            serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        assert!(restored.claim(available, 50, 11, Some(101)).is_none());
        assert!(
            restored
                .claim(
                    notice(NoticeKind::RestartPending, "v6.0.0"),
                    50,
                    11,
                    Some(101)
                )
                .is_some()
        );
        assert!(
            restored
                .claim(notice(NoticeKind::Available, "v5.9.0"), 100, 11, Some(101))
                .is_none()
        );
    }

    #[test]
    fn abandoned_claims_expire_and_stale_acknowledgements_are_rejected() {
        let mut ledger = Ledger::default();
        let n = notice(NoticeKind::Available, "v6.0.0");
        let first = ledger.claim(n.clone(), 1, 10, Some(100)).unwrap();
        let next = ledger.claim(n.clone(), 31, 10, Some(101)).unwrap();
        assert!(!ledger.finish(&first, false));
        assert!(ledger.finish(&next, false));
        assert!(ledger.claim(n, 32, 12, None).is_some());
    }

    #[test]
    fn a_newer_release_in_the_same_state_is_not_suppressed() {
        let mut ledger = Ledger::default();
        let first = ledger
            .claim(notice(NoticeKind::Available, "v99.0.0"), 100, 1, None)
            .unwrap();
        assert!(ledger.finish(&first, true));
        assert!(
            ledger
                .claim(notice(NoticeKind::Available, "v99.0.1"), 101, 1, None)
                .is_some()
        );
    }

    #[test]
    fn late_presented_receipts_preserve_new_claims_and_newer_seen_versions() {
        let mut ledger = Ledger::default();
        let old = ledger
            .claim(notice(NoticeKind::Available, "v6.0.0"), 1, 1, None)
            .unwrap();
        let new = ledger
            .claim(notice(NoticeKind::Available, "v6.0.1"), 31, 2, None)
            .unwrap();
        assert!(ledger.finish(&old, true));
        assert_eq!(ledger.busy_until(32), Some(new.until));
        assert!(ledger.finish(&new, true));
        assert!(ledger.finish(&old, true));
        assert!(
            ledger
                .claim(notice(NoticeKind::Available, "v6.0.1"), 100, 3, None)
                .is_none()
        );
    }

    #[test]
    fn failed_receipts_are_retried_without_another_presentation() {
        let (events, receiver) = std::sync::mpsc::channel();
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let writer = ReceiptWriter::with_retry(Duration::from_millis(10), move |claim| {
            let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            events.send((attempt, claim.notice.clone())).unwrap();
            attempt > 0
        })
        .unwrap();
        let claim = Ledger::default()
            .claim(notice(NoticeKind::Failed, "v6.0.0"), 1, 1, None)
            .unwrap();
        writer.note(claim.clone());
        for expected in [0, 1] {
            let (attempt, received) = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(attempt, expected);
            assert_eq!(received, claim.notice);
        }
    }

    #[test]
    fn receipt_queue_coalesces_versions_to_three_state_watermarks() {
        let (wake, _receiver) = crossbeam_channel::bounded(1);
        let writer = ReceiptWriter {
            pending: Default::default(),
            wake,
        };
        for version in (0..100).rev() {
            for kind in [
                NoticeKind::Available,
                NoticeKind::Failed,
                NoticeKind::RestartPending,
            ] {
                let claim = Ledger::default()
                    .claim(notice(kind, &format!("v6.0.{version}")), 1, 1, None)
                    .unwrap();
                writer.note(claim);
            }
        }
        let pending = writer.pending.lock().unwrap();
        assert_eq!(pending.len(), 3);
        assert!(pending.values().all(|claim| claim.notice.tag == "v6.0.99"));
    }

    #[test]
    fn a_backwards_wall_clock_does_not_strand_a_delivery_claim() {
        let mut ledger = Ledger::default();
        let first = ledger
            .claim(notice(NoticeKind::Available, "v6.0.0"), 100, 1, None)
            .unwrap();
        let second = ledger
            .claim(notice(NoticeKind::Available, "v6.0.0"), 1, 2, None)
            .unwrap();
        assert!(!ledger.finish(&first, false));
        assert!(ledger.finish(&second, true));
    }

    /// Under reduced motion a toast stays fully opaque until it goes and
    /// asks for no fade frames; with motion it fades over its last 150 ms.
    #[test]
    fn reduced_motion_keeps_a_toast_opaque_until_it_goes() {
        let now = Instant::now();
        let mut clock = ToastClock::new(now);
        clock.presented = true;
        clock.tick(now, true, false);
        clock.tick(now + LIFETIME - Duration::from_millis(50), true, false);
        assert!(clock.opacity(true) < 0.5);
        assert_eq!(clock.opacity(false), 1.0);
        assert_eq!(clock.wake_after(true), Some(Duration::from_millis(16)));
        assert_eq!(clock.wake_after(false), Some(Duration::from_millis(50)));
    }

    /// Hovered or focused by assistive technology during its fade, a toast
    /// is drawn whole at once, not frozen part-faded. When the pause ends
    /// it has time to be read again, and then fades from the start.
    #[test]
    fn a_toast_paused_while_fading_is_whole_and_fades_again_from_the_start() {
        let now = Instant::now();
        let mut clock = ToastClock::new(now);
        clock.presented = true;
        clock.tick(now, true, false);
        let fading = now + LIFETIME - Duration::from_millis(55);
        clock.tick(fading, true, false);
        let drawn = clock.opacity(true);
        assert!(drawn < 0.5, "{drawn}");
        clock.tick(fading + Duration::from_millis(5), true, true);
        assert_eq!(clock.opacity(true), 1.0, "a paused toast is drawn whole");
        assert!(
            clock.needs_frame(drawn, true),
            "and gets a frame to show it"
        );
        assert_eq!(clock.wake_after(true), None);
        let resumed = fading + Duration::from_secs(60);
        clock.tick(resumed, true, true);
        assert!(!clock.needs_frame(1.0, true), "a held toast draws nothing");
        clock.tick(resumed, true, false);
        assert_eq!(clock.remaining(), AFTER_PAUSE);
        assert_eq!(clock.opacity(true), 1.0);
        assert!(!clock.needs_frame(1.0, true));
        assert_eq!(clock.wake_after(true), Some(AFTER_PAUSE - FADE));
        clock.tick(resumed + AFTER_PAUSE - FADE / 2, true, false);
        let half = clock.opacity(true);
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert!(clock.needs_frame(1.0, true));
        // Without motion there is no fade to restart, and the pause still
        // leaves time to read it.
        let mut still = ToastClock::new(now);
        still.presented = true;
        still.tick(now, true, false);
        still.tick(now + LIFETIME - Duration::from_millis(10), true, true);
        still.tick(now + LIFETIME, true, false);
        assert_eq!(still.remaining(), AFTER_PAUSE);
        assert_eq!(still.opacity(false), 1.0);
        assert_eq!(still.wake_after(false), Some(AFTER_PAUSE));
    }

    #[test]
    fn background_time_and_hover_do_not_expire_the_toast() {
        let now = Instant::now();
        let claim = Ledger::default()
            .claim(notice(NoticeKind::Available, "v6.0.0"), 1, 1, None)
            .unwrap();
        let mut toast = Toast::new(claim, 1, now);
        toast.tick(now + Duration::from_secs(100), true, false);
        assert_eq!(toast.remaining(), LIFETIME);
        toast.clock.presented = true;
        toast.tick(now + Duration::from_secs(100), true, false);
        toast.tick(now + Duration::from_secs(102), false, false);
        assert_eq!(toast.remaining(), Duration::from_secs(5));
        toast.tick(now + Duration::from_secs(202), true, true);
        assert_eq!(toast.remaining(), Duration::from_secs(5));
        assert_eq!(toast.wake_after(false), None);
        toast.tick(now + Duration::from_secs(302), true, false);
        assert_eq!(toast.wake_after(false), Some(Duration::from_secs(5)));
        toast.tick(now + Duration::from_secs(307), true, false);
        assert_eq!(toast.remaining(), Duration::ZERO);
    }
}
