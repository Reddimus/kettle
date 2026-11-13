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
//! Every count and the settle follow one keystroke at a time, and only a
//! typed-text keystroke (a printable character or a backspace, see
//! [`is_typed_text`]) sent to a pane whose output the window had fully
//! painted (see [`KeyTarget`]). Only that pane's output is its echo. Enter,
//! Tab, arrows, control chords and pastes, output from sibling panes, and
//! type-ahead into a pane still streaming output are left out: their output
//! comes in several writes in any shell.
//!
//! Times come from each pane's publication stamp
//! (`Terminal::output_published_at`), not from when the UI thread gets to a
//! wake, which can be after a whole frame of rendering.
//!
//! The echo settle acts on those counts. Once such an echo has split, the
//! pane's later typed-text echoes are held at their first wake instead of
//! painting at once. Arming reopens the pane's output latch, which that wake
//! had closed, so the echo's second write raises a wake of its own. The UI
//! thread requests the frame when it handles a wake that finds the pane
//! published twice since the keystroke ([`TypedEcho::plan_paint`]).
//! [`ECHO_SETTLE`] after the first wake is the cap, for an echo that comes in
//! one write: `about_to_wait` waits for that deadline once and polls nothing.
//! A pane whose typed keys echo in one write never arms it and pays nothing.

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

/// The shortest wait the settle asks `about_to_wait` for, so a deadline a
/// few microseconds off (a timer that fires early) never becomes a zero wait
/// that spins the loop. Far below [`ECHO_SETTLE`], so the paint lands within
/// it of the deadline.
pub(crate) const SETTLE_WAIT_FLOOR: Duration = Duration::from_micros(100);

/// Publications since the keystroke that complete a split echo: the probe
/// never saw more than two writes for one key. A third write, if a shell
/// made one, would come after the paint and show in `intermediate_frames`.
///
/// A count cannot tell whose publications they are, so a pane can reach it
/// before the echo's second write:
/// - the echo's first write is split across two PTY reads, each of which
///   publishes;
/// - the previous key's late write (its suggestion, arriving after this key
///   went out to a pane that looked quiet) lands after this key's baseline;
/// - an asynchronous prompt redraw, such as a theme's async git status,
///   publishes in between.
///
/// Each releases the settle early, and the echo's real second write then
/// paints in a frame of its own, as it would with no settle; that frame's
/// predecessor counts in `intermediate_frames`. Releasing at the number of
/// publications seen when the pane's split was learned was considered and
/// not adopted: it cannot tell those publications apart either.
const SPLIT_ECHO_PUBLICATIONS: u64 = 2;

/// How long one split echo keeps a pane's later echoes settled. In the probe
/// about one key in four split, so while someone types, splits recur well
/// inside this; a pause longer than this costs one unsettled split.
pub(crate) const SPLIT_MEMORY: Duration = Duration::from_secs(5);

/// The most bytes one typed-text keystroke can send: a few composed
/// characters from an input method. Anything longer is a paste.
const MAX_TYPED_TEXT_BYTES: usize = 32;

/// Whether `bytes` is one typed-text keystroke: printable characters (any
/// UTF-8) or a lone backspace. Enter, Tab, control chords and escape
/// sequences are not: their echoes legitimately come in several writes (Enter
/// prints a newline, runs the command, then draws the prompt), so learning
/// from them would settle a shell whose typing never splits. Pastes never
/// reach this test: they are written as pasted input (`Pane::feed_paste`).
pub(crate) fn is_typed_text(bytes: &[u8]) -> bool {
    match bytes {
        [] => false,
        [0x7f] | [0x08] => true,
        _ => {
            bytes.len() <= MAX_TYPED_TEXT_BYTES
                && bytes.iter().all(|&byte| byte >= 0x20 && byte != 0x7f)
        }
    }
}

/// Panes remembered as splitting. Entries also expire after
/// [`SPLIT_MEMORY`], so a closed pane's entry goes away by itself.
const MAX_SPLIT_PANES: usize = 8;

/// Panes one keystroke is followed in: a broadcast can reach many, but the
/// counts and the settle need only a few.
const MAX_KEY_PANES: usize = 16;

/// One pane a keystroke was sent to, as the window saw it then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyTarget {
    pub(crate) pane: u64,
    /// The pane's queued input was typed text (`Pane::last_input_was_typed_text`).
    pub(crate) typed_text: bool,
    /// The window had painted all of the pane's output, so what follows is
    /// this key's echo and not output that was already streaming.
    pub(crate) quiet: bool,
    /// The pane's output generation when the key was queued. In a quiet pane
    /// every publication after it is this key's echo, so the settle can tell
    /// when a split echo has published both writes.
    pub(crate) generation: u64,
}

/// One pane a followed keystroke went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyPane {
    pane: u64,
    /// The pane's output generation when the key was queued.
    generation: u64,
    /// Whether every publication after `generation` can be taken for this
    /// key's echo. False when the previous key was followed in the pane at
    /// the same generation: that key's echo had not published yet, so its
    /// writes come after this baseline too, and counting publications would
    /// release this key's settle with its echo half-written. Such a pane's
    /// settle waits for the deadline.
    certain: bool,
}

/// The latest keystroke and the panes whose output is its echo.
#[derive(Debug)]
struct SentKey {
    at: Instant,
    /// Typed text into a quiet pane; empty for any other keystroke.
    panes: Vec<KeyPane>,
}

impl SentKey {
    fn follows(&self, pane: u64) -> bool {
        self.panes.iter().any(|key_pane| key_pane.pane == pane)
    }
}

/// Where a window's settled echo paint is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettlePhase {
    #[default]
    Idle,
    /// The echo of the keystroke sent at `key` waits for its paint at
    /// `deadline`, which never moves. The panes it waits for are in
    /// [`TypedEcho::armed`].
    Armed { deadline: Instant, key: Instant },
    /// A redraw was requested from winit: at the deadline, or at the wake
    /// that completed the echo.
    Queued,
}

/// What a typed output wake does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EchoPaint {
    /// Request a redraw now, as every typed echo did before the settle. Also
    /// returned when the wake completes a settled echo.
    Now,
    /// Leave it to the settle: its deadline, or the wake of the echo's second
    /// write.
    Settle,
}

/// What `about_to_wait` does for the settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettlePoll {
    /// Nothing pending: no redraw, no wake.
    Idle,
    /// Wake at the deadline, this long from now; never shorter than
    /// [`SETTLE_WAIT_FLOOR`].
    Wait(Duration),
    /// The deadline passed: request a redraw.
    Request,
}

/// Counts since the window opened. Counts only, never content. All of them
/// follow typed-text keystrokes into quiet panes, and only those panes'
/// output (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TypedEchoCounts {
    /// Keystroke echoes: the first output wake that carries the keystroke's
    /// pane's output.
    pub(crate) echoes: u64,
    /// Echoes whose pane published again within [`ECHO_SETTLE`] of the
    /// first publication, before the next keystroke.
    pub(crate) split_echoes: u64,
    /// Frames presented within the typing window of a keystroke, for any
    /// reason. Per keystroke, this over `echoes`.
    pub(crate) frames_presented: u64,
    /// Of those, frames that painted new output of the keystroke's pane.
    pub(crate) output_frames: u64,
    /// Output frames whose keystroke's pane published again within
    /// [`ECHO_SETTLE`] of the frame's capture, before the next keystroke: the
    /// in-between state of a split echo reached the screen.
    pub(crate) intermediate_frames: u64,
    /// Output frames superseded the same way, later than [`ECHO_SETTLE`] but
    /// within one output frame budget: on screen for about one refresh.
    pub(crate) late_superseded_frames: u64,
    /// Settles that reached their deadline and requested the frame, counted
    /// when they request it. A settle that another frame painted first, or
    /// that a hidden window cancelled while still armed, is not counted; one
    /// cancelled after it requested its frame already was.
    pub(crate) settled_echoes: u64,
    /// Settles released before their deadline by a wake that found the
    /// echo's panes had published their second write, counted when they
    /// request the frame. Not included in `settled_echoes`: the sum of the
    /// two is the settles that requested their frame. An echo whose writes
    /// were all in by the time its first wake was handled never arms and is
    /// in neither.
    pub(crate) settled_early: u64,
}

/// One pane's output at a sample: id, output generation, publication time.
pub(crate) type PanePublication = (u64, u64, Option<Instant>);

#[derive(Debug)]
struct EchoOrigin {
    /// The keystroke this echo answers.
    typed_at: Option<Instant>,
    /// The keystroke's panes with new output at the echo's first wake.
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
    key: Option<SentKey>,
    origin: Option<EchoOrigin>,
    watch: Option<FrameWatch>,
    /// Generations the frame being drawn captured, reused across frames.
    captured: Vec<(u64, u64)>,
    settle: SettlePhase,
    /// The panes an armed settle waits for, with each one's baseline: the
    /// learned panes of the armed keystroke, and of any later keystroke whose
    /// echo joined the armed paint. Reused across settles.
    armed: Vec<KeyPane>,
    /// More panes joined the armed paint than [`MAX_KEY_PANES`]: only the
    /// deadline releases it.
    armed_overflow: bool,
    /// The panes of the keystroke the latest one replaced, to tell whether
    /// that keystroke's echo had published before the latest went out.
    previous_panes: Vec<KeyPane>,
    /// Panes whose echo last split, and when.
    split_panes: Vec<(u64, Instant)>,
}

impl TypedEcho {
    pub(crate) fn counts(&self) -> TypedEchoCounts {
        self.counts
    }

    /// A keystroke was queued at `at` to `targets`. Only typed text into a
    /// quiet pane is followed. Called again for the same instant (one
    /// keystroke reaching several panes), it adds to that keystroke.
    ///
    /// A pane the replaced keystroke was followed in, at the generation the
    /// pane still has, gets an uncertain baseline ([`KeyPane::certain`]):
    /// both keys' echoes come after it. That is the case in a burst, where
    /// zsh-autosuggestions cancels one key's suggestion fetch when the next
    /// key arrives.
    pub(crate) fn key_sent(&mut self, at: Instant, targets: impl IntoIterator<Item = KeyTarget>) {
        if self.key.as_ref().is_none_or(|key| key.at != at) {
            let mut panes = std::mem::take(&mut self.previous_panes);
            panes.clear();
            if let Some(replaced) = self.key.take() {
                self.previous_panes = replaced.panes;
            }
            self.key = Some(SentKey { at, panes });
        }
        let previous = &self.previous_panes;
        let Some(key) = self.key.as_mut() else {
            return;
        };
        for target in targets {
            if target.typed_text
                && target.quiet
                && key.panes.len() < MAX_KEY_PANES
                && !key.follows(target.pane)
            {
                let echo_unpublished = previous.iter().any(|replaced| {
                    replaced.pane == target.pane && replaced.generation == target.generation
                });
                key.panes.push(KeyPane {
                    pane: target.pane,
                    generation: target.generation,
                    certain: !echo_unpublished,
                });
            }
        }
    }

    /// The keystroke `last_typed` names, if it is the one recorded.
    fn key_for(&self, last_typed: Option<Instant>) -> Option<&SentKey> {
        self.key.as_ref().filter(|key| Some(key.at) == last_typed)
    }

    /// An output wake inside the typing window. Returns whether it is the
    /// echo's first wake: the first that carries new output of a pane the
    /// keystroke is followed in. A sibling pane's output, a later write of the
    /// same echo, and any output after a keystroke that is not followed return
    /// false.
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
        let Some(key) = self.key_for(last_typed) else {
            return false;
        };
        let panes: Vec<PanePublication> = new_output
            .into_iter()
            .filter(|&(id, ..)| key.follows(id))
            .collect();
        if panes.is_empty() {
            return false;
        }
        self.counts.echoes += 1;
        self.origin = Some(EchoOrigin {
            typed_at: last_typed,
            panes,
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
            if let Some(pane) = split_pane {
                origin.split = true;
                self.counts.split_echoes += 1;
                self.remember_split(pane, now);
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

    fn remember_split(&mut self, pane: u64, now: Instant) {
        self.split_panes
            .retain(|&(id, at)| id != pane && now.saturating_duration_since(at) <= SPLIT_MEMORY);
        if self.split_panes.len() >= MAX_SPLIT_PANES {
            self.split_panes.remove(0);
        }
        self.split_panes.push((pane, now));
    }

    /// Whether `pane`'s echoes split recently enough to settle.
    fn learned(&self, pane: u64, now: Instant) -> bool {
        self.split_panes
            .iter()
            .any(|&(id, at)| id == pane && now.saturating_duration_since(at) <= SPLIT_MEMORY)
    }

    /// The current echo and its keystroke, if the echo answers the latest
    /// keystroke.
    fn current_echo(&self) -> Option<(&EchoOrigin, &SentKey)> {
        let origin = self.origin.as_ref()?;
        let key = self.key.as_ref()?;
        (origin.typed_at == Some(key.at)).then_some((origin, key))
    }

    /// Whether the current echo comes from a pane that split recently enough
    /// to settle it. Only a followed keystroke has an echo at all, so Enter,
    /// control keys and pastes never get here.
    fn echo_splits(&self, now: Instant) -> bool {
        self.current_echo()
            .is_some_and(|(origin, _)| origin.panes.iter().any(|&(id, ..)| self.learned(id, now)))
    }

    /// Add the learned panes of the latest keystroke to the panes the armed
    /// paint waits for. A pane already there keeps the newer baseline, which
    /// is at least as late, so waiting for it waits for the older one too; it
    /// stays uncertain if either baseline was.
    fn join_armed(&mut self, now: Instant) {
        let Some(key) = self.key.as_ref() else {
            return;
        };
        for &key_pane in &key.panes {
            if !self.learned(key_pane.pane, now) {
                continue;
            }
            if let Some(armed) = self.armed.iter_mut().find(|a| a.pane == key_pane.pane) {
                *armed = KeyPane {
                    certain: armed.certain && key_pane.certain,
                    ..key_pane
                };
            } else if self.armed.len() < MAX_KEY_PANES {
                self.armed.push(key_pane);
            } else {
                self.armed_overflow = true;
            }
        }
    }

    /// Whether every pane the armed paint waits for has published its split
    /// echo's second write. That includes a pane of a broadcast that had not
    /// published yet at the echo's first wake; a pane whose echoes do not
    /// split is not waited for. A pane with an uncertain baseline, or one
    /// that closed, is never complete: the deadline releases it.
    ///
    /// Each pane still short of its count gets its output latch reopened
    /// (`acknowledge`) before its generation is read, so a publication either
    /// shows in that read or raises a fresh wake. A pane that is complete, or
    /// that only the deadline can release, is left closed: at most
    /// [`SPLIT_ECHO_PUBLICATIONS`] wakes per pane and keystroke come from this.
    fn armed_complete(
        &self,
        acknowledge: impl Fn(u64),
        generation: impl Fn(u64) -> Option<u64>,
    ) -> bool {
        if self.armed.is_empty()
            || self.armed_overflow
            || self.armed.iter().any(|armed| !armed.certain)
        {
            return false;
        }
        let published = |armed: &KeyPane| {
            generation(armed.pane).is_some_and(|current| {
                current.saturating_sub(armed.generation) >= SPLIT_ECHO_PUBLICATIONS
            })
        };
        let mut complete = true;
        for armed in &self.armed {
            if published(armed) {
                continue;
            }
            acknowledge(armed.pane);
            complete &= published(armed);
        }
        complete
    }

    /// Decide how a typed output wake paints. `new_echo` is what
    /// [`Self::typed_wake`] returned for it; `settles` is whether this
    /// platform settles echoes at all. `acknowledge` reopens a pane's output
    /// latch and `generation` reads its output generation; see
    /// [`Self::armed_complete`] for the order they are called in.
    ///
    /// The wake that arms the settle reopens the latch of each pane it waits
    /// for, so the echo's second write raises a wake that comes back here.
    /// While armed, a wake that finds every such pane two publications past
    /// its keystroke releases the settle (`settled_early`).
    pub(crate) fn plan_paint(
        &mut self,
        now: Instant,
        new_echo: bool,
        settles: bool,
        acknowledge: impl Fn(u64),
        generation: impl Fn(u64) -> Option<u64>,
    ) -> EchoPaint {
        match self.settle {
            // A later keystroke's output paints at once unless it is itself
            // an echo that would settle; then the armed paint takes it, and
            // the deadline never moves.
            SettlePhase::Armed { key, .. }
                if self.key.as_ref().is_some_and(|sent| sent.at != key)
                    && !(new_echo && self.echo_splits(now)) =>
            {
                return EchoPaint::Now;
            }
            // A later write of the armed echo, another pane's wake, or a later
            // keystroke's echo joining the armed paint. The paint then waits
            // for that keystroke's panes as well as the armed ones, and
            // follows that keystroke, so the wakes of either echo's later
            // writes come back to this arm.
            SettlePhase::Armed { deadline, .. } => {
                if new_echo && let Some(key) = self.key.as_ref().map(|key| key.at) {
                    self.join_armed(now);
                    self.settle = SettlePhase::Armed { deadline, key };
                }
                // A due deadline counts as the deadline, as in `poll_settle`.
                if now >= deadline {
                    self.settle = SettlePhase::Queued;
                    self.counts.settled_echoes += 1;
                    return EchoPaint::Now;
                }
                if self.armed_complete(acknowledge, generation) {
                    self.settle = SettlePhase::Queued;
                    self.counts.settled_early += 1;
                    return EchoPaint::Now;
                }
                return EchoPaint::Settle;
            }
            // Requesting again is idempotent and covers a redraw winit never
            // delivered, so an echo cannot get stuck behind it.
            SettlePhase::Queued => return EchoPaint::Now,
            SettlePhase::Idle => {}
        }
        // A later write of the same keystroke is the rest of a split echo.
        // Holding it back would only keep the half-written frame up longer.
        if !settles || !new_echo || !self.echo_splits(now) {
            return EchoPaint::Now;
        }
        let Some(key) = self.key.as_ref().map(|key| key.at) else {
            return EchoPaint::Now;
        };
        self.armed.clear();
        self.armed_overflow = false;
        self.join_armed(now);
        // Every write landed before the UI thread reached the first wake.
        if self.armed_complete(acknowledge, generation) {
            return EchoPaint::Now;
        }
        self.settle = SettlePhase::Armed {
            deadline: now + ECHO_SETTLE,
            key,
        };
        EchoPaint::Settle
    }

    /// Poll the settle from `about_to_wait`. `blocked` carries the output
    /// coalescer's guards: the window is hidden or a frame recovery is
    /// pending. Either cancels the settle instead of waking for a redraw that
    /// would return early. The output stays unpainted, and the window's
    /// restore wake or the recovery redraw paints it. The echo's second write
    /// is not looked for here: its own wake releases the settle
    /// ([`Self::plan_paint`]), so an armed settle asks for one wake, at its
    /// deadline.
    pub(crate) fn poll_settle(&mut self, now: Instant, blocked: bool) -> SettlePoll {
        match self.settle {
            SettlePhase::Idle => SettlePoll::Idle,
            SettlePhase::Armed { .. } | SettlePhase::Queued if blocked => {
                self.settle = SettlePhase::Idle;
                SettlePoll::Idle
            }
            SettlePhase::Queued => SettlePoll::Idle,
            SettlePhase::Armed { deadline, .. } if now >= deadline => {
                self.settle = SettlePhase::Queued;
                self.counts.settled_echoes += 1;
                SettlePoll::Request
            }
            SettlePhase::Armed { deadline, .. } => SettlePoll::Wait(
                deadline
                    .saturating_duration_since(now)
                    .max(SETTLE_WAIT_FLOOR),
            ),
        }
    }

    /// A redraw callback began. It consumes a queued request even when it
    /// returns before drawing, so a failed redraw is not retried every turn.
    pub(crate) fn redraw_started(&mut self) {
        if self.settle == SettlePhase::Queued {
            self.settle = SettlePhase::Idle;
        }
    }

    /// A frame is about to sample output: any settled echo paints in it.
    pub(crate) fn frame_sampled(&mut self) {
        self.settle = SettlePhase::Idle;
    }

    /// Output wakes were quiesced (hidden, recovering, or no renderer).
    pub(crate) fn quiesce(&mut self) {
        self.settle = SettlePhase::Idle;
    }

    /// Whether a settled paint is still to come; blocks the blink handoff.
    pub(crate) fn settle_pending(&self) -> bool {
        self.settle != SettlePhase::Idle
    }

    /// Start recording the generations a frame captures.
    pub(crate) fn begin_capture(&mut self) {
        self.captured.clear();
    }

    /// One pane's generation, sampled while its terminal lock is held for
    /// the frame's snapshot. Only the latest keystroke's panes are kept.
    pub(crate) fn note_capture(&mut self, pane: u64, generation: u64) {
        if self.key.as_ref().is_some_and(|key| key.follows(pane)) {
            self.captured.push((pane, generation));
        }
    }

    /// Whether the captured frame paints output of the keystroke's panes the
    /// window has not shown. Call it before the frame's generations are
    /// committed.
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
    /// Only a followed keystroke's frames count.
    pub(crate) fn frame_presented(
        &mut self,
        captured_at: Instant,
        typed: bool,
        painted_output: bool,
        last_typed: Option<Instant>,
        budget: Duration,
    ) {
        if !typed
            || self
                .key_for(last_typed)
                .is_none_or(|key| key.panes.is_empty())
        {
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

    /// A typed-text key into quiet `pane`, whose output generation was
    /// `generation` when the key was queued.
    fn typed(pane: u64, generation: u64) -> KeyTarget {
        KeyTarget {
            pane,
            typed_text: true,
            quiet: true,
            generation,
        }
    }

    /// Enter, Tab, a control key, an arrow or a paste into quiet `pane`.
    fn not_typed(pane: u64) -> KeyTarget {
        KeyTarget {
            pane,
            typed_text: false,
            quiet: true,
            generation: 0,
        }
    }

    /// Send a typed-text key to pane 1 at `at`; the pane's output generation
    /// was `generation` then.
    fn key(echo: &mut TypedEcho, at: Instant, generation: u64) {
        echo.key_sent(at, [typed(1, generation)]);
    }

    /// Plan a typed wake's paint with every followed pane's generation where
    /// the echo's first wake saw it: nothing published since that wake.
    fn plan(echo: &mut TypedEcho, now: Instant, new_echo: bool, settles: bool) -> EchoPaint {
        let seen: Vec<(u64, u64)> = echo
            .origin
            .iter()
            .flat_map(|origin| {
                origin
                    .panes
                    .iter()
                    .map(|&(id, generation, _)| (id, generation))
            })
            .collect();
        echo.plan_paint(
            now,
            new_echo,
            settles,
            |_| {},
            |id| seen.iter().find(|&&(pane, _)| pane == id).map(|&(_, g)| g),
        )
    }

    /// Plan a typed wake's paint on a settling platform with each pane's
    /// generation from `generation`.
    fn plan_at(
        echo: &mut TypedEcho,
        now: Instant,
        new_echo: bool,
        generation: impl Fn(u64) -> Option<u64>,
    ) -> EchoPaint {
        echo.plan_paint(now, new_echo, true, |_| {}, generation)
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
        let at = Instant::now();
        let first = at + Duration::from_micros(600);
        let second = first + Duration::from_micros(900);
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        assert!(echo.typed_wake(Some(at), [(1, 11, Some(first))]));
        present(&mut echo, first + Duration::from_micros(200), at, 11);
        // The second write lands while the first frame is on its way.
        let found = echo.observe(second, Some(at), |_| Some((12, Some(second))));
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
            echo.observe(second + ms(1), Some(at), |_| Some((12, Some(second)))),
            None
        );
        assert_eq!(echo.counts(), counts);
    }

    #[test]
    fn the_publication_time_judges_a_frame_not_the_wake_time() {
        // The UI thread sees the second write only after a slow frame, well
        // past the settle; it was published 1 ms after the capture.
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        echo.typed_wake(Some(at), [(1, 11, Some(at + ms(1)))]);
        present(&mut echo, at + ms(1), at, 11);
        echo.observe(at + ms(9), Some(at), |_| Some((12, Some(at + ms(2)))));
        assert_eq!(echo.counts().intermediate_frames, 1);
        assert_eq!(echo.counts().split_echoes, 1);
    }

    #[test]
    fn output_after_the_settle_is_late_and_after_a_refresh_is_neither() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        echo.typed_wake(Some(at), [(1, 11, Some(at))]);
        present(&mut echo, at, at, 11);
        echo.observe(at + ms(8), Some(at), |_| Some((12, Some(at + ms(8)))));
        let counts = echo.counts();
        assert_eq!(counts.intermediate_frames, 0);
        assert_eq!(counts.late_superseded_frames, 1);
        assert_eq!(counts.split_echoes, 0, "8 ms apart is not one split echo");

        present(&mut echo, at + ms(9), at, 12);
        // Nothing new within a refresh: the watch expires uncounted.
        echo.observe(at + ms(30), Some(at), |_| Some((12, Some(at + ms(8)))));
        echo.observe(at + ms(31), Some(at), |_| Some((13, Some(at + ms(31)))));
        assert_eq!(echo.counts().late_superseded_frames, 1);
        assert_eq!(echo.counts().intermediate_frames, 0);
    }

    #[test]
    fn the_next_keystroke_echo_does_not_supersede_the_last_frame() {
        let at = Instant::now();
        let next_key = at + ms(1);
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        echo.typed_wake(Some(at), [(1, 11, Some(at))]);
        present(&mut echo, at, at, 11);
        // The next key's echo is new content, not a split of the last one.
        key(&mut echo, next_key, 11);
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
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        echo.typed_wake(Some(at), [(1, 11, Some(at))]);
        // A cursor-only frame inside the typing window paints no output.
        echo.begin_capture();
        echo.note_capture(1, 10);
        let committed = std::collections::HashMap::from([(1, 10)]);
        assert!(!echo.capture_paints_output(&committed));
        echo.frame_presented(at, true, false, Some(at), BUDGET);
        // A frame outside the typing window is not counted at all.
        echo.frame_presented(at, false, true, Some(at), BUDGET);
        echo.observe(at + ms(1), Some(at), |_| Some((11, Some(at + ms(1)))));
        let counts = echo.counts();
        assert_eq!(counts.frames_presented, 1);
        assert_eq!(counts.output_frames, 0);
        assert_eq!(counts.intermediate_frames, 0);
    }

    #[test]
    fn later_wakes_of_one_keystroke_are_one_echo() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        assert!(echo.typed_wake(Some(at), [(1, 11, Some(at))]));
        assert!(!echo.typed_wake(Some(at), [(1, 12, Some(at + ms(1)))]));
        key(&mut echo, at + ms(40), 12);
        assert!(echo.typed_wake(Some(at + ms(40)), [(1, 13, Some(at + ms(41)))]));
        assert_eq!(echo.counts().echoes, 2);
    }

    /// The typed key at `at` to pane 1 echoes in two writes; returns the time
    /// of the next key.
    fn learn_split(echo: &mut TypedEcho, at: Instant) -> Instant {
        key(echo, at, 10);
        assert!(echo.typed_wake(Some(at), [(1, 11, Some(at))]));
        assert_eq!(plan(echo, at, true, true), EchoPaint::Now);
        let second = at + ms(1);
        assert_eq!(
            echo.observe(second, Some(at), |_| Some((12, Some(second)))),
            Some(1)
        );
        at + ms(50)
    }

    /// The typed key at `next` to pane 1 arms the settle.
    fn arm(echo: &mut TypedEcho, next: Instant) {
        key(echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        assert_eq!(plan(echo, next, true, true), EchoPaint::Settle);
    }

    #[test]
    fn only_a_pane_whose_echo_split_settles_its_later_echoes() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        // One write per key: never settled.
        for n in 0..3 {
            let t = at + ms(40 * n);
            key(&mut echo, t, 9 + n);
            assert!(echo.typed_wake(Some(t), [(1, 10 + n, Some(t))]));
            assert_eq!(plan(&mut echo, t, true, true), EchoPaint::Now);
        }
        let next = learn_split(&mut echo, at + ms(200));
        arm(&mut echo, next);
        assert_eq!(
            echo.poll_settle(next, false),
            SettlePoll::Wait(ECHO_SETTLE),
            "the paint waits from the first wake, for the deadline or the second write"
        );

        // Another pane's echo is not settled by pane 1's history.
        let mut other = TypedEcho::default();
        let next = learn_split(&mut other, at);
        other.key_sent(next, [typed(2, 4)]);
        assert!(other.typed_wake(Some(next), [(2, 5, Some(next))]));
        assert_eq!(plan(&mut other, next, true, true), EchoPaint::Now);
    }

    #[test]
    fn split_evidence_expires_and_other_platforms_never_settle() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        key(&mut echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        assert_eq!(plan(&mut echo, next, true, false), EchoPaint::Now);

        let late = at + ms(1) + SPLIT_MEMORY + ms(1);
        key(&mut echo, late, 13);
        assert!(echo.typed_wake(Some(late), [(1, 14, Some(late))]));
        assert_eq!(plan(&mut echo, late, true, true), EchoPaint::Now);
    }

    #[test]
    fn a_split_inside_a_settled_echo_keeps_the_pane_settled() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        arm(&mut echo, next);
        // The second write raises no wake; the settled frame sees it.
        let second = next + ms(1);
        echo.frame_sampled();
        assert_eq!(
            echo.observe(next + ms(3), Some(next), |_| Some((14, Some(second)))),
            Some(1)
        );
        // Past the first split's memory, inside the second's.
        let much_later = at + ms(1) + SPLIT_MEMORY + ms(10);
        arm(&mut echo, much_later);
    }

    #[test]
    fn only_typed_text_keys_teach_the_settle() {
        for typed in [
            &b"a"[..],
            "\u{e9}".as_bytes(),
            b" ",
            b"\x7f",
            b"\x08",
            b"ab",
        ] {
            assert!(is_typed_text(typed), "{typed:?}");
        }
        for other in [
            &b""[..],
            b"\r",
            b"ls\r",
            b"\t",
            b"\x03",
            b"\x1b[A",
            b"\x1b[200~pasted\x1b[201~",
            &[b'x'; MAX_TYPED_TEXT_BYTES + 1],
        ] {
            assert!(!is_typed_text(other), "{other:?}");
        }

        // Enter's echo splits (newline, then the prompt) but is not followed:
        // no echo, no split, and the next typed key still paints at once.
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        echo.key_sent(at, [not_typed(1)]);
        assert!(!echo.typed_wake(Some(at), [(1, 11, Some(at))]));
        let second = at + Duration::from_micros(200);
        assert_eq!(
            echo.observe(second, Some(at), |_| Some((12, Some(second)))),
            None
        );
        assert_eq!(echo.counts(), TypedEchoCounts::default());
        let next = at + ms(50);
        key(&mut echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        assert_eq!(plan(&mut echo, next, true, true), EchoPaint::Now);
    }

    #[test]
    fn a_non_typed_keystroke_in_a_learned_pane_paints_at_once() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        // Enter, Tab, an arrow or a paste into the learned pane: not armed.
        echo.key_sent(next, [not_typed(1)]);
        assert!(!echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        assert_eq!(plan(&mut echo, next, false, true), EchoPaint::Now);
        assert!(!echo.settle_pending());
        assert_eq!(
            echo.poll_settle(next + ECHO_SETTLE, false),
            SettlePoll::Idle
        );

        // Nor does one pressed while a typed key's echo is armed wait for it.
        let typed_at = next + ms(50);
        arm(&mut echo, typed_at);
        let enter = typed_at + ms(1);
        echo.key_sent(enter, [not_typed(1)]);
        assert!(!echo.typed_wake(Some(enter), [(1, 14, Some(enter))]));
        assert_eq!(plan(&mut echo, enter, false, true), EchoPaint::Now);
    }

    #[test]
    fn a_sibling_panes_wake_is_not_the_keystrokes_echo() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        // Pane 2 streams while the key goes to pane 1: its wake is no echo,
        // and pane 1's wake after it still is.
        key(&mut echo, at, 10);
        assert!(!echo.typed_wake(Some(at), [(2, 40, Some(at))]));
        assert_eq!(echo.counts().echoes, 0);
        assert!(echo.typed_wake(Some(at), [(1, 11, Some(at)), (2, 41, Some(at))]));
        assert_eq!(echo.counts().echoes, 1);
        // Pane 2 publishing again within the settle is not a split of the echo.
        let soon = at + ms(1);
        let pane2 = |id: u64| {
            Some(if id == 2 {
                (42, Some(soon))
            } else {
                (11, Some(at))
            })
        };
        assert_eq!(echo.observe(soon, Some(at), pane2), None);
        assert_eq!(echo.counts().split_echoes, 0);

        // Pane 2's own split history does not settle a key typed into pane 1.
        let mut echo = TypedEcho::default();
        echo.key_sent(at, [typed(2, 4)]);
        assert!(echo.typed_wake(Some(at), [(2, 5, Some(at))]));
        assert_eq!(
            echo.observe(soon, Some(at), |_| Some((6, Some(soon)))),
            Some(2)
        );
        let next = at + ms(50);
        key(&mut echo, next, 12);
        assert!(!echo.typed_wake(Some(next), [(2, 7, Some(next))]));
        assert_eq!(plan(&mut echo, next, false, true), EchoPaint::Now);
        assert!(!echo.settle_pending());
    }

    #[test]
    fn type_ahead_while_output_streams_neither_learns_nor_arms() {
        let streaming = KeyTarget {
            pane: 1,
            typed_text: true,
            quiet: false,
            generation: 10,
        };
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        // Output that was already streaming splits "the echo": not learned.
        echo.key_sent(at, [streaming]);
        assert!(!echo.typed_wake(Some(at), [(1, 11, Some(at))]));
        let soon = at + ms(1);
        assert_eq!(
            echo.observe(soon, Some(at), |_| Some((12, Some(soon)))),
            None
        );
        let next = at + ms(50);
        key(&mut echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        assert_eq!(plan(&mut echo, next, true, true), EchoPaint::Now);

        // A learned pane that is streaming when the key arrives: not armed.
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        echo.key_sent(next, [streaming]);
        assert!(!echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        assert_eq!(plan(&mut echo, next, false, true), EchoPaint::Now);
        assert!(!echo.settle_pending());
    }

    #[test]
    fn frame_counts_follow_only_the_keystrokes_pane() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        key(&mut echo, at, 10);
        assert!(echo.typed_wake(Some(at), [(1, 11, Some(at))]));
        // A frame that paints only the sibling's new output is no output frame.
        echo.begin_capture();
        echo.note_capture(1, 11);
        echo.note_capture(2, 40);
        let committed = std::collections::HashMap::from([(1, 11), (2, 39)]);
        assert!(!echo.capture_paints_output(&committed));
        echo.frame_presented(at, true, false, Some(at), BUDGET);
        // One that paints the echo is; the sibling publishing right after it
        // does not make it an intermediate frame.
        echo.begin_capture();
        echo.note_capture(1, 12);
        echo.note_capture(2, 41);
        let committed = std::collections::HashMap::from([(1, 11), (2, 40)]);
        assert!(echo.capture_paints_output(&committed));
        echo.frame_presented(at + ms(1), true, true, Some(at), BUDGET);
        let soon = at + ms(2);
        echo.observe(soon, Some(at), |id| {
            Some(if id == 2 {
                (42, Some(soon))
            } else {
                (12, Some(at))
            })
        });
        let counts = echo.counts();
        assert_eq!(counts.frames_presented, 2);
        assert_eq!(counts.output_frames, 1);
        assert_eq!(counts.intermediate_frames, 0);
        assert_eq!(counts.late_superseded_frames, 0);
    }

    #[test]
    fn only_settles_that_fire_are_counted() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        arm(&mut echo, next);
        assert_eq!(echo.counts().settled_echoes, 0, "armed is not fired");
        // Another frame painted it before the deadline.
        echo.frame_sampled();
        assert_eq!(
            echo.poll_settle(next + ECHO_SETTLE, false),
            SettlePoll::Idle
        );
        // A hidden window cancelled it.
        let later = next + ms(50);
        arm(&mut echo, later);
        assert_eq!(
            echo.poll_settle(later + ECHO_SETTLE, true),
            SettlePoll::Idle
        );
        assert_eq!(echo.counts().settled_echoes, 0);
        // It reached its deadline.
        let last = later + ms(50);
        arm(&mut echo, last);
        assert_eq!(
            echo.poll_settle(last + ECHO_SETTLE, false),
            SettlePoll::Request
        );
        assert_eq!(echo.counts().settled_echoes, 1);
    }

    #[test]
    fn the_settle_waits_for_its_deadline_and_never_for_less_than_the_floor() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        arm(&mut echo, next);
        let deadline = next + ECHO_SETTLE;
        for before in [ECHO_SETTLE, ms(1), Duration::from_micros(200)] {
            assert_eq!(
                echo.poll_settle(deadline - before, false),
                SettlePoll::Wait(before),
                "the wait runs to the deadline, in one step"
            );
        }
        assert_eq!(
            echo.poll_settle(deadline - Duration::from_micros(1), false),
            SettlePoll::Wait(SETTLE_WAIT_FLOOR),
            "never a zero wait"
        );
        assert!(SETTLE_WAIT_FLOOR < ECHO_SETTLE / 10);
    }

    #[test]
    fn one_keystroke_to_several_panes_follows_each_typed_quiet_one() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        echo.key_sent(at, [typed(1, 0), not_typed(2)]);
        echo.key_sent(at, [typed(3, 0)]);
        assert!(!echo.typed_wake(Some(at), [(2, 1, Some(at))]));
        assert!(echo.typed_wake(Some(at), [(3, 1, Some(at))]));
        // Many recipients are followed up to the bound.
        echo.key_sent(at + ms(1), (0..100).map(|pane| typed(pane, 0)));
        assert_eq!(echo.key.as_ref().unwrap().panes.len(), MAX_KEY_PANES);
    }

    #[test]
    fn the_split_memory_is_bounded() {
        let now = Instant::now();
        let mut echo = TypedEcho::default();
        for pane in 0..100 {
            echo.remember_split(pane, now);
        }
        echo.remember_split(5, now);
        assert_eq!(echo.split_panes.len(), MAX_SPLIT_PANES);
        assert_eq!(
            echo.split_panes.iter().filter(|(id, _)| *id == 5).count(),
            1
        );
    }

    #[test]
    fn the_settle_fits_inside_every_supported_frame_budget() {
        // The 4 ms minimum output budget (scheduling.rs) bounds any settle.
        assert!(ECHO_SETTLE < ms(4));
        // The probe's p95 span was 1.8 ms.
        assert!(ECHO_SETTLE >= ms(2));
    }

    #[test]
    fn the_settle_paints_at_the_wake_of_the_echos_second_write() {
        let at = Instant::now();
        let us = Duration::from_micros;
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        // Pane 1 was at generation 12 when the key was sent; the first wake
        // saw 13. Arming reopens the pane's latch, so the second write wakes.
        key(&mut echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        let acked = std::cell::RefCell::new(Vec::new());
        assert_eq!(
            echo.plan_paint(
                next,
                true,
                true,
                |id| acked.borrow_mut().push(id),
                |_| Some(13)
            ),
            EchoPaint::Settle
        );
        assert_eq!(*acked.borrow(), [1], "arming reopens the pane's latch");
        // A sibling pane's wake: pane 1 has written once, keep waiting.
        let sibling = |id: u64| Some(if id == 1 { 13 } else { 100 });
        assert_eq!(
            plan_at(&mut echo, next + us(300), false, sibling),
            EchoPaint::Settle
        );
        assert_eq!(
            echo.poll_settle(next + us(300), false),
            SettlePoll::Wait(ECHO_SETTLE - us(300)),
            "one wait, to the deadline"
        );
        // The wake the second write raised releases it.
        assert_eq!(
            plan_at(&mut echo, next + us(550), false, |_| Some(14)),
            EchoPaint::Now,
            "the split echo is complete: paint now, not at the deadline"
        );
        let counts = echo.counts();
        assert_eq!((counts.settled_early, counts.settled_echoes), (1, 0));
        // Queued: no second request or wake from the poll, and the deadline
        // adds nothing; a typed wake requests the redraw again.
        for poll in [next + ms(1), next + ECHO_SETTLE, next + ms(5)] {
            assert_eq!(echo.poll_settle(poll, false), SettlePoll::Idle);
        }
        assert_eq!(
            plan_at(&mut echo, next + ms(1), false, |_| Some(14)),
            EchoPaint::Now
        );
        assert_eq!(echo.counts(), counts);
        // The frame paints it, and the split still keeps the pane learned.
        echo.redraw_started();
        echo.frame_sampled();
        assert_eq!(
            echo.observe(next + ms(1), Some(next), |_| Some((
                14,
                Some(next + us(600))
            ))),
            Some(1)
        );

        // A wake handled at or after the deadline counts as the deadline,
        // even if the echo is also complete.
        let later = next + ms(50);
        arm(&mut echo, later);
        assert_eq!(
            plan_at(&mut echo, later + ECHO_SETTLE, false, |_| Some(14)),
            EchoPaint::Now
        );
        assert_eq!(
            (echo.counts().settled_echoes, echo.counts().settled_early),
            (1, 1)
        );
        echo.redraw_started();
        echo.frame_sampled();

        // A hidden window cancels an armed settle without a request or a
        // count; one cancelled after it requested its frame was counted.
        let last = later + ms(50);
        arm(&mut echo, last);
        assert_eq!(echo.poll_settle(last + us(500), true), SettlePoll::Idle);
        assert!(!echo.settle_pending());
        assert_eq!(
            (echo.counts().settled_echoes, echo.counts().settled_early),
            (1, 1)
        );
        let last = last + ms(50);
        arm(&mut echo, last);
        assert_eq!(
            echo.poll_settle(last + ECHO_SETTLE, false),
            SettlePoll::Request
        );
        assert_eq!(echo.poll_settle(last + ECHO_SETTLE, true), SettlePoll::Idle);
        assert_eq!(echo.counts().settled_echoes, 2);
    }

    #[test]
    fn a_single_write_echo_paints_at_the_deadline_after_one_wait() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        arm(&mut echo, next);
        // Pane 1 never writes again; a sibling pane prints all along, and its
        // wakes do not release the settle.
        let generation = |id: u64| Some(if id == 1 { 13 } else { 100 });
        assert_eq!(
            plan_at(&mut echo, next + ms(1), false, generation),
            EchoPaint::Settle
        );
        let wait = match echo.poll_settle(next, false) {
            SettlePoll::Wait(wait) => wait,
            other => panic!("{other:?}: the armed settle must wait"),
        };
        assert_eq!(wait, ECHO_SETTLE, "one wait, to the deadline");
        assert_eq!(
            echo.poll_settle(next + wait, false),
            SettlePoll::Request,
            "the deadline caps a one-write echo"
        );
        let counts = echo.counts();
        assert_eq!((counts.settled_echoes, counts.settled_early), (1, 0));
        assert_eq!(echo.poll_settle(next + wait, false), SettlePoll::Idle);
    }

    #[test]
    fn the_settle_reopens_a_latch_before_it_reads_the_generation() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        key(&mut echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 13, Some(next))]));
        // The second write published after the first wake read 13 but
        // before the latch reopened: it raised no wake, so the read after
        // the acknowledgement must see it.
        let acked = std::cell::Cell::new(false);
        let raced = |_| Some(if acked.get() { 14 } else { 13 });
        assert_eq!(
            echo.plan_paint(next, true, true, |_| acked.set(true), raced),
            EchoPaint::Now
        );
        assert!(!echo.settle_pending());
        let counts = echo.counts();
        assert_eq!(
            (counts.settled_echoes, counts.settled_early),
            (0, 0),
            "an echo complete before it arms is in neither count"
        );

        // The same race while armed: a sibling's wake arrives, and the
        // second write lands between that wake and the re-acknowledgement.
        let later = next + ms(50);
        key(&mut echo, later, 14);
        assert!(echo.typed_wake(Some(later), [(1, 15, Some(later))]));
        assert_eq!(plan(&mut echo, later, true, true), EchoPaint::Settle);
        acked.set(false);
        let raced = |_| Some(if acked.get() { 16 } else { 15 });
        assert_eq!(
            echo.plan_paint(later + ms(1), false, true, |_| acked.set(true), raced),
            EchoPaint::Now
        );
        assert_eq!(echo.counts().settled_early, 1);
        echo.redraw_started();
        echo.frame_sampled();

        // A pane already complete is not reopened.
        let last = later + ms(50);
        key(&mut echo, last, 16);
        assert!(echo.typed_wake(Some(last), [(1, 18, Some(last))]));
        let reopened = std::cell::Cell::new(0);
        assert_eq!(
            echo.plan_paint(
                last,
                true,
                true,
                |_| reopened.set(reopened.get() + 1),
                |_| Some(18)
            ),
            EchoPaint::Now
        );
        assert_eq!(reopened.get(), 0);
    }

    #[test]
    fn an_echo_whose_writes_both_landed_before_its_wake_paints_at_once() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        // The key went out at generation 12 and the UI reached the wake only
        // after both writes: 14. Nothing is left to wait for.
        key(&mut echo, next, 12);
        assert!(echo.typed_wake(Some(next), [(1, 14, Some(next))]));
        assert_eq!(plan(&mut echo, next, true, true), EchoPaint::Now);
        assert!(!echo.settle_pending());
        assert_eq!(echo.counts().settled_early, 0);
        // One write at the wake still settles.
        arm(&mut echo, next + ms(50));
    }

    #[test]
    fn a_broadcast_settle_waits_for_each_pane_that_splits() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        // Panes 1 and 2 have split; pane 3 echoes in one write.
        for (n, pane) in [1, 2].into_iter().enumerate() {
            let t = at + ms(50 * n as u64);
            let base = pane * 10;
            echo.key_sent(t, [typed(pane, base)]);
            assert!(echo.typed_wake(Some(t), [(pane, base + 1, Some(t))]));
            let second = t + ms(1);
            assert_eq!(
                echo.observe(second, Some(t), |_| Some((base + 2, Some(second)))),
                Some(pane)
            );
        }
        let next = at + ms(200);
        echo.key_sent(next, [typed(1, 12), typed(2, 22), typed(3, 30)]);
        assert!(echo.typed_wake(
            Some(next),
            [
                (1, 13, Some(next)),
                (2, 23, Some(next)),
                (3, 31, Some(next))
            ]
        ));
        let acked = std::cell::RefCell::new(Vec::new());
        let wake = |echo: &mut TypedEcho, new_echo: bool, one: u64, two: u64| {
            echo.plan_paint(
                next + ms(1),
                new_echo,
                true,
                |id| acked.borrow_mut().push(id),
                |id| match id {
                    1 => Some(one),
                    2 => Some(two),
                    _ => Some(31),
                },
            )
        };
        assert_eq!(wake(&mut echo, true, 13, 23), EchoPaint::Settle);
        assert_eq!(*acked.borrow(), [1, 2], "pane 3 is not waited for");
        assert_eq!(
            wake(&mut echo, false, 14, 23),
            EchoPaint::Settle,
            "pane 2 is still half-written"
        );
        assert_eq!(
            wake(&mut echo, false, 14, 24),
            EchoPaint::Now,
            "pane 3 never splits and is not waited for"
        );
        assert_eq!(echo.counts().settled_early, 1);
        echo.redraw_started();
        echo.frame_sampled();

        // Pane 2 had not published at all when pane 1's first write woke the
        // window: pane 1 finishing first must not paint pane 2 half-written,
        // and each of pane 2's writes must wake the window again.
        let last = next + ms(50);
        echo.key_sent(last, [typed(1, 14), typed(2, 24)]);
        assert!(echo.typed_wake(Some(last), [(1, 15, Some(last))]));
        acked.borrow_mut().clear();
        let wake = |echo: &mut TypedEcho, one: u64, two: u64, new_echo: bool| {
            echo.plan_paint(
                last + ms(1),
                new_echo,
                true,
                |id| acked.borrow_mut().push(id),
                |id| Some(if id == 1 { one } else { two }),
            )
        };
        assert_eq!(wake(&mut echo, 15, 24, true), EchoPaint::Settle);
        assert_eq!(*acked.borrow(), [1, 2]);
        acked.borrow_mut().clear();
        // Pane 2's first write: its wake reopens pane 2 for the second.
        assert_eq!(wake(&mut echo, 16, 25, false), EchoPaint::Settle);
        assert_eq!(*acked.borrow(), [2], "only the pane still short");
        assert_eq!(wake(&mut echo, 16, 26, false), EchoPaint::Now);
        assert_eq!(echo.counts().settled_early, 2);
    }

    #[test]
    fn a_key_sent_before_the_last_keys_echo_waits_for_the_deadline() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        // K1 and K2 both go out at generation 12: K1's echo has not
        // published when K2 is sent, as in a burst where
        // zsh-autosuggestions cancels K1's fetch when K2 arrives.
        key(&mut echo, next, 12);
        let k2 = next + ms(30);
        key(&mut echo, k2, 12);
        assert!(echo.typed_wake(Some(k2), [(1, 13, Some(k2))]));
        let reopened = std::cell::Cell::new(0);
        assert_eq!(
            echo.plan_paint(
                k2,
                true,
                true,
                |_| reopened.set(reopened.get() + 1),
                |_| Some(13)
            ),
            EchoPaint::Settle
        );
        assert_eq!(reopened.get(), 0, "only the deadline releases it");
        // 14 is K2's first write, not its second: no release.
        assert_eq!(
            plan_at(&mut echo, k2 + ms(1), false, |_| Some(14)),
            EchoPaint::Settle
        );
        assert_eq!(
            echo.poll_settle(k2 + ms(1), false),
            SettlePoll::Wait(ECHO_SETTLE - ms(1))
        );
        assert_eq!(
            echo.poll_settle(k2 + ECHO_SETTLE, false),
            SettlePoll::Request
        );
        let counts = echo.counts();
        assert_eq!((counts.settled_echoes, counts.settled_early), (1, 0));
        echo.redraw_started();
        echo.frame_sampled();

        // Three keys at one generation: still uncertain.
        let k3 = k2 + ms(50);
        key(&mut echo, k3, 15);
        key(&mut echo, k3 + ms(1), 15);
        key(&mut echo, k3 + ms(2), 15);
        assert!(echo.typed_wake(Some(k3 + ms(2)), [(1, 16, Some(k3))]));
        assert_eq!(plan(&mut echo, k3 + ms(2), true, true), EchoPaint::Settle);
        assert_eq!(
            plan_at(&mut echo, k3 + ms(3), false, |_| Some(17)),
            EchoPaint::Settle
        );
        echo.frame_sampled();

        // A key sent after the last key's echo published is certain.
        let k4 = k3 + ms(50);
        key(&mut echo, k4, 17);
        key(&mut echo, k4 + ms(1), 18);
        assert!(echo.typed_wake(Some(k4 + ms(1)), [(1, 19, Some(k4))]));
        assert_eq!(plan(&mut echo, k4 + ms(1), true, true), EchoPaint::Settle);
        assert_eq!(
            plan_at(&mut echo, k4 + ms(2), false, |_| Some(20)),
            EchoPaint::Now
        );
        // A keystroke reaching a second pane is still judged against the
        // keystroke before it, not against itself.
        let k5 = k4 + ms(50);
        echo.key_sent(k5, [typed(2, 30)]);
        let k6 = k5 + ms(1);
        echo.key_sent(k6, [typed(1, 20)]);
        echo.key_sent(k6, [typed(2, 30)]);
        let certain: Vec<(u64, bool)> = echo
            .key
            .as_ref()
            .unwrap()
            .panes
            .iter()
            .map(|p| (p.pane, p.certain))
            .collect();
        assert_eq!(certain, [(1, true), (2, false)]);
    }

    #[test]
    fn a_later_keys_echo_joining_the_armed_paint_waits_for_both_keys() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        // Panes 1 and 2 have split.
        for (n, pane) in [1, 2].into_iter().enumerate() {
            let t = at + ms(50 * n as u64);
            let base = pane * 10;
            echo.key_sent(t, [typed(pane, base)]);
            assert!(echo.typed_wake(Some(t), [(pane, base + 1, Some(t))]));
            let second = t + ms(1);
            assert_eq!(
                echo.observe(second, Some(t), |_| Some((base + 2, Some(second)))),
                Some(pane)
            );
        }
        // K1 to pane 1 arms the settle.
        let k1 = at + ms(200);
        echo.key_sent(k1, [typed(1, 12)]);
        assert!(echo.typed_wake(Some(k1), [(1, 13, Some(k1))]));
        let generation = |one: u64, two: u64| move |id| Some(if id == 1 { one } else { two });
        assert_eq!(
            plan_at(&mut echo, k1, true, generation(13, 22)),
            EchoPaint::Settle
        );
        // K2 to pane 2, before K1's echo completes; its echo joins.
        let k2 = k1 + ms(1);
        echo.key_sent(k2, [typed(2, 22)]);
        assert!(echo.typed_wake(Some(k2), [(2, 23, Some(k2))]));
        assert_eq!(
            plan_at(&mut echo, k2, true, generation(13, 23)),
            EchoPaint::Settle
        );
        // Pane 2 completes, pane 1 is still half-written.
        assert_eq!(
            plan_at(
                &mut echo,
                k2 + Duration::from_micros(500),
                false,
                generation(13, 24)
            ),
            EchoPaint::Settle,
            "the armed key's pane is still half-written"
        );
        // Pane 1's second write releases both.
        assert_eq!(
            plan_at(&mut echo, k2 + ms(1), false, generation(14, 24)),
            EchoPaint::Now
        );
        assert_eq!(echo.counts().settled_early, 1);
        // The deadline never moved.
        echo.redraw_started();
        echo.frame_sampled();
        let k3 = k2 + ms(50);
        echo.key_sent(k3, [typed(1, 14)]);
        assert!(echo.typed_wake(Some(k3), [(1, 15, Some(k3))]));
        assert_eq!(
            plan_at(&mut echo, k3, true, generation(15, 24)),
            EchoPaint::Settle
        );
        let k4 = k3 + ms(2);
        echo.key_sent(k4, [typed(2, 24)]);
        assert!(echo.typed_wake(Some(k4), [(2, 25, Some(k4))]));
        assert_eq!(
            plan_at(&mut echo, k4, true, generation(15, 25)),
            EchoPaint::Settle
        );
        assert_eq!(
            echo.poll_settle(k4, false),
            SettlePoll::Wait(ECHO_SETTLE - ms(2))
        );
    }

    #[test]
    fn a_pane_closed_mid_settle_falls_back_to_the_deadline() {
        let at = Instant::now();
        let mut echo = TypedEcho::default();
        let next = learn_split(&mut echo, at);
        arm(&mut echo, next);
        // Pane 1 closed: its generation can no longer be read.
        assert_eq!(
            plan_at(&mut echo, next + ms(1), false, |_| None),
            EchoPaint::Settle
        );
        assert_eq!(
            echo.poll_settle(next + ms(1), false),
            SettlePoll::Wait(ECHO_SETTLE - ms(1))
        );
        assert_eq!(
            echo.poll_settle(next + ECHO_SETTLE, false),
            SettlePoll::Request
        );
        assert_eq!(
            (echo.counts().settled_echoes, echo.counts().settled_early),
            (1, 0)
        );
    }
}
