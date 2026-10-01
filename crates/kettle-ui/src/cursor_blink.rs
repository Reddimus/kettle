//! The cursor blink's timeline, and the state of a window whose blink the
//! window server animates.
//!
//! `App::about_to_wait_inner` blinks the cursor by redrawing at every
//! half-period edge. On macOS a window that has been quiet for a half-period
//! can hand the blink to a Core Animation layer instead (`macos_cursor_layer`):
//! Kettle then presents no frame and does not wake until something else needs
//! one. Everything here is pure, so the layer's plan and the phase the app
//! resumes from are checked against the scheduler on every platform's CI.
//!
//! The scheduler's rules, which these functions replay with on-time wakes:
//! an edge falls `interval` after the previous one and flips the phase, and
//! the blink stops for good at the first edge that finds the cursor visible
//! at or after `activity + timeout`, so an idle blink always rests visible.

use std::time::{Duration, Instant};

/// Whether an idle cursor has stopped blinking. It stops only in its visible
/// phase, so the final toggle always leaves the cursor showing. Each blink
/// repaints the window, and on macOS a window that keeps drawing keeps its GPU
/// driver memory resident, so an idle window should settle.
pub(crate) fn cursor_blink_timed_out(
    idle: Duration,
    timeout: Option<Duration>,
    cursor_on: bool,
) -> bool {
    cursor_on && timeout.is_some_and(|timeout| idle >= timeout)
}

pub(crate) fn next_cursor_blink_phase(
    active: bool,
    elapsed: Duration,
    interval: Duration,
    current: bool,
) -> Option<bool> {
    (active && elapsed >= interval).then_some(!current)
}

/// The phase and the last edge the scheduler would hold at `now`, had it woken
/// on time at every edge since `last_edge`.
///
/// Composes: materializing to `t1` and then to `t2` gives the same result as
/// materializing straight to `t2`, so a redraw that ends up presenting nothing
/// can materialize again later.
pub(crate) fn materialize(
    on: bool,
    last_edge: Instant,
    activity: Instant,
    interval: Duration,
    timeout: Option<Duration>,
    now: Instant,
) -> (bool, Instant) {
    let step = interval.as_nanos();
    if step == 0 || now <= last_edge {
        return (on, last_edge);
    }
    let due = now.duration_since(last_edge).as_nanos() / step;
    let taken = match timeout.and_then(|timeout| activity.checked_add(timeout)) {
        None => due,
        Some(stop) => {
            // Edge k (1-based) falls at `last_edge + k * step`. The first edge
            // at or after `stop` ends the blink if the cursor is visible
            // before it; if it is hidden, that edge shows it and the next one
            // ends the blink.
            let first_late = if stop <= last_edge {
                1
            } else {
                stop.duration_since(last_edge)
                    .as_nanos()
                    .div_ceil(step)
                    .max(1)
            };
            let visible_before = |k: u128| on ^ ((k - 1) % 2 == 1);
            let stop_edge = if visible_before(first_late) {
                first_late
            } else {
                first_late + 1
            };
            due.min(stop_edge - 1)
        }
    };
    let advanced = u64::try_from(taken.saturating_mul(step)).unwrap_or(u64::MAX);
    (
        on ^ (taken % 2 == 1),
        last_edge
            .checked_add(Duration::from_nanos(advanced))
            .unwrap_or(last_edge),
    )
}

/// When the blink next changes what is drawn after `now`, or `None` once it
/// has stopped on its visible phase.
pub(crate) fn next_edge(
    on: bool,
    last_edge: Instant,
    activity: Instant,
    interval: Duration,
    timeout: Option<Duration>,
    now: Instant,
) -> Option<Instant> {
    if interval.is_zero() {
        return None;
    }
    let (on, edge) = materialize(on, last_edge, activity, interval, timeout, now);
    let next = edge.checked_add(interval)?;
    let stops = on
        && timeout
            .and_then(|timeout| activity.checked_add(timeout))
            .is_some_and(|stop| next >= stop);
    (!stops).then_some(next)
}

/// What a Core Animation layer must animate to show the same blink the
/// scheduler would draw from a hand-off edge onwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayerBlinkPlan {
    /// The on-to-off edge the hand-off frame drew. The layer's first
    /// half-period, which hides the cursor, starts here.
    pub(crate) begin: Instant,
    /// Half-periods the layer animates before it rests visible, always odd so
    /// the last one hides the cursor. `None` blinks until a frame ends it
    /// (`cursor-blink-timeout = 0`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) half_periods: Option<u64>,
    pub(crate) interval: Duration,
    /// The idle timeout the plan was made with, kept so the phase is
    /// materialized with the same rules the layer animates.
    pub(crate) timeout: Option<Duration>,
    /// The blink activity time the plan was made with.
    pub(crate) activity: Instant,
}

/// The plan for a layer that takes the blink over at `last_edge`, or `None`
/// unless that edge just hid the cursor.
///
/// Only on-to-off edges hand over: the layer's animation then starts hidden,
/// so the patch it shows first becomes visible a whole half-period after the
/// hand-off frame, long after both the patch and the frame beneath it reach
/// the screen, and entry needs no synchronization.
pub(crate) fn layer_plan(
    on: bool,
    last_edge: Instant,
    activity: Instant,
    interval: Duration,
    timeout: Option<Duration>,
) -> Option<LayerBlinkPlan> {
    if on || interval.is_zero() {
        return None;
    }
    let half_periods = match timeout {
        None => None,
        Some(timeout) => {
            // The hidden half-period starting at `last_edge`, then one more
            // per later off edge `last_edge + 2jI` that falls before the stop.
            let stop = activity.checked_add(timeout)?;
            let remaining = stop.saturating_duration_since(last_edge).as_nanos();
            let later_hidden = remaining
                .div_ceil(interval.as_nanos() * 2)
                .saturating_sub(1);
            Some(u64::try_from(later_hidden * 2 + 1).ok()?)
        }
    };
    Some(LayerBlinkPlan {
        begin: last_edge,
        half_periods,
        interval,
        timeout,
        activity,
    })
}

/// The discrete opacity keyframes that play a [`LayerBlinkPlan`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlinkAnimation {
    /// Opacity for each half of one period: hidden first, then visible.
    pub(crate) values: [f32; 2],
    /// Discrete key times: one more than the values, from 0 to 1.
    pub(crate) key_times: [f32; 3],
    /// One whole period, in seconds.
    pub(crate) duration: f64,
    /// Periods to play; a half means it ends after a hidden half-period, and
    /// the layer's model opacity (1) then shows the cursor for good.
    pub(crate) repeat_count: f32,
    /// When the first period starts, on the `CACurrentMediaTime` clock.
    pub(crate) begin_time: f64,
}

impl LayerBlinkPlan {
    /// The keyframes, given one reading of each clock taken together.
    /// `CACurrentMediaTime` and `Instant` both count Mach absolute time on
    /// macOS, so paired readings establish the offset between them.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn animation(&self, now: Instant, media_now: f64) -> BlinkAnimation {
        let elapsed = now.saturating_duration_since(self.begin).as_secs_f64();
        BlinkAnimation {
            values: [0.0, 1.0],
            key_times: [0.0, 0.5, 1.0],
            duration: self.interval.as_secs_f64() * 2.0,
            repeat_count: match self.half_periods {
                None => f32::INFINITY,
                Some(half_periods) => half_periods as f32 / 2.0,
            },
            begin_time: media_now - elapsed,
        }
    }

    /// Keep entry in the first hidden half-period, with half of it left for
    /// the patch drawable to reach the screen before it becomes visible.
    pub(crate) fn can_start(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.begin) <= self.interval / 2
    }

    /// Whether the layer shows the cursor at `at`.
    #[cfg(test)]
    pub(crate) fn visible_at(&self, at: Instant) -> bool {
        let Some(elapsed) = at.checked_duration_since(self.begin) else {
            return false;
        };
        let half = elapsed.as_nanos() / self.interval.as_nanos();
        match self.half_periods {
            Some(half_periods) if half >= u128::from(half_periods) => true,
            _ => half % 2 == 1,
        }
    }
}

/// Everything that must hold at an on-to-off edge before the window server
/// takes the blink over. Any of these would otherwise present a frame soon,
/// and every frame ends the layer's blink, so the hand-off would be wasted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HandoffInputs {
    /// `macos-cursor-blink-layer` is on, the platform has the layer, and this
    /// window has not fallen back to GPU blink.
    pub(crate) enabled: bool,
    /// This edge hid the cursor.
    pub(crate) edge_hides_cursor: bool,
    /// The scheduler's own gate: focused, visible, blinking, not timed out.
    pub(crate) blink_active: bool,
    /// The focused application cursor is visible, in the viewport and not a vi cursor.
    pub(crate) cursor_eligible: bool,
    /// No frame but the blink's own was presented since the previous edge.
    pub(crate) quiet: bool,
    /// A bell, program or background animation, autoscroll, the resize chip,
    /// a media receipt or a completion timer will draw again.
    pub(crate) other_animation: bool,
    /// An input method is composing text at the cursor.
    pub(crate) ime_preedit: bool,
    /// A modal holds the cursor steady.
    pub(crate) modal_steady: bool,
    /// Deferred output, a screenshot, a resize, frame recovery or an
    /// accessibility update is waiting for a frame.
    pub(crate) work_pending: bool,
}

/// How long a window must have drawn nothing but its blink before an
/// on-to-off edge hands the blink over: one half-period, so output and typing
/// never churn the layer.
///
/// With on-to-off entry only, the hand-off lands between one and three
/// half-periods after the last other frame (1.59 s at the default 530 ms),
/// and the driver pools go about 1.2 s after that. A shorter window, or entry
/// at off-to-on edges too, brings the worst case closer to the last frame.
pub(crate) fn quiet_window(interval: Duration) -> Duration {
    interval
}

pub(crate) fn handoff_allowed(inputs: HandoffInputs) -> bool {
    inputs.enabled
        && inputs.edge_hides_cursor
        && inputs.blink_active
        && inputs.cursor_eligible
        && inputs.quiet
        && !inputs.other_animation
        && !inputs.ime_preedit
        && !inputs.modal_steady
        && !inputs.work_pending
}

/// A device-pixel rect `[x, y, w, h]` (top-left origin, surface space) as a
/// Core Animation frame `[x, y, w, h]` in points in the root layer's
/// coordinates. An unflipped root counts y up from its bottom edge.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn layer_frame_points(
    rect_px: [u32; 4],
    scale: f64,
    root_height: f64,
    root_flipped: bool,
) -> [f64; 4] {
    let [x, y, w, h] = rect_px.map(f64::from);
    let (x, y, w, h) = (x / scale, y / scale, w / scale, h / scale);
    let y = if root_flipped { y } else { root_height - y - h };
    [x, y, w, h]
}

/// Which of the layer's margins stretch when the root resizes, so the patch
/// stays pinned to the top-left until the app's resize handler hides it.
/// Bits follow `CAAutoresizingMask`:
/// max-x margin (4) always, and the bottom margin, max-y (32) in a flipped
/// root or min-y (8) in an unflipped one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn pinned_autoresizing_mask(root_flipped: bool) -> u32 {
    const MAX_X_MARGIN: u32 = 1 << 2;
    const MIN_Y_MARGIN: u32 = 1 << 3;
    const MAX_Y_MARGIN: u32 = 1 << 5;
    MAX_X_MARGIN
        | if root_flipped {
            MAX_Y_MARGIN
        } else {
            MIN_Y_MARGIN
        }
}

/// Who draws a window's blink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    /// Kettle redraws at every edge.
    #[default]
    Gpu,
    /// The next frame is an on-to-off edge that may hand the blink over.
    PendingHandoff,
    /// The window server animates the blink. `anchor` is `last_blink` as the
    /// app last materialized it; any other value means a writer (a reset, a
    /// DEC mode 12 change) has taken over since.
    Active {
        anchor: Instant,
        plan: LayerBlinkPlan,
        rect_px: [u32; 4],
    },
}

/// Exit-frame durations, the last 64, for `ui_geometry`.
#[derive(Clone, Debug)]
pub(crate) struct ExitFrameTimes {
    recent_us: [u32; 64],
    count: u64,
    max_us: u32,
}

impl Default for ExitFrameTimes {
    fn default() -> Self {
        Self {
            recent_us: [0; 64],
            count: 0,
            max_us: 0,
        }
    }
}

impl ExitFrameTimes {
    fn record(&mut self, frame: Duration) {
        let us = u32::try_from(frame.as_micros()).unwrap_or(u32::MAX);
        self.recent_us[(self.count % 64) as usize] = us;
        self.count = self.count.saturating_add(1);
        self.max_us = self.max_us.max(us);
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn max_us(&self) -> u32 {
        self.max_us
    }

    /// The nearest-rank percentile of the recent exit frames, or `None`
    /// before the first one.
    pub(crate) fn percentile_us(&self, percent: u32) -> Option<u32> {
        let held = self.count.min(64) as usize;
        if held == 0 {
            return None;
        }
        let mut recent = self.recent_us[..held].to_vec();
        recent.sort_unstable();
        let rank = (held * percent.min(100) as usize).div_ceil(100).max(1);
        recent.get(rank - 1).copied()
    }
}

/// One window's side of the layer blink: who draws it now, whether the window
/// has been quiet, and counters for `ui_geometry`.
#[derive(Clone, Debug, Default)]
pub(crate) struct BlinkLayer {
    mode: Mode,
    /// When the last frame that was not an edge's own blink frame presented.
    last_other_frame: Option<Instant>,
    /// The next presented frame is the last edge's own blink frame.
    edge_frame_pending: bool,
    /// A refused patch waits for a non-blink frame before trying again.
    handoff_blocked: bool,
    /// Why this window gave up on the layer, once it has.
    fallback: Option<&'static str>,
    pub(crate) handoffs: u64,
    /// Frames that ended the layer's blink, presented in step with hiding it.
    pub(crate) exits: u64,
    /// Times the layer was hidden without a frame: focus loss, occlusion, a
    /// scale change, a renderer rebuild, or the key turned off.
    pub(crate) hides: u64,
    pub(crate) exit_frames: ExitFrameTimes,
}

impl BlinkLayer {
    pub(crate) fn is_active(&self) -> bool {
        matches!(self.mode, Mode::Active { .. })
    }

    /// The anchor, plan and patch rect while the window server blinks.
    pub(crate) fn active(&self) -> Option<(Instant, LayerBlinkPlan, [u32; 4])> {
        match self.mode {
            Mode::Active {
                anchor,
                plan,
                rect_px,
            } => Some((anchor, plan, rect_px)),
            _ => None,
        }
    }

    pub(crate) fn fallback(&self) -> Option<&'static str> {
        self.fallback
    }

    /// Record a blink edge at `now` and return whether only blink frames
    /// presented in the `quiet` before it. Any hand-off a previous edge asked
    /// for and no frame took is dropped: it belonged to that edge.
    pub(crate) fn note_edge(&mut self, now: Instant, quiet: Duration) -> bool {
        self.edge_frame_pending = true;
        if self.mode == Mode::PendingHandoff {
            self.mode = Mode::Gpu;
        }
        !self.handoff_blocked
            && self
                .last_other_frame
                .is_none_or(|at| now.saturating_duration_since(at) >= quiet)
    }

    /// Record a frame presented at `now`. The first one after an edge is that
    /// edge's blink frame; any other one restarts the quiet.
    pub(crate) fn note_presented(&mut self, now: Instant) {
        if !std::mem::take(&mut self.edge_frame_pending) {
            self.last_other_frame = Some(now);
            self.handoff_blocked = false;
        }
    }

    /// Ask the next frame to hand the blink over.
    pub(crate) fn request_handoff(&mut self) {
        if self.mode == Mode::Gpu && self.fallback.is_none() && !self.handoff_blocked {
            self.mode = Mode::PendingHandoff;
        }
    }

    /// Whether this frame should hand the blink over. Consumes the request.
    pub(crate) fn take_pending_handoff(&mut self) -> bool {
        let pending = self.mode == Mode::PendingHandoff;
        if pending {
            self.mode = Mode::Gpu;
        }
        pending
    }

    /// Back off after an ineligible patch or refused animation. Blink frames
    /// alone cannot change the eligibility that caused the failure.
    pub(crate) fn failed_handoff(&mut self) {
        self.handoff_blocked = true;
    }

    /// A hand-off frame was presented and the layer now blinks.
    pub(crate) fn started(&mut self, plan: LayerBlinkPlan, rect_px: [u32; 4]) {
        self.handoff_blocked = false;
        self.mode = Mode::Active {
            anchor: plan.begin,
            plan,
            rect_px,
        };
        self.handoffs = self.handoffs.saturating_add(1);
    }

    /// Resume the layer timeline only while no writer has replaced its anchor.
    pub(crate) fn materialize(
        &mut self,
        on: bool,
        last_blink: Instant,
        now: Instant,
    ) -> Option<(bool, Instant)> {
        let (anchor, plan, _) = self.active()?;
        if last_blink != anchor {
            return None;
        }
        let phase = materialize(
            on,
            last_blink,
            plan.activity,
            plan.interval,
            plan.timeout,
            now,
        );
        if let Mode::Active { anchor, .. } = &mut self.mode {
            *anchor = phase.1;
        }
        Some(phase)
    }

    /// A frame presented in step with hiding the layer ended its blink.
    pub(crate) fn exited(&mut self, frame: Duration) {
        if self.is_active() {
            self.mode = Mode::Gpu;
            self.exits = self.exits.saturating_add(1);
            self.exit_frames.record(frame);
        }
    }

    /// The layer was hidden without a frame.
    pub(crate) fn hidden(&mut self) {
        if self.is_active() {
            self.hides = self.hides.saturating_add(1);
        }
        self.mode = Mode::Gpu;
    }

    /// Give up on the layer for this window. Returns true the first time, so
    /// the caller logs once.
    pub(crate) fn fall_back(&mut self, reason: &'static str) -> bool {
        self.mode = Mode::Gpu;
        self.fallback.replace(reason).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// The GPU scheduler, stepped with on-time wakes every `tick`: the same two
    /// production rules that `about_to_wait_inner` applies.
    struct Scheduler {
        on: bool,
        last: Instant,
        activity: Instant,
        interval: Duration,
        timeout: Option<Duration>,
        stopped: bool,
    }

    impl Scheduler {
        fn step_to(&mut self, now: Instant) {
            let timed_out = cursor_blink_timed_out(
                now.saturating_duration_since(self.activity),
                self.timeout,
                self.on,
            );
            if timed_out {
                self.stopped = true;
            }
            if let Some(on) = next_cursor_blink_phase(
                !self.stopped,
                now.saturating_duration_since(self.last),
                self.interval,
                self.on,
            ) {
                self.on = on;
                self.last = now;
            }
        }
    }

    /// A small deterministic generator, so the property test needs no crate.
    struct SplitMix(u64);

    impl SplitMix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    /// The layer's plan, taken over at an on-to-off edge, shows exactly what
    /// the GPU scheduler would have drawn at every millisecond until well past
    /// the timeout, for intervals across the whole 50-5000 ms config range,
    /// timeouts of 0 (endless) or 1-60 s, and random activity offsets.
    #[test]
    fn layer_plan_replays_the_gpu_blink_timeline() {
        let mut rng = SplitMix(7);
        let origin = Instant::now();
        for case in 0..200 {
            let interval = MS * (50 + rng.below(4951) as u32);
            let timeout = match rng.below(4) {
                0 => None,
                _ => Some(Duration::from_secs(1 + rng.below(60))),
            };
            let activity = origin + MS * rng.below(3000) as u32;
            // Run the scheduler from the activity until an on-to-off edge.
            let mut gpu = Scheduler {
                on: true,
                last: activity,
                activity,
                interval,
                timeout,
                stopped: false,
            };
            let mut now = activity;
            let skip = rng.below(6);
            let mut offs = 0;
            let entry = loop {
                now += MS;
                let was_on = gpu.on;
                gpu.step_to(now);
                if was_on && !gpu.on {
                    if offs == skip {
                        break Some(now);
                    }
                    offs += 1;
                }
                if gpu.stopped || now > activity + Duration::from_secs(200) {
                    break None;
                }
            };
            let Some(entry) = entry else { continue };
            let plan = layer_plan(false, entry, activity, interval, timeout)
                .expect("an on-to-off edge has a plan");
            let horizon = timeout.map_or(Duration::from_secs(30), |t| t + interval * 4);
            let end = activity + horizon;
            let mut t = entry;
            while t < end {
                t += MS;
                gpu.step_to(t);
                assert_eq!(
                    plan.visible_at(t),
                    gpu.on,
                    "case {case}: interval {interval:?} timeout {timeout:?} at {:?} after entry",
                    t - entry
                );
                // The app resumes from exactly what the scheduler holds.
                let (on, _) = materialize(false, entry, activity, interval, timeout, t);
                assert_eq!(on, gpu.on, "case {case}: materialize at {:?}", t - entry);
            }
        }
    }

    #[test]
    fn a_plan_ends_on_a_hidden_half_period_and_rests_visible() {
        let origin = Instant::now();
        let interval = MS * 530;
        let activity = origin;
        // The first on-to-off edge, 530 ms after activity, with the default
        // 10 s timeout.
        let edge = origin + interval;
        let plan = layer_plan(
            false,
            edge,
            activity,
            interval,
            Some(Duration::from_secs(10)),
        )
        .unwrap();
        // Off edges before the 10 s stop: 530, 1590, ..., 9010 ms.
        assert_eq!(plan.half_periods, Some(17));
        let anim = plan.animation(edge, 100.0);
        assert_eq!(anim.values, [0.0, 1.0]);
        assert_eq!(anim.key_times, [0.0, 0.5, 1.0]);
        assert_eq!(anim.duration, 1.06);
        assert_eq!(anim.repeat_count, 8.5);
        assert_eq!(anim.begin_time, 100.0);
        assert!(plan.visible_at(edge + interval * 17 + MS * 5000));
        // No plan from a visible phase, and an endless one without a timeout.
        assert!(layer_plan(true, edge, activity, interval, None).is_none());
        let endless = layer_plan(false, edge, activity, interval, None).unwrap();
        assert_eq!(endless.animation(edge, 0.0).repeat_count, f32::INFINITY);
    }

    #[test]
    fn a_late_hand_off_starts_the_animation_in_the_past() {
        let origin = Instant::now();
        let plan = layer_plan(false, origin, origin, MS * 500, None).unwrap();
        let anim = plan.animation(origin + MS * 40, 10.0);
        assert!((anim.begin_time - 9.96).abs() < 1e-9);
    }

    /// Materializing after any gap equals stepping the scheduler through it,
    /// and composes: t1 then t2 is t2.
    #[test]
    fn materialize_matches_the_scheduler_after_any_gap() {
        let mut rng = SplitMix(12);
        let origin = Instant::now();
        for _ in 0..300 {
            let interval = MS * (50 + rng.below(1000) as u32);
            let timeout = (rng.below(3) != 0).then(|| MS * (500 + rng.below(20_000) as u32));
            let activity = origin;
            let on = rng.below(2) == 0;
            let last = origin + MS * rng.below(2000) as u32;
            let mut gpu = Scheduler {
                on,
                last,
                activity,
                interval,
                timeout,
                stopped: on && timeout.is_some_and(|t| last - activity >= t),
            };
            let gap = MS * rng.below(40_000) as u32;
            let mut t = last;
            while t < last + gap {
                t += MS;
                gpu.step_to(t);
            }
            let (m_on, m_edge) = materialize(on, last, activity, interval, timeout, last + gap);
            assert_eq!(m_on, gpu.on, "phase after {gap:?}");
            assert_eq!(m_edge, gpu.last, "edge after {gap:?}");
            let mid = last + gap / 3;
            let (a_on, a_edge) = materialize(on, last, activity, interval, timeout, mid);
            assert_eq!(
                materialize(a_on, a_edge, activity, interval, timeout, last + gap),
                (m_on, m_edge)
            );
        }
    }

    /// A reset (`reset_blink_phase`) or a DEC mode 12 change writes
    /// `last_blink` while the layer blinks. The anchor no longer matches, so
    /// the app keeps the writer's phase instead of materializing over it.
    #[test]
    fn a_reset_or_mode_12_during_the_layer_blink_wins_over_materialize() {
        let origin = Instant::now();
        let plan = layer_plan(false, origin, origin, MS * 500, None).unwrap();
        let mut layer = BlinkLayer::default();
        layer.started(plan, [0, 0, 8, 16]);
        let written = origin + MS * 700;
        assert_eq!(layer.materialize(true, written, origin + MS * 1200), None);
        assert_eq!(layer.active().unwrap().0, origin);
        assert_eq!(
            layer.materialize(false, origin, origin + MS * 700),
            Some((true, origin + MS * 500))
        );
        assert_eq!(layer.active().unwrap().0, origin + MS * 500);
        assert_eq!(
            layer.materialize(true, origin + MS * 500, origin + MS * 1200),
            Some((false, origin + MS * 1000))
        );
    }

    #[test]
    fn handoff_requires_every_condition() {
        let all = HandoffInputs {
            enabled: true,
            edge_hides_cursor: true,
            blink_active: true,
            cursor_eligible: true,
            quiet: true,
            other_animation: false,
            ime_preedit: false,
            modal_steady: false,
            work_pending: false,
        };
        assert!(handoff_allowed(all));
        let flips: [fn(&mut HandoffInputs); 9] = [
            |i| i.enabled = false,
            |i| i.edge_hides_cursor = false,
            |i| i.blink_active = false,
            |i| i.cursor_eligible = false,
            |i| i.quiet = false,
            |i| i.other_animation = true,
            |i| i.ime_preedit = true,
            |i| i.modal_steady = true,
            |i| i.work_pending = true,
        ];
        for (index, flip) in flips.iter().enumerate() {
            let mut inputs = all;
            flip(&mut inputs);
            assert!(
                !handoff_allowed(inputs),
                "condition {index} must block a hand-off"
            );
        }
    }

    #[test]
    fn quiet_means_no_frame_but_the_blinks_own_for_a_half_period() {
        let origin = Instant::now();
        let half = MS * 530;
        let quiet = quiet_window(half);
        assert_eq!(quiet, half);
        let mut layer = BlinkLayer::default();
        layer.note_presented(origin); // the first frame, not a blink's
        assert!(!layer.note_edge(origin + MS * 300, quiet));
        layer.note_presented(origin + MS * 305); // that edge's blink frame
        assert!(
            layer.note_edge(origin + MS * 830, quiet),
            "only the blink drew in the half-period before this edge"
        );
        layer.note_presented(origin + MS * 835); // this edge's blink frame
        layer.note_presented(origin + MS * 900); // typing echo
        assert!(
            !layer.note_edge(origin + MS * 1360, quiet),
            "a frame of output 460 ms ago breaks the quiet"
        );
        layer.note_presented(origin + MS * 1365);
        assert!(layer.note_edge(origin + MS * 1890, quiet));
        // A pending hand-off lasts only until the next edge.
        layer.request_handoff();
        layer.note_edge(origin + MS * 2420, quiet);
        assert!(!layer.take_pending_handoff());
        layer.request_handoff();
        assert!(layer.take_pending_handoff());
        assert!(!layer.take_pending_handoff());
    }

    #[test]
    fn refused_handoffs_wait_for_a_non_blink_frame() {
        let origin = Instant::now();
        let mut layer = BlinkLayer::default();
        assert!(layer.note_edge(origin, MS * 500));
        layer.request_handoff();
        assert!(layer.take_pending_handoff());
        layer.note_presented(origin);
        layer.failed_handoff();
        for edge in 1..=10 {
            let now = origin + MS * 500 * edge;
            assert!(!layer.note_edge(now, MS * 500));
            layer.request_handoff();
            assert!(!layer.take_pending_handoff());
            layer.note_presented(now);
        }
        layer.note_presented(origin + MS * 5100);
        assert!(!layer.note_edge(origin + MS * 5500, MS * 500));
        layer.note_presented(origin + MS * 5500);
        assert!(layer.note_edge(origin + MS * 6000, MS * 500));
        layer.request_handoff();
        assert!(layer.take_pending_handoff());
    }

    #[test]
    fn late_handoffs_leave_time_for_the_patch_drawable() {
        let origin = Instant::now();
        let plan = layer_plan(false, origin, origin, MS * 500, None).unwrap();
        assert!(plan.can_start(origin + MS * 250));
        assert!(!plan.can_start(origin + MS * 251));
        assert!(!plan.can_start(origin + MS * 500));
    }

    #[test]
    fn exits_and_hides_count_separately_and_fallback_logs_once() {
        let origin = Instant::now();
        let plan = layer_plan(false, origin, origin, MS * 500, None).unwrap();
        let mut layer = BlinkLayer::default();
        layer.started(plan, [1, 2, 3, 4]);
        layer.exited(Duration::from_micros(1500));
        assert!(!layer.is_active());
        layer.started(plan, [1, 2, 3, 4]);
        layer.hidden();
        assert_eq!((layer.handoffs, layer.exits, layer.hides), (2, 1, 1));
        assert_eq!(layer.exit_frames.percentile_us(95), Some(1500));
        assert!(layer.fall_back("surface"));
        assert!(!layer.fall_back("surface"));
        layer.request_handoff();
        assert!(
            !layer.take_pending_handoff(),
            "a window that fell back never hands off"
        );
    }

    #[test]
    fn exit_frame_percentiles_use_the_last_64() {
        let mut times = ExitFrameTimes::default();
        for us in 1..=100u64 {
            times.record(Duration::from_micros(us));
        }
        assert_eq!(times.count(), 100);
        assert_eq!(times.max_us(), 100);
        // Held: 37..=100. The nearest-rank p95 of 64 values is the 61st.
        assert_eq!(times.percentile_us(95), Some(97));
        assert_eq!(times.percentile_us(50), Some(68));
    }

    #[test]
    fn layer_frame_points_for_flipped_and_unflipped_roots() {
        // 1x, flipped: pixels are points.
        assert_eq!(
            layer_frame_points([10, 20, 9, 17], 1.0, 600.0, true),
            [10.0, 20.0, 9.0, 17.0]
        );
        // 1x, unflipped: y counts up from the bottom.
        assert_eq!(
            layer_frame_points([10, 20, 9, 17], 1.0, 600.0, false),
            [10.0, 563.0, 9.0, 17.0]
        );
        // 2x with odd pixel sizes lands on half points, which are whole
        // device pixels.
        assert_eq!(
            layer_frame_points([11, 21, 17, 33], 2.0, 300.0, true),
            [5.5, 10.5, 8.5, 16.5]
        );
        assert_eq!(
            layer_frame_points([11, 21, 17, 33], 2.0, 300.0, false),
            [5.5, 273.0, 8.5, 16.5]
        );
        assert_eq!(pinned_autoresizing_mask(true), 4 | 32);
        assert_eq!(pinned_autoresizing_mask(false), 4 | 8);
    }

    #[test]
    fn next_edge_stops_with_the_blink() {
        let origin = Instant::now();
        let interval = MS * 500;
        let timeout = Some(Duration::from_secs(2));
        assert_eq!(
            next_edge(true, origin, origin, interval, timeout, origin + MS * 100),
            Some(origin + interval)
        );
        // Visible at 2000 ms with the timeout reached: no more edges.
        assert_eq!(
            next_edge(true, origin, origin, interval, timeout, origin + MS * 2600),
            None
        );
        assert_eq!(
            next_edge(true, origin, origin, interval, None, origin + MS * 2600),
            Some(origin + MS * 3000)
        );
    }
}
