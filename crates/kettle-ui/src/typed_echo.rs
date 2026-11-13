//! Typed-echo paint diagnostics.
//!
//! Some shells answer one keystroke with more than one PTY write. With
//! zsh-autosuggestions in its default async mode, a key that changes the
//! suggestion is echoed first as the typed text with the old suggestion
//! erased, then, about a millisecond later, as the new gray suggestion. Kettle
//! paints typed echo as soon as it wakes, so it can present the first write on
//! its own, and the swapchain keeps that frame on screen for a whole refresh
//! before the second write replaces it. These counts say how often that
//! happens, so a live A/B can measure it rather than infer it.
//!
//! Times come from each pane's publication stamp
//! (`Terminal::output_published_at`), not from when the UI thread gets to a
//! wake, which can be after a whole frame of rendering.

use std::time::{Duration, Instant};

/// The longest gap between the first and last write of one split echo that
/// counts as one echo; also the window an intermediate frame is judged in.
///
/// From the 2026-11-13 headless PTY probe (oh-my-zsh with `git` and
/// `zsh-autosuggestions`, synthetic history, 792 keys at 20-120 ms gaps, 192
/// split into two writes): first-to-last span p50 0.6 ms, p95 1.8 ms, and 97%
/// within 3 ms. Every longer span came with a stalled first write under heavy
/// load. It must stay below the 4 ms minimum output frame budget.
pub(crate) const ECHO_SETTLE: Duration = Duration::from_millis(3);

/// Counts since the window opened. Counts only, never content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TypedEchoCounts {
    /// Keystroke echoes: the first output wake after a new keystroke.
    pub(crate) echoes: u64,
    /// Echoes whose pane published again within [`ECHO_SETTLE`] of the
    /// first publication, before the next keystroke.
    pub(crate) split_echoes: u64,
    /// Frames presented within the typing window of a keystroke, for any
    /// reason. Per keystroke, this over `echoes`.
    pub(crate) frames_presented: u64,
    /// Of those, frames that painted new PTY output.
    pub(crate) output_frames: u64,
    /// Output frames that a newer publication of a painted pane superseded
    /// within [`ECHO_SETTLE`] of the frame's capture, before the next
    /// keystroke: the in-between state of a split echo reached the screen.
    pub(crate) intermediate_frames: u64,
    /// Output frames superseded the same way, later than [`ECHO_SETTLE`] but
    /// within one output frame budget: on screen for about one refresh.
    pub(crate) late_superseded_frames: u64,
}

/// One pane's output at a sample: id, output generation, publication time.
pub(crate) type PanePublication = (u64, u64, Option<Instant>);

#[derive(Debug)]
struct EchoOrigin {
    /// The keystroke this echo answers.
    typed_at: Option<Instant>,
    /// Panes with new output at the echo's first wake.
    panes: Vec<PanePublication>,
    split: bool,
}

#[derive(Debug)]
struct FrameWatch {
    captured_at: Instant,
    typed_at: Option<Instant>,
    budget: Duration,
    /// Each painted pane's generation, sampled under its terminal lock.
    panes: Vec<(u64, u64)>,
}

/// Per-window typed-echo state.
#[derive(Debug, Default)]
pub(crate) struct TypedEcho {
    counts: TypedEchoCounts,
    origin: Option<EchoOrigin>,
    watch: Option<FrameWatch>,
    /// Generations the frame being drawn captured, reused across frames.
    captured: Vec<(u64, u64)>,
}

impl TypedEcho {
    pub(crate) fn counts(&self) -> TypedEchoCounts {
        self.counts
    }

    /// An output wake inside the typing window. Returns whether it is the
    /// first wake after a new keystroke (a new echo) rather than a later
    /// write of the same one.
    pub(crate) fn typed_wake(
        &mut self,
        last_typed: Option<Instant>,
        new_output: impl IntoIterator<Item = PanePublication>,
    ) -> bool {
        if self
            .origin
            .as_ref()
            .is_some_and(|origin| origin.typed_at == last_typed)
        {
            return false;
        }
        self.counts.echoes += 1;
        self.origin = Some(EchoOrigin {
            typed_at: last_typed,
            panes: new_output.into_iter().collect(),
            split: false,
        });
        true
    }

    /// Compare the current output of the panes being watched with what the
    /// echo's first wake and the last presented frame saw. Call it at every
    /// output wake and after every present. Returns the pane whose echo this
    /// call found split, the first time it does.
    pub(crate) fn observe(
        &mut self,
        now: Instant,
        last_typed: Option<Instant>,
        current: impl Fn(u64) -> Option<(u64, Option<Instant>)>,
    ) -> Option<u64> {
        let mut split_pane = None;
        if let Some(origin) = self.origin.as_mut()
            && !origin.split
            && origin.typed_at == last_typed
        {
            split_pane = origin.panes.iter().find_map(|&(id, generation, first)| {
                let (now_generation, latest) = current(id)?;
                let gap = latest?.saturating_duration_since(first?);
                (now_generation > generation && gap <= ECHO_SETTLE).then_some(id)
            });
            if split_pane.is_some() {
                origin.split = true;
                self.counts.split_echoes += 1;
            }
        }
        if let Some(watch) = self.watch.take() {
            let after = |id: u64, generation: u64| {
                let (now_generation, latest) = current(id)?;
                (now_generation > generation).then(|| {
                    latest
                        .unwrap_or(now)
                        .saturating_duration_since(watch.captured_at)
                })
            };
            let superseded = watch
                .panes
                .iter()
                .filter_map(|&(id, generation)| after(id, generation))
                .min();
            match superseded {
                _ if watch.typed_at != last_typed => {}
                Some(after) if after <= ECHO_SETTLE => self.counts.intermediate_frames += 1,
                Some(after) if after <= watch.budget => self.counts.late_superseded_frames += 1,
                Some(_) => {}
                None if now.saturating_duration_since(watch.captured_at) <= watch.budget => {
                    self.watch = Some(watch);
                }
                None => {}
            }
        }
        split_pane
    }

    /// Start recording the generations a frame captures.
    pub(crate) fn begin_capture(&mut self) {
        self.captured.clear();
    }

    /// One pane's generation, sampled while its terminal lock is held for
    /// the frame's snapshot.
    pub(crate) fn note_capture(&mut self, pane: u64, generation: u64) {
        self.captured.push((pane, generation));
    }

    /// Whether the captured frame paints output the window has not shown.
    /// Call it before the frame's generations are committed.
    pub(crate) fn capture_paints_output(
        &self,
        committed: &std::collections::HashMap<u64, u64>,
    ) -> bool {
        self.captured
            .iter()
            .any(|(id, generation)| committed.get(id) != Some(generation))
    }

    /// A frame presented. `typed` says whether it fell inside the typing
    /// window; `painted_output` comes from [`Self::capture_paints_output`].
    pub(crate) fn frame_presented(
        &mut self,
        captured_at: Instant,
        typed: bool,
        painted_output: bool,
        last_typed: Option<Instant>,
        budget: Duration,
    ) {
        if !typed {
            return;
        }
        self.counts.frames_presented += 1;
        if !painted_output {
            return;
        }
        self.counts.output_frames += 1;
        let mut panes = self
            .watch
            .take()
            .map(|watch| watch.panes)
            .unwrap_or_default();
        panes.clear();
        panes.extend_from_slice(&self.captured);
        self.watch = Some(FrameWatch {
            captured_at,
            typed_at: last_typed,
            budget,
            panes,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_nanos(16_666_667);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A frame painting pane 1 at generation `generation`, captured at `at`.
    fn present(echo: &mut TypedEcho, at: Instant, typed_at: Instant, generation: u64) {
        echo.begin_capture();
        echo.note_capture(1, generation);
        let committed = std::collections::HashMap::from([(1, generation - 1)]);
        let painted = echo.capture_paints_output(&committed);
        echo.frame_presented(at, true, painted, Some(typed_at), BUDGET);
    }

    #[test]
    fn a_split_echo_painted_between_its_writes_is_an_intermediate_frame() {
        let key = Instant::now();
        let first = key + Duration::from_micros(600);
        let second = first + Duration::from_micros(900);
        let mut echo = TypedEcho::default();
        assert!(echo.typed_wake(Some(key), [(1, 11, Some(first))]));
        present(&mut echo, first + Duration::from_micros(200), key, 11);
        // The second write lands while the first frame is on its way.
        let found = echo.observe(second, Some(key), |_| Some((12, Some(second))));
        assert_eq!(
            found,
            Some(1),
            "the second write within the settle splits the echo"
        );
        let counts = echo.counts();
        assert_eq!(counts.echoes, 1);
        assert_eq!(counts.split_echoes, 1);
        assert_eq!(counts.frames_presented, 1);
        assert_eq!(counts.output_frames, 1);
        assert_eq!(counts.intermediate_frames, 1);
        assert_eq!(counts.late_superseded_frames, 0);
        // Judged once: observing again counts nothing more.
        assert_eq!(
            echo.observe(second + ms(1), Some(key), |_| Some((12, Some(second)))),
            None
        );
        assert_eq!(echo.counts(), counts);
    }

    #[test]
    fn the_publication_time_judges_a_frame_not_the_wake_time() {
        // The UI thread sees the second write only after a slow frame, well
        // past the settle; it was published 1 ms after the capture.
        let key = Instant::now();
        let mut echo = TypedEcho::default();
        echo.typed_wake(Some(key), [(1, 11, Some(key + ms(1)))]);
        present(&mut echo, key + ms(1), key, 11);
        echo.observe(key + ms(9), Some(key), |_| Some((12, Some(key + ms(2)))));
        assert_eq!(echo.counts().intermediate_frames, 1);
        assert_eq!(echo.counts().split_echoes, 1);
    }

    #[test]
    fn output_after_the_settle_is_late_and_after_a_refresh_is_neither() {
        let key = Instant::now();
        let mut echo = TypedEcho::default();
        echo.typed_wake(Some(key), [(1, 11, Some(key))]);
        present(&mut echo, key, key, 11);
        echo.observe(key + ms(8), Some(key), |_| Some((12, Some(key + ms(8)))));
        let counts = echo.counts();
        assert_eq!(counts.intermediate_frames, 0);
        assert_eq!(counts.late_superseded_frames, 1);
        assert_eq!(counts.split_echoes, 0, "8 ms apart is not one split echo");

        present(&mut echo, key + ms(9), key, 12);
        // Nothing new within a refresh: the watch expires uncounted.
        echo.observe(key + ms(30), Some(key), |_| Some((12, Some(key + ms(8)))));
        echo.observe(key + ms(31), Some(key), |_| Some((13, Some(key + ms(31)))));
        assert_eq!(echo.counts().late_superseded_frames, 1);
        assert_eq!(echo.counts().intermediate_frames, 0);
    }

    #[test]
    fn the_next_keystroke_echo_does_not_supersede_the_last_frame() {
        let key = Instant::now();
        let next_key = key + ms(1);
        let mut echo = TypedEcho::default();
        echo.typed_wake(Some(key), [(1, 11, Some(key))]);
        present(&mut echo, key, key, 11);
        // The next key's echo is new content, not a split of the last one.
        echo.observe(next_key + ms(1), Some(next_key), |_| {
            Some((12, Some(next_key + ms(1))))
        });
        assert!(echo.typed_wake(Some(next_key), [(1, 12, Some(next_key + ms(1)))]));
        let counts = echo.counts();
        assert_eq!(counts.echoes, 2);
        assert_eq!(counts.split_echoes, 0);
        assert_eq!(counts.intermediate_frames, 0);
    }

    #[test]
    fn only_typed_frames_that_paint_output_are_watched() {
        let key = Instant::now();
        let mut echo = TypedEcho::default();
        echo.typed_wake(Some(key), [(1, 11, Some(key))]);
        // A cursor-only frame inside the typing window paints no output.
        echo.begin_capture();
        echo.note_capture(1, 10);
        let committed = std::collections::HashMap::from([(1, 10)]);
        assert!(!echo.capture_paints_output(&committed));
        echo.frame_presented(key, true, false, Some(key), BUDGET);
        // A frame outside the typing window is not counted at all.
        echo.frame_presented(key, false, true, Some(key), BUDGET);
        echo.observe(key + ms(1), Some(key), |_| Some((11, Some(key + ms(1)))));
        let counts = echo.counts();
        assert_eq!(counts.frames_presented, 1);
        assert_eq!(counts.output_frames, 0);
        assert_eq!(counts.intermediate_frames, 0);
    }

    #[test]
    fn later_wakes_of_one_keystroke_are_one_echo() {
        let key = Instant::now();
        let mut echo = TypedEcho::default();
        assert!(echo.typed_wake(Some(key), [(1, 11, Some(key))]));
        assert!(!echo.typed_wake(Some(key), [(1, 12, Some(key + ms(1)))]));
        assert!(echo.typed_wake(Some(key + ms(40)), [(1, 13, Some(key + ms(41)))]));
        assert_eq!(echo.counts().echoes, 2);
    }

    #[test]
    fn the_settle_fits_inside_every_supported_frame_budget() {
        // The 4 ms minimum output budget (scheduling.rs) bounds any settle.
        assert!(ECHO_SETTLE < ms(4));
        // The probe's p95 span was 1.8 ms.
        assert!(ECHO_SETTLE >= ms(2));
    }
}
