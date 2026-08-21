//! Tabs + a binary split tree (Terminator-style tiling). Each leaf owns an
//! independent terminal; splits tile the tab area; focus moves by geometry.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use kettle_config::{Config, CursorStyle, ModifyOtherKeysMode};
use kettle_core::{
    CursorShape, PtyGeometry, PtyOutputSender, TermEvent, Terminal, TerminalCapabilities, Waker,
};

/// At the PTY reader's 64 KiB maximum chunk size, this bounds lossless recorder
/// backlog to 512 KiB per pane. The sender blocks before taking the terminal
/// lock, so backpressure cannot deadlock rendering.
const LOSSLESS_OUTPUT_QUEUE_DEPTH: usize = 8;

/// Semantic terminal events are generated while the parser holds the terminal
/// mutex, so their sender may never block. Overflow is a hostile/stalled-pane
/// condition handled explicitly by the UI instead of growing memory without a
/// limit or silently dropping protocol replies.
const TERM_EVENT_QUEUE_DEPTH: usize = 1024;

/// All UI-originated PTY input and terminal protocol replies are serialized on
/// a worker so a child that stops reading cannot block the window thread.
const PTY_INPUT_QUEUE_DEPTH: usize = 64;
const PTY_INPUT_WRITE_CHUNK_BYTES: usize = 8 * 1024;
// `LOCAL_PASTE_MAX` is 4 MiB; leave room for the bracketed-paste envelope and
// a small amount of already-queued interactive input.
const MAX_USER_INPUT_MESSAGE_BYTES: usize = 4 * 1024 * 1024 + 64;
const MAX_QUEUED_USER_INPUT_BYTES: usize = MAX_USER_INPUT_MESSAGE_BYTES + 64 * 1024;
// A 1 MiB OSC 52 clipboard payload expands to roughly 1.34 MiB after base64.
const MAX_PROTOCOL_REPLY_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_QUEUED_PROTOCOL_REPLY_BYTES: usize = 2 * 1024 * 1024;

fn pane_environment(config: &Config) -> Vec<(String, String)> {
    let mut environment = config.env.clone();
    // Append after user env so the runtime capability is authoritative. The
    // terminal's existing extra-env route also carries it into WSLENV.
    environment.push((
        "KETTLE_COMPLETION_OVERLAY".to_string(),
        if config.completion_overlay == kettle_config::CompletionOverlayMode::Auto {
            "1"
        } else {
            "0"
        }
        .to_string(),
    ));
    environment
}

/// Result of attempting to enqueue input for a pane.
///
/// Keeping these states distinct is a correctness boundary: read-only is a
/// user policy, backpressure is transient and retryable, oversize is a caller
/// error, and a failed transport requires closing the pane. Conflating them
/// previously made agent RPCs report every queue failure as `read_only` and
/// let GUI input disappear without feedback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum PaneInputResult {
    Queued,
    ReadOnly,
    Backpressured,
    Oversize,
    Failed,
}

impl PaneInputResult {
    #[inline]
    pub fn is_queued(self) -> bool {
        self == Self::Queued
    }

    /// Worst-outcome-wins, so a broadcast reports the most serious thing that
    /// happened to any of its targets. `pub(crate)` because a cross-window
    /// broadcast merges results from several muxes in `App`.
    pub(crate) fn merge(self, other: Self) -> Self {
        use PaneInputResult::{Backpressured, Failed, Oversize, Queued, ReadOnly};
        match (self, other) {
            (Failed, _) | (_, Failed) => Failed,
            (Oversize, _) | (_, Oversize) => Oversize,
            (Backpressured, _) | (_, Backpressured) => Backpressured,
            (ReadOnly, _) | (_, ReadOnly) => ReadOnly,
            _ => Queued,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneInputDelivery {
    pub result: PaneInputResult,
    pub accepted: bool,
    /// Whether the caller-designated receipt pane accepted its own bytes. A
    /// broadcast can succeed elsewhere while that pane rejects, which must not
    /// create success chrome or dismiss a still-valid receipt there.
    pub receipt_accepted: bool,
}

impl PaneInputDelivery {
    fn new() -> Self {
        Self {
            result: PaneInputResult::Queued,
            accepted: false,
            receipt_accepted: false,
        }
    }

    fn record(&mut self, result: PaneInputResult) {
        self.accepted |= result.is_queued();
        self.result = self.result.merge(result);
    }

    fn record_receipt_target(&mut self, result: PaneInputResult) {
        self.receipt_accepted |= result.is_queued();
        self.record(result);
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            result: self.result.merge(other.result),
            accepted: self.accepted || other.accepted,
            receipt_accepted: self.receipt_accepted || other.receipt_accepted,
        }
    }
}

fn pane_input_policy(failed: bool, read_only: bool) -> Option<PaneInputResult> {
    if failed {
        Some(PaneInputResult::Failed)
    } else if read_only {
        Some(PaneInputResult::ReadOnly)
    } else {
        None
    }
}

struct QueuedPtyInput {
    bytes: Arc<[u8]>,
    queued_bytes: Arc<AtomicUsize>,
}

impl Drop for QueuedPtyInput {
    fn drop(&mut self) {
        self.queued_bytes
            .fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

struct PendingPtyInput {
    message: QueuedPtyInput,
    offset: usize,
}

impl PendingPtyInput {
    fn new(message: QueuedPtyInput) -> Self {
        Self { message, offset: 0 }
    }

    fn next_chunk(&self) -> &[u8] {
        let end = self
            .offset
            .saturating_add(PTY_INPUT_WRITE_CHUNK_BYTES)
            .min(self.message.bytes.len());
        &self.message.bytes[self.offset..end]
    }

    fn advance(&mut self, written: usize) -> bool {
        self.offset = self.offset.saturating_add(written);
        self.offset == self.message.bytes.len()
    }
}

fn run_pty_input_worker<F>(
    reply_rx: Receiver<QueuedPtyInput>,
    user_rx: Receiver<QueuedPtyInput>,
    failed: &AtomicBool,
    stop: &AtomicBool,
    waker: &Waker,
    mut try_write: F,
) where
    F: FnMut(&[u8]) -> Result<usize>,
{
    let mut current: Option<(bool, PendingPtyInput)> = None;
    let mut prefer_reply = true;
    let mut replies_open = true;
    let mut user_open = true;
    let never_reply = crossbeam_channel::never::<QueuedPtyInput>();
    let never_user = crossbeam_channel::never::<QueuedPtyInput>();

    while !stop.load(Ordering::Acquire) {
        if current.is_none() && !prefer_reply && user_open {
            match user_rx.try_recv() {
                Ok(message) => current = Some((false, PendingPtyInput::new(message))),
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => user_open = false,
            }
        }
        if current.is_none() && replies_open {
            match reply_rx.try_recv() {
                Ok(message) => current = Some((true, PendingPtyInput::new(message))),
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => replies_open = false,
            }
        }
        if current.is_none() && user_open {
            match user_rx.try_recv() {
                Ok(message) => current = Some((false, PendingPtyInput::new(message))),
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => user_open = false,
            }
        }

        if let Some((_, pending)) = current.as_ref() {
            match try_write(pending.next_chunk()) {
                Ok(0) => std::thread::sleep(Duration::from_millis(1)),
                Ok(written) => {
                    if current
                        .as_mut()
                        .is_some_and(|(_, pending)| pending.advance(written))
                    {
                        prefer_reply = !current.as_ref().is_some_and(|(reply, _)| *reply);
                        current = None;
                    }
                }
                Err(error) => {
                    log::error!("PTY input worker failed: {error:#}");
                    if !failed.swap(true, Ordering::AcqRel) {
                        (waker)();
                    }
                    break;
                }
            }
            continue;
        }

        if !replies_open && !user_open {
            break;
        }
        let reply_receiver = if replies_open {
            &reply_rx
        } else {
            &never_reply
        };
        let user_receiver = if user_open { &user_rx } else { &never_user };
        if prefer_reply {
            crossbeam_channel::select_biased! {
                recv(reply_receiver) -> message => match message {
                    Ok(message) => current = Some((true, PendingPtyInput::new(message))),
                    Err(_) => replies_open = false,
                },
                recv(user_receiver) -> message => match message {
                    Ok(message) => current = Some((false, PendingPtyInput::new(message))),
                    Err(_) => user_open = false,
                },
            }
        } else {
            crossbeam_channel::select_biased! {
                recv(user_receiver) -> message => match message {
                    Ok(message) => current = Some((false, PendingPtyInput::new(message))),
                    Err(_) => user_open = false,
                },
                recv(reply_receiver) -> message => match message {
                    Ok(message) => current = Some((true, PendingPtyInput::new(message))),
                    Err(_) => replies_open = false,
                },
            }
        }
    }
}

struct PtyInputQueue {
    user_tx: Sender<QueuedPtyInput>,
    reply_tx: Sender<QueuedPtyInput>,
    queued_user_bytes: Arc<AtomicUsize>,
    queued_reply_bytes: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    waker: Waker,
}

impl PtyInputQueue {
    fn new(term: &Terminal, waker: Waker) -> Result<Self> {
        let (user_tx, user_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let queued_user_bytes = Arc::new(AtomicUsize::new(0));
        let queued_reply_bytes = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let worker_stop = stop.clone();
        let worker_waker = waker.clone();
        let mut pty_stdin = term.stdin_handle()?;
        std::thread::Builder::new()
            .name("kettle-pty-input".into())
            .spawn(move || {
                run_pty_input_worker(
                    reply_rx,
                    user_rx,
                    &worker_failed,
                    &worker_stop,
                    &worker_waker,
                    |bytes| pty_stdin.try_write(bytes),
                );
            })
            .context("cannot spawn PTY input worker")?;
        Ok(Self {
            user_tx,
            reply_tx,
            queued_user_bytes,
            queued_reply_bytes,
            failed,
            stop,
            waker,
        })
    }

    #[cfg(test)]
    fn disconnected_for_test(waker: Waker) -> Self {
        let (user_tx, user_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        drop(user_rx);
        drop(reply_rx);
        Self {
            user_tx,
            reply_tx,
            queued_user_bytes: Arc::new(AtomicUsize::new(0)),
            queued_reply_bytes: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            waker,
        }
    }

    fn enqueue_user(&self, bytes: Arc<[u8]>) -> PaneInputResult {
        self.enqueue(
            &self.user_tx,
            &self.queued_user_bytes,
            MAX_USER_INPUT_MESSAGE_BYTES,
            MAX_QUEUED_USER_INPUT_BYTES,
            bytes,
            false,
        )
    }

    fn enqueue_reply(&self, bytes: Arc<[u8]>) -> PaneInputResult {
        self.enqueue(
            &self.reply_tx,
            &self.queued_reply_bytes,
            MAX_PROTOCOL_REPLY_MESSAGE_BYTES,
            MAX_QUEUED_PROTOCOL_REPLY_BYTES,
            bytes,
            true,
        )
    }

    fn enqueue(
        &self,
        tx: &Sender<QueuedPtyInput>,
        queued_bytes: &Arc<AtomicUsize>,
        max_message_bytes: usize,
        max_queued_bytes: usize,
        bytes: Arc<[u8]>,
        fail_on_reject: bool,
    ) -> PaneInputResult {
        if bytes.is_empty() {
            return if self.failed.load(Ordering::Acquire) {
                PaneInputResult::Failed
            } else {
                PaneInputResult::Queued
            };
        }
        if self.failed.load(Ordering::Acquire) {
            return PaneInputResult::Failed;
        }
        if bytes.len() > max_message_bytes {
            if fail_on_reject {
                self.fail();
                return PaneInputResult::Failed;
            }
            return PaneInputResult::Oversize;
        }
        if queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes.len())
                    .filter(|next| *next <= max_queued_bytes)
            })
            .is_err()
        {
            if fail_on_reject {
                self.fail();
                return PaneInputResult::Failed;
            }
            return PaneInputResult::Backpressured;
        }

        let message = QueuedPtyInput {
            bytes,
            queued_bytes: queued_bytes.clone(),
        };
        match tx.try_send(message) {
            Ok(()) => PaneInputResult::Queued,
            Err(TrySendError::Full(_)) => {
                // The unsent message releases its byte reservation on drop.
                if fail_on_reject {
                    self.fail();
                    PaneInputResult::Failed
                } else {
                    PaneInputResult::Backpressured
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                // A disconnected worker is terminal even for user input; no
                // retry can succeed and drain_events must tear the pane down.
                self.fail();
                PaneInputResult::Failed
            }
        }
    }

    fn fail(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            (self.waker)();
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl Drop for PtyInputQueue {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Initial pane title seeded from the launching argv before the program's
/// first OSC 2. Plain shells use the placeholder "kettle" — the
/// cwd-basename fallback fills in for those once OSC 7 arrives. SSH panes
/// have no local cwd, so we surface the target inline (`ssh me@box`) so a
/// tab full of them is distinguishable while connections are
/// establishing. For any *other* explicit `-e PROG` (e.g. `kettle -e htop`,
/// `kettle -e vim file`), the user has already told us what's running —
/// surface that program's basename instead of the generic "kettle", since
/// many TUIs (htop, top, less, vim's default, …) never emit OSC 2 and
/// have no usable cwd to back-fill from. Pure so the argv → title decision
/// is unit-tested.
fn initial_pane_title(argv: &[String]) -> String {
    let Some(arg0) = argv.first().map(String::as_str) else {
        return "kettle".into();
    };
    if arg0 == "ssh" {
        let host = argv
            .iter()
            .skip(1)
            .find(|a| !a.starts_with('-'))
            .cloned()
            .unwrap_or_default();
        return if host.is_empty() {
            "ssh".into()
        } else {
            format!("ssh {host}")
        };
    }
    // Basename of the program path — `/usr/bin/htop` → `htop`. Falls back
    // to the raw arg if it has no path separators.
    let base = std::path::Path::new(arg0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(arg0);
    // Shells are intentionally placeholders: the cwd-basename fallback
    // (`~/repos/kettle` → `kettle`) is more useful than the literal "bash".
    if is_known_shell(base) {
        return "kettle".into();
    }
    base.to_string()
}

fn is_known_shell(program: &str) -> bool {
    const SHELLS: &[&str] = &[
        "sh",
        "bash",
        "zsh",
        "fish",
        "dash",
        "ash",
        "ksh",
        "csh",
        "tcsh",
        "nu",
        "elvish",
        "xonsh",
        "pwsh",
        "powershell",
        "cmd",
        "cmd.exe",
        "powershell.exe",
        "pwsh.exe",
    ];
    SHELLS.contains(&program)
}

/// Whether a foreground argv identifies a composer known to understand
/// Kettle's pre-negotiation modified-Enter fallback.
///
/// This is deliberately an allowlist, not "anything that is not a shell".
/// Python, psql, gdb, and countless other readline/libedit programs also put
/// the PTY in noncanonical mode and would print the tail of an unsolicited
/// xterm sequence. Unknown programs get plain Enter until they negotiate a
/// standard xterm/Kitty keyboard protocol; `modify-other-keys = always`
/// remains the explicit compatibility escape hatch.
#[cfg(any(unix, windows, test))]
fn argv_accepts_unnegotiated_modified_enter(argv: &[String]) -> bool {
    kettle_remote::argv_accepts_unnegotiated_modified_enter(argv)
}

#[cfg(any(unix, test))]
fn unix_foreground_program_acceptance(
    foreground_pid: Option<u32>,
    child_pid: Option<u32>,
    launch_argv: &[String],
    snapshot: Option<&kettle_remote::ForegroundProcess>,
) -> Option<bool> {
    let pid = foreground_pid?;
    snapshot
        .filter(|process| process.pid == pid)
        .map(|process| argv_accepts_unnegotiated_modified_enter(&process.argv))
        .or_else(|| {
            (child_pid == Some(pid)).then(|| argv_accepts_unnegotiated_modified_enter(launch_argv))
        })
}

/// Map the kettle config cursor style to the engine's seed shape. `Bar` and
/// `Beam` are the same thing under different names (vertical thin stroke);
/// the engine has more variants (`HollowBlock`, `Hidden`) that only ever
/// arrive via DECSCUSR from a running program, so they're never the
/// *default*.
fn engine_cursor_shape(s: CursorStyle) -> CursorShape {
    match s {
        CursorStyle::Block => CursorShape::Block,
        CursorStyle::Underline => CursorShape::Underline,
        CursorStyle::Bar => CursorShape::Beam,
    }
}

fn unnegotiated_modified_enter(mode: ModifyOtherKeysMode) -> bool {
    mode == ModifyOtherKeysMode::Always
}

use crate::session::{MAX_RESTORE_PANES, SNode, STab, Session};

/// Pixel rectangle: `(x, y, w, h)`.
pub type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Children placed side-by-side (vertical divider between them).
    Horizontal,
    /// Children stacked (horizontal divider between them).
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneTitleOrigin {
    Placeholder,
    ExplicitLaunch,
    Osc,
    Remote,
    /// The user named this pane by hand.
    ///
    /// Without this variant the edit was overwritten by the next OSC 0/2 the
    /// shell emitted — which bash and zsh send on EVERY prompt — so naming a
    /// pane `db-prod` lasted under a second. Terminator keeps the equivalent
    /// state as `titlebar.set_custom_string`, and its editable label no-ops
    /// while custom (editablelabel.py:60-64).
    Manual,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PtyOutputClosePhase {
    #[default]
    NotStarted,
    InProgress(std::time::Instant),
    Failed {
        retry_at: std::time::Instant,
    },
}

pub struct Pane {
    pub term: Terminal,
    pub rx: Receiver<TermEvent>,
    pty_input: PtyInputQueue,
    /// Terminator plugin parity: optional output sidechannel. `Some` when the
    /// LuaEngine subscribed at App startup; the App drains it each tick and
    /// fires LuaEvent::Output(pane_id, bytes).
    pub output_rx: Option<Receiver<Vec<u8>>>,
    pub title: String,
    /// v2.29.0: whether `title` is still the generated seed (no genuine OSC 2
    /// title has arrived). Tab/window/pane labels treat a placeholder title as
    /// "show the cwd instead". Crucially this lets us IGNORE the bogus full-exe
    /// path that conhost/ConPTY injects as the startup OSC 2 title for a native
    /// Windows shell (it would otherwise outrank the cwd). Set `false` the
    /// instant any real OSC 2 title is stored, so a program-set title is never
    /// suppressed. Seeded `true` only for generic-shell panes (see `spawn_pane`).
    pub title_is_placeholder: bool,
    pub(crate) title_origin: PaneTitleOrigin,
    pub(crate) title_before_remote: Option<(String, PaneTitleOrigin)>,
    /// Terminator parity, named broadcast groups foundation: per-pane group
    /// name. When set, the pane is part of a named broadcast group;
    /// keyboard input to any member of the group broadcasts to every
    /// member. None means the pane has no group (Terminator default).
    ///
    /// Distinct from the per-tab broadcast (`BroadcastScope::Tab`, which is
    /// scope=tab, no name): named groups can span multiple tabs +
    /// be selectively enabled. Per-tab broadcast remains the
    /// quick-toggle path.
    pub group_name: Option<String>,
    /// Terminator parity (`icon_bell`): this pane rang its bell and the user
    /// has not looked at it since.
    ///
    /// The tab bar has had its own bell latch for a while, but the per-PANE
    /// one was missing entirely — the renderer's titlebar indicator read a
    /// field the frame builder hard-coded to `false`, so `icon_bell` parsed,
    /// validated, defaulted to on, was documented, and could never draw
    /// anything. Latched like the tab's: set when the bell rings in a pane the
    /// user is not looking at, cleared when they focus it.
    pub bell: bool,
    pub closed: bool,
    /// `exit-action = hold` was silently broken — `reap()`
    /// removed any pane whose child had exited regardless of intent. `held`
    /// marks a pane deliberately KEPT on screen after its shell exited (Hold);
    /// reap skips it until the user explicitly closes it (which sets `closed`).
    pub held: bool,
    /// A held pane keeps its grid but must not keep an exited direct child as a
    /// zombie. `false` means the ordered PTY exit arrived before `try_wait`
    /// could collect the process and about-to-wait must retry on a bounded
    /// cadence.
    pub held_child_reaped: bool,
    /// The UI consumed the PTY's exit event after the reader drained preceding
    /// output. Reaping from a direct process-status poll raced that event and
    /// could drop a fast pane before its final bytes or `exit-action` policy
    /// were applied.
    pub exit_observed: bool,
    /// Windows ConPTY close was started after the direct child exited but EOF
    /// did not arrive within the bounded drain window. Unix enforces the same
    /// bound in its pollable PTY pump.
    #[cfg(windows)]
    pub pty_output_close_phase: PtyOutputClosePhase,
    /// PTY output generation observed at the previous full redraw. This is the
    /// lock-free edge for tab activity and `scroll-on-output`; `None` keeps the
    /// first frame from treating pre-spawn output as new activity.
    pub last_output_generation: Option<u64>,
    /// Launching argv ([] means the configured shell). Held so a
    /// closed-tab snapshot can re-spawn the same program in
    /// `Action::UndoCloseTab` — SSH tabs and `-e PROG`
    /// tabs reopen as the same SSH connection / TUI, not a generic
    /// shell. Doesn't track environment / cwd-after-launch — those
    /// re-derive from the OSC-7 cwd that's already snapshotted.
    pub argv: Vec<String>,
    /// Terminator parity, `plugins/remote.py`, phase 6 of
    /// [`TERMINATOR-REMOTE-DESIGN.md`](
    /// ../../../docs/TERMINATOR-REMOTE-DESIGN.md): the most-recently
    /// detected remote-session context for this pane. Updated by
    /// the App's periodic poll (to be wired). `None`
    /// means either the pane's process tree has no SSH / container
    /// descendant, or the poll hasn't run yet. When non-None, the
    /// pane title shows `format_remote_title(...)` and the right-
    /// click menu exposes a "Clone session" entry.
    pub remote_context: Option<kettle_remote::RemoteContext>,
    /// Latest bounded process-scan result for the program currently attached
    /// to the PTY. Unix input policy accepts it only when its pid still matches
    /// a fresh `tcgetpgrp` snapshot; Windows combines it with OSC 133 state.
    pub foreground_process: Option<kettle_remote::ForegroundProcess>,
    /// Agent-first: set while an agent control connection has
    /// targeted this pane (a mutating method or `subscribe`). Drives the
    /// titlebar agent badge; cleared when the last attached connection drops.
    pub agent_attached: bool,
    /// Terminator parity, terminal_popup_menu.py "Read only": when
    /// true, user input (keystrokes / paste / broadcast) is dropped before it
    /// reaches this pane's PTY — the child keeps producing output, but the pane
    /// can't be typed into. Toggled via `Action::TogglePaneReadOnly` or the
    /// right-click "Read only" item; shown as `[RO]` in the titlebar.
    pub read_only: bool,
    /// Completion integration is negotiated when this shell starts. Config
    /// reloads affect new panes only, so an existing Fish/PowerShell wrapper
    /// never keeps intercepting Tab while its visible card disappears.
    pub completion_overlay: bool,
}

impl Pane {
    /// Live terminal mode adjusted by Kettle's pre-negotiation modified-Enter
    /// policy for this pane. Protocol bits selected by the application remain
    /// untouched and therefore retain precedence in the encoder.
    pub(crate) fn effective_key_mode(
        &self,
        policy: ModifyOtherKeysMode,
        sample_automatic_context: bool,
    ) -> kettle_core::TermMode {
        let enable_fallback = match policy {
            ModifyOtherKeysMode::Always => true,
            ModifyOtherKeysMode::Off => false,
            ModifyOtherKeysMode::Auto if sample_automatic_context => {
                modified_enter_fallback(policy, self.modified_enter_context())
            }
            ModifyOtherKeysMode::Auto => false,
        };
        let mut mode = self
            .term
            .term
            .lock()
            .ok()
            .map(|term| *term.mode())
            .unwrap_or_else(kettle_core::TermMode::empty);
        mode.set(
            kettle_core::TermMode::UNNEGOTIATED_MODIFIED_ENTER,
            enable_fallback,
        );
        mode
    }

    fn modified_enter_context(&self) -> ModifiedEnterContext {
        #[cfg(unix)]
        {
            // Noncanonical mode alone is not evidence of a TUI: zsh, nested
            // shells, and readline REPLs all use it too. Pair a fresh
            // foreground process-group id with the bounded background process
            // snapshot, and accept only a known composer. A stale or missing
            // snapshot returns plain CR.
            let foreground_program = unix_foreground_program_acceptance(
                self.term.foreground_process_group().ok(),
                self.term.child_pid(),
                &self.argv,
                self.foreground_process.as_ref(),
            );
            let context = ModifiedEnterContext::UnixPty {
                canonical: self.term.input_is_canonical().ok(),
                foreground_program,
            };
            log::trace!("modified-Enter auto context: {context:?}");
            context
        }
        #[cfg(windows)]
        {
            ModifiedEnterContext::WindowsShell {
                activity: self.term.shell_activity(),
                foreground_program: self
                    .foreground_process
                    .as_ref()
                    .map(|process| argv_accepts_unnegotiated_modified_enter(&process.argv)),
                launch_program: argv_accepts_unnegotiated_modified_enter(&self.argv),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            ModifiedEnterContext::Unsupported
        }
    }

    /// Terminator parity: write user-originated input (keystroke /
    /// paste / IME / drag-drop / send-text) to the PTY, honoring the read-only
    /// toggle. Returns the explicit enqueue outcome. VTE
    /// `feed_child` + `input-enabled` semantics: read-only blocks the *user*
    /// (and anything acting as the user — Lua send_text, remote.cmd, agent
    /// `send_text`/`run_command`), NOT the terminal protocol. Both paths share
    /// the bounded PTY-input worker so neither can block the UI.
    pub fn feed_input(&self, bytes: &[u8]) -> PaneInputResult {
        if let Some(result) = pane_input_policy(self.pty_input.failed(), self.read_only) {
            return result;
        }
        if bytes.len() > MAX_USER_INPUT_MESSAGE_BYTES {
            return PaneInputResult::Oversize;
        }
        self.feed_input_shared(Arc::from(bytes))
    }

    pub fn feed_input_shared(&self, bytes: Arc<[u8]>) -> PaneInputResult {
        if let Some(result) = pane_input_policy(self.pty_input.failed(), self.read_only) {
            return result;
        }
        let input = bytes.as_ref();
        self.term.with_completion_input_admission(&[input], || {
            let result = self.pty_input.enqueue_user(bytes.clone());
            (result, result.is_queued())
        })
    }

    /// Admit an already validated synthetic key batch as one queue item while
    /// retaining each key boundary for completion request accounting.
    pub fn feed_key_inputs(&self, inputs: &[Vec<u8>]) -> PaneInputResult {
        if let Some(result) = pane_input_policy(self.pty_input.failed(), self.read_only) {
            return result;
        }
        let Some(total) = inputs
            .iter()
            .try_fold(0usize, |total, input| total.checked_add(input.len()))
        else {
            return PaneInputResult::Oversize;
        };
        if total > MAX_USER_INPUT_MESSAGE_BYTES {
            return PaneInputResult::Oversize;
        }
        let mut joined = Vec::with_capacity(total);
        for input in inputs {
            joined.extend_from_slice(input);
        }
        let boundaries: Vec<&[u8]> = inputs.iter().map(Vec::as_slice).collect();
        self.term.with_completion_input_admission(&boundaries, || {
            let result = self.pty_input.enqueue_user(Arc::from(joined));
            (result, result.is_queued())
        })
    }

    /// Queue DEC focus reporting in the chronological user-input lane without
    /// classifying it as an editor mutation. `CSI I` / `CSI O` are generated by
    /// Kettle after an OS focus event, not typed text; treating them as text
    /// consumed the first managed completion sync before PowerShell's initial
    /// prompt had crossed ConPTY. Keep the user lane so a report cannot jump
    /// ahead of already queued keystrokes.
    pub fn feed_focus_report(&self, focused: bool) -> PaneInputResult {
        if let Some(result) = pane_input_policy(self.pty_input.failed(), self.read_only) {
            return result;
        }
        self.pty_input.enqueue_user(Arc::from(if focused {
            &b"\x1b[I"[..]
        } else {
            &b"\x1b[O"[..]
        }))
    }

    /// Queue a terminal-protocol reply or report irrespective of read-only
    /// mode. Failure is sticky and causes the pane to be torn down.
    pub fn queue_protocol_reply(&self, bytes: &[u8]) -> PaneInputResult {
        if bytes.len() > MAX_PROTOCOL_REPLY_MESSAGE_BYTES {
            self.pty_input.fail();
            return PaneInputResult::Failed;
        }
        self.pty_input.enqueue_reply(Arc::from(bytes))
    }

    pub fn pty_input_failed(&self) -> bool {
        self.pty_input.failed()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModifiedEnterContext {
    #[cfg(any(unix, test))]
    UnixPty {
        canonical: Option<bool>,
        foreground_program: Option<bool>,
    },
    #[cfg(any(windows, test))]
    WindowsShell {
        activity: kettle_core::ShellActivity,
        foreground_program: Option<bool>,
        launch_program: bool,
    },
    #[cfg(any(not(any(unix, windows)), test))]
    Unsupported,
}

fn modified_enter_fallback(policy: ModifyOtherKeysMode, context: ModifiedEnterContext) -> bool {
    match policy {
        ModifyOtherKeysMode::Always => true,
        ModifyOtherKeysMode::Off => false,
        ModifyOtherKeysMode::Auto => match context {
            #[cfg(any(unix, test))]
            ModifiedEnterContext::UnixPty {
                canonical: Some(false),
                foreground_program: Some(true),
            } => true,
            #[cfg(any(windows, test))]
            ModifiedEnterContext::WindowsShell {
                activity: kettle_core::ShellActivity::Running,
                foreground_program: Some(true),
                ..
            } => true,
            #[cfg(any(windows, test))]
            ModifiedEnterContext::WindowsShell {
                launch_program: true,
                ..
            } => true,
            #[cfg(any(unix, test))]
            ModifiedEnterContext::UnixPty { .. } => false,
            #[cfg(any(windows, test))]
            ModifiedEnterContext::WindowsShell { .. } => false,
            #[cfg(any(not(any(unix, windows)), test))]
            ModifiedEnterContext::Unsupported => false,
        },
    }
}

/// Pure reap predicate. A pane is removed when explicitly closed (Close /
/// Restart / ClosePane set `closed`) OR after the UI consumes its drained PTY
/// exit event and it is not being HELD on screen (`exit-action = hold`). A raw
/// child-status poll is deliberately insufficient: it can win before the
/// reader publishes the child's final output and before exit policy runs.
pub(crate) fn is_reapable(closed: bool, held: bool, exit_observed: bool) -> bool {
    closed || (!held && exit_observed)
}

/// Whether splitting a pane launched as `argv` should start the configured
/// shell instead of repeating that launch.
///
/// A pane with no argv is already the shell. Beyond that, kettle clones an
/// ordinary shell launch but deliberately falls back for a *direct* one — an
/// editor, an agent CLI — so the new pane is somewhere to work rather than a
/// second copy of the program you were reading. Terminator has no such
/// distinction: `always_split_with_profile` forces the new terminal onto the
/// parent's profile, custom command and all (`paned.py`), and setting it here
/// asks for exactly that.
pub(crate) fn split_falls_back_to_shell(argv: &[String], always_with_profile: bool) -> bool {
    argv.is_empty() || (!always_with_profile && direct_launch_splits_to_shell(argv))
}

pub enum Node {
    Leaf(u64),
    Split {
        dir: Dir,
        /// Fraction of the area given to child `a`. Kept strictly inside
        /// `(0, 1)`; how small a pane may actually get is decided in pixels by
        /// [`split_extent_px`], not by this number.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    fn first_leaf(&self) -> u64 {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { a, .. } => a.first_leaf(),
        }
    }

    /// Find the leaf id that should receive focus when
    /// `id` is removed from this tree. Returns the first leaf of
    /// `id`'s sibling subtree at the deepest Split containing
    /// `id`. Returns `None` if `id` isn't a leaf in this tree, or
    /// if the tree is a single Leaf (no sibling to promote).
    ///
    /// User-reported bug: `close_focused` was setting
    /// `tab.focus = tab.root.first_leaf()` after the close, which
    /// always jumps to the LEFTMOST leaf of the whole tab — i.e.,
    /// the first pane the user split from. Closing a deeply-nested
    /// pane felt teleporting. `neighbor_of` walks to the closed
    /// pane's split-mate instead, matching what every other
    /// terminal multiplexer does (tmux, wezterm, kitty).
    fn neighbor_of(&self, id: u64) -> Option<u64> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { a, b, .. } => {
                // If `id` is a direct Leaf child of this Split, the
                // sibling subtree's first leaf is the right neighbor.
                // Otherwise recurse — the deeper recursion finds the
                // sibling at the actual Split that contains `id`.
                if matches!(a.as_ref(), Node::Leaf(x) if *x == id) {
                    return Some(b.first_leaf());
                }
                if matches!(b.as_ref(), Node::Leaf(x) if *x == id) {
                    return Some(a.first_leaf());
                }
                a.neighbor_of(id).or_else(|| b.neighbor_of(id))
            }
        }
    }

    fn contains(&self, id: u64) -> bool {
        match self {
            Node::Leaf(x) => *x == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// DFS-order index of the leaf with id `target`, or `None` if not
    /// present. Used by session save to record which leaf is focused
    /// without depending on the per-pane numeric id (which is reallocated
    /// across restores). Walk order is the same `first → second` child
    /// order that `nth_leaf` uses, so the round trip is symmetric.
    fn leaf_index_of(&self, target: u64) -> Option<usize> {
        fn walk(n: &Node, target: u64, idx: &mut usize) -> Option<usize> {
            match n {
                Node::Leaf(id) => {
                    let here = *idx;
                    *idx += 1;
                    if *id == target { Some(here) } else { None }
                }
                Node::Split { a, b, .. } => walk(a, target, idx).or_else(|| walk(b, target, idx)),
            }
        }
        let mut idx = 0;
        walk(self, target, &mut idx)
    }

    /// All leaf ids in DFS-order. Used by `broadcast_write` to scope
    /// broadcast input to one tab's panes rather than every pane in every
    /// tab (`Action::ToggleBroadcastAll` was originally
    /// "every pane in the whole mux", a footgun for users with several
    /// tabs since typing one char would echo into every pane everywhere;
    /// per-tab matches Terminator's `broadcast_all` and is what users
    /// actually mean when they're paralleling SSH sessions).
    pub fn leaf_ids(&self) -> Vec<u64> {
        fn walk(n: &Node, out: &mut Vec<u64>) {
            match n {
                Node::Leaf(id) => out.push(*id),
                Node::Split { a, b, .. } => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut v = Vec::new();
        walk(self, &mut v);
        v
    }

    /// Leaf id at DFS-order position `n`, or the first leaf if `n` is past
    /// the end (graceful fallback so a session pointing to a no-longer-
    /// existent pane still produces a focused tab).
    fn nth_leaf(&self, n: usize) -> u64 {
        fn walk(node: &Node, n: usize, idx: &mut usize) -> Option<u64> {
            match node {
                Node::Leaf(id) => {
                    if *idx == n {
                        return Some(*id);
                    }
                    *idx += 1;
                    None
                }
                Node::Split { a, b, .. } => walk(a, n, idx).or_else(|| walk(b, n, idx)),
            }
        }
        let mut idx = 0;
        walk(self, n, &mut idx).unwrap_or_else(|| self.first_leaf())
    }

    /// Replace the leaf `id` with a split of itself and `new_id`.
    fn split_leaf(&mut self, id: u64, new_id: u64, dir: Dir) -> bool {
        self.split_leaf_ordered(id, new_id, dir, false)
    }

    /// Split leaf `id`, placing `new_id` first when `new_first`.
    ///
    /// A plain split always appends, which is right when the user asked for
    /// "split right" and the new pane belongs on the right. Moving a pane is
    /// different: dropping it on the LEFT half of a target means it goes to the
    /// left, and appending would silently put it on the other side of the pane
    /// the user aimed at.
    fn split_leaf_ordered(&mut self, id: u64, new_id: u64, dir: Dir, new_first: bool) -> bool {
        match self {
            Node::Leaf(x) if *x == id => {
                let (a, b) = if new_first {
                    (Node::Leaf(new_id), Node::Leaf(id))
                } else {
                    (Node::Leaf(id), Node::Leaf(new_id))
                };
                *self = Node::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(a),
                    b: Box::new(b),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                a.split_leaf_ordered(id, new_id, dir, new_first)
                    || b.split_leaf_ordered(id, new_id, dir, new_first)
            }
        }
    }

    /// Remove leaf `id`; `Err(None)` means this node was the leaf (caller
    /// drops it), `Err(Some(sibling))` means replace this node with `sibling`.
    fn remove_leaf(self, id: u64) -> Result<Node, Option<Node>> {
        match self {
            Node::Leaf(x) if x == id => Err(None),
            Node::Leaf(x) => Ok(Node::Leaf(x)),
            Node::Split { dir, ratio, a, b } => match a.remove_leaf(id) {
                Err(None) => Err(Some(*b)),
                Err(Some(n)) => Ok(Node::Split {
                    dir,
                    ratio,
                    a: Box::new(n),
                    b,
                }),
                Ok(a) => match b.remove_leaf(id) {
                    Err(None) => Err(Some(a)),
                    Err(Some(n)) => Ok(Node::Split {
                        dir,
                        ratio,
                        a: Box::new(a),
                        b: Box::new(n),
                    }),
                    Ok(b) => Ok(Node::Split {
                        dir,
                        ratio,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                },
            },
        }
    }

    fn layout(&self, rect: Rect, out: &mut Vec<(u64, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { dir, ratio, a, b } => {
                let (x, y, w, h) = rect;
                match dir {
                    Dir::Horizontal => {
                        let aw = split_extent_px(w, *ratio);
                        a.layout((x, y, aw, h), out);
                        b.layout((x + aw, y, w - aw, h), out);
                    }
                    Dir::Vertical => {
                        let ah = split_extent_px(h, *ratio);
                        a.layout((x, y, w, ah), out);
                        b.layout((x, y + ah, w, h - ah), out);
                    }
                }
            }
        }
    }

    /// v2.20.0 (`equalize_splits`, Ghostty/Terminator parity): rebalance the
    /// whole tree so every LEAF gets equal area. Each split's ratio becomes
    /// `leaves(a) / (leaves(a) + leaves(b))` — for a chain of N panes along
    /// one axis that yields 1/N each; mixed orientations get equal areas
    /// proportionally. Returns the subtree's leaf count. Pure tree math
    /// (unit-tested); the caller follows with `resize_all` to push the new
    /// geometry into the PTYs.
    ///
    /// The exact `la / (la + lb)` is stored, with no ratio band applied. A
    /// balanced chain of N panes on one axis needs ratios of `1/N`, and the old
    /// fixed `[0.05, 0.95]` clamp could not represent anything past twenty —
    /// past that the outermost panes were handed more than their share and every
    /// pane after them was starved, so "equalize" visibly stopped equalizing.
    /// The floor that keeps a pane usable lives in `split_extent_px`, measured
    /// in pixels against the space actually available.
    pub(crate) fn equalize(&mut self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, ratio, .. } => {
                let la = a.equalize();
                let lb = b.equalize();
                *ratio = la as f32 / (la + lb) as f32;
                la + lb
            }
        }
    }

    /// Adjust the ratio of the innermost split matching `dir` that contains
    /// `focus`.
    fn resize(&mut self, focus: u64, dir: Dir, delta: f32) -> bool {
        if let Node::Split {
            dir: d,
            ratio,
            a,
            b,
        } = self
        {
            if a.resize(focus, dir, delta) || b.resize(focus, dir, delta) {
                return true;
            }
            if *d == dir && (a.contains(focus) || b.contains(focus)) {
                *ratio = sane_ratio(*ratio + delta);
                return true;
            }
        }
        false
    }

    /// Collect every split's divider seam, each tagged with
    /// the `path` (a/b descent from the root) that addresses it for mutation,
    /// its `dir`, the split's full `rect`, and the seam coordinate `pos` (x for
    /// a Horizontal split's vertical divider, y for a Vertical split's
    /// horizontal divider). Mirrors `layout`'s geometry exactly so a hit-test
    /// against these seams matches what the renderer drew. Drives mouse
    /// drag-to-resize of split dividers.
    fn dividers(&self, rect: Rect, path: &mut Vec<bool>, out: &mut Vec<SplitSeam>) {
        if let Node::Split { dir, ratio, a, b } = self {
            let (x, y, w, h) = rect;
            let (a_rect, b_rect, pos) = match dir {
                Dir::Horizontal => {
                    let aw = split_extent_px(w, *ratio);
                    ((x, y, aw, h), (x + aw, y, w - aw, h), x + aw)
                }
                Dir::Vertical => {
                    let ah = split_extent_px(h, *ratio);
                    ((x, y, w, ah), (x, y + ah, w, h - ah), y + ah)
                }
            };
            out.push(SplitSeam {
                path: path.clone(),
                dir: *dir,
                rect,
                pos,
            });
            path.push(false);
            a.dividers(a_rect, path, out);
            path.pop();
            path.push(true);
            b.dividers(b_rect, path, out);
            path.pop();
        }
    }

    /// Set the ratio of the split addressed by `path` (the a/b
    /// descent produced by `dividers`). Returns false if the path doesn't land
    /// on a split (stale path after a layout change). Callers that have the
    /// split's rect (the drag handler, via `ratio_from_pos`) already hold the
    /// divider `MIN_SPLIT_PX` away from either edge; this only rejects the
    /// non-finite and out-of-range values a caller without a rect could pass.
    fn set_ratio_at(&mut self, path: &[bool], ratio: f32) -> bool {
        match self {
            Node::Split { ratio: r, a, b, .. } => match path.split_first() {
                None => {
                    *r = sane_ratio(ratio);
                    true
                }
                Some((&go_b, rest)) => {
                    if go_b {
                        b.set_ratio_at(rest, ratio)
                    } else {
                        a.set_ratio_at(rest, ratio)
                    }
                }
            },
            Node::Leaf(_) => false,
        }
    }
}

/// One split divider, addressable for mouse drag-to-resize.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitSeam {
    /// a/b descent from the tab root that uniquely addresses the split node.
    pub path: Vec<bool>,
    pub dir: Dir,
    /// The split's full rect — the basis for converting a cursor position into
    /// a new ratio.
    pub rect: Rect,
    /// Seam coordinate: x for a Horizontal split (vertical divider line), y for
    /// a Vertical split (horizontal divider line).
    pub pos: f32,
}

/// Smallest extent, in pixels, either side of a split may be given.
///
/// This replaces a fixed `[0.05, 0.95]` ratio band that used to be re-applied at
/// every layout and mutation site. A fixed fraction cannot express a balanced
/// chain of more than twenty panes on one axis, and it scales the wrong way: on
/// a 1900px-wide window it reserved 95px for a pane the user was trying to drag
/// out of the way, while on a narrow window it reserved almost nothing. A pixel
/// floor binds only when a pane would actually become too small to read or to
/// grab by its divider, and stays out of the way otherwise.
///
/// Sized against what a pane needs to remain usable rather than pretty: roughly
/// one cell of height or two of width at 96 DPI, and comfortably wider than the
/// `seam_at` hit tolerance, so the divider of a pane squeezed to the floor can
/// still be grabbed and dragged back.
pub const MIN_SPLIT_PX: f32 = 16.0;

/// Ratio guard for the paths that have no rect to measure against — keyboard
/// resize, a restored session file, a control-plane request. It only keeps the
/// ratio finite and strictly inside `(0, 1)`; the usable minimum is enforced in
/// pixels by [`split_extent_px`] at layout time, where the space is known.
pub fn sane_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(MIN_SPLIT_RATIO, 1.0 - MIN_SPLIT_RATIO)
    } else {
        0.5
    }
}

/// Exactly representable, and small enough that it never rounds a legitimate
/// `1/N` for any pane count a window can hold.
const MIN_SPLIT_RATIO: f32 = 1.0 / 1024.0;

/// Extent of a split's first child along the split axis, in pixels.
///
/// Both children keep at least [`MIN_SPLIT_PX`] whenever the split is big enough
/// to afford it; below that the space is halved rather than handing one child
/// everything. Every place that turns a ratio into geometry goes through this —
/// `layout`, `dividers`, and the session restore path — so a divider is always
/// drawn where the pane it separates actually starts.
pub fn split_extent_px(total: f32, ratio: f32) -> f32 {
    if !total.is_finite() || total <= 0.0 {
        return 0.0;
    }
    let floor = MIN_SPLIT_PX.min(total / 2.0);
    (total * sane_ratio(ratio))
        .round()
        .clamp(floor, total - floor)
}

/// The ratio a Horizontal/Vertical split should take so its divider
/// sits under the cursor, held far enough from either edge that both panes stay
/// grabbable — the same [`MIN_SPLIT_PX`] floor `layout` enforces.
pub fn ratio_from_pos(rect: Rect, dir: Dir, px: f32, py: f32) -> f32 {
    let (x, y, w, h) = rect;
    let (total, offset) = match dir {
        Dir::Horizontal => (w, px - x),
        Dir::Vertical => (h, py - y),
    };
    if !total.is_finite() || total <= 0.0 || !offset.is_finite() {
        return 0.5;
    }
    let floor = MIN_SPLIT_PX.min(total / 2.0);
    sane_ratio(offset.clamp(floor, total - floor) / total)
}

/// Index of the first seam within `tol` px of the cursor (along the
/// seam's perpendicular axis) and inside the split's cross-axis extent. Inner
/// (deeper) seams are pushed AFTER their ancestors by `dividers`, so a tie near
/// nested dividers resolves to the outer split — a stable, predictable pick.
pub fn seam_at(seams: &[SplitSeam], px: f32, py: f32, tol: f32) -> Option<usize> {
    seams.iter().position(|s| {
        let (x, y, w, h) = s.rect;
        match s.dir {
            // Vertical divider line at x = pos; cursor must be near it
            // horizontally and within the split's vertical span.
            Dir::Horizontal => (px - s.pos).abs() <= tol && py >= y && py <= y + h,
            // Horizontal divider line at y = pos.
            Dir::Vertical => (py - s.pos).abs() <= tol && px >= x && px <= x + w,
        }
    })
}

/// Which edge of the pane at `rect` the cursor sits nearest, expressed as the
/// `(dir, before)` pair [`Mux::move_pane_beside`] takes — the mouse half of
/// dragging a terminal somewhere else (Terminator's `terminal.py` drag/drop).
///
/// The rect is split into four triangles by its diagonals, so every point
/// inside belongs to exactly one edge and there is no dead centre where a drop
/// would mean nothing. A quarter-band model was the alternative and was
/// rejected for exactly that: it leaves a middle region with no defined action,
/// and on a narrow pane the bands overlap and the middle vanishes instead.
///
/// `None` when the point is outside `rect` or the rect has no area — a caller
/// with no target must draw no hint, not guess one.
pub fn pane_drop_zone(rect: Rect, px: f32, py: f32) -> Option<(Dir, bool)> {
    let (x, y, w, h) = rect;
    if !(w > 0.0 && h > 0.0 && px.is_finite() && py.is_finite()) {
        return None;
    }
    if px < x || px >= x + w || py < y || py >= y + h {
        return None;
    }
    // Normalised so the diagonals are the lines v = u and v = 1 - u whatever
    // the pane's aspect ratio. Comparing raw pixels instead would tilt the
    // triangles on any non-square pane, and a wide pane would answer "top" for
    // most of its width.
    let u = (px - x) / w;
    let v = (py - y) / h;
    // Ordered, with `<=` on the first two arms, so a point exactly on a
    // diagonal resolves the same way every frame. An undecided boundary would
    // flicker the hint between two zones as the pointer crawls along it.
    Some(if v <= u && v <= 1.0 - u {
        (Dir::Vertical, true) // top
    } else if v >= u && v >= 1.0 - u {
        (Dir::Vertical, false) // bottom
    } else if u < v {
        (Dir::Horizontal, true) // left
    } else {
        (Dir::Horizontal, false) // right
    })
}

/// The sub-rect of `rect` a drop into `(dir, before)` would fill, for painting
/// the hint. Half the pane along the split axis, matching the 50/50 ratio
/// [`Mux::move_pane_beside`] grafts at, so the preview shows the real result
/// rather than a decorative stripe.
pub fn pane_drop_preview(rect: Rect, dir: Dir, before: bool) -> Rect {
    let (x, y, w, h) = rect;
    match (dir, before) {
        (Dir::Horizontal, true) => (x, y, w / 2.0, h),
        (Dir::Horizontal, false) => (x + w / 2.0, y, w / 2.0, h),
        (Dir::Vertical, true) => (x, y, w, h / 2.0),
        (Dir::Vertical, false) => (x, y + h / 2.0, w, h / 2.0),
    }
}

pub struct Tab {
    pub root: Node,
    pub focus: u64,
    /// Terminator parity, terminatorlib/notebook.py: an
    /// optional user-set title override. When `Some(s)`, the tab
    /// bar displays `s` instead of the focused pane's title.
    /// Cleared automatically when the user opens a new tab —
    /// sticky-override behavior matches Terminator.
    pub title_override: Option<String>,
    /// When true, only the focused pane is shown at full size.
    pub zoomed: bool,
    /// Per-tab activity state for the tab-bar dot
    /// indicator. `last_output_at` updates whenever any pane in this
    /// tab produces output. `last_seen_at` updates when this tab
    /// becomes active. The renderer compares the two to decide
    /// whether to draw the "new output in inactive tab" dot. `bell`
    /// latches a `TermEvent::Bell` from any pane in this tab until
    /// the user activates the tab. Matches the Terminator "Activity
    /// Watcher" affordance.
    pub last_output_at: Option<std::time::Instant>,
    pub last_seen_at: Option<std::time::Instant>,
    pub bell: bool,
}

/// Activity state of an *inactive* tab, used by the renderer to pick
/// the tab-bar indicator-dot color. Active tabs are always `Normal`
/// (the focused tab's "you are here" accent already says enough).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabActivity {
    /// Nothing to surface — active tab OR inactive tab with no output
    /// since the user last saw it.
    Normal,
    /// Output arrived since the user last looked at this tab. Drawn
    /// as a cyan dot — the standard "something happened" cue.
    Output,
    /// A `TermEvent::Bell` fired since the user last looked. Drawn as
    /// a yellow dot, overrides `Output` because a bell is a stronger
    /// signal (the focused program explicitly asked for attention).
    Bell,
    /// Tab had unseen output but no further bytes for ≥ the
    /// configured `tab-silence-threshold-ms`. Terminator's "Silence
    /// Watcher" affordance — useful for tail-following long jobs
    /// (`tail -f`, build watchers, network monitors) where the
    /// *absence* of recent output is the signal the user wants.
    /// Drawn as a dim chrome-gray dot to read as a state distinct
    /// from `Output` (cyan) and `Bell` (yellow).
    Silent,
}

/// Pure: classify an inactive tab's activity from its state. Active
/// tabs short-circuit to `Normal` because the focused-pane border and
/// the tab-bar accent already convey focus — adding a dot there would
/// be redundant.
///
/// `now` and `silence_threshold` drive the `TabActivity::Silent` variant
/// — when an inactive tab had unseen output that's been quiet for at
/// least the threshold, the indicator transitions Output → Silent.
/// Passing the wall clock in (rather than calling `Instant::now()`
/// internally) keeps the function pure and unit-testable.
pub fn classify_tab_activity(
    is_active: bool,
    bell: bool,
    last_output_at: Option<std::time::Instant>,
    last_seen_at: Option<std::time::Instant>,
    now: std::time::Instant,
    silence_threshold: std::time::Duration,
) -> TabActivity {
    if is_active {
        return TabActivity::Normal;
    }
    if bell {
        return TabActivity::Bell;
    }
    let unseen_output = match (last_output_at, last_seen_at) {
        (Some(o), Some(s)) => o > s,
        (Some(_), None) => true,
        _ => false,
    };
    if !unseen_output {
        return TabActivity::Normal;
    }
    // Unwrap-safe: `unseen_output` is true only when `last_output_at`
    // is Some.
    let last_out = last_output_at.unwrap();
    // `saturating_duration_since` so a tab whose `last_output_at` is
    // (somehow) in the future doesn't flip Silent — that'd be a
    // monotonic-clock bug, not a tab actually going quiet.
    if now.saturating_duration_since(last_out) >= silence_threshold {
        TabActivity::Silent
    } else {
        TabActivity::Output
    }
}

/// Snapshot of a tab captured at close time so `Action::UndoCloseTab`
/// can re-spawn the same program in the same directory. WezTerm /
/// browser-tab convention; closing a tab is no longer irreversible.
/// Tree topology isn't preserved — undo re-creates as a single pane
/// from the first leaf's argv+cwd (the user's complaint is "bring my
/// tab back," not "reproduce my exact split layout from N closes ago").
#[derive(Clone)]
pub struct ClosedTab {
    /// Tab index at the time of close. On undo we clamp to the
    /// current tab-count so an `undo` after several intervening
    /// `new_tab`s still lands somewhere sensible.
    pub original_index: usize,
    /// Argv of the first leaf — empty means the configured shell.
    pub argv: Vec<String>,
    /// OSC-7 cwd of the first leaf at the moment of close, or `None`
    /// if no usable cwd was reported.
    pub cwd: Option<String>,
}

/// Max closed-tab snapshots held for `Action::UndoCloseTab`. Browser-
/// standard is 8-10; we keep 10 to amortize accidental close-bursts.
const CLOSED_TAB_RING_CAP: usize = 10;

/// Phase 2 of [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
/// ../../../docs/TERMINATOR-NAMED-GROUPS-DESIGN.md): the
/// broadcast-scope enum the design proposes. The existing
/// `mux.broadcast: bool` represents `Off | Tab`; a follow-up
/// will migrate it to this richer enum so `Group(name)`
/// (scope-by-name-across-tabs) becomes expressible.
///
/// Lands the type now so the `Action::GroupTab` etc.
/// dispatch can be wired against the final shape ahead of the
/// refactor.
// 2026-05-23: removed stale `#[allow(dead_code)]`. The
// All + Group variants are now consumed by the
// GroupTab + GroupWindow + CreateGroup dispatch arms in app.rs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BroadcastScope {
    #[default]
    Off,
    /// Per-tab broadcast: every pane in the focused
    /// tab receives input. Today's `mux.broadcast = true`
    /// behavior.
    Tab,
    /// Window-wide: every pane in every tab receives input.
    All,
    /// Named group: every pane whose `Pane::group_name` matches
    /// receives input. Span across tabs is what makes named
    /// groups distinct from per-tab broadcast.
    Group(String),
}

/// Pure helper that computes the set of pane IDs that
/// should receive a broadcast for the given scope. Pure — takes
/// `(scope, focused_pane_id, panes_in_focused_tab, all_panes_with_groups)`
/// and returns the target list. Unit-testable.
///
/// `all_panes_with_groups` is a slice of `(pane_id, Option<&str>
/// group)` pairs covering every pane in every tab. The caller
/// is responsible for assembling it (a one-liner over
/// `self.panes.iter()`).
// 2026-05-23: removed stale `#[allow(dead_code)]`.
// `compute_broadcast_targets` is the impl behind the public
// `Mux::broadcast_targets` (called from app.rs).
pub fn compute_broadcast_targets(
    scope: &BroadcastScope,
    focused_pane: u64,
    panes_in_focused_tab: &[u64],
    all_panes_with_groups: &[(u64, Option<&str>)],
) -> Vec<u64> {
    match scope {
        BroadcastScope::Off => vec![focused_pane],
        BroadcastScope::Tab => panes_in_focused_tab.to_vec(),
        BroadcastScope::All => all_panes_with_groups.iter().map(|(id, _)| *id).collect(),
        BroadcastScope::Group(name) => {
            // Every pane tagged with this group, regardless of tab — plus the
            // focused (on-screen) pane, so input is never routed AWAY from the
            // pane the user is looking at with no cue. The focused pane may not
            // be a group member (e.g. broadcasting from an ungrouped pane into a
            // named group); union it in (deduped) so the on-screen pane always
            // receives input, mirroring how Off/Tab/All already include it.
            let mut targets: Vec<u64> = all_panes_with_groups
                .iter()
                .filter(|(_, g)| g.as_deref() == Some(name.as_str()))
                .map(|(id, _)| *id)
                .collect();
            if !targets.contains(&focused_pane) {
                targets.push(focused_pane);
            }
            targets
        }
    }
}

/// C2 (multi-window): process-global pane-id allocator. Pane ids must be
/// unique across EVERY window's Mux — the agent control API (`kettle ctl
/// --pane N`), Lua hooks, and `pending_runs` all address panes by bare id,
/// and a live tab move (C5) carries its panes' ids into another window's Mux.
/// A per-Mux counter would collide the moment a second window spawned a pane.
/// Starts at 1 (id 0 is never a valid pane, matching the old per-Mux seed).
static NEXT_PANE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A tab lifted out of one Mux, panes and all, ready to be attached to
/// another (the C5 live tab move — PTYs keep running, nothing respawns).
/// Pane ids stay valid across the move because they're process-global.
pub struct DetachedTab {
    pub tab: Tab,
    pub panes: Vec<(u64, Pane)>,
}

pub struct Mux {
    pub tabs: Vec<Tab>,
    pub panes: HashMap<u64, Pane>,
    pub active: usize,
    /// Phase 3 of the named-groups design:
    /// migrated from `bool` to `BroadcastScope`. The
    /// per-tab broadcast = `BroadcastScope::Tab`; old "off" =
    /// `Off`. New variants: `All` (window-wide), `Group(name)`
    /// (cross-tab named group). Callers that just want a
    /// yes/no should use `Mux::is_broadcast_on()`.
    pub broadcast: BroadcastScope,
    /// Set when a LuaEngine subscribes at App startup.
    /// Controls whether spawn_pane attaches the output sidechannel
    /// to new PTYs (zero-cost when false: no per-PTY-read alloc).
    pub lua_output_subscribed: bool,
    /// When the dev recorder is teeing PTY output, use bounded lossless
    /// backpressure rather than the Lua plugin tap's best-effort delivery.
    pub record_lossless: bool,
    /// Effective OSC 52 write capability after combining config policy with
    /// platform clipboard availability. New panes inherit it and live panes
    /// receive updates through [`Mux::set_osc52_copy_allowed`].
    pub osc52_copy_allowed: bool,
    /// Ring buffer of recently-closed tab snapshots.
    /// Bounded so a long-running session doesn't accumulate state.
    /// LIFO: `pop_back` returns the most-recently-closed tab.
    pub closed_tabs: std::collections::VecDeque<ClosedTab>,
    /// Terminator's `autoclean_groups` (`terminator.py:group_hoover`): forget a
    /// broadcast group once its last pane is gone. Terminator keeps an explicit
    /// group list to prune; kettle's groups are just the names its panes carry,
    /// so the only thing that can outlive its members is a broadcast *scope*
    /// still aimed at the dead group — which would keep the titlebar claiming a
    /// group that no longer exists and re-capture any pane later given the same
    /// name. `App` owns the process-wide membership sweep because a group can
    /// span several muxes. Mirrored from the config at window construction.
    pub autoclean_groups: bool,
}

impl Mux {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            panes: HashMap::new(),
            active: 0,
            broadcast: BroadcastScope::Off,
            lua_output_subscribed: false,
            record_lossless: false,
            osc52_copy_allowed: true,
            closed_tabs: std::collections::VecDeque::with_capacity(CLOSED_TAB_RING_CAP),
            autoclean_groups: true,
        }
    }

    /// Push an edited scrollback budget into every live pane. Returns how many
    /// panes' effective cap actually moved — zero when the setting is unchanged
    /// or already satisfied at this geometry.
    ///
    /// See [`kettle_core::Terminal::set_scrollback_limits`] for why a *setting*
    /// change may lower a cap that a *resize* must not.
    pub fn set_scrollback_limits(&mut self, lines: usize, bytes: usize) -> usize {
        let mut changed = 0;
        for pane in self.panes.values_mut() {
            if pane.term.set_scrollback_limits(lines, bytes) {
                changed += 1;
            }
        }
        changed
    }

    pub fn set_osc52_copy_allowed(&mut self, allowed: bool) {
        self.osc52_copy_allowed = allowed;
        for pane in self.panes.values() {
            pane.term.set_osc52_copy_allowed(allowed);
        }
    }

    pub fn set_unnegotiated_modified_enter(&mut self, mode: ModifyOtherKeysMode) {
        let enabled = unnegotiated_modified_enter(mode);
        for pane in self.panes.values_mut() {
            pane.term.set_unnegotiated_modified_enter(enabled);
        }
    }

    /// Mark the active tab as just-seen by the user — clears its bell
    /// flag and updates `last_seen_at` so `classify_tab_activity` no
    /// longer reports `Output` / `Bell` on it. Call after any
    /// `self.active = ...` change so the tab the user just switched
    /// to drops its indicator immediately.
    pub fn touch_active_tab_seen(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.last_seen_at = Some(std::time::Instant::now());
            tab.bell = false;
        }
    }

    /// Find the tab containing `pane_id` and record output activity on
    /// it. Skipped for the currently-active tab (the user is looking at
    /// it; surfacing a dot would be visual noise). Called from the
    /// chrome layer on every pane redraw — see `App::drain_events`.
    pub fn touch_tab_output(&mut self, pane_id: u64) {
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i == active {
                continue;
            }
            if tab.root.contains(pane_id) {
                tab.last_output_at = Some(std::time::Instant::now());
                return;
            }
        }
    }

    /// Latch a `TermEvent::Bell` from `pane_id` onto its containing
    /// tab so the indicator survives until the user activates the
    /// tab. Skipped for the active tab (the visual-bell flash already
    /// surfaces it there).
    pub fn touch_tab_bell(&mut self, pane_id: u64) {
        // Terminator parity (`icon_bell`): the pane's own titlebar indicator
        // latches on the same rule as the tab's — the pane the user is
        // already looking at needs no "look here", and the visual-bell flash
        // covers it.
        if Some(pane_id) != self.active_focus()
            && let Some(pane) = self.panes.get_mut(&pane_id)
        {
            pane.bell = true;
        }
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i == active {
                continue;
            }
            if tab.root.contains(pane_id) {
                tab.bell = true;
                return;
            }
        }
    }

    /// Clear the pane bell indicator now that the user is looking at it.
    ///
    /// Mirrors `touch_active_tab_seen` for the per-pane latch: the indicator
    /// answers "something happened while you were away", so focusing the pane
    /// is the event that answers it.
    pub fn clear_focused_pane_bell(&mut self) {
        if let Some(id) = self.active_focus()
            && let Some(pane) = self.panes.get_mut(&id)
        {
            pane.bell = false;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_pane(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
        cwd: Option<&str>,
        argv: &[String],
    ) -> Result<u64> {
        let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) =
            crossbeam_channel::bounded(TERM_EVENT_QUEUE_DEPTH);
        // Terminator plugin parity: optional output sidechannel for
        // LuaEvent::Output emission.
        // The Mux's output_tx is set when a LuaEngine subscribes
        // (App configures it post-construction); None when no
        // plugin is listening so the alloc-per-PTY-read is skipped.
        // Recorder delivery is bounded and blocking. `PtyOutputSender::send`
        // runs before the reader takes the terminal lock, so this applies PTY
        // backpressure without the event-channel deadlock described above.
        let (out_tx, out_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = if self.record_lossless {
            crossbeam_channel::bounded(LOSSLESS_OUTPUT_QUEUE_DEPTH)
        } else {
            crossbeam_channel::bounded(64)
        };
        let output_tx = if self.lua_output_subscribed {
            Some(if self.record_lossless {
                PtyOutputSender::lossless(out_tx)
            } else {
                PtyOutputSender::best_effort(out_tx)
            })
        } else {
            None
        };
        // Shell integration must not take over a stock completion binding when
        // the matching UI is disabled.
        let pane_env = pane_environment(cfg);
        // Terminator parity: route through new_with_env so
        // cfg.term / cfg.colorterm / cfg.login_shell take effect at
        // PTY spawn. The legacy `Terminal::new` shim still exists
        // for non-Mux callers (currently none in-tree).
        let term = Terminal::new_with_env_and_output_geometry_and_capabilities(
            argv,
            cwd,
            cfg.scrollback,
            cfg.scrollback_bytes,
            geometry,
            cfg.cursor_blink,
            engine_cursor_shape(cfg.cursor_style),
            Some(cfg.word_delimiters.as_str()),
            &cfg.term,
            &cfg.colorterm,
            &pane_env,
            cfg.login_shell,
            cfg.shell_integration,
            TerminalCapabilities {
                osc52_copy: self.osc52_copy_allowed,
                unnegotiated_modified_enter: unnegotiated_modified_enter(cfg.modify_other_keys),
                contain_process_tree: false,
                observe_child_exit: true,
            },
            tx,
            waker.clone(),
            output_tx,
        )?;
        let pty_input = PtyInputQueue::new(&term, waker)?;
        let id = NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let initial_title = initial_pane_title(argv);
        // Only generic-shell panes ("kettle" seed) are eligible for conhost
        // startup-title suppression + cwd labelling; a `-e htop`/`ssh` pane keeps
        // its real seed and is never treated as a placeholder.
        let title_is_placeholder = initial_title == "kettle";
        let title_origin = if title_is_placeholder {
            PaneTitleOrigin::Placeholder
        } else {
            PaneTitleOrigin::ExplicitLaunch
        };
        let output_rx = if self.lua_output_subscribed {
            Some(out_rx)
        } else {
            None
        };
        self.panes.insert(
            id,
            Pane {
                term,
                rx,
                pty_input,
                output_rx,
                title: initial_title,
                title_is_placeholder,
                title_origin,
                title_before_remote: None,
                group_name: None,
                closed: false,
                held: false,
                held_child_reaped: false,
                exit_observed: false,
                #[cfg(windows)]
                pty_output_close_phase: PtyOutputClosePhase::NotStarted,
                last_output_generation: None,
                argv: argv.to_vec(),
                remote_context: None,
                foreground_process: None,
                agent_attached: false,
                read_only: false,
                completion_overlay: cfg.completion_overlay
                    == kettle_config::CompletionOverlayMode::Auto,
                bell: false,
            },
        );
        Ok(id)
    }

    fn snap(&self, n: &Node) -> SNode {
        match n {
            Node::Leaf(id) => SNode::Leaf {
                cwd: self.panes.get(id).and_then(|p| p.term.current_dir()),
                cmd: self
                    .panes
                    .get(id)
                    .map(|p| p.term.argv.clone())
                    .unwrap_or_default(),
                // C7 (audit v2.32.0): persist broadcast-group membership so a
                // restored pane rejoins its group instead of silently losing it.
                group: self.panes.get(id).and_then(|p| p.group_name.clone()),
            },
            Node::Split { dir, ratio, a, b } => SNode::Split {
                vertical: *dir == Dir::Vertical,
                ratio: *ratio,
                a: Box::new(self.snap(a)),
                b: Box::new(self.snap(b)),
            },
        }
    }

    /// Terminator parity, detachable-tabs Bucket-D:
    /// serialize ONE tab (by index) to the same
    /// STab wire format that session.json uses. Returns None when
    /// the index is out-of-range.
    // C5: its production callers were the serialize-and-respawn handoff
    // senders, retired in favor of the live in-process tab move. Kept (tests
    // pin the contract) — the deprecated `--tab-handoff` receive path still
    // consumes the wire format for one release, and C7's per-window session
    // serialization is the natural next consumer.
    #[allow(dead_code)]
    pub fn serialize_tab(&self, idx: usize) -> Option<STab> {
        let t = self.tabs.get(idx)?;
        Some(STab {
            root: self.snap(&t.root),
            focus: t.root.leaf_index_of(t.focus).unwrap_or(0),
            title_override: t.title_override.clone(),
            zoomed: t.zoomed,
        })
    }

    /// Capture the full tab/split tree + per-pane cwd.
    pub fn snapshot(&self) -> Session {
        Session {
            tabs: self
                .tabs
                .iter()
                .map(|t| STab {
                    root: self.snap(&t.root),
                    // DFS-order index of the focused leaf so restore can
                    // recreate the focus on the new tree (pane ids are
                    // reallocated across restores, so the id itself isn't
                    // portable). `0` means "first leaf" — same as the
                    // original behavior, which is what missing-field
                    // restores fall back to via #[serde(default)].
                    focus: t.root.leaf_index_of(t.focus).unwrap_or(0),
                    title_override: t.title_override.clone(),
                    zoomed: t.zoomed,
                })
                .collect(),
            active: self.active,
            // Filled in by App::save_session (it owns the active theme).
            theme: None,
            // C7: snapshot() is the ONE-window serializer; App::save_session
            // assembles the multi-window `windows` vec from per-window calls.
            windows: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_node(
        &mut self,
        n: &SNode,
        cfg: &Config,
        geometries: &mut dyn Iterator<Item = PtyGeometry>,
        mk: &dyn Fn() -> Waker,
        // Every pane id spawned while building this
        // subtree is appended here so the caller can reap them if a LATER
        // sibling fails. Without it, a split whose first child spawned but
        // whose second child failed left the first child's PTY + child
        // process orphaned in `self.panes` (attached to no tab) — a leaked
        // process per partially-restored split.
        spawned: &mut Vec<u64>,
    ) -> Result<Node> {
        match n {
            SNode::Leaf { cwd, cmd, group } => {
                let argv = if cmd.is_empty() {
                    shell_argv(cfg)
                } else {
                    cmd.clone()
                };
                let geometry = geometries
                    .next()
                    .unwrap_or_else(|| PtyGeometry::from_cell_size(80, 24, 8, 16));
                let id = self.spawn_pane(cfg, geometry, mk(), cwd.as_deref(), &argv)?;
                spawned.push(id);
                // C7 (audit v2.32.0): rejoin the saved broadcast group.
                if let Some(p) = self.panes.get_mut(&id) {
                    p.group_name = group.clone();
                }
                Ok(Node::Leaf(id))
            }
            SNode::Split {
                vertical,
                ratio,
                a,
                b,
            } => {
                let a = self.build_node(a, cfg, geometries, mk, spawned)?;
                let b = self.build_node(b, cfg, geometries, mk, spawned)?;
                Ok(Node::Split {
                    dir: if *vertical {
                        Dir::Vertical
                    } else {
                        Dir::Horizontal
                    },
                    // The session file is on disk and hand-editable, so a
                    // restored ratio is untrusted input: sanitize it here rather
                    // than letting a NaN or a 12.0 reach the tree.
                    ratio: sane_ratio(*ratio),
                    a: Box::new(a),
                    b: Box::new(b),
                })
            }
        }
    }

    /// Rebuild tabs/splits from a saved session, spawning shells in their
    /// recorded directories. Returns whether anything was restored.
    #[allow(dead_code)]
    pub fn restore(
        &mut self,
        s: &Session,
        cfg: &Config,
        cw: u16,
        ch: u16,
        mk: &dyn Fn() -> Waker,
    ) -> bool {
        let mut total = 0usize;
        let mut geometries = Vec::with_capacity(s.tabs.len().min(MAX_RESTORE_PANES));
        for tab in &s.tabs {
            let Some(leaves) = tab
                .root
                .bounded_leaf_count(MAX_RESTORE_PANES.saturating_sub(total))
            else {
                log::warn!("session restore rejected: pane fan-out exceeds the global cap");
                return false;
            };
            total += leaves;
            geometries.push(
                std::iter::repeat_n(PtyGeometry::from_cell_size(80, 24, cw, ch), leaves).collect(),
            );
        }
        self.restore_geometry(s, cfg, &geometries, mk)
    }

    /// Restore with one exact initial geometry per serialized leaf, in DFS
    /// order. The UI computes these from the live surface and saved split tree
    /// before any child starts, avoiding an initial rounded 80x24 ioctl.
    pub fn restore_geometry(
        &mut self,
        s: &Session,
        cfg: &Config,
        geometries: &[Vec<PtyGeometry>],
        mk: &dyn Fn() -> Waker,
    ) -> bool {
        // Bound the total PTY fan-out. The 16 MiB file-size
        // cap is a weak proxy — a small session.json of minimal flat leaves
        // (~30 bytes each) could ask to fork hundreds of thousands of shells on
        // launch, hanging/OOMing the machine. Stop restoring further tabs once
        // the running pane count would exceed the cap (256 panes is far past any
        // real layout) and surface why.
        let mut spawned = 0usize;
        for (i, st) in s.tabs.iter().enumerate() {
            let Some(tab_leaves) = st
                .root
                .bounded_leaf_count(MAX_RESTORE_PANES.saturating_sub(spawned))
            else {
                log::warn!(
                    "session restore: stopping at tab {i} — would exceed the \
                     {MAX_RESTORE_PANES}-pane restore cap (session may be corrupt or crafted)"
                );
                break;
            };
            // Track every pane id this tab's tree spawns so
            // a partial failure (e.g. the 2nd pane of a split fails to fork)
            // can reap the panes already created for the same tree instead of
            // leaking their PTYs + child processes.
            let mut tab_pane_ids: Vec<u64> = Vec::new();
            let mut tab_geometries = geometries
                .get(i)
                .into_iter()
                .flat_map(|values| values.iter().copied());
            match self.build_node(&st.root, cfg, &mut tab_geometries, mk, &mut tab_pane_ids) {
                Ok(root) => {
                    spawned += tab_leaves;
                    // Restore the focused leaf at its DFS index (saved
                    // by `snapshot`). `nth_leaf` falls back to the
                    // first leaf if the index is past the end, which
                    // keeps trimmed-tree sessions sane.
                    let focus = root.nth_leaf(st.focus);
                    self.tabs.push(Tab {
                        root,
                        focus,
                        // C7 (audit v2.32.0): restore the saved tab title
                        // override + zoom state (was hardcoded to defaults).
                        title_override: st.title_override.clone(),
                        zoomed: st.zoomed,
                        last_output_at: None,
                        last_seen_at: None,
                        bell: false,
                    });
                }
                Err(e) => {
                    // Reap any panes the partially-built
                    // tree already spawned. A split's first child can fork
                    // fine and the second fail (cwd gone, fork under quota);
                    // those orphans would otherwise sit in `self.panes`
                    // attached to no tab, leaking a PTY + child process each.
                    for id in &tab_pane_ids {
                        self.panes.remove(id);
                    }
                    // Don't fail the whole restore — a single broken
                    // tab (e.g. saved cwd no longer exists, PTY
                    // allocation under quota) shouldn't sink the
                    // others. But surface it in the log so a user
                    // wondering "where did my session go?" can see
                    // the cause under `RUST_LOG=warn` (the default
                    // filter). Pre-fix this was a silent skip — the
                    // user just saw fewer tabs than they remembered.
                    log::warn!(
                        "session restore: tab {i} failed to rebuild and was skipped \
                         ({} orphaned pane(s) reaped): {e}",
                        tab_pane_ids.len()
                    );
                }
            }
        }
        self.active = s.active.min(self.tabs.len().saturating_sub(1));
        !self.tabs.is_empty()
    }

    #[allow(dead_code)]
    pub fn new_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        self.new_tab_geometry(cfg, PtyGeometry::from_cell_size(cols, rows, cw, ch), waker)
    }

    pub fn new_tab_geometry(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
    ) -> Result<()> {
        let argv = shell_argv(cfg);
        let cwd = self.focused_cwd();
        self.new_tab_with_geometry(cfg, geometry, waker, &argv, cwd.as_deref())
    }

    /// Open a new tab running an explicit `argv` in `cwd` (CLI `-e`/`-d`);
    /// an empty `argv` means the configured shell.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn new_tab_with(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Result<()> {
        self.new_tab_with_geometry(
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
            argv,
            cwd,
        )
    }

    pub fn new_tab_with_geometry(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Result<()> {
        let id = self.spawn_pane(cfg, geometry, waker, cwd, argv)?;
        let new_tab = Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        // Terminator parity, terminatorlib/config.py:97
        // `new_tab_after_current_tab`: when true, insert the new
        // tab right AFTER the active one (vs at the end of the
        // tabs list). The new tab becomes active either way.
        if cfg.new_tab_after_current_tab && self.active + 1 < self.tabs.len() {
            self.tabs.insert(self.active + 1, new_tab);
            self.active += 1;
        } else if cfg.new_tab_after_current_tab && self.active + 1 == self.tabs.len() {
            // Already at the end — same as appending.
            self.tabs.push(new_tab);
            self.active = self.tabs.len() - 1;
        } else {
            self.tabs.push(new_tab);
            self.active = self.tabs.len() - 1;
        }
        Ok(())
    }

    /// New tab running an explicit `argv` + cwd, with the same
    /// WSL-aware `--cd` dir translation `split_with` applies. The new-tab ▾
    /// dropdown's WSL entry routed through `new_tab_with` directly (a raw spawn),
    /// so a WSL launcher's Linux cwd failed the Windows `is_dir` gate and the new
    /// tab fell back to the home dir — the same class of regression fixed for
    /// splits/duplicates by wiring them through `launch_cwd`.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn new_tab_with_launch(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        argv: Vec<String>,
        raw_cwd: Option<String>,
    ) -> Result<()> {
        self.new_tab_with_launch_geometry(
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
            argv,
            raw_cwd,
        )
    }

    pub fn new_tab_with_launch_geometry(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
        argv: Vec<String>,
        raw_cwd: Option<String>,
    ) -> Result<()> {
        let (argv, cwd) = launch_cwd(argv, raw_cwd);
        self.new_tab_with_geometry(cfg, geometry, waker, &argv, cwd.as_deref())
    }

    /// Open a new tab running `ssh -t <target>` (SSH multiplexing).
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn new_ssh_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        target: &str,
    ) -> Result<()> {
        self.new_ssh_tab_geometry(
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
            target,
        )
    }

    pub fn new_ssh_tab_geometry(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
        target: &str,
    ) -> Result<()> {
        let argv = vec!["ssh".to_string(), "-t".to_string(), target.to_string()];
        // `spawn_pane` sees argv[0] == "ssh" and seeds the pane title to
        // `ssh <target>` so the tab is distinguishable from a regular
        // shell tab while the connection is establishing. The OSC 2
        // handler overwrites this when the remote shell sets a title.
        let id = self.spawn_pane(cfg, geometry, waker, None, &argv)?;
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// The `(argv, spawn-cwd)` that reproduces the focused pane
    /// in a new pane/tab — clones its launch command and inherits its cwd, so a
    /// pane launched as WSL / ssh / a specific shell duplicates into the same.
    /// A default-shell pane's argv IS the configured shell, so the common case
    /// is unchanged; an empty argv (legacy "≡ configured shell") falls back to
    /// the shell.
    ///
    /// WSL-aware dir: WSL reports a Linux cwd (`/mnt/c/...` or a native path) a
    /// Windows spawn can't `cd` into, so for a `wsl` launcher the dir is carried
    /// via `wsl --cd <dir>` (which accepts both Windows and Linux paths) and the
    /// Windows spawn cwd is left unset — otherwise the new pane would fall back
    /// to the home dir (the bug the user hit: split a WSL pane → pwsh in ~).
    fn clone_focused_launch(&self, cfg: &Config) -> (Vec<String>, Option<String>) {
        let (mut argv, raw_cwd) = match self.active_focus().and_then(|id| self.panes.get(&id)) {
            Some(pane) => (pane.argv.clone(), pane.term.current_dir()),
            None => (Vec::new(), None),
        };
        if argv.is_empty() {
            argv = shell_argv(cfg);
        }
        launch_cwd(argv, raw_cwd)
    }

    /// Resolve the command a split should spawn when no interactive foreground
    /// shell was detected by the process scanner. Duplicate actions need exact
    /// launch cloning, but Split should stay a "give me another usable prompt"
    /// action. Direct agent/editor launches (`kettle -e codex`, `-e nvim`, etc.)
    /// often have transient helper shells underneath them; if we clone the
    /// direct launch argv, the new pane can immediately exit or open another
    /// full-screen app instead of becoming a prompt. Use the configured shell in
    /// the focused cwd for those direct launchers, while preserving exact cloning
    /// for shells, WSL, SSH, and ordinary explicit commands.
    fn split_focused_launch(&self, cfg: &Config) -> (Vec<String>, Option<String>) {
        let (mut argv, raw_cwd) = match self.active_focus().and_then(|id| self.panes.get(&id)) {
            Some(pane) => (pane.argv.clone(), pane.term.current_dir()),
            None => (Vec::new(), None),
        };
        if split_falls_back_to_shell(&argv, cfg.always_split_with_profile) {
            argv = shell_argv(cfg);
        }
        launch_cwd(argv, raw_cwd)
    }

    /// Terminator's `split_to_group` (`paned.py`: `if widget.group and
    /// self.config['split_to_group']`): put a freshly split pane in the same
    /// broadcast group as the pane it came from, so splitting a grouped pane
    /// widens the group instead of quietly dropping out of it.
    ///
    /// Must run BEFORE the graft — grafting moves focus to the new pane, and
    /// the group being inherited is the *old* focus's.
    fn inherit_split_group(&mut self, cfg: &Config, new_id: u64) {
        if !cfg.split_to_group {
            return;
        }
        let group = self
            .active_focus()
            .and_then(|id| self.panes.get(&id))
            .and_then(|pane| pane.group_name.clone());
        if let Some(group) = group
            && let Some(pane) = self.panes.get_mut(&new_id)
        {
            pane.group_name = Some(group);
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn split(
        &mut self,
        dir: Dir,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        self.split_geometry(
            dir,
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
        )
    }

    pub fn split_geometry(
        &mut self,
        dir: Dir,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
    ) -> Result<()> {
        if self.tabs.is_empty() {
            return self.new_tab_geometry(cfg, geometry, waker);
        }
        // v2.33.1: clone shell-like launches, but keep direct
        // agent/editor panes split-friendly by falling back to a shell in the
        // focused cwd. See `split_focused_launch`.
        let (argv, cwd) = self.split_focused_launch(cfg);
        let new_id = self.spawn_pane(cfg, geometry, waker, cwd.as_deref(), &argv)?;
        self.inherit_split_group(cfg, new_id);
        let a = self.active;
        let grafted = self
            .tabs
            .get_mut(a)
            .map(|tab| insert_split(tab, new_id, dir))
            .unwrap_or(false);
        if !grafted {
            // The graft failed (no active tab, or the
            // tree had no leaf to attach to). Reap the just-spawned pane rather
            // than leaking its PTY, and surface a real error — this path was a
            // silent `Ok(())` that left an orphaned pane behind.
            self.panes.remove(&new_id);
            anyhow::bail!("split failed: no pane available to attach the new split");
        }
        Ok(())
    }

    /// Split running an explicit `argv` + cwd — e.g. a shell detected
    /// running inside the focused pane (`pwsh → wsl`). Mirrors `split` but with a
    /// caller-supplied command instead of cloning the pane's launch argv; the
    /// WSL `--cd` dir handling is applied via `launch_cwd`.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn split_with(
        &mut self,
        dir: Dir,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        argv: Vec<String>,
        raw_cwd: Option<String>,
    ) -> Result<()> {
        self.split_with_geometry(
            dir,
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
            argv,
            raw_cwd,
        )
    }

    pub fn split_with_geometry(
        &mut self,
        dir: Dir,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
        argv: Vec<String>,
        raw_cwd: Option<String>,
    ) -> Result<()> {
        let (argv, cwd) = launch_cwd(argv, raw_cwd);
        if self.tabs.is_empty() {
            return self.new_tab_with_geometry(cfg, geometry, waker, &argv, cwd.as_deref());
        }
        let new_id = self.spawn_pane(cfg, geometry, waker, cwd.as_deref(), &argv)?;
        self.inherit_split_group(cfg, new_id);
        let a = self.active;
        let grafted = self
            .tabs
            .get_mut(a)
            .map(|tab| insert_split(tab, new_id, dir))
            .unwrap_or(false);
        if !grafted {
            // The graft failed (no active tab, or the
            // tree had no leaf to attach to). Reap the just-spawned pane rather
            // than leaking its PTY, and surface a real error — this path was a
            // silent `Ok(())` that left an orphaned pane behind.
            self.panes.remove(&new_id);
            anyhow::bail!("split failed: no pane available to attach the new split");
        }
        Ok(())
    }

    pub fn layout(&self, tab: usize, area: Rect) -> Vec<(u64, Rect)> {
        let mut v = Vec::new();
        if let Some(t) = self.tabs.get(tab) {
            if t.zoomed {
                // Zoomed: only the focused pane, full area.
                v.push((t.focus, area));
            } else {
                t.root.layout(area, &mut v);
            }
        }
        v
    }

    pub fn active_pane_count(&self) -> usize {
        self.tabs
            .get(self.active)
            .map(|tab| tab.root.leaf_ids().len())
            .unwrap_or(0)
    }

    /// Rectangle the newly spawned second child will occupy after a 50/50 split
    /// of the focused leaf. This intentionally ignores zoom (the split action
    /// exits zoom) and mirrors `Node::layout` rounding exactly.
    pub fn prospective_split_rect(&self, dir: Dir, area: Rect) -> Option<Rect> {
        let tab = self.tabs.get(self.active)?;
        let mut layout = Vec::new();
        tab.root.layout(area, &mut layout);
        let (_, (x, y, width, height)) = layout.into_iter().find(|(id, _)| *id == tab.focus)?;
        Some(match dir {
            Dir::Horizontal => {
                let first_width = (width * 0.5).round();
                (x + first_width, y, width - first_width, height)
            }
            Dir::Vertical => {
                let first_height = (height * 0.5).round();
                (x, y + first_height, width, height - first_height)
            }
        })
    }

    /// The divider seams of `tab` laid out over `area`,
    /// matching `layout`'s geometry. Empty when the tab is zoomed (one pane, no
    /// dividers) — so mouse drag-to-resize is inert in zoom, as it should be.
    pub fn split_seams(&self, tab: usize, area: Rect) -> Vec<SplitSeam> {
        let mut out = Vec::new();
        if let Some(t) = self.tabs.get(tab)
            && !t.zoomed
        {
            let mut path = Vec::new();
            t.root.dividers(area, &mut path, &mut out);
        }
        out
    }

    /// Set the ratio of the split addressed by `path` in `tab`.
    /// Returns whether a split was found (false on a stale path). The ratio is
    /// kept finite and strictly inside `(0, 1)`; `layout` holds the divider
    /// [`MIN_SPLIT_PX`] clear of either edge when it turns it into geometry.
    pub fn set_split_ratio(&mut self, tab: usize, path: &[bool], ratio: f32) -> bool {
        self.tabs
            .get_mut(tab)
            .map(|t| t.root.set_ratio_at(path, ratio))
            .unwrap_or(false)
    }

    /// Toggle zoom (maximize the focused pane) for the active tab.
    pub fn toggle_zoom(&mut self) {
        let a = self.active;
        if let Some(t) = self.tabs.get_mut(a) {
            t.zoomed = !t.zoomed;
        }
    }

    /// Whether the active tab is currently zoomed (used by
    /// `Action::ScaledZoom` to decide whether it's the enter-zoom path
    /// — which scales the font up — or the leave-zoom path — which
    /// restores the saved size).
    pub fn is_zoomed(&self) -> bool {
        self.tabs
            .get(self.active)
            .map(|t| t.zoomed)
            .unwrap_or(false)
    }

    /// Whether zoom currently hides at least one sibling pane.
    ///
    /// The persisted zoom bit can remain set after a split collapses to one
    /// leaf, and users can toggle zoom on a one-pane tab. Input routing must
    /// distinguish that inert state from a real zoom whose hidden panes still
    /// own directional-focus chords.
    pub fn zoom_hides_siblings(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|tab| tab.zoomed && !matches!(tab.root, Node::Leaf(_)))
    }

    pub fn active_focus(&self) -> Option<u64> {
        self.tabs.get(self.active).map(|t| t.focus)
    }

    /// The focused pane's current directory (reported via OSC 7), used so a
    /// new tab/split opens where you are — like WezTerm/iTerm/kitty. A
    /// since-deleted directory falls back to the default (handled by
    /// [`usable_cwd`]).
    fn focused_cwd(&self) -> Option<String> {
        let id = self.active_focus()?;
        usable_cwd(self.panes.get(&id).and_then(|p| p.term.current_dir()))
    }

    pub fn focused(&mut self) -> Option<&mut Pane> {
        let id = self.tabs.get(self.active)?.focus;
        self.panes.get_mut(&id)
    }

    /// The focused pane's launching argv (empty if none). Read-only, so the
    /// paste / drag-drop path can pick shell-appropriate path quoting and WSL
    /// translation without taking a `&mut` borrow of the pane.
    pub(crate) fn focused_argv(&self) -> Vec<String> {
        self.active_focus()
            .and_then(|id| self.panes.get(&id))
            .map(|p| p.argv.clone())
            .unwrap_or_default()
    }

    /// Which pane's rect contains `(px, py)`, with that rect — the drop-target
    /// half of dragging a terminal.
    ///
    /// Deliberately not [`Mux::focus_at`], which snaps to the nearest pane so a
    /// click on a seam still focuses something. A drag has no such obligation:
    /// a point outside every pane must read as "no target here" so the drop hint
    /// disappears, rather than naming a pane the cursor is not over.
    pub fn pane_rect_at(&self, area: Rect, px: f32, py: f32) -> Option<(u64, Rect)> {
        self.layout(self.active, area)
            .into_iter()
            .find(|&(_, (x, y, w, h))| px >= x && px < x + w && py >= y && py < y + h)
    }

    /// Terminator parity: move a pane to a new position beside another pane in
    /// the same tab — the tree half of dragging a terminal somewhere else.
    ///
    /// `moving` is lifted out (collapsing whatever split it leaves behind, the
    /// same way closing it would) and re-grafted as a sibling of `target`,
    /// split along `dir`, on the near side when `before`. Returns whether
    /// anything moved.
    ///
    /// Refused, rather than half-done, when: the two are the same pane; either
    /// is not a leaf of the active tab; or the tab has fewer than two panes.
    /// The order matters — the lift has to happen before the graft, or a
    /// `moving` that is `target`'s own sibling would be grafted onto a tree it
    /// is still part of, and appear twice.
    pub fn move_pane_beside(&mut self, moving: u64, target: u64, dir: Dir, before: bool) -> bool {
        if moving == target {
            return false;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        if !tab.root.contains(moving) || !tab.root.contains(target) {
            return false;
        }
        let root = std::mem::replace(&mut tab.root, Node::Leaf(moving));
        // `remove_leaf` reports three outcomes, and two of them are ordinary:
        //   `Ok(tree)`       the pane sat somewhere below the root; the tree
        //                    comes back with its parent split collapsed.
        //   `Err(Some(rest))` the pane was a DIRECT child of the root split, so
        //                    the root itself collapsed and its sibling subtree
        //                    is the new root. Normal, and the common case when
        //                    a tab holds one split.
        //   `Err(None)`      the pane WAS the whole tree. That means a tab of
        //                    one pane, which cannot also contain the target —
        //                    the `contains` guards above already excluded it —
        //                    so it is unreachable here. Refuse rather than
        //                    leave the tab holding the placeholder.
        let lifted = match root.remove_leaf(moving) {
            Ok(tree) => tree,
            Err(Some(rest)) => rest,
            Err(None) => return false,
        };
        tab.root = lifted;
        if !tab.root.split_leaf_ordered(target, moving, dir, before) {
            // The target vanished with the lift (it cannot, given the guards
            // above) — put the pane back beside the first leaf rather than
            // dropping it out of the tree entirely.
            let anchor = tab.root.first_leaf();
            tab.root.split_leaf_ordered(anchor, moving, dir, before);
        }
        tab.focus = moving;
        tab.zoomed = false;
        true
    }

    /// Terminator parity (`terminatorlib/window.py:rotate`): turn the active
    /// tab's whole layout a quarter turn. Returns whether there was anything to
    /// rotate — a single-pane tab has no splits and is left alone.
    ///
    /// Terminator rotates every pane in the visible tab, not just the one the
    /// cursor happens to be in, and it leaves zoom first so the user sees the
    /// result. Both are matched here.
    pub fn rotate_layout(&mut self, clockwise: bool) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        if matches!(tab.root, Node::Leaf(_)) {
            return false;
        }
        // Rotating a tree the user cannot see would rearrange their panes
        // behind a zoomed one, so drop zoom the way Terminator does.
        tab.zoomed = false;
        rotate_tree(&mut tab.root, clockwise);
        true
    }

    /// 0-based index of the focused pane within its tab's
    /// in-order traversal of the binary split tree. Used by
    /// `InsertPaneNumber` + `InsertPanePadded` to send the pane index
    /// to the PTY. Returns None when no tab exists.
    pub fn focused_pane_index_in_tab(&self) -> Option<usize> {
        let tab = self.tabs.get(self.active)?;
        let focus = tab.focus;
        fn walk(node: &Node, target: u64, idx: &mut usize) -> bool {
            match node {
                Node::Leaf(id) => {
                    if *id == target {
                        return true;
                    }
                    *idx += 1;
                    false
                }
                Node::Split { a, b, .. } => walk(a, target, idx) || walk(b, target, idx),
            }
        }
        let mut idx = 0;
        if walk(&tab.root, focus, &mut idx) {
            Some(idx)
        } else {
            None
        }
    }

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
            self.touch_active_tab_seen();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
            self.touch_active_tab_seen();
        }
    }

    /// Move focus to the pane immediately **adjacent** in a direction, the way
    /// tmux / Terminator do: among panes that border the focused pane on the
    /// pressed side AND overlap it on the perpendicular axis, pick the smallest
    /// primary-axis gap, tie-broken by perpendicular center proximity.
    ///
    /// User-reported on native Ubuntu: the old rule ranked
    /// candidates purely by Euclidean distance between pane **centers**, gated
    /// only by "candidate center is on the requested side". In a nested layout a
    /// **diagonal** pane whose center happened to be closer than a directly
    /// bordering pane's center would win — focus "jumped to a diagonal pane" and
    /// "skipped the adjacent one" (and a Right press could even select an
    /// up-and-to-the-right pane whose center merely had a larger x). Comparing
    /// pane **edges** with a required perpendicular overlap fixes both: a pane
    /// that only shares a corner (zero overlap) is never a neighbor.
    ///
    /// No-op when nothing borders the focused pane in that direction. Zoomed
    /// tabs no-op implicitly: `layout` returns only the focused pane while
    /// zoomed, so the candidate loop is empty.
    pub fn focus_dir(&mut self, area: Rect, dx: i32, dy: i32) {
        if let Some(id) = self.pane_in_direction(area, dx, dy)
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            tab.focus = id;
        }
    }

    /// The focused pane's nearest neighbour in a direction, by the same
    /// geometry `focus_dir` navigates with: it must genuinely lie on that side
    /// and overlap on the other axis, nearest gap wins, ties broken by
    /// cross-axis centre distance.
    ///
    /// Shared rather than duplicated so moving a pane lands where focusing
    /// would have gone. Two copies of this would eventually disagree about what
    /// counts as "the pane to the left", and the user would meet the difference
    /// as a pane that moves somewhere other than where they were looking.
    pub fn pane_in_direction(&self, area: Rect, dx: i32, dy: i32) -> Option<u64> {
        // `layout` rounds split seams with `.round()`, so a shared border between
        // adjacent panes can drift by up to ~1px; admit that slack on the side
        // test and clamp a tiny negative gap to 0.
        const EPS: f32 = 1.0;
        let a = self.active;
        let rects = self.layout(a, area);
        let tab = self.tabs.get(a)?;
        let &(_, (fx, fy, fw, fh)) = rects.iter().find(|(id, _)| *id == tab.focus)?;
        let (fl, fr, ft, fb) = (fx, fx + fw, fy, fy + fh);
        let (fcx, fcy) = (fx + fw / 2.0, fy + fh / 2.0);

        // best = (primary-axis gap, perpendicular center distance, id);
        // smaller gap wins, ties broken by smaller perpendicular distance.
        let mut best: Option<(f32, f32, u64)> = None;
        for (id, (x, y, w, h)) in &rects {
            if *id == tab.focus {
                continue;
            }
            let (l, r, t, b) = (*x, *x + *w, *y, *y + *h);
            let (cx, cy) = (*x + *w / 2.0, *y + *h / 2.0);

            let (gap, perp) = if dx < 0 {
                if r > fl + EPS {
                    continue; // must lie to the LEFT (its right edge at/before our left)
                }
                if fb.min(b) - ft.max(t) <= 0.0 {
                    continue; // no vertical overlap → diagonal, not a neighbor
                }
                ((fl - r).max(0.0), (cy - fcy).abs())
            } else if dx > 0 {
                if l < fr - EPS {
                    continue;
                }
                if fb.min(b) - ft.max(t) <= 0.0 {
                    continue;
                }
                ((l - fr).max(0.0), (cy - fcy).abs())
            } else if dy < 0 {
                if b > ft + EPS {
                    continue; // must lie ABOVE (its bottom edge at/before our top)
                }
                if fr.min(r) - fl.max(l) <= 0.0 {
                    continue; // no horizontal overlap
                }
                ((ft - b).max(0.0), (cx - fcx).abs())
            } else {
                if t < fb - EPS {
                    continue;
                }
                if fr.min(r) - fl.max(l) <= 0.0 {
                    continue;
                }
                ((t - fb).max(0.0), (cx - fcx).abs())
            };

            let better = match best {
                None => true,
                // A small slack keeps two real neighbors whose gaps differ only
                // by rounding in the same tier so the perpendicular tie-break
                // (closest to the focused pane's cross-axis center) decides.
                Some((bg, bp, _)) => gap < bg - 1e-3 || ((gap - bg).abs() <= 1e-3 && perp < bp),
            };
            if better {
                best = Some((gap, perp, *id));
            }
        }
        best.map(|(_, _, id)| id)
    }

    pub fn focus_cycle(&mut self, area: Rect, forward: bool) {
        let a = self.active;
        let rects = self.layout(a, area);
        if let Some(tab) = self.tabs.get_mut(a)
            && let Some(pos) = rects.iter().position(|(id, _)| *id == tab.focus)
        {
            let n = rects.len();
            let next = if forward {
                (pos + 1) % n
            } else {
                (pos + n - 1) % n
            };
            tab.focus = rects[next].0;
        }
    }

    pub fn resize_focus(&mut self, dir: Dir, delta: f32) {
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            let f = tab.focus;
            tab.root.resize(f, dir, delta);
        }
    }

    /// Move the active tab `delta` positions along the bar, sliding every tab
    /// it passes over back by one. `delta > 0` moves the tab right, `delta <
    /// 0` moves it left. Clamps at the edges — this is the *drag* path, where a
    /// cursor that overshoots the last segment must stop there rather than
    /// fling the tab back to the front. The keyboard path is
    /// [`Mux::nudge_active_tab`], which wraps. Returns `true` if the tab
    /// actually moved.
    ///
    /// This used to `swap`, which is only the same thing when `|delta| == 1`:
    /// for anything larger the tab sitting at the destination teleported back
    /// to the dragged tab's original slot. Drag-to-reorder reaches larger
    /// deltas routinely — the drag handler passes
    /// `tab_drag_target_index(cursor_x) - active`, Windows coalesces
    /// `WM_MOUSEMOVE` so one event can cross several narrow segments, and an
    /// overshoot past the right edge clamps to the LAST segment. So dragging
    /// one tab silently reordered the others.
    pub fn move_active_tab(&mut self, delta: i32) -> bool {
        let n = self.tabs.len();
        if n < 2 || delta == 0 {
            return false;
        }
        let to = (self.active as i32 + delta).clamp(0, n as i32 - 1) as usize;
        self.relocate_active_tab(to)
    }

    /// `move_tab_left` / `move_tab_right`: shift the active tab one place and
    /// wrap around the ends, which is what Terminator's `move_tab` does
    /// (`window.py`: left from the first tab goes to the end, right from the
    /// last comes back to the front).
    ///
    /// Separate from [`Mux::move_active_tab`] because the two callers want
    /// different edge behaviour, and one function cannot have both: a keyboard
    /// press that stops dead at the end of the bar feels broken, while a *drag*
    /// that wraps would fling the tab across the bar the moment the cursor
    /// overshot the last segment. Drag clamps; the keys wrap.
    pub fn nudge_active_tab(&mut self, delta: i32) -> bool {
        let n = self.tabs.len();
        if n < 2 || delta == 0 {
            return false;
        }
        let to = (self.active as i32 + delta).rem_euclid(n as i32) as usize;
        self.relocate_active_tab(to)
    }

    /// Lift the active tab out of the bar and put it back down at `to`,
    /// following it with the focus. Shared by the drag and keyboard paths so
    /// they cannot drift apart on the part that actually reorders.
    fn relocate_active_tab(&mut self, to: usize) -> bool {
        let from = self.active;
        if to == from || to >= self.tabs.len() {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = to;
        true
    }

    /// Focus whichever pane contains the pixel `(px, py)`.
    pub fn focus_at(&mut self, area: Rect, px: f32, py: f32) {
        let a = self.active;
        let rects = self.layout(a, area);
        if let Some(tab) = self.tabs.get_mut(a) {
            for (id, (x, y, w, h)) in rects {
                if px >= x && px < x + w && py >= y && py < y + h {
                    tab.focus = id;
                    break;
                }
            }
        }
    }

    /// Close the focused pane. Returns true if no tabs remain.
    ///
    /// `Node::remove_leaf` returns three distinct shapes that need three
    /// different responses; the previous `match Err(_)` arm conflated two
    /// of them and closed the whole tab when only a sibling-promote was
    /// needed:
    ///
    /// - `Ok(n)` — the leaf was nested deep; tree was restructured around
    ///   it. Replace the root with `n` and keep the tab.
    /// - `Err(Some(n))` — the focused leaf was directly under the root
    ///   `Split`; the sibling `n` is now the new root. Keep the tab —
    ///   `Ctrl+Shift+E` then `Ctrl+Shift+W` should close the pane, not
    ///   the whole tab.
    /// - `Err(None)` — the focused leaf was the only one in the tab
    ///   (single-pane tab); the tab is now empty and should close.
    pub fn close_focused(&mut self) -> bool {
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            let focus = tab.focus;
            // Pick the post-close focus BEFORE removing the leaf
            // so we know which sibling subtree to promote. The old approach
            // used `tab.root.first_leaf()` POST-remove, which always
            // jumped to the leftmost leaf of the whole tab — a regression
            // the user described as "closing a pane sets my cursor back
            // to my first focused terminal" (the leftmost = first split).
            // `neighbor_of` walks the tree and returns the first leaf of
            // the closed pane's sibling subtree, matching tmux/wezterm/
            // kitty's neighbor-promotion semantics.
            let neighbor = tab.root.neighbor_of(focus);
            let root = std::mem::replace(&mut tab.root, Node::Leaf(0));
            match root.remove_leaf(focus) {
                Ok(n) | Err(Some(n)) => {
                    tab.root = n;
                    // Only repair focus when it's no
                    // longer a leaf in the collapsed tree — the same guard
                    // `reap_tabs` already has (mux.rs ~1809). close_focused always
                    // removes the focused leaf so this normally fires; matching the
                    // two close paths keeps focus on a valid leaf if that ever
                    // changes. `neighbor` is None only on a single-Leaf tree
                    // (handled by Err(None) below), so first_leaf is the safe
                    // fallback against a stale focus pointer.
                    if !tab.root.contains(tab.focus) {
                        tab.focus = neighbor.unwrap_or_else(|| tab.root.first_leaf());
                    }
                    self.panes.remove(&focus);
                }
                Err(None) => {
                    self.panes.remove(&focus);
                    self.tabs.remove(a);
                    if self.active >= self.tabs.len() && self.active > 0 {
                        self.active -= 1;
                    }
                }
            }
        }
        self.tabs.is_empty()
    }

    /// Point focus at `pane`, wherever it lives, so a subsequent
    /// [`Mux::close_focused`] acts on it. Returns false if the pane is gone.
    ///
    /// Used by the `ask-before-closing` confirm path: the prompt names the
    /// pane the user actually pointed at, and by the time they answer, focus
    /// may have moved to a sibling because the target's own shell exited.
    /// Re-focusing before closing keeps `close_focused`'s sibling-promotion
    /// and tab-collapse behavior exactly as-is rather than duplicating it.
    pub fn focus_pane(&mut self, pane: u64) -> bool {
        let Some(idx) = self.tab_index_of_any_pane(&[pane]) else {
            return false;
        };
        self.active = idx;
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.focus = pane;
        }
        true
    }

    pub fn close_tab(&mut self) -> bool {
        let a = self.active;
        self.close_tab_at(a)
    }

    /// Pane ids that name the tab at `idx` for as long as ANY of them lives —
    /// the inverse of [`Mux::tab_index_of_any_pane`], and the way to hold onto
    /// a tab across anything that can renumber the tab list.
    ///
    /// All of the tab's leaves, not just the first: anchoring on one pane
    /// meant a split tab stopped resolving the moment that particular pane's
    /// shell exited, even though the tab was still on screen with its other
    /// panes running — and the confirm prompt would then quietly do nothing.
    pub fn tab_anchor_panes(&self, idx: usize) -> Vec<u64> {
        self.tabs
            .get(idx)
            .map(|t| t.root.leaf_ids())
            .unwrap_or_default()
    }

    /// Index of the tab holding any of `panes`, or `None` if none of them is
    /// in a tab any more (the tab is gone).
    ///
    /// A tab index is only meaningful at the instant it is read: a shell
    /// exiting reaps its pane, which can drop a whole tab and shift every
    /// index after it down one. That is fine for code that closes a tab
    /// immediately, but not for anything that remembers a tab across a
    /// pause — most of all the `ask-before-closing` prompt, which can sit on
    /// screen indefinitely waiting for an answer. Naming the tab by the panes
    /// it holds survives those shifts, and a pane id is never reused, so a
    /// stale set resolves to `None` (the tab is gone) rather than to somebody
    /// else's tab.
    pub fn tab_index_of_any_pane(&self, panes: &[u64]) -> Option<usize> {
        // `Node::contains` walks the tree and answers membership directly.
        // Building a `leaf_ids()` Vec per tab just to search it allocated on
        // every confirm re-resolve and on every `focus_pane`, which passes a
        // single id.
        self.tabs
            .iter()
            .position(|t| panes.iter().any(|id| t.root.contains(*id)))
    }

    /// Terminator parity, detachable-tabs Bucket-D: extract a tab
    /// from the tabs list WITHOUT
    /// dropping its panes' PTYs. Used by the cross-process tab
    /// handoff send path: the source process extracts
    /// the tab → sends the serialized state + PTY fds via
    /// SCM_RIGHTS to the target process → target reconstructs
    /// the tab.
    ///
    /// Returns the extracted Tab struct + the focused pane id;
    /// the Pane structs themselves stay in self.panes (extract
    /// only touches tabs vec). The caller is responsible for
    /// transferring or dropping those Pane refs.
    ///
    /// Returns None for out-of-range idx.
    ///
    /// 2026-05-23: the `#[allow(dead_code)]` covered
    /// the period before the SCM_RIGHTS IPC actually
    /// landed. Today this is exercised by `mux::tests` round-trip
    /// drift guards (extract→insert restores the tab state) — the
    /// IPC integration ships under a feature gate the binary
    /// activates via `--tab-handoff-fd`. Production consumes
    /// `serialize_tab` directly; this helper stays available for
    /// the upcoming live-PTY adoption work.
    #[allow(dead_code)]
    pub fn extract_tab(&mut self, idx: usize) -> Option<Tab> {
        if idx >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(idx);
        // Keep `active` valid + consistent with close_tab_at/reap_tabs: shift
        // left only when a tab strictly BEFORE active was removed; when the
        // active tab itself is removed the right neighbor slides into the slot
        // so focus moves RIGHT (active stays put), clamping if it ran off the
        // end (removing the last tab).
        if self.active > idx {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        Some(tab)
    }

    /// Companion to extract_tab: insert a Tab into
    /// the tabs vec at the given index. Used by the cross-process
    /// receive path when an incoming handoff lands.
    /// `at` clamps to [0, tabs.len()].
    #[allow(dead_code)]
    pub fn insert_tab(&mut self, at: usize, tab: Tab) {
        let pos = at.min(self.tabs.len());
        self.tabs.insert(pos, tab);
        // Make the inserted tab active so the user sees the
        // transferred work immediately.
        self.active = pos;
    }

    /// C2 (multi-window): lift the tab at `idx` out of this Mux, LIVE —
    /// the Tab struct plus its `Pane`s (PTYs, reader threads, scrollback,
    /// everything) leave together, untouched. The C5 in-process tab move
    /// feeds the result straight into another window's `attach_tab`.
    ///
    /// Composition contract: `extract_tab` handles the tabs-vec removal and
    /// the active-index fixup (shift-left / clamp, drift-guarded by
    /// `extract_and_insert_tab_roundtrip`); this adds the pane transfer.
    /// Unlike `close_tab_at`, nothing is pushed to `closed_tabs` — the tab
    /// isn't closing, it's moving. Returns `None` for an out-of-range idx.
    pub fn detach_tab(&mut self, idx: usize) -> Option<DetachedTab> {
        let tab = self.extract_tab(idx)?;
        let mut ids = Vec::new();
        collect_ids(&tab.root, &mut ids);
        let panes = ids
            .into_iter()
            .filter_map(|id| self.panes.remove(&id).map(|p| (id, p)))
            .collect();
        // The user lands on whichever tab slid into focus; mark it seen so
        // its activity dot clears (same as every other tab-switch path).
        self.touch_active_tab_seen();
        Some(DetachedTab { tab, panes })
    }

    /// C2 (multi-window): attach a detached tab (panes and all) to this Mux
    /// at `at` (clamped; `None` = append). The inserted tab becomes active —
    /// `insert_tab` semantics — and is marked seen. Returns the index it
    /// landed at. Pane ids can't collide: they're process-global
    /// (`NEXT_PANE_ID`), and a detached tab's ids left their source map.
    pub fn attach_tab(&mut self, dt: DetachedTab, at: Option<usize>) -> usize {
        for (id, p) in dt.panes {
            debug_assert!(
                !self.panes.contains_key(&id),
                "pane id {id} already present in target Mux (global-id invariant broken)"
            );
            // Release builds must SURVIVE an id collision, not silently corrupt:
            // a blind `insert` would overwrite the resident pane and LEAK it (its
            // PTY + child process would dangle, untracked, until the OS reaps
            // them). The global `NEXT_PANE_ID` allocator makes a collision a bug,
            // but if one ever slips through (a stale detached tab re-attached
            // twice, a future per-Mux regression), log it and DROP the displaced
            // pane so its PTY/child end cleanly instead of leaking. The
            // `debug_assert!` above still trips the invariant in test builds.
            if let Some(old) = self.panes.insert(id, p) {
                log::error!(
                    "attach_tab: pane id {id} collided with an existing pane in the \
                     target Mux (global-id invariant broken); dropping the displaced \
                     pane to end its PTY/child"
                );
                drop(old);
            }
        }
        let pos = at.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.insert_tab(pos, dt.tab);
        self.touch_active_tab_seen();
        pos
    }

    /// Close the entire window: drop every pane in every tab. The caller
    /// (the chrome layer) then exits the event loop because `tabs` is
    /// empty. Distinct from `close_tab` which only closes the focused
    /// tab — they were split apart so the keybinds (`close_tab`
    /// vs `close_window`) finally do different things. Returns true
    /// (kept for parity with `close_tab` / `close_tab_at`; the chrome
    /// callers use it as "exit now").
    pub fn close_window(&mut self) -> bool {
        self.panes.clear();
        self.tabs.clear();
        self.active = 0;
        true
    }

    /// Close the tab at `idx` (all its panes). Returns true if no tabs remain.
    pub fn close_tab_at(&mut self, idx: usize) -> bool {
        if idx < self.tabs.len() {
            // Snapshot the first leaf's argv+cwd before
            // dropping the tab so `Action::UndoCloseTab` can bring it
            // back. The ring is LIFO-bounded; closing tabs faster than
            // we undo evicts the oldest.
            let first_leaf = self.tabs[idx].root.first_leaf();
            if let Some(pane) = self.panes.get(&first_leaf) {
                let snap = ClosedTab {
                    original_index: idx,
                    argv: pane.argv.clone(),
                    cwd: usable_cwd(pane.term.current_dir()),
                };
                if self.closed_tabs.len() >= CLOSED_TAB_RING_CAP {
                    self.closed_tabs.pop_front();
                }
                self.closed_tabs.push_back(snap);
            }
            let mut ids = Vec::new();
            collect_ids(&self.tabs[idx].root, &mut ids);
            for id in ids {
                self.panes.remove(&id);
            }
            self.tabs.remove(idx);
            // Keep `active` valid: clamp if it ran off the end, or shift
            // left if a tab before it was removed.
            if (self.active >= self.tabs.len() || self.active > idx) && self.active > 0 {
                self.active -= 1;
            }
        }
        self.tabs.is_empty()
    }

    /// Open a new tab that duplicates the focused pane's argv + OSC-7
    /// cwd (iTerm2's "Duplicate Tab" affordance). Falls
    /// back to the configured shell when the focused pane has empty
    /// argv (`new_tab_with` semantics — empty argv ≡ shell). Returns
    /// `Ok(())` even if there's no focused tab to duplicate; the
    /// chrome layer treats that as a no-op the same way it treats
    /// `new_tab` on an empty mux.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn duplicate_focused_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        self.duplicate_focused_tab_geometry(
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
        )
    }

    pub fn duplicate_focused_tab_geometry(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
    ) -> Result<()> {
        if self
            .active_focus()
            .and_then(|id| self.panes.get(&id))
            .is_none()
        {
            return self.new_tab_geometry(cfg, geometry, waker);
        }
        // Clone via the shared helper so a WSL tab duplicates
        // with `wsl --cd <dir>` instead of falling back to the home dir.
        let (argv, cwd) = self.clone_focused_launch(cfg);
        self.new_tab_with_geometry(cfg, geometry, waker, &argv, cwd.as_deref())
    }

    /// Split the focused pane and run the *same* program in the new
    /// half (iTerm2's "Duplicate Pane" affordance). Mirrors `split`
    /// but reads the focused pane's argv instead of the configured
    /// shell — so a `kettle -e vim file` pane duplicates into a
    /// second vim instance in the same cwd.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn duplicate_focused_pane(
        &mut self,
        dir: Dir,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        self.duplicate_focused_pane_geometry(
            dir,
            cfg,
            PtyGeometry::from_cell_size(cols, rows, cw, ch),
            waker,
        )
    }

    pub fn duplicate_focused_pane_geometry(
        &mut self,
        dir: Dir,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
    ) -> Result<()> {
        if self.tabs.is_empty() {
            return self.new_tab_geometry(cfg, geometry, waker);
        }
        // Shares `clone_focused_launch` with `split` (now also a
        // clone) — clones the focused pane's argv + cwd, WSL-aware.
        let (argv, cwd) = self.clone_focused_launch(cfg);
        let new_id = self.spawn_pane(cfg, geometry, waker, cwd.as_deref(), &argv)?;
        let a = self.active;
        let grafted = self
            .tabs
            .get_mut(a)
            .map(|tab| insert_split(tab, new_id, dir))
            .unwrap_or(false);
        if !grafted {
            // The graft failed (no active tab, or the
            // tree had no leaf to attach to). Reap the just-spawned pane rather
            // than leaking its PTY, and surface a real error — this path was a
            // silent `Ok(())` that left an orphaned pane behind.
            self.panes.remove(&new_id);
            anyhow::bail!("split failed: no pane available to attach the new split");
        }
        Ok(())
    }

    /// Restore the most-recently-closed tab. Returns `true` if a tab
    /// was actually restored. Inserts at the original index (clamped
    /// to the current tab count); the new tab becomes active.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn undo_close_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<bool> {
        self.undo_close_tab_geometry(cfg, PtyGeometry::from_cell_size(cols, rows, cw, ch), waker)
    }

    pub fn undo_close_tab_geometry(
        &mut self,
        cfg: &Config,
        geometry: PtyGeometry,
        waker: Waker,
    ) -> Result<bool> {
        let Some(snap) = self.closed_tabs.pop_back() else {
            return Ok(false);
        };
        // Re-spawn the same argv + cwd. Empty argv → configured shell
        // (matches `new_tab_with`'s contract).
        let id = self.spawn_pane(cfg, geometry, waker, snap.cwd.as_deref(), &snap.argv)?;
        let insert_at = snap.original_index.min(self.tabs.len());
        self.tabs.insert(
            insert_at,
            Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            },
        );
        self.active = insert_at;
        self.touch_active_tab_seen();
        Ok(true)
    }

    /// Reap panes whose child exited; prune empty splits/tabs.
    pub fn reap(&mut self) -> bool {
        let dead: Vec<u64> = self
            .panes
            .iter_mut()
            .filter_map(|(id, p)| {
                if is_reapable(p.closed, p.held, p.exit_observed) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        for id in &dead {
            self.panes.remove(id);
        }
        Self::reap_tabs(&mut self.tabs, &mut self.active, &dead);
        self.tabs.is_empty()
    }

    /// Retry collection for a held pane whose ordered PTY EOF beat the direct
    /// child's exit status. This never decides pane lifetime; it only prevents
    /// Hold from leaving a zombie until the user eventually closes the pane.
    /// Returns whether another bounded poll is still needed.
    pub fn poll_held_child_statuses(&mut self) -> bool {
        let mut pending = false;
        for pane in self.panes.values_mut() {
            if pane.held && pane.exit_observed && !pane.held_child_reaped {
                pane.held_child_reaped = pane.term.child_exit_code().is_some();
                pending |= !pane.held_child_reaped;
            }
        }
        pending
    }

    /// Pure helper for `reap`'s tab-mutation step: walk every tab,
    /// prune dead panes from its split tree, drop any tab whose tree
    /// collapses to empty, and keep `active` pointing at the *same
    /// tab the user is focused on* after the shift (not the same
    /// numeric index). Extracted so the active-index bookkeeping is
    /// testable without spawning real PTYs to populate `self.panes`.
    pub(crate) fn reap_tabs(tabs: &mut Vec<Tab>, active: &mut usize, dead_ids: &[u64]) {
        for id in dead_ids {
            let mut ti = 0;
            while ti < tabs.len() {
                // Companion to `close_focused`'s neighbor-promotion
                // fix. When a PTY exits and the dying leaf IS the
                // focused one, capture the neighbor BEFORE the
                // destructive `remove_leaf` so the post-rebuild
                // focus can promote it instead of jumping to the
                // leftmost leaf of the whole tab. Before this fix,
                // typing `exit` in the rightmost pane of a 4-pane
                // tab would jump focus to the leftmost pane —
                // exactly the same user-described "first focused
                // terminal" symptom that motivated the close_focused
                // fix, just triggered by shell exit instead of `close-pane`.
                let neighbor_if_focused = if tabs[ti].focus == *id {
                    tabs[ti].root.neighbor_of(*id)
                } else {
                    None
                };
                let root = std::mem::replace(&mut tabs[ti].root, Node::Leaf(0));
                match root.remove_leaf(*id) {
                    // Previously this match used
                    // `Err(_) => tabs.remove(ti)` which conflated two
                    // distinct outcomes. `Err(Some(n))` means the
                    // dying leaf was a direct child of root and `n`
                    // is the surviving sibling — the tab MUST stay
                    // with `n` as the new root. Before this fix, any 2-pane
                    // tab + `exit` in either pane deleted the whole
                    // tab (the surviving sibling went with it).
                    // Reachable in production after `Mux::reap` consumes the
                    // pane's drained PTY exit event. Mirrors the same Ok(n)/Err(Some(n))/
                    // Err(None) distinction already in `close_focused` below.
                    Ok(n) | Err(Some(n)) => {
                        tabs[ti].root = n;
                        if !tabs[ti].root.contains(tabs[ti].focus) {
                            tabs[ti].focus =
                                neighbor_if_focused.unwrap_or_else(|| tabs[ti].root.first_leaf());
                        }
                        ti += 1;
                    }
                    Err(None) => {
                        tabs.remove(ti);
                        // Keep `active` pointing at the
                        // same tab the user is focused on after the
                        // shift, not the same numeric index. Removing
                        // a tab at `ti < active` shifts every later
                        // tab left by one, so subtract one from
                        // active. `ti == active` (the user IS focused
                        // on the tab being closed): leave active
                        // alone so focus naturally falls on the tab
                        // that takes its slot (the previous tab+1,
                        // matching every modern terminal — close
                        // current tab, focus moves to its right
                        // neighbor; the trailing-clamp below catches
                        // the case where active was the last tab).
                        if ti < *active {
                            *active -= 1;
                        }
                    }
                }
            }
        }
        if *active >= tabs.len() && *active > 0 {
            *active = tabs.len().saturating_sub(1);
        }
    }

    /// Deliver identical bytes to the panes selected by this mux's broadcast
    /// scope and report both the worst rejection and whether any pane accepted.
    pub(crate) fn broadcast_write_delivery(
        &mut self,
        bytes: &[u8],
        scroll_to_bottom: bool,
    ) -> PaneInputDelivery {
        self.broadcast_write_inner(bytes, scroll_to_bottom)
    }

    fn broadcast_write_inner(&mut self, bytes: &[u8], scroll_to_bottom: bool) -> PaneInputDelivery {
        // Respect the `BroadcastScope` enum (phase 3 of the named-groups
        // design). Off short-circuits; Tab keeps the
        // active-tab behavior; All targets every pane
        // window-wide; Group(name) targets cross-tab matches.
        let ids = self.broadcast_target_ids();
        let mut delivery = PaneInputDelivery::new();
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id) {
                // A read-only pane drops user input (keystroke /
                // paste / broadcast). The child still produces output.
                let pane_result = p.feed_input(bytes);
                if pane_result.is_queued()
                    && scroll_to_bottom
                    && let Ok(mut term) = p.term.term.lock()
                {
                    term.scroll_display(kettle_core::Scroll::Bottom);
                }
                delivery.record(pane_result);
            }
        }
        delivery
    }

    /// Deliver a broadcast that ANOTHER window is originating, to the panes in
    /// this one that its scope selects.
    ///
    /// A named group is a set the user declared, and `group_all` already spans
    /// every window (`window.py:933`, matching Terminator's process-wide
    /// terminal collection). The broadcast did not: it stopped at whichever
    /// window you happened to be typing in, so grouping panes across two
    /// windows and then typing reached only half of them. Nothing announced the
    /// boundary — the titlebars of the panes in the other window still showed
    /// the group.
    ///
    /// Only `Group` crosses. `Tab` is defined by a focused tab, which exists in
    /// exactly one window; `All` is kettle's own window-wide scope and stays
    /// that way; `Off` sends nothing anywhere.
    pub(crate) fn broadcast_write_foreign_delivery(
        &mut self,
        scope: &BroadcastScope,
        bytes: &[u8],
        scroll_to_bottom: bool,
    ) -> PaneInputDelivery {
        let mut delivery = PaneInputDelivery::new();
        for id in self.foreign_target_ids(scope) {
            if let Some(pane) = self.panes.get_mut(&id) {
                let pane_result = pane.feed_input(bytes);
                if pane_result.is_queued()
                    && scroll_to_bottom
                    && let Ok(mut term) = pane.term.term.lock()
                {
                    term.scroll_display(kettle_core::Scroll::Bottom);
                }
                delivery.record(pane_result);
            }
        }
        delivery
    }

    pub(crate) fn broadcast_encoded<F>(
        &mut self,
        policy: ModifyOtherKeysMode,
        sample_automatic_context: bool,
        scroll_to_bottom: bool,
        encode: F,
    ) -> PaneInputDelivery
    where
        F: FnMut(kettle_core::TermMode) -> Option<Vec<u8>>,
    {
        let ids = self.broadcast_target_ids();
        self.write_encoded_into(
            ids,
            policy,
            sample_automatic_context,
            scroll_to_bottom,
            encode,
        )
    }

    pub(crate) fn broadcast_encoded_foreign<F>(
        &mut self,
        scope: &BroadcastScope,
        policy: ModifyOtherKeysMode,
        sample_automatic_context: bool,
        scroll_to_bottom: bool,
        encode: F,
    ) -> PaneInputDelivery
    where
        F: FnMut(kettle_core::TermMode) -> Option<Vec<u8>>,
    {
        let ids = self.foreign_target_ids(scope);
        self.write_encoded_into(
            ids,
            policy,
            sample_automatic_context,
            scroll_to_bottom,
            encode,
        )
    }

    fn write_encoded_into<F>(
        &mut self,
        ids: Vec<u64>,
        policy: ModifyOtherKeysMode,
        sample_automatic_context: bool,
        scroll_to_bottom: bool,
        encode: F,
    ) -> PaneInputDelivery
    where
        F: FnMut(kettle_core::TermMode) -> Option<Vec<u8>>,
    {
        let target_modes = ids.into_iter().filter_map(|id| {
            let pane = self.panes.get(&id)?;
            let mode = pane.effective_key_mode(policy, sample_automatic_context);
            Some((id, mode))
        });
        let encoded = encode_target_modes(target_modes, encode);
        let mut delivery = PaneInputDelivery::new();
        for (id, bytes) in encoded {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            let pane_result = pane.feed_input(&bytes);
            if pane_result.is_queued()
                && scroll_to_bottom
                && let Ok(mut term) = pane.term.term.lock()
            {
                term.scroll_display(kettle_core::Scroll::Bottom);
            }
            delivery.record(pane_result);
        }
        delivery
    }

    /// Whether a scope reaches panes outside the window that owns it, so the
    /// caller knows whether to walk the other windows at all.
    pub fn scope_crosses_windows(scope: &BroadcastScope) -> bool {
        matches!(scope, BroadcastScope::Group(_))
    }

    /// Toggle the focused pane's read-only state; returns the new
    /// value (or `false` if there's no focused pane).
    pub fn toggle_focused_read_only(&mut self) -> bool {
        if let Some(p) = self.focused() {
            p.read_only = !p.read_only;
            p.read_only
        } else {
            false
        }
    }

    /// Is broadcast active in any scope (Tab/All/Group)?
    /// Most callers just need a yes/no — this preserves the old
    /// `bool` ergonomics post-migration.
    pub fn is_broadcast_on(&self) -> bool {
        !matches!(self.broadcast, BroadcastScope::Off)
    }

    /// Compute the pane IDs that should receive a
    /// broadcast given the current `self.broadcast` scope. Returns
    /// an empty Vec when scope is Off. Used by `broadcast_write`
    /// and `broadcast_paste_delivery`.
    fn broadcast_target_ids(&self) -> Vec<u64> {
        if matches!(self.broadcast, BroadcastScope::Off) {
            return Vec::new();
        }
        // No active tab → no anchor pane and nothing to broadcast to.
        // Previously the focused-pane id fell back to `0` (a sentinel that
        // is never a real pane), which `compute_broadcast_targets` would
        // hand back as a phantom target in `Off` scope; guarding here keeps
        // an invalid id from ever entering the pipeline.
        let Some(tab) = self.tabs.get(self.active) else {
            return Vec::new();
        };
        let panes_in_focused_tab = tab.root.leaf_ids();
        let all_with_groups: Vec<(u64, Option<&str>)> = self
            .panes
            .iter()
            .map(|(id, p)| (*id, p.group_name.as_deref()))
            .collect();
        let targets = compute_broadcast_targets(
            &self.broadcast,
            tab.focus,
            &panes_in_focused_tab,
            &all_with_groups,
        );
        // Self-heal an emptied named group: if the active scope is a named
        // Group but no pane currently matches it (the last member was closed
        // or ungrouped, or the focused pane was never in the group), the
        // target set is empty — which would BLACK-HOLE every keystroke while
        // the broadcast indicator stays lit (the user types and nothing
        // happens, with no cue). Fall back to the focused pane so input is
        // never silently swallowed. This single point covers ungroup /
        // last-member-closed / focused-not-in-group; Off/Tab/All can't reach
        // here empty (they always include the focused/tab panes).
        if targets.is_empty() && matches!(self.broadcast, BroadcastScope::Group(_)) {
            return vec![tab.focus];
        }
        targets
    }

    /// Return true when any writable pane in the active broadcast target set
    /// would receive a raw paste instead of bracketed paste wrapping. Used by
    /// the app-level paste protection prompt: a multi-line paste is safe to
    /// send directly only when every target has enabled BRACKETED_PASTE.
    pub fn broadcast_paste_has_raw_writable_target(&self) -> bool {
        self.broadcast_target_ids().into_iter().any(|id| {
            self.panes.get(&id).is_some_and(|p| {
                !p.read_only
                    && !p
                        .term
                        .term
                        .lock()
                        .ok()
                        .map(|t| t.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
                        .unwrap_or(false)
            })
        })
    }

    /// Distribute a clipboard paste to every pane in the active tab's
    /// broadcast set. Companion to `broadcast_write`: with broadcast on
    /// (group-input mode, Ctrl+Shift+G), keystrokes go to every pane, and paste is
    /// also user input so it should follow the same scoping. Each pane
    /// gets its own `BRACKETED_PASTE` wrap decision read from its own
    /// `Term::mode()` — panes can disagree on whether the running
    /// program enabled bracketed paste (e.g. one is in vim and one is
    /// at a shell prompt), and wrapping the wrong way would either
    /// inject literal `\e[200~`/`\e[201~` markers into the shell's
    /// command line or leave bytes vulnerable to the bracketed-paste
    /// auto-execute attack inside vim. Pure modulo the writes; the
    /// per-pane wrap is the only logic here.
    pub fn broadcast_paste(&mut self, text: &str) -> PaneInputResult {
        // Route through the scope-aware target computation
        // (phase 3 of the named-groups design), same as broadcast_write.
        let ids = self.broadcast_target_ids();
        self.paste_into(ids, text)
    }

    /// Deliver a paste that ANOTHER window is originating, to the panes in this
    /// one that its scope selects. The companion to
    /// `broadcast_write_foreign_delivery`; see it for why only a named group
    /// crosses.
    pub fn broadcast_paste_foreign(
        &mut self,
        scope: &BroadcastScope,
        text: &str,
    ) -> PaneInputResult {
        let ids = self.foreign_target_ids(scope);
        self.paste_into(ids, text)
    }

    pub(crate) fn broadcast_paste_paths_delivery(
        &mut self,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        max_text_bytes: usize,
        receipt_pane: Option<u64>,
    ) -> PaneInputDelivery {
        let ids = self.broadcast_target_ids();
        self.paste_paths_into(ids, paths, trailing_space, max_text_bytes, receipt_pane)
    }

    pub fn broadcast_paste_paths_within_limit(
        &self,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        max_text_bytes: usize,
    ) -> bool {
        self.paste_paths_within_limit(
            self.broadcast_target_ids(),
            paths,
            trailing_space,
            max_text_bytes,
        )
    }

    pub(crate) fn broadcast_paste_paths_foreign_delivery(
        &mut self,
        scope: &BroadcastScope,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        max_text_bytes: usize,
    ) -> PaneInputDelivery {
        let ids = self.foreign_target_ids(scope);
        self.paste_paths_into(ids, paths, trailing_space, max_text_bytes, None)
    }

    pub fn broadcast_paste_paths_foreign_within_limit(
        &self,
        scope: &BroadcastScope,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        max_text_bytes: usize,
    ) -> bool {
        self.paste_paths_within_limit(
            self.foreign_target_ids(scope),
            paths,
            trailing_space,
            max_text_bytes,
        )
    }

    /// Would a paste under ANOTHER window's scope land raw and executable in
    /// one of this window's panes?
    ///
    /// The paste-protection prompt fires when a multi-line paste can reach a
    /// pane with no bracketed-paste mode, because there the newline runs the
    /// line. Once a group paste crosses windows, the panes that answer that
    /// question live in more than one of them: a group member at a shell prompt
    /// in a second window is exactly the target the prompt exists for, and
    /// asking only the focused window would have suppressed it.
    pub fn broadcast_paste_foreign_has_raw_writable_target(&self, scope: &BroadcastScope) -> bool {
        self.foreign_target_ids(scope).into_iter().any(|id| {
            self.panes.get(&id).is_some_and(|pane| {
                !pane.read_only
                    && !pane
                        .term
                        .term
                        .lock()
                        .ok()
                        .map(|t| t.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
                        .unwrap_or(false)
            })
        })
    }

    /// Panes in THIS window that a scope owned by another window selects.
    fn foreign_target_ids(&self, scope: &BroadcastScope) -> Vec<u64> {
        let BroadcastScope::Group(name) = scope else {
            return Vec::new();
        };
        self.panes
            .iter()
            .filter(|(_, pane)| pane.group_name.as_deref() == Some(name.as_str()))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The per-pane paste loop, shared by the local and cross-window paths so
    /// the bracketed-paste decision cannot be made one way in one window and
    /// the other way in the next.
    fn paste_into(&mut self, ids: Vec<u64>, text: &str) -> PaneInputResult {
        if ids.is_empty() {
            return PaneInputResult::Queued;
        }
        // Build the two possible payloads lazily — only when we hit the
        // first pane that needs each variant. With a 4 MiB clipboard
        // paste and 5 panes (or more, for shells-broadcast-on-CI
        // patterns), the old code allocated 5 copies of the
        // wrap (5 × 4 MiB = 20 MiB temporary). With caching, at most
        // two copies regardless of pane count. `OnceCell`-style lazy
        // via `Option`: skip even one allocation when the broadcast
        // set is entirely one BRACKETED_PASTE state.
        let mut raw: Option<Arc<[u8]>> = None;
        let mut wrapped: Option<Arc<[u8]>> = None;
        let mut result = PaneInputResult::Queued;
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id) {
                if p.pty_input_failed() {
                    result = result.merge(PaneInputResult::Failed);
                    continue;
                }
                if p.read_only {
                    result = result.merge(PaneInputResult::ReadOnly);
                    continue;
                }
                let bracketed = p
                    .term
                    .term
                    .lock()
                    .ok()
                    .map(|t| t.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
                    .unwrap_or(false);
                let bytes = if bracketed {
                    wrapped
                        .get_or_insert_with(|| Arc::from(crate::input::paste_payload(text, true)))
                } else {
                    raw.get_or_insert_with(|| Arc::from(crate::input::paste_payload(text, false)))
                };
                // Paste is user input — read-only panes drop it.
                result = result.merge(p.feed_input_shared(bytes.clone()));
            }
        }
        result
    }

    fn paste_paths_into(
        &mut self,
        ids: Vec<u64>,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        max_text_bytes: usize,
        receipt_pane: Option<u64>,
    ) -> PaneInputDelivery {
        let targets = ids
            .iter()
            .filter_map(|id| self.panes.get(id).map(|pane| (*id, pane.argv.as_slice())));
        let formatted = format_paths_for_targets(targets, paths, trailing_space);
        if formatted.iter().any(|(id, text)| {
            self.panes.get(id).is_some_and(|pane| {
                !pane.read_only && !pane.pty_input_failed() && text.len() > max_text_bytes
            })
        }) {
            return PaneInputDelivery {
                result: PaneInputResult::Oversize,
                accepted: false,
                receipt_accepted: false,
            };
        }

        let mut delivery = PaneInputDelivery::new();
        for (id, text) in formatted {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            if pane.pty_input_failed() {
                delivery.record(PaneInputResult::Failed);
                continue;
            }
            if pane.read_only {
                delivery.record(PaneInputResult::ReadOnly);
                continue;
            }
            let bracketed = pane
                .term
                .term
                .lock()
                .ok()
                .map(|term| term.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
                .unwrap_or(false);
            let bytes = crate::input::paste_payload(&text, bracketed);
            let result = pane.feed_input(&bytes);
            if receipt_pane == Some(id) {
                delivery.record_receipt_target(result);
            } else {
                delivery.record(result);
            }
        }
        delivery
    }

    fn paste_paths_within_limit(
        &self,
        ids: Vec<u64>,
        paths: &[std::path::PathBuf],
        trailing_space: bool,
        max_text_bytes: usize,
    ) -> bool {
        let targets = ids
            .iter()
            .filter_map(|id| self.panes.get(id).map(|pane| (*id, pane.argv.as_slice())));
        format_paths_for_targets(targets, paths, trailing_space)
            .iter()
            .all(|(id, text)| {
                self.panes.get(id).is_none_or(|pane| {
                    pane.read_only || pane.pty_input_failed() || text.len() <= max_text_bytes
                })
            })
    }

    pub fn tab_titles(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let pane = self.panes.get(&t.focus);
                let title = pane.map(|p| p.title.as_str()).unwrap_or("");
                // A pane we can't find (shouldn't happen) is treated as a
                // placeholder so the cwd/`tab N` fallback applies, not an empty
                // verbatim title.
                let placeholder = pane.map(|p| p.title_is_placeholder).unwrap_or(true);
                let cwd = pane.and_then(|p| p.term.current_dir_or_native());
                resolve_tab_title(
                    t.title_override.as_deref(),
                    title,
                    placeholder,
                    cwd.as_deref(),
                    i,
                )
            })
            .collect()
    }

    /// v2.26.0: like [`tab_titles`](Self::tab_titles) but also returns, for tabs
    /// whose label comes from the working directory, the home-abbreviated full
    /// path so the renderer can tier the label (full path → leaf dir name →
    /// truncated tail) to the available tab width.
    pub fn tab_labels(&self) -> Vec<TabLabel> {
        let home = home_dir_string();
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let pane = self.panes.get(&t.focus);
                let title = pane.map(|p| p.title.as_str()).unwrap_or("");
                // See `tab_titles`: a missing pane defaults to placeholder so the
                // cwd/`tab N` fallback applies rather than an empty verbatim label.
                let placeholder = pane.map(|p| p.title_is_placeholder).unwrap_or(true);
                let cwd = pane.and_then(|p| p.term.current_dir_or_native());
                resolve_tab_label(
                    t.title_override.as_deref(),
                    title,
                    placeholder,
                    cwd.as_deref(),
                    home.as_deref(),
                    i,
                )
            })
            .collect()
    }
}

/// Display title for one tab, in priority order: an explicit `title_override`
/// (Action::EditTabTitle) wins; else the focused pane's title; else — while the
/// title is still the `kettle` placeholder — the cwd basename; else `tab N`.
///
/// The override branch was missing from `tab_titles`, so a
/// custom tab title was stored but never shown (a silent no-op overwritten by
/// the shell's next OSC 2 title). Pulled out as a pure fn so the precedence is
/// drift-tested without standing up a PTY.
///
/// Most shells set the title quickly via OSC 2 on every prompt; until that
/// first prompt fires, the `kettle` placeholder is all we have, so a fresh tab
/// in `~/Repos/kettle` reads as `kettle` instead of the program name (matching
/// iTerm2 / Ghostty / WezTerm). Once a shell sets a real title, that wins.
fn resolve_tab_title(
    title_override: Option<&str>,
    pane_title: &str,
    placeholder: bool,
    cwd: Option<&str>,
    idx: usize,
) -> String {
    resolve_tab_label(title_override, pane_title, placeholder, cwd, None, idx).text
}

/// v2.26.0: a resolved tab label. `text` is the compact display string (used by
/// non-render consumers and as the fallback); `path` carries the home-abbreviated
/// full working-directory path when the label is derived from the cwd, so the
/// renderer can tier it (full path → leaf dir name → truncated tail) to the
/// available tab width. `path` is `None` for explicit/override and shell-set
/// (OSC 2) titles, which are shown verbatim (middle-ellipsized only if they
/// overflow the segment).
pub(crate) struct TabLabel {
    pub(crate) text: String,
    pub(crate) path: Option<String>,
}

/// The pure core of tab-label resolution (precedence: override → real pane title
/// → cwd → `tab N`), additionally surfacing the cwd path for the renderer's
/// width-aware tiering. `home`, when given, collapses a leading `$HOME` to `~` in
/// the surfaced path.
fn resolve_tab_label(
    title_override: Option<&str>,
    pane_title: &str,
    placeholder: bool,
    cwd: Option<&str>,
    home: Option<&str>,
    idx: usize,
) -> TabLabel {
    if let Some(ov) = title_override
        && !ov.is_empty()
    {
        return TabLabel {
            text: ov.to_string(),
            path: None,
        };
    }
    // v2.32.0 (audit): branch on the authoritative `Pane::title_is_placeholder`
    // flag, NOT a string compare against the "kettle" seed. A real shell title
    // that happens to equal the seed string ("kettle") is a genuine title and
    // must be shown verbatim — the flag is the single source of truth (the
    // instant any real OSC 2 title arrives the flag is cleared; consistent with
    // app.rs's `p.title_is_placeholder` titlebar branch).
    if placeholder || pane_title.is_empty() {
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            let full = abbreviate_home(cwd, home);
            // Platform-independent leaf: a cwd's separator style follows the
            // shell, not the build target, so split on BOTH `/` and `\` on every
            // OS. (`std::path::file_name` treats `\` as an ordinary char on Unix,
            // which mis-set the label to the whole `C:\…` string for Windows-style
            // cwds on Linux/macOS.) Matches the renderer's fit_tab_path leaf logic.
            if let Some(name) = cwd.rsplit(['/', '\\']).find(|s| !s.is_empty()) {
                return TabLabel {
                    text: name.to_string(),
                    path: Some(full),
                };
            }
            // Only separators (e.g. "/") — show the (abbreviated) full path.
            return TabLabel {
                text: full.clone(),
                path: Some(full),
            };
        }
        return TabLabel {
            text: format!("tab {}", idx + 1),
            path: None,
        };
    }
    if let Some(cwd) = cwd.filter(|c| !c.is_empty())
        && let Some(label) = cwd_label_for_shell_title(pane_title, cwd, home)
    {
        return label;
    }
    TabLabel {
        text: pane_title.to_string(),
        path: None,
    }
}

/// If a real shell title is just a prompt-rendered cwd (or an ellipsized suffix
/// of it), recover the cwd-derived label so wide tabs/window titles are not
/// stuck with the shell's already-truncated text. Oh My Zsh's term support, for
/// example, emits `%15<..<%~%<<`, yielding titles like `..PI-1/platform` even
/// when Kettle also has the authoritative OSC 7 cwd.
pub(crate) fn cwd_label_for_shell_title(
    title: &str,
    cwd: &str,
    home: Option<&str>,
) -> Option<TabLabel> {
    let leaf = cwd.rsplit(['/', '\\']).find(|s| !s.is_empty())?;
    if !shell_title_matches_cwd(title, cwd, leaf) {
        return None;
    }
    Some(TabLabel {
        text: leaf.to_string(),
        path: Some(abbreviate_home(cwd, home)),
    })
}

fn shell_title_matches_cwd(title: &str, cwd: &str, leaf: &str) -> bool {
    let title = title.trim();
    if title == leaf {
        return true;
    }

    let Some(suffix) = title
        .strip_prefix('…')
        .or_else(|| title.strip_prefix("..."))
        .or_else(|| title.strip_prefix(".."))
    else {
        return false;
    };

    if suffix.chars().count() < 8 {
        return false;
    }

    leaf.ends_with(suffix) || cwd.ends_with(suffix) || abbreviate_home(cwd, None).ends_with(suffix)
}

/// v2.26.0: collapse a leading `$HOME` in `path` to `~` (e.g.
/// `C:\Users\me\Repos\kettle` → `~\Repos\kettle`), preserving the original
/// separator style. Best-effort — a path whose prefix doesn't match `home`
/// (different separator convention, MSYS `/c/...` vs `C:\...`, etc.) is returned
/// unchanged. Pure → unit-tested.
pub(crate) fn abbreviate_home(path: &str, home: Option<&str>) -> String {
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        if path == home {
            return "~".to_string();
        }
        for sep in ['/', '\\'] {
            let prefix = format!("{home}{sep}");
            if let Some(rest) = path.strip_prefix(prefix.as_str()) {
                return format!("~{sep}{rest}");
            }
        }
    }
    path.to_string()
}

/// The user's home directory (`USERPROFILE` on Windows, else `HOME`), used to
/// abbreviate cwd-derived tab labels. `None` when unset/empty.
pub(crate) fn home_dir_string() -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// Turn a whole split tree a quarter turn, matching
/// `terminatorlib/paned.py:rotate_recursive`.
///
/// Every split flips axis. Whether its children also swap — and its ratio
/// inverts along with them — follows from where the rectangles land: rotating
/// clockwise sends the left pane of a side-by-side pair to the top (same order,
/// same ratio) and the top pane of a stacked pair to the right (reversed order,
/// mirrored ratio). Counter-clockwise is the mirror of that, which is what makes
/// the two directions exact inverses and four turns the identity — a property
/// the previous "flip the focused pane's parent, swap only when clockwise"
/// version had neither of, and one the tests now pin.
fn rotate_tree(node: &mut Node, clockwise: bool) {
    if let Node::Split { dir, ratio, a, b } = node {
        rotate_tree(a, clockwise);
        rotate_tree(b, clockwise);
        let reverse = match *dir {
            Dir::Horizontal => !clockwise,
            Dir::Vertical => clockwise,
        };
        *dir = match *dir {
            Dir::Horizontal => Dir::Vertical,
            Dir::Vertical => Dir::Horizontal,
        };
        if reverse {
            *ratio = 1.0 - *ratio;
            std::mem::swap(a, b);
        }
    }
}

/// Apply the *post-spawn* tree mutation for a split: graft the new pane id
/// next to the currently-focused leaf in direction `dir`, move focus to
/// the new pane, and **exit zoom** if it was on.
///
/// Splitting while zoomed used to leave the tab zoomed AND
/// focused on the new pane, so the user only saw the new pane — the
/// half they just split from disappeared from view (still alive, just
/// hidden by `Mux::layout`'s zoom-collapse). Every modern terminal
/// treats `split` as "show me both" — tmux's `display-panes` UX
/// after `split-window`, WezTerm's `SplitHorizontal/Vertical`. Pure so
/// the contract is unit-testable without a real spawn.
fn insert_split(tab: &mut Tab, new_id: u64, dir: Dir) -> bool {
    let focus = tab.focus;
    if tab.root.split_leaf(focus, new_id, dir) {
        tab.focus = new_id;
        tab.zoomed = false;
        return true;
    }
    // `tab.focus` was stale — not a leaf in this tree
    // (a focus-desync class of bug). Previously `split_leaf` silently no-op'd
    // and the freshly-spawned pane was orphaned (leaked PTY + child) while the
    // split still reported success. Repair focus to a real leaf and retry; the
    // caller reaps the pane if even this fails, instead of leaking it.
    let repaired = tab.root.first_leaf();
    if tab.root.split_leaf(repaired, new_id, dir) {
        tab.focus = new_id;
        tab.zoomed = false;
        return true;
    }
    false
}

fn shell_argv(cfg: &Config) -> Vec<String> {
    match &cfg.shell {
        Some(s) => vec![s.clone()],
        None => Vec::new(),
    }
}

fn argv0_base_lower(argv: &[String]) -> String {
    let base = argv
        .first()
        .map(|s| s.rsplit(['/', '\\']).next().unwrap_or(s))
        .unwrap_or("");
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Direct agent/editor launches are poor split templates: cloning them can
/// create a second full-screen app or a short-lived helper-backed pane. Split
/// should produce a usable prompt; Duplicate still preserves exact argv cloning.
fn direct_launch_splits_to_shell(argv: &[String]) -> bool {
    matches!(
        argv0_base_lower(argv).as_str(),
        "codex" | "claude" | "nvim" | "vim"
    )
}

/// Keep a candidate cwd only if it still names an existing directory — a
/// pane may have been `cd`'d into a since-removed path, in which case a new
/// tab/split should fall back to the default rather than fail to spawn.
fn usable_cwd(dir: Option<String>) -> Option<String> {
    dir.filter(|d| std::path::Path::new(d).is_dir())
}

/// Is this argv launching WSL (`wsl` / `wsl.exe`, by argv[0]
/// basename)? Used to route the cloned cwd through `wsl --cd` instead of the
/// Windows spawn cwd. Mirrors `kettle_core`'s private `is_wsl_launcher`.
///
/// v2.29.0: also consulted by the native-cwd poll — wsl.exe is a relay whose
/// own Windows cwd is its launch dir and never tracks the in-distro `cd`, so the
/// native read must be skipped for WSL (OSC 7 from inside the distro is the only
/// correct source there).
pub(crate) fn argv_is_wsl(argv: &[String]) -> bool {
    argv.first()
        .map(|p| {
            let last = p.rsplit(['/', '\\']).next().unwrap_or(p);
            last.eq_ignore_ascii_case("wsl") || last.eq_ignore_ascii_case("wsl.exe")
        })
        .unwrap_or(false)
}

/// Given a cloned `argv` + the focused pane's raw reported cwd,
/// decide the `(argv, spawn-cwd)` to launch with. For a WSL launcher the dir is
/// carried via `wsl --cd <dir>` (which accepts Windows AND Linux paths) and no
/// Windows spawn cwd is set — WSL reports a Linux path a Windows spawn would
/// reject, leaving the new pane in the home dir. Non-WSL panes inherit the
/// usable Windows dir as before. Pure (unit-tested).
fn launch_cwd(mut argv: Vec<String>, raw_cwd: Option<String>) -> (Vec<String>, Option<String>) {
    if argv_is_wsl(&argv) {
        if let Some(d) = raw_cwd.filter(|d| !d.is_empty())
            && !argv.iter().any(|a| a == "--cd")
        {
            // Insert `--cd <dir>` immediately AFTER the
            // launcher (index 1), in WSL's option section. Appending at the
            // end was wrong whenever argv carried a command —
            // `wsl -d Ubuntu -- bash -l` became
            // `wsl -d Ubuntu -- bash -l --cd <dir>`, where `--cd <dir>` is
            // passed to `bash`, not WSL, so the working dir was ignored.
            // WSL parses all options (in any order) before the command, so
            // placing `--cd` first is always valid and never lands past a
            // `--` separator or a positional command token.
            argv.insert(1, d);
            argv.insert(1, "--cd".to_string());
        }
        (argv, None)
    } else {
        let cwd = usable_cwd(raw_cwd);
        (argv, cwd)
    }
}

/// Which family of shell a pane's argv launches, for path quoting. WSL panes
/// run a POSIX shell in-distro; native Windows panes run PowerShell or cmd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneShellKind {
    /// bash/zsh/fish/… (incl. anything launched through `wsl`).
    Posix,
    /// Windows PowerShell / PowerShell 7 (`pwsh`).
    PowerShell,
    /// Legacy `cmd.exe`.
    Cmd,
}

/// Classify a pane's launch argv into a [`PaneShellKind`]. A WSL launcher is
/// POSIX (the in-distro shell); `pwsh`/`powershell` and `cmd` are matched by
/// argv[0] basename. An empty argv means the configured default shell — POSIX
/// everywhere except Windows, where the built-in default is a Windows shell.
/// Unknown programs default to POSIX (the portable, most common case). Pure.
pub(crate) fn shell_kind_for_argv(argv: &[String]) -> PaneShellKind {
    if argv_is_wsl(argv) {
        return PaneShellKind::Posix;
    }
    match argv0_base_lower(argv).as_str() {
        "pwsh" | "powershell" => PaneShellKind::PowerShell,
        "cmd" => PaneShellKind::Cmd,
        "" if cfg!(windows) => PaneShellKind::PowerShell,
        _ => PaneShellKind::Posix,
    }
}

/// Translate a Windows path to the WSL path a Linux shell can open, or `None`
/// if it is not a Windows-style path (already POSIX / unrecognized). Handles
/// drive paths (`C:\Users\me\v.mp4` → `/mnt/c/Users/me/v.mp4`) and the WSL UNC
/// shares Explorer produces for in-distro files (`\\wsl.localhost\Ubuntu\home\
/// me\v` and the legacy `\\wsl$\…` → `/home/me/v`). Pure.
pub(crate) fn windows_path_to_wsl(p: &std::path::Path) -> Option<String> {
    let s = p.to_string_lossy();
    // `\\wsl.localhost\<distro>\rest` or `\\wsl$\<distro>\rest` → `/rest`
    // (drop the distro component; the pane's own distro is what matters).
    for prefix in [
        r"\\wsl.localhost\",
        r"\\wsl$\",
        "//wsl.localhost/",
        "//wsl$/",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let rest = rest.replace('\\', "/");
            let after_distro = rest.split_once('/').map(|(_, r)| r).unwrap_or("");
            return Some(format!("/{after_distro}"));
        }
    }
    // Drive-letter path `X:\…` / `X:/…`.
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let drive = (b[0] as char).to_ascii_lowercase();
        let rest = s[2..].replace('\\', "/");
        let rest = rest.strip_prefix('/').unwrap_or(&rest);
        return Some(format!("/mnt/{drive}/{rest}"));
    }
    None
}

/// Quote a single path string for one shell family so the user can press Enter
/// without hand-escaping. POSIX/PowerShell wrap in single quotes (POSIX escapes
/// an embedded `'` as `'\''`; PowerShell doubles it to `''`); cmd wraps in
/// double quotes (Windows filenames cannot contain `"`, so this is lossless).
/// Always quotes, even plain paths, for predictable output. Pure.
pub(crate) fn quote_path_for(kind: PaneShellKind, s: &str) -> String {
    match kind {
        PaneShellKind::Posix => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('\'');
            for ch in s.chars() {
                if ch == '\'' {
                    out.push_str("'\\''");
                } else {
                    out.push(ch);
                }
            }
            out.push('\'');
            out
        }
        PaneShellKind::PowerShell => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('\'');
            for ch in s.chars() {
                if ch == '\'' {
                    out.push_str("''");
                } else {
                    out.push(ch);
                }
            }
            out.push('\'');
            out
        }
        PaneShellKind::Cmd => format!("\"{s}\""),
    }
}

/// Format OS file paths (from a clipboard file-list or a drag-drop) into text
/// to feed the focused pane: translate to the pane's WSL path when it runs
/// WSL, quote each for the pane's shell family, and space-join multiple paths.
/// Pure so the whole rule is unit-tested.
pub(crate) fn format_paths_for_paste(argv: &[String], paths: &[std::path::PathBuf]) -> String {
    let wsl = argv_is_wsl(argv);
    let kind = shell_kind_for_argv(argv);
    paths
        .iter()
        .map(|p| {
            let s = if wsl {
                windows_path_to_wsl(p).unwrap_or_else(|| p.to_string_lossy().into_owned())
            } else {
                p.to_string_lossy().into_owned()
            };
            quote_path_for(kind, &s)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn format_paths_for_targets<'a>(
    targets: impl IntoIterator<Item = (u64, &'a [String])>,
    paths: &[std::path::PathBuf],
    trailing_space: bool,
) -> Vec<(u64, String)> {
    targets
        .into_iter()
        .map(|(id, argv)| {
            let mut text = format_paths_for_paste(argv, paths);
            if trailing_space {
                text.push(' ');
            }
            (id, text)
        })
        .collect()
}

pub(crate) fn encode_target_modes(
    targets: impl IntoIterator<Item = (u64, kettle_core::TermMode)>,
    mut encode: impl FnMut(kettle_core::TermMode) -> Option<Vec<u8>>,
) -> Vec<(u64, Vec<u8>)> {
    targets
        .into_iter()
        .filter_map(|(id, mode)| encode(mode).map(|bytes| (id, bytes)))
        .collect()
}

fn collect_ids(n: &Node, out: &mut Vec<u64>) {
    match n {
        Node::Leaf(id) => out.push(*id),
        Node::Split { a, b, .. } => {
            collect_ids(a, out);
            collect_ids(b, out);
        }
    }
}

impl Default for Mux {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod node_tests {
    use super::*;

    /// The production half of this file: everything above the test module.
    ///
    /// The production source of this file, excluding test-only items.
    fn production_source() -> String {
        let production = kettle_test_support::production_source(include_str!("mux.rs"));
        assert!(
            !production.contains("fn production_source()"),
            "the production slice retained its own helper"
        );
        assert!(
            !production.contains("#[test]"),
            "the production slice retained a test function"
        );
        assert!(
            !production.contains("#[cfg(test)]"),
            "the production slice retained a test-only item"
        );
        production
    }

    #[test]
    fn pane_input_outcome_precedence_is_explicit() {
        use PaneInputResult::{Backpressured, Failed, Oversize, Queued, ReadOnly};

        assert_eq!(Queued.merge(ReadOnly), ReadOnly);
        assert_eq!(ReadOnly.merge(Queued), ReadOnly);
        assert_eq!(ReadOnly.merge(Backpressured), Backpressured);
        assert_eq!(Backpressured.merge(Oversize), Oversize);
        assert_eq!(Oversize.merge(Failed), Failed);
        assert_eq!(Failed.merge(Queued), Failed);

        assert_eq!(pane_input_policy(false, false), None);
        assert_eq!(pane_input_policy(false, true), Some(ReadOnly));
        assert_eq!(
            pane_input_policy(true, true),
            Some(Failed),
            "sticky transport failure must dominate read-only policy"
        );
    }

    #[test]
    fn broadcast_key_encoding_runs_for_each_target_mode() {
        let kitty = kettle_core::TermMode::REPORT_ALL_KEYS_AS_ESC
            | kettle_core::TermMode::REPORT_EVENT_TYPES;
        let targets = [(10, kettle_core::TermMode::empty()), (20, kitty)];
        let press = encode_target_modes(targets, |mode| {
            Some(
                if mode.contains(kettle_core::TermMode::REPORT_ALL_KEYS_AS_ESC) {
                    b"\x1b[97;1u".to_vec()
                } else {
                    b"a".to_vec()
                },
            )
        });
        assert_eq!(press, [(10, b"a".to_vec()), (20, b"\x1b[97;1u".to_vec())]);

        let release = encode_target_modes(targets, |mode| {
            mode.contains(kettle_core::TermMode::REPORT_EVENT_TYPES)
                .then(|| b"\x1b[97;1:3u".to_vec())
        });
        assert_eq!(release, [(20, b"\x1b[97;1:3u".to_vec())]);
    }

    #[test]
    fn full_user_channel_is_backpressure_and_releases_failed_reservation() {
        let (user_tx, user_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let (reply_tx, _reply_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let queued_user_bytes = Arc::new(AtomicUsize::new(0));
        let queue = PtyInputQueue {
            user_tx,
            reply_tx,
            queued_user_bytes: queued_user_bytes.clone(),
            queued_reply_bytes: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            waker: Arc::new(|| {}),
        };

        for _ in 0..PTY_INPUT_QUEUE_DEPTH {
            assert_eq!(
                queue.enqueue_user(Arc::from(&b"x"[..])),
                PaneInputResult::Queued
            );
        }
        assert_eq!(
            queued_user_bytes.load(Ordering::Acquire),
            PTY_INPUT_QUEUE_DEPTH
        );
        assert_eq!(
            queue.enqueue_user(Arc::from(&b"y"[..])),
            PaneInputResult::Backpressured
        );
        assert_eq!(
            queued_user_bytes.load(Ordering::Acquire),
            PTY_INPUT_QUEUE_DEPTH,
            "the unsent message must release its one-byte reservation"
        );
        assert!(!queue.failed(), "user backpressure must remain retryable");

        // Crossbeam retains buffered values while either side still owns the
        // channel. Tear down both endpoints before asserting that every queued
        // message's Drop released its byte reservation.
        drop(queue);
        drop(user_rx);
        assert_eq!(
            queued_user_bytes.load(Ordering::Acquire),
            0,
            "tearing down the channel must release every queued byte reservation"
        );
    }

    #[test]
    fn pty_input_queues_preserve_paste_contract_and_fail_replies_closed() {
        let (user_tx, user_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_DEPTH);
        let queued_user_bytes = Arc::new(AtomicUsize::new(0));
        let queued_reply_bytes = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let queue = PtyInputQueue {
            user_tx,
            reply_tx,
            queued_user_bytes: queued_user_bytes.clone(),
            queued_reply_bytes: queued_reply_bytes.clone(),
            failed: failed.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            waker: Arc::new(move || {
                wakes_for_callback.fetch_add(1, Ordering::Relaxed);
            }),
        };

        assert_eq!(
            queue.enqueue_user(Arc::from(vec![b'x'; 4 * 1024 * 1024 + 12])),
            PaneInputResult::Queued
        );
        assert_eq!(
            queued_user_bytes.load(Ordering::Acquire),
            4 * 1024 * 1024 + 12
        );
        let aggregate_headroom =
            MAX_QUEUED_USER_INPUT_BYTES - queued_user_bytes.load(Ordering::Acquire);
        assert_eq!(
            queue.enqueue_user(Arc::from(vec![b'b'; aggregate_headroom])),
            PaneInputResult::Queued,
            "the exact aggregate byte budget must remain admissible"
        );
        assert_eq!(
            queued_user_bytes.load(Ordering::Acquire),
            MAX_QUEUED_USER_INPUT_BYTES
        );
        assert_eq!(
            queue.enqueue_user(Arc::from(&b"b"[..])),
            PaneInputResult::Backpressured,
            "one byte over the bounded user-input budget is retryable, not read-only or failed"
        );
        assert!(!failed.load(Ordering::Acquire));
        assert_eq!(
            queue.enqueue_user(Arc::from(vec![b'x'; MAX_USER_INPUT_MESSAGE_BYTES + 1])),
            PaneInputResult::Oversize,
            "oversized user input must be distinct and must not kill the pane"
        );
        assert!(!failed.load(Ordering::Acquire));

        assert_eq!(
            queue.enqueue_reply(Arc::from(vec![b'r'; MAX_PROTOCOL_REPLY_MESSAGE_BYTES])),
            PaneInputResult::Queued
        );
        assert_eq!(
            queue.enqueue_reply(Arc::from(&b"y"[..])),
            PaneInputResult::Failed,
            "protocol reply byte-budget exhaustion must fail closed"
        );
        assert!(failed.load(Ordering::Acquire));
        assert_eq!(wakes.load(Ordering::Acquire), 1);
        assert_eq!(
            queue.enqueue_reply(Arc::from(&b"z"[..])),
            PaneInputResult::Failed,
            "failure must remain sticky"
        );
        assert_eq!(wakes.load(Ordering::Acquire), 1, "wake only on the edge");

        drop(user_rx);
        drop(reply_rx);
    }

    #[test]
    fn pty_input_worker_finishes_started_message_before_priority_reply() {
        let (user_tx, user_rx) = crossbeam_channel::bounded(1);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let user_bytes: Arc<[u8]> = Arc::from(vec![b'u'; PTY_INPUT_WRITE_CHUNK_BYTES + 37]);
        let reply_bytes: Arc<[u8]> = Arc::from(&b"reply"[..]);
        let queued_user_bytes = Arc::new(AtomicUsize::new(user_bytes.len()));
        let queued_reply_bytes = Arc::new(AtomicUsize::new(0));
        user_tx
            .send(QueuedPtyInput {
                bytes: user_bytes.clone(),
                queued_bytes: queued_user_bytes.clone(),
            })
            .unwrap();
        drop(user_tx);

        let failed = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        let waker: Waker = Arc::new(|| {});
        let mut reply_tx = Some(reply_tx);
        let mut written = Vec::new();
        let mut writes = 0usize;
        run_pty_input_worker(reply_rx, user_rx, &failed, &stop, &waker, |bytes| {
            let count = bytes.len().min(97);
            written.extend_from_slice(&bytes[..count]);
            writes += 1;
            if writes == 1 {
                queued_reply_bytes.store(reply_bytes.len(), Ordering::Release);
                reply_tx
                    .take()
                    .unwrap()
                    .send(QueuedPtyInput {
                        bytes: reply_bytes.clone(),
                        queued_bytes: queued_reply_bytes.clone(),
                    })
                    .unwrap();
            }
            Ok(count)
        });

        let mut expected = user_bytes.to_vec();
        expected.extend_from_slice(&reply_bytes);
        assert_eq!(written, expected);
        assert_eq!(queued_user_bytes.load(Ordering::Acquire), 0);
        assert_eq!(queued_reply_bytes.load(Ordering::Acquire), 0);
        assert!(!failed.load(Ordering::Acquire));
    }

    #[test]
    fn pty_input_worker_bounds_reply_priority_between_messages() {
        let (user_tx, user_rx) = crossbeam_channel::bounded(1);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(2);
        let queued_user_bytes = Arc::new(AtomicUsize::new(1));
        let queued_reply_bytes = Arc::new(AtomicUsize::new(2));
        user_tx
            .send(QueuedPtyInput {
                bytes: Arc::from(&b"u"[..]),
                queued_bytes: queued_user_bytes.clone(),
            })
            .unwrap();
        drop(user_tx);
        for byte in *b"12" {
            reply_tx
                .send(QueuedPtyInput {
                    bytes: Arc::from(&[byte][..]),
                    queued_bytes: queued_reply_bytes.clone(),
                })
                .unwrap();
        }
        drop(reply_tx);

        let failed = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        let waker: Waker = Arc::new(|| {});
        let mut written = Vec::new();
        run_pty_input_worker(reply_rx, user_rx, &failed, &stop, &waker, |bytes| {
            written.extend_from_slice(bytes);
            Ok(bytes.len())
        });

        assert_eq!(written, b"1u2");
        assert_eq!(queued_user_bytes.load(Ordering::Acquire), 0);
        assert_eq!(queued_reply_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn disconnected_pty_input_queue_releases_its_byte_reservation() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let queue = PtyInputQueue::disconnected_for_test(Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        }));

        assert_eq!(
            queue.enqueue_reply(Arc::from(&b"reply"[..])),
            PaneInputResult::Failed
        );
        assert_eq!(
            queue.enqueue_user(Arc::from(&b"user"[..])),
            PaneInputResult::Failed,
            "a disconnected user-input worker is terminal rather than retryable"
        );
        assert_eq!(queue.queued_reply_bytes.load(Ordering::Acquire), 0);
        assert!(queue.failed());
        assert_eq!(wakes.load(Ordering::Acquire), 1);
    }

    /// Drift guard. Session focus persistence is the core
    /// state machine for relaunch: `snapshot` records the focused pane's
    /// DFS-order *index* via `leaf_index_of` (pane ids are reallocated across
    /// restores, so the id itself isn't portable), and `restore` recreates
    /// focus with `nth_leaf` at that index. The two walk children in the same
    /// `a → b` order and MUST stay exact inverses, or relaunch silently focuses
    /// the wrong pane. An off-by-one here is invisible to a behavioral test
    /// (every shell still spawns) — this pins the invariant directly.
    #[test]
    fn leaf_index_of_and_nth_leaf_are_inverse() {
        // Split( Split(L1,L2), Split(L3,L4) ) — DFS leaf order 1,2,3,4.
        let split = |a, b| Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(a),
            b: Box::new(b),
        };
        let tree = split(
            split(Node::Leaf(1), Node::Leaf(2)),
            split(Node::Leaf(3), Node::Leaf(4)),
        );
        assert_eq!(tree.leaf_ids(), vec![1, 2, 3, 4], "DFS leaf order");
        for (idx, id) in [(0usize, 1u64), (1, 2), (2, 3), (3, 4)] {
            assert_eq!(tree.leaf_index_of(id), Some(idx), "index of leaf {id}");
            assert_eq!(tree.nth_leaf(idx), id, "leaf at index {idx}");
            // The exact round trip restore relies on:
            assert_eq!(tree.nth_leaf(tree.leaf_index_of(id).unwrap()), id);
        }
        // A pane id no longer in the tree → None (snapshot then stores 0).
        assert_eq!(tree.leaf_index_of(999), None);
        // An index past a trimmed tree falls back to the first leaf, so a
        // stale session still produces a valid focus instead of panicking.
        assert_eq!(tree.nth_leaf(99), tree.first_leaf());
        assert_eq!(tree.first_leaf(), 1);
        // Single-leaf tab: index 0 ↔ the lone pane.
        let solo = Node::Leaf(7);
        assert_eq!(solo.leaf_index_of(7), Some(0));
        assert_eq!(solo.nth_leaf(0), 7);
    }

    // ---- Directional pane-focus navigation scaffolding ----

    /// A representative wide area (matches the user's HiDPI screenshot ratio).
    const AREA: Rect = (0.0, 0.0, 2560.0, 1440.0);

    fn push_tab(m: &mut Mux, root: Node, focus: u64) {
        m.tabs.push(Tab {
            root,
            focus,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });
        m.active = m.tabs.len() - 1;
    }

    #[test]
    fn prospective_split_geometry_matches_post_graft_second_child() {
        let mut mux = Mux::new();
        push_tab(&mut mux, Node::Leaf(7), 7);
        assert_eq!(
            mux.prospective_split_rect(Dir::Horizontal, (0.0, 0.0, 101.0, 51.0)),
            Some((51.0, 0.0, 50.0, 51.0))
        );
        assert_eq!(
            mux.prospective_split_rect(Dir::Vertical, (0.0, 0.0, 101.0, 51.0)),
            Some((0.0, 26.0, 101.0, 25.0))
        );

        mux.tabs[0].zoomed = true;
        assert_eq!(
            mux.prospective_split_rect(Dir::Horizontal, (0.0, 0.0, 101.0, 51.0)),
            Some((51.0, 0.0, 50.0, 51.0)),
            "splitting exits zoom, so initial geometry must use the unzoomed tree"
        );
    }
    fn hsplit(ratio: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }
    fn vsplit(ratio: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Vertical,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// The screenshot layout. Leaf ids: 1=left (full height), 2=top-wide,
    /// 3=midleft (tall, left of the lower-right region), 4=midL, 5=midR
    /// (the mid row of two), 6=botright (the focused pane).
    fn screenshot_tree() -> Node {
        hsplit(
            0.5,
            Node::Leaf(1),
            vsplit(
                0.33,
                Node::Leaf(2),
                hsplit(
                    0.5,
                    Node::Leaf(3),
                    vsplit(
                        0.5,
                        hsplit(0.5, Node::Leaf(4), Node::Leaf(5)),
                        Node::Leaf(6),
                    ),
                ),
            ),
        )
    }

    /// The OLD Euclidean-center rule, inlined so a future revert to
    /// center-distance fails this test (it documents exactly why it was wrong).
    fn old_focus_dir(rects: &[(u64, Rect)], focus: u64, dx: i32, dy: i32) -> Option<u64> {
        let (_, (fx, fy, fw, fh)) = *rects.iter().find(|(id, _)| *id == focus)?;
        let (fcx, fcy) = (fx + fw / 2.0, fy + fh / 2.0);
        let mut best: Option<(f32, u64)> = None;
        for (id, (x, y, w, h)) in rects {
            if *id == focus {
                continue;
            }
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            let ok = (dx > 0 && cx > fcx)
                || (dx < 0 && cx < fcx)
                || (dy > 0 && cy > fcy)
                || (dy < 0 && cy < fcy);
            if !ok {
                continue;
            }
            let d = (cx - fcx).powi(2) + (cy - fcy).powi(2);
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, *id));
            }
        }
        best.map(|(_, id)| id)
    }

    #[test]
    fn focus_dir_screenshot_layout_picks_adjacent_not_diagonal() {
        let mut m = Mux::new();
        push_tab(&mut m, screenshot_tree(), 6);
        let rects = m.layout(0, AREA);

        // (a) Document the bug: the old center-distance rule jumps Left to the
        // DIAGONAL midL (4) and Right to the up-right midR (5) — there is no real
        // right neighbor of botright at all.
        assert_eq!(
            old_focus_dir(&rects, 6, -1, 0),
            Some(4),
            "old rule jumps Left to the diagonal midL (the reported bug)"
        );
        assert_eq!(
            old_focus_dir(&rects, 6, 1, 0),
            Some(5),
            "old rule jumps Right to the up-right midR (phantom neighbor)"
        );

        // (b) The new edge+overlap rule moves to the true neighbors.
        m.focus_dir(AREA, -1, 0);
        assert_eq!(
            m.tabs[0].focus, 3,
            "Left -> the full-height pane bordering botright's left edge"
        );
        m.tabs[0].focus = 6;
        m.focus_dir(AREA, 0, -1);
        assert_eq!(
            m.tabs[0].focus, 4,
            "Up -> the pane directly above (midL wins the midL/midR tie via DFS order)"
        );
        m.tabs[0].focus = 6;
        m.focus_dir(AREA, 1, 0);
        assert_eq!(
            m.tabs[0].focus, 6,
            "Right -> nothing borders the right edge; no-op"
        );
        m.focus_dir(AREA, 0, 1);
        assert_eq!(m.tabs[0].focus, 6, "Down -> nothing below; no-op");
    }

    #[test]
    fn focus_dir_2x2_grid_moves_to_orthogonal_neighbor() {
        // H{ V{A=1,C=2}, V{B=3,D=4} }: A=TL C=BL B=TR D=BR.
        let tree = hsplit(
            0.5,
            vsplit(0.5, Node::Leaf(1), Node::Leaf(2)),
            vsplit(0.5, Node::Leaf(3), Node::Leaf(4)),
        );
        let area = (0.0, 0.0, 200.0, 100.0);
        let mut m = Mux::new();
        push_tab(&mut m, tree, 4); // start at D (bottom-right)
        m.focus_dir(area, 0, -1);
        assert_eq!(m.tabs[0].focus, 3, "D Up -> B");
        m.tabs[0].focus = 4;
        m.focus_dir(area, -1, 0);
        assert_eq!(m.tabs[0].focus, 2, "D Left -> C");
        m.tabs[0].focus = 1; // A (top-left)
        m.focus_dir(area, 1, 0);
        assert_eq!(m.tabs[0].focus, 3, "A Right -> B");
        m.tabs[0].focus = 1;
        m.focus_dir(area, 0, 1);
        assert_eq!(m.tabs[0].focus, 2, "A Down -> C");
    }

    #[test]
    fn focus_dir_two_pane_split_and_edge_noops() {
        let tree = hsplit(0.5, Node::Leaf(1), Node::Leaf(2)); // A | B
        let area = (0.0, 0.0, 200.0, 100.0);
        let mut m = Mux::new();
        push_tab(&mut m, tree, 1);
        m.focus_dir(area, 1, 0);
        assert_eq!(m.tabs[0].focus, 2, "A Right -> B");
        m.tabs[0].focus = 1;
        for (dx, dy) in [(-1, 0), (0, -1), (0, 1)] {
            m.focus_dir(area, dx, dy);
            assert_eq!(m.tabs[0].focus, 1, "no neighbor that way -> stay on A");
        }
        m.tabs[0].focus = 2;
        m.focus_dir(area, -1, 0);
        assert_eq!(m.tabs[0].focus, 1, "B Left -> A");
        m.tabs[0].focus = 2;
        for (dx, dy) in [(1, 0), (0, -1), (0, 1)] {
            m.focus_dir(area, dx, dy);
            assert_eq!(m.tabs[0].focus, 2, "no neighbor that way -> stay on B");
        }
    }

    #[test]
    fn focus_dir_is_reversible_on_grid() {
        let tree = hsplit(
            0.5,
            vsplit(0.5, Node::Leaf(1), Node::Leaf(2)),
            vsplit(0.5, Node::Leaf(3), Node::Leaf(4)),
        );
        let area = (0.0, 0.0, 200.0, 100.0);
        let mut m = Mux::new();
        push_tab(&mut m, tree, 1); // A
        m.focus_dir(area, 1, 0);
        assert_eq!(m.tabs[0].focus, 3);
        m.focus_dir(area, -1, 0);
        assert_eq!(m.tabs[0].focus, 1, "Right then Left returns to A");
        m.focus_dir(area, 0, 1);
        assert_eq!(m.tabs[0].focus, 2);
        m.focus_dir(area, 0, -1);
        assert_eq!(m.tabs[0].focus, 1, "Down then Up returns to A");
    }

    #[test]
    fn focus_dir_noop_when_zoomed() {
        let mut m = Mux::new();
        push_tab(&mut m, screenshot_tree(), 6);
        m.tabs[0].zoomed = true; // layout returns only the focused pane
        assert!(m.zoom_hides_siblings());
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            m.focus_dir(AREA, dx, dy);
            assert_eq!(m.tabs[0].focus, 6, "zoomed: focus_dir must be a no-op");
        }

        let mut single = Mux::new();
        push_tab(&mut single, Node::Leaf(7), 7);
        single.tabs[0].zoomed = true;
        assert!(
            single.is_zoomed(),
            "the persisted zoom bit remains truthful"
        );
        assert!(
            !single.zoom_hides_siblings(),
            "one pane has no hidden focus target, even with zoom toggled"
        );
    }

    /// Drift guard. When a saved split-tree partially
    /// rebuilds — the first child spawns, a later sibling fails (cwd gone,
    /// fork under quota) — `build_node` returns `Err` and the whole tree is
    /// discarded, but the panes already spawned for the first child stay in
    /// `self.panes`, orphaned: a leaked PTY + child process each. The fix
    /// threads a `spawned: &mut Vec<u64>` accumulator through `build_node`
    /// and reaps those ids on the restore error path. A behavioral test
    /// would need a real PTY + event-loop `Waker` (unavailable in unit
    /// tests, like every other spawn path here), so the wiring is pinned at
    /// the source level.
    #[test]
    fn build_node_reaps_orphan_panes_on_partial_restore_failure() {
        let src = production_source();
        assert!(
            src.contains("spawned: &mut Vec<u64>"),
            "build_node must thread a spawned-id accumulator so a partial \
             subtree's panes can be reaped on failure"
        );
        assert!(
            src.contains("spawned.push(id);"),
            "each spawned pane id must be recorded in the accumulator"
        );
        assert!(
            src.contains("for id in &tab_pane_ids {") && src.contains("self.panes.remove(id);"),
            "the restore error arm must reap every pane the partial tree \
             spawned, or a failed split leaks PTYs + child processes"
        );
    }

    /// Split divider drag-to-resize geometry. `dividers`
    /// must mirror `layout` exactly, `set_ratio_at` must address the right
    /// split via its path, and the pos→ratio + hit-test helpers must be
    /// correct. These are the pieces a behavioral mouse test can't reach
    /// (no window), so unit-test the math directly.
    #[test]
    fn split_divider_geometry_round_trips() {
        use super::{Dir, Node, ratio_from_pos, seam_at};

        // Horizontal split (side-by-side) at ratio 0.5 over a 200x100 area
        // anchored at (0,0): one vertical divider at x=100, spanning y∈[0,100].
        let split = |dir, ratio, a, b| Node::Split {
            dir,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        };
        let root = split(Dir::Horizontal, 0.5, Node::Leaf(1), Node::Leaf(2));
        let mut seams = Vec::new();
        root.dividers((0.0, 0.0, 200.0, 100.0), &mut Vec::new(), &mut seams);
        assert_eq!(seams.len(), 1);
        assert_eq!(seams[0].pos, 100.0);
        assert_eq!(seams[0].dir, Dir::Horizontal);
        assert!(seams[0].path.is_empty(), "root split has empty path");

        // Hit-test: a cursor within tol of the vertical seam (x≈100) and inside
        // the vertical span hits; one far away misses.
        assert_eq!(seam_at(&seams, 102.0, 50.0, 4.0), Some(0));
        assert_eq!(seam_at(&seams, 140.0, 50.0, 4.0), None); // too far in x
        assert_eq!(seam_at(&seams, 100.0, 150.0, 4.0), None); // outside y span

        // pos→ratio: dragging the seam to x=150 over the 200-wide split → 0.75.
        let r = ratio_from_pos(seams[0].rect, seams[0].dir, 150.0, 50.0);
        assert!((r - 0.75).abs() < 1e-6, "ratio was {r}");
        // Clamp: dragging past either edge leaves MIN_SPLIT_PX of pane behind,
        // never 0/1 — measured in pixels against this 200-wide split, so the
        // stop is the same physical distance whatever the window size.
        let min_ratio = super::MIN_SPLIT_PX / 200.0;
        assert_eq!(
            ratio_from_pos(seams[0].rect, Dir::Horizontal, -50.0, 0.0),
            min_ratio
        );
        assert_eq!(
            ratio_from_pos(seams[0].rect, Dir::Horizontal, 999.0, 0.0),
            1.0 - min_ratio
        );

        // Nested tree: root Horizontal(0.5){ Leaf1, Vertical(0.5){Leaf2,Leaf3} }.
        // Two seams: the root vertical divider (path []) and the right child's
        // horizontal divider (path [true]).
        let mut nested = split(
            Dir::Horizontal,
            0.5,
            Node::Leaf(1),
            split(Dir::Vertical, 0.5, Node::Leaf(2), Node::Leaf(3)),
        );
        let mut seams = Vec::new();
        nested.dividers((0.0, 0.0, 200.0, 100.0), &mut Vec::new(), &mut seams);
        assert_eq!(seams.len(), 2);
        // Outer first, then inner (so a tie resolves to the outer split).
        assert_eq!(seams[0].path, Vec::<bool>::new());
        assert_eq!(seams[1].path, vec![true]);
        assert_eq!(seams[1].dir, Dir::Vertical);

        // Set the inner (path [true]) split's ratio and confirm only it moved.
        assert!(nested.set_ratio_at(&[true], 0.8));
        let mut seams2 = Vec::new();
        nested.dividers((0.0, 0.0, 200.0, 100.0), &mut Vec::new(), &mut seams2);
        // Inner Vertical split now at 0.8 of its 100-tall right column → y=80.
        let inner = seams2.iter().find(|s| s.path == vec![true]).unwrap();
        assert_eq!(inner.pos, 80.0);
        // A path that doesn't land on a split returns false (stale path).
        assert!(!nested.set_ratio_at(&[false, true], 0.5)); // descends into Leaf1
    }

    /// Build the split tree that repeatedly splitting the newest pane produces:
    /// `Split{ Leaf1, Split{ Leaf2, Split{ ... } } }`, `count` leaves deep.
    fn chain(dir: Dir, count: usize) -> Node {
        assert!(count >= 1, "a chain needs at least one pane");
        let mut node = Node::Leaf(count as u64);
        for id in (1..count).rev() {
            node = Node::Split {
                dir,
                ratio: 0.5,
                a: Box::new(Node::Leaf(id as u64)),
                b: Box::new(node),
            };
        }
        node
    }

    /// `equalize` used to clamp every ratio it computed into a fixed
    /// `[0.05, 0.95]` band, so a chain needing `1/N < 0.05` could not be
    /// represented: at 23 panes the widths came out 7,7,7,6,6,… and by 28 the
    /// widest pane was 1.75x the narrowest. The band is gone; the only floor is
    /// in pixels, and at these sizes it never binds.
    #[test]
    fn equalize_stays_exact_past_the_pane_count_a_ratio_band_could_hold() {
        // Precondition: this test is only meaningful past the old band. A chain
        // of 28 wants 1/28 at its outermost split, well under the old 0.05
        // floor — if that stops being true the test has stopped testing.
        let panes = 28;
        assert!(
            1.0 / panes as f32 <= 0.05,
            "fixture no longer exercises the sub-band region"
        );

        for (dir, width, height) in [
            (Dir::Horizontal, 1900.0_f32, 1000.0_f32),
            (Dir::Vertical, 1000.0, 1900.0),
        ] {
            let mut root = chain(dir, panes);
            assert_eq!(root.equalize(), panes);

            let mut out = Vec::new();
            root.layout((0.0, 0.0, width, height), &mut out);
            assert_eq!(out.len(), panes);

            let extent = |r: &Rect| if dir == Dir::Horizontal { r.2 } else { r.3 };
            let sizes: Vec<f32> = out.iter().map(|(_, r)| extent(r)).collect();
            let smallest = sizes.iter().cloned().fold(f32::INFINITY, f32::min);
            let largest = sizes.iter().cloned().fold(0.0_f32, f32::max);
            // Every pane within a pixel of every other: all that separates them
            // is `layout`'s per-split rounding.
            assert!(
                largest - smallest <= 1.0,
                "{dir:?} panes ranged {smallest}..{largest}, sizes {sizes:?}"
            );
            // And they still tile the area exactly, with no seam drift.
            let total: f32 = sizes.iter().sum();
            let axis = if dir == Dir::Horizontal {
                width
            } else {
                height
            };
            assert!(
                (total - axis).abs() < 1e-3,
                "{dir:?} panes summed to {total}, not {axis}"
            );
        }
    }

    /// The pixel floor is what keeps a pane grabbable, so it has to bind when
    /// the space really does run out — and it has to split what is left evenly
    /// rather than handing one side everything.
    #[test]
    fn a_split_too_small_for_the_floor_is_halved_instead_of_collapsed() {
        // Roomy: the ratio is honored outright.
        assert_eq!(split_extent_px(1000.0, 0.25), 250.0);
        // Extreme ratios still leave a grabbable pane on both sides.
        assert_eq!(split_extent_px(1000.0, 0.0), MIN_SPLIT_PX);
        assert_eq!(split_extent_px(1000.0, 1.0), 1000.0 - MIN_SPLIT_PX);
        // Too small to seat two floors: halve rather than collapse.
        let cramped = MIN_SPLIT_PX;
        assert_eq!(split_extent_px(cramped, 0.9), cramped / 2.0);
        // Degenerate input never escapes as a NaN rect.
        assert_eq!(split_extent_px(f32::NAN, 0.5), 0.0);
        assert_eq!(split_extent_px(0.0, 0.5), 0.0);
        assert_eq!(split_extent_px(1000.0, f32::NAN), 500.0);
    }

    /// Keyboard resize used to clamp into the same `[0.05, 0.95]` band, which
    /// meant that on a tab with many panes asking for a pane to get *smaller*
    /// made it get bigger — the shrink landed below 0.05 and clamped back up.
    #[test]
    fn shrinking_a_pane_below_the_old_band_actually_shrinks_it() {
        let mut root = chain(Dir::Horizontal, 28);
        root.equalize();
        let width_of = |root: &Node, id: u64| {
            let mut out = Vec::new();
            root.layout((0.0, 0.0, 1900.0, 1000.0), &mut out);
            out.iter().find(|(i, _)| *i == id).unwrap().1.2
        };
        let before = width_of(&root, 1);
        // Precondition: pane 1's share is under the old floor, so the old code
        // could not have narrowed it at all.
        assert!(
            before / 1900.0 < 0.05,
            "pane 1 held {before}px of 1900 — not below the old band"
        );
        assert!(root.resize(1, Dir::Horizontal, -0.01));
        let after = width_of(&root, 1);
        assert!(
            after < before,
            "shrink moved {before}px to {after}px — the wrong direction"
        );
    }

    /// Terminator's `always_split_with_profile`. Off (the default), splitting
    /// `kettle -e vim` gives you a shell to work in; on, it gives you the same
    /// launch again, which is what a Terminator profile with a custom command
    /// does.
    #[test]
    fn always_split_with_profile_repeats_a_direct_launch_instead_of_dropping_to_a_shell() {
        let argv =
            |parts: &[&str]| -> Vec<String> { parts.iter().map(|s| (*s).to_string()).collect() };
        let editor = argv(&["nvim", "notes.md"]);
        // Precondition: this argv must be one kettle would otherwise refuse to
        // repeat, or the test proves nothing about the flag.
        assert!(
            direct_launch_splits_to_shell(&editor),
            "fixture must be a direct launch for the flag to have anything to do"
        );
        assert!(split_falls_back_to_shell(&editor, false));
        assert!(!split_falls_back_to_shell(&editor, true));

        // A pane with no argv IS the shell, whatever the flag says.
        assert!(split_falls_back_to_shell(&[], false));
        assert!(split_falls_back_to_shell(&[], true));

        // An ordinary shell launch was always cloned; the flag doesn't
        // change that.
        let shell = argv(&["pwsh", "-NoLogo"]);
        assert!(!direct_launch_splits_to_shell(&shell));
        assert!(!split_falls_back_to_shell(&shell, false));
        assert!(!split_falls_back_to_shell(&shell, true));
    }

    /// `split_to_group` has to be applied to the new pane BEFORE the graft,
    /// because grafting moves focus onto it — after that, "the focused pane's
    /// group" is the new pane's own (empty) one and the inheritance silently
    /// does nothing. Both split entry points are checked: a new one that
    /// forgets the call would ship a config key that works on one code path.
    #[test]
    fn both_split_paths_inherit_the_group_before_the_graft_moves_focus() {
        let src = production_source();
        let calls = src
            .matches("self.inherit_split_group(cfg, new_id);")
            .count();
        assert_eq!(
            calls, 2,
            "expected the group inheritance on both split paths \
             (split_geometry / split_with_geometry); found {calls}"
        );
        for (idx, _) in src.match_indices("self.inherit_split_group(cfg, new_id);") {
            let after = &src[idx..];
            let graft = after
                .find("insert_split(tab, new_id, dir)")
                .expect("each inheritance is followed by its graft");
            let next_call = after[1..]
                .find("self.inherit_split_group(cfg, new_id);")
                .map(|i| i + 1)
                .unwrap_or(usize::MAX);
            assert!(
                graft < next_call,
                "the graft belonging to this inheritance must come before the \
                 next split path begins"
            );
        }
    }

    /// A named group is a set the user declared, and `group_all` already puts
    /// panes in every window into one. The broadcast did not follow: it stopped
    /// at whichever window was focused, so typing reached half a group with
    /// nothing on screen to explain it — the other window's panes still wore
    /// the group name in their titlebars.
    #[test]
    fn only_a_named_group_reaches_panes_in_another_window() {
        // A window that is not the one being typed in receives under Group,
        // and under nothing else. `Tab` is defined by a focused tab, which
        // exists in exactly one window; `All` is kettle's own window-wide
        // scope; `Off` sends nothing anywhere.
        assert!(Mux::scope_crosses_windows(&BroadcastScope::Group(
            "fleet".into()
        )));
        for local in [
            BroadcastScope::Off,
            BroadcastScope::Tab,
            BroadcastScope::All,
        ] {
            assert!(
                !Mux::scope_crosses_windows(&local),
                "{local:?} is defined by something window-local and must not \
                 reach across"
            );
        }

        // And the selection itself: only panes carrying that exact name.
        // Panes need a live PTY, so drive the same membership question the
        // foreign path asks, over the pane table it would read.
        let members = [
            (1u64, Some("fleet")),
            (2u64, None),
            (3u64, Some("fleet")),
            (4u64, Some("other")),
        ];
        let selected: Vec<u64> = members
            .iter()
            .filter(|(_, g)| *g == Some("fleet"))
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            selected,
            vec![1, 3],
            "a foreign window contributes its group members and nobody else"
        );
        assert!(
            members.iter().any(|(_, g)| *g == Some("other")),
            "the fixture must contain a non-member group, or the filter is \
             untested"
        );
    }

    /// Moving a pane is a lift followed by a graft, and the order is what makes
    /// it correct: grafting first would attach the pane to a tree it is still
    /// part of, and it would appear twice. The sibling case is the one that
    /// catches it — there, the lift collapses the very split the target lives
    /// in.
    #[test]
    fn moving_a_pane_lifts_it_before_grafting_it_beside_the_target() {
        let leaves = |m: &Mux| {
            let mut ids = m.tabs[0].root.leaf_ids();
            ids.sort_unstable();
            ids
        };

        // Split{ Split{1,2}, Split{3,4} }: move 1 next to 4. Panes 1 and 2 are
        // siblings, so lifting 1 collapses their split to a bare Leaf(2).
        let build = || Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(1)),
                b: Box::new(Node::Leaf(2)),
            }),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(3)),
                b: Box::new(Node::Leaf(4)),
            }),
        };
        let mut m = Mux::new();
        push_tab(&mut m, build(), 2);
        assert!(m.move_pane_beside(1, 4, Dir::Horizontal, false));
        assert_eq!(
            leaves(&m),
            vec![1, 2, 3, 4],
            "every pane must survive the move exactly once -- a graft before \
             the lift duplicates the moving pane"
        );
        assert_eq!(m.tabs[0].focus, 1, "the moved pane takes focus");

        // Dropping on the near side puts it there, not on the far side of the
        // pane the user aimed at.
        let mut near = Mux::new();
        push_tab(&mut near, build(), 1);
        assert!(near.move_pane_beside(1, 4, Dir::Horizontal, true));
        let order = near.tabs[0].root.leaf_ids();
        let (i1, i4) = (
            order.iter().position(|&x| x == 1).unwrap(),
            order.iter().position(|&x| x == 4).unwrap(),
        );
        assert!(
            i1 < i4,
            "before=true must place the pane ahead of the target"
        );
        let mut far = Mux::new();
        push_tab(&mut far, build(), 1);
        assert!(far.move_pane_beside(1, 4, Dir::Horizontal, false));
        let order = far.tabs[0].root.leaf_ids();
        assert!(
            order.iter().position(|&x| x == 1).unwrap()
                > order.iter().position(|&x| x == 4).unwrap(),
            "before=false must place it after"
        );

        // Refusals, rather than a half-applied move.
        let mut solo = Mux::new();
        push_tab(&mut solo, Node::Leaf(1), 1);
        assert!(
            !solo.move_pane_beside(1, 1, Dir::Horizontal, false),
            "same pane"
        );
        assert!(
            !solo.move_pane_beside(1, 99, Dir::Horizontal, false),
            "unknown target"
        );
        assert!(
            !solo.move_pane_beside(99, 1, Dir::Horizontal, false),
            "unknown mover"
        );
        assert_eq!(
            solo.tabs[0].root.leaf_ids(),
            vec![1],
            "a refused move must leave the tree exactly as it was"
        );
    }

    /// The four triangles must tile the pane: every interior point answers
    /// exactly one edge, and the answer is the edge the point is nearest.
    #[test]
    fn pane_drop_zone_picks_the_nearest_edge() {
        // Deliberately non-square (400x100): a pixel-distance model would call
        // most of this pane's width "top", because every point is within 50px
        // of the top edge while the left edge is 200px away at the centre.
        let rect = (10.0f32, 20.0f32, 400.0f32, 100.0f32);
        for &(px, py, want, what) in &[
            (20.0, 70.0, (Dir::Horizontal, true), "left edge"),
            (400.0, 70.0, (Dir::Horizontal, false), "right edge"),
            (210.0, 25.0, (Dir::Vertical, true), "top edge"),
            (210.0, 115.0, (Dir::Vertical, false), "bottom edge"),
        ] {
            assert_eq!(
                pane_drop_zone(rect, px, py),
                Some(want),
                "({px}, {py}) is over the {what} of a 400x100 pane"
            );
        }
        // The precondition the aspect-independence claim rests on: at the
        // horizontal midpoint the cursor really is nearer the top in PIXELS,
        // so a distance-based model would have answered Vertical here and this
        // case would not distinguish the two models.
        let (x, y, w, h) = rect;
        let (px, py) = (x + w * 0.25, y + h * 0.5);
        assert!(
            (py - y) < (px - x),
            "fixture must place the point nearer the top edge in raw pixels"
        );
        assert_eq!(
            pane_drop_zone(rect, px, py),
            Some((Dir::Horizontal, true)),
            "a quarter of the way in, on the vertical midline, is the LEFT \
             triangle -- the zones are normalised, not pixel-distance"
        );
    }

    #[test]
    fn pane_drop_zone_covers_every_point_exactly_once() {
        let rect = (0.0f32, 0.0f32, 37.0f32, 23.0f32);
        // `Dir` is not `Hash`, and giving it that derive purely for a test
        // would widen the type's contract for no caller. A four-slot tally
        // keyed by the same (dir, before) pair does the counting instead.
        let slot = |(dir, before): (Dir, bool)| match (dir, before) {
            (Dir::Vertical, true) => 0usize,
            (Dir::Vertical, false) => 1,
            (Dir::Horizontal, true) => 2,
            (Dir::Horizontal, false) => 3,
        };
        let mut seen = [0usize; 4];
        for iy in 0..23 {
            for ix in 0..37 {
                let z = pane_drop_zone(rect, ix as f32 + 0.5, iy as f32 + 0.5);
                let z = z.unwrap_or_else(|| panic!("({ix}, {iy}) is inside the pane"));
                seen[slot(z)] += 1;
            }
        }
        assert!(
            seen.iter().all(|&n| n > 0),
            "all four zones must be reachable: {seen:?}"
        );
        assert_eq!(
            seen.iter().sum::<usize>(),
            37 * 23,
            "the zones must tile the pane with no point counted twice"
        );
    }

    #[test]
    fn pane_drop_zone_refuses_points_outside_and_empty_rects() {
        let rect = (10.0f32, 20.0f32, 40.0f32, 30.0f32);
        assert_eq!(pane_drop_zone(rect, 9.0, 30.0), None, "left of the pane");
        assert_eq!(
            pane_drop_zone(rect, 50.0, 30.0),
            None,
            "past the right edge"
        );
        assert_eq!(
            pane_drop_zone(rect, 20.0, 50.0),
            None,
            "past the bottom edge"
        );
        assert_eq!(pane_drop_zone(rect, f32::NAN, 30.0), None, "NaN cursor");
        assert_eq!(
            pane_drop_zone((10.0, 20.0, 0.0, 30.0), 10.0, 30.0),
            None,
            "a zero-width pane has no zones to be over"
        );
    }

    /// The preview must be the half the pane would actually give up, so the
    /// hint cannot promise one geometry and the drop deliver another.
    #[test]
    fn pane_drop_preview_is_the_half_the_split_would_take() {
        let rect = (10.0f32, 20.0f32, 40.0f32, 30.0f32);
        assert_eq!(
            pane_drop_preview(rect, Dir::Horizontal, true),
            (10.0, 20.0, 20.0, 30.0)
        );
        assert_eq!(
            pane_drop_preview(rect, Dir::Horizontal, false),
            (30.0, 20.0, 20.0, 30.0)
        );
        assert_eq!(
            pane_drop_preview(rect, Dir::Vertical, true),
            (10.0, 20.0, 40.0, 15.0)
        );
        assert_eq!(
            pane_drop_preview(rect, Dir::Vertical, false),
            (10.0, 35.0, 40.0, 15.0)
        );
        // Each preview must sit inside the pane and take exactly half its area.
        for (dir, before) in [
            (Dir::Horizontal, true),
            (Dir::Horizontal, false),
            (Dir::Vertical, true),
            (Dir::Vertical, false),
        ] {
            let (px, py, pw, ph) = pane_drop_preview(rect, dir, before);
            let (x, y, w, h) = rect;
            assert!(
                px >= x && py >= y && px + pw <= x + w && py + ph <= y + h,
                "{dir:?}/{before} preview escapes the pane"
            );
            assert!(
                (pw * ph - w * h / 2.0).abs() < 0.01,
                "{dir:?}/{before} preview is not half the pane"
            );
        }
    }

    /// A drop hint must name the pane the cursor is genuinely over. `focus_at`
    /// deliberately snaps to the nearest pane instead, which is right for a
    /// click and wrong here.
    #[test]
    fn pane_rect_at_reports_only_a_pane_under_the_cursor() {
        let mut m = Mux::new();
        push_tab(
            &mut m,
            Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(1)),
                b: Box::new(Node::Leaf(2)),
            },
            1,
        );
        let area = (0.0f32, 0.0f32, 100.0f32, 50.0f32);
        let rects = m.layout(m.active, area);
        assert_eq!(rects.len(), 2, "fixture must lay out two panes");
        assert_eq!(m.pane_rect_at(area, 10.0, 25.0).map(|(id, _)| id), Some(1));
        assert_eq!(m.pane_rect_at(area, 90.0, 25.0).map(|(id, _)| id), Some(2));
        assert_eq!(
            m.pane_rect_at(area, -1.0, 25.0),
            None,
            "outside the area entirely -- no target, so no hint"
        );
        assert_eq!(
            m.pane_rect_at(area, 50.0, 60.0),
            None,
            "below every pane -- no target, so no hint"
        );
    }

    /// tab + window state to the set of target pane IDs.
    /// Phase 2 of the named-groups design.
    #[test]
    fn compute_broadcast_targets_matrix() {
        let in_tab = vec![1u64, 2, 3];
        let all = vec![
            (1u64, Some("fleet")),
            (2u64, Some("fleet")),
            (3u64, None),
            (4u64, Some("misc")),
            (5u64, Some("fleet")),
        ];
        // Off: only the focused pane receives.
        assert_eq!(
            compute_broadcast_targets(&BroadcastScope::Off, 2, &in_tab, &all),
            vec![2]
        );
        // Tab: every pane in the focused tab.
        assert_eq!(
            compute_broadcast_targets(&BroadcastScope::Tab, 2, &in_tab, &all),
            vec![1, 2, 3]
        );
        // All: every pane window-wide.
        assert_eq!(
            compute_broadcast_targets(&BroadcastScope::All, 2, &in_tab, &all),
            vec![1, 2, 3, 4, 5]
        );
        // Group("fleet") with the focused pane (2) a MEMBER: every pane tagged
        // "fleet", regardless of tab; the focused pane is already in the set so
        // it is NOT duplicated.
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("fleet".to_string()),
                2,
                &in_tab,
                &all
            ),
            vec![1, 2, 5]
        );
        // Group("fleet") with the focused pane (4) NOT a member: the on-screen
        // pane is unioned in (appended, deduped) so input is never routed away
        // from it. v2.32.0 (audit) — the Group arm now always includes the
        // focused pane, mirroring Off/Tab/All.
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("fleet".to_string()),
                4,
                &in_tab,
                &all
            ),
            vec![1, 2, 5, 4]
        );
        // Group with no group matches still yields the focused pane (never an
        // empty set that would black-hole input). v2.32.0 (audit).
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("nonexistent".to_string()),
                2,
                &in_tab,
                &all
            ),
            vec![2]
        );
        // Default scope is Off.
        assert_eq!(BroadcastScope::default(), BroadcastScope::Off);
    }

    /// `broadcast_target_ids` must never emit a phantom pane id when there
    /// is no active tab. A fresh `Mux` has no tabs/panes; in every scope
    /// the target set is empty rather than the old `[0]` sentinel that the
    /// `Off` arm would have produced from `unwrap_or(0)`.
    #[test]
    fn broadcast_target_ids_empty_when_no_active_tab() {
        let mut mux = Mux::new();
        for scope in [
            BroadcastScope::Off,
            BroadcastScope::Tab,
            BroadcastScope::All,
            BroadcastScope::Group("fleet".to_string()),
        ] {
            mux.broadcast = scope.clone();
            assert!(
                mux.broadcast_target_ids().is_empty(),
                "scope {scope:?} should yield no targets with no active tab"
            );
        }
    }

    /// v2.32.0 (audit, HIGH): an emptied named broadcast Group must NEVER
    /// black-hole input. When the active scope is a `Group` but no pane matches
    /// it (last member closed / ungrouped / the focused pane was never in the
    /// group), `broadcast_target_ids` self-heals to `[focus]` so typing still
    /// reaches the on-screen pane instead of vanishing while the indicator stays
    /// lit. Built without a PTY: the method only reads `tab.focus` and the group
    /// names in `self.panes`, so an empty `panes` map with a `Group` scope
    /// exercises the empty-group path directly.
    #[test]
    fn broadcast_target_ids_self_heals_empty_group_to_focus() {
        let mut mux = Mux::new();
        mux.tabs.push(Tab {
            root: Node::Leaf(42),
            focus: 42,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });
        mux.active = 0;
        // No pane carries the "fleet" group (panes map is empty) → the raw
        // target set is empty, but the self-heal returns the focused pane.
        mux.broadcast = BroadcastScope::Group("fleet".to_string());
        assert_eq!(
            mux.broadcast_target_ids(),
            vec![42],
            "an empty named group must heal to the focused pane, never black-hole input"
        );
        // Sanity: the self-heal is Group-only — scope Off still short-circuits
        // to an empty set (broadcast disabled, the caller writes to the focused
        // pane directly), unchanged by this fix.
        mux.broadcast = BroadcastScope::Off;
        assert!(mux.broadcast_target_ids().is_empty());
    }

    #[test]
    fn resolve_tab_title_precedence() {
        use super::resolve_tab_title;
        // An explicit override wins over a real pane title
        // AND over the cwd fallback — the bug was that it was ignored entirely.
        // (The `bool` arg is `Pane::title_is_placeholder`.)
        assert_eq!(
            resolve_tab_title(Some("deploy"), "bash", false, Some("/home/u/proj"), 0),
            "deploy"
        );
        assert_eq!(
            resolve_tab_title(Some("notes"), "kettle", true, Some("/home/u/proj"), 2),
            "notes"
        );
        // Empty override is ignored (falls through to the normal chain).
        assert_eq!(resolve_tab_title(Some(""), "vim", false, None, 0), "vim");
        // No override: a real shell title wins.
        assert_eq!(
            resolve_tab_title(None, "vim - main.rs", false, None, 0),
            "vim - main.rs"
        );
        // Placeholder title (still the seed) → cwd basename.
        assert_eq!(
            resolve_tab_title(None, "kettle", true, Some("/home/u/Repos/kettle"), 0),
            "kettle"
        );
        // v2.32.0 (audit): a REAL shell title that happens to equal the seed
        // string "kettle" (placeholder = false) is shown VERBATIM — it must NOT
        // be re-derived as a placeholder via a string compare against the seed.
        assert_eq!(
            resolve_tab_title(None, "kettle", false, Some("/home/u/Repos/proj"), 0),
            "kettle"
        );
        // Placeholder + no cwd → "tab N" (1-based).
        assert_eq!(resolve_tab_title(None, "kettle", true, None, 3), "tab 4");
        // Empty title is always a placeholder regardless of the flag.
        assert_eq!(resolve_tab_title(None, "", true, None, 0), "tab 1");
        assert_eq!(resolve_tab_title(None, "", false, None, 0), "tab 1");
    }

    #[test]
    fn resolve_tab_label_surfaces_cwd_path() {
        use super::resolve_tab_label;
        // cwd fallback (placeholder title still the seed): compact text is the
        // leaf, but the full (abbreviated) path is surfaced for the renderer to
        // tier. (The `bool` arg is `Pane::title_is_placeholder`.)
        let l = resolve_tab_label(
            None,
            "kettle",
            true,
            Some("/home/u/Repos/kettle"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert_eq!(l.path.as_deref(), Some("~/Repos/kettle"));
        // No home match → full path unabbreviated.
        let l = resolve_tab_label(None, "kettle", true, Some("/srv/app"), Some("/home/u"), 0);
        assert_eq!(l.text, "app");
        assert_eq!(l.path.as_deref(), Some("/srv/app"));
        // v2.32.0 (audit): a REAL title equal to the seed string "kettle"
        // (placeholder = false) is shown verbatim and carries NO cwd path —
        // the flag, not a string compare, decides placeholder-ness.
        let l = resolve_tab_label(
            None,
            "kettle",
            false,
            Some("/home/u/Repos/proj"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert!(l.path.is_none());
        // A real shell title that is exactly the cwd leaf still carries the
        // full cwd path for width-aware tab fitting. The title itself wins;
        // the path is metadata only.
        let l = resolve_tab_label(
            None,
            "flight-event-line-server-go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "flight-event-line-server-go");
        assert_eq!(
            l.path.as_deref(),
            Some("~/Repos/SPI-1/flight-event-line-server-go")
        );
        // Shells/prompts may set an already-left-truncated title. When that
        // title is a clear suffix of the cwd leaf, recover the full leaf/path so
        // wide tabs can show all available context instead of preserving stale
        // truncation.
        let l = resolve_tab_label(
            None,
            "..ine-server-go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "flight-event-line-server-go");
        assert_eq!(
            l.path.as_deref(),
            Some("~/Repos/SPI-1/flight-event-line-server-go")
        );
        let l = resolve_tab_label(
            None,
            "…ine-server-go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "flight-event-line-server-go");
        assert_eq!(
            l.path.as_deref(),
            Some("~/Repos/SPI-1/flight-event-line-server-go")
        );
        let l = resolve_tab_label(
            None,
            "..go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "..go");
        assert!(l.path.is_none());
        for truncated in ["...PI-1/platform", "..PI-1/platform", "…PI-1/platform"] {
            let l = resolve_tab_label(
                None,
                truncated,
                false,
                Some("/home/u/Repos/SPI-1/platform"),
                Some("/home/u"),
                0,
            );
            assert_eq!(l.text, "platform", "{truncated}");
            assert_eq!(l.path.as_deref(), Some("~/Repos/SPI-1/platform"));
        }
        let l = resolve_tab_label(
            None,
            "..PI-1/platform",
            false,
            Some("/home/u/Repos/other/platform"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "..PI-1/platform");
        assert!(l.path.is_none());
        // Override / real title / no-cwd carry no path (shown verbatim).
        assert!(
            resolve_tab_label(Some("deploy"), "bash", false, Some("/x/y"), None, 0)
                .path
                .is_none()
        );
        assert!(
            resolve_tab_label(None, "vim - main.rs", false, None, None, 0)
                .path
                .is_none()
        );
        assert!(
            resolve_tab_label(None, "kettle", true, None, None, 3)
                .path
                .is_none()
        );
        // Windows-style separators abbreviate too.
        let l = resolve_tab_label(
            None,
            "kettle",
            true,
            Some("C:\\Users\\me\\Repos\\kettle"),
            Some("C:\\Users\\me"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert_eq!(l.path.as_deref(), Some("~\\Repos\\kettle"));
        let l = resolve_tab_label(
            None,
            "...Repos\\kettle",
            false,
            Some("C:\\Users\\me\\Repos\\kettle"),
            Some("C:\\Users\\me"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert_eq!(l.path.as_deref(), Some("~\\Repos\\kettle"));
    }

    #[test]
    fn abbreviate_home_rules() {
        use super::abbreviate_home;
        assert_eq!(abbreviate_home("/home/u/proj", Some("/home/u")), "~/proj");
        assert_eq!(abbreviate_home("/home/u", Some("/home/u")), "~");
        // Not under home → unchanged.
        assert_eq!(abbreviate_home("/etc/hosts", Some("/home/u")), "/etc/hosts");
        // No home → unchanged.
        assert_eq!(abbreviate_home("/home/u/proj", None), "/home/u/proj");
        // A non-boundary prefix must NOT match (/home/user vs home /home/u).
        assert_eq!(
            abbreviate_home("/home/user/x", Some("/home/u")),
            "/home/user/x"
        );
        assert_eq!(
            abbreviate_home("C:\\Users\\me\\p", Some("C:\\Users\\me")),
            "~\\p"
        );
    }

    /// A rotation is a rotation of the *picture*, so the honest test is
    /// geometric: turn the tree, and every pane must be where turning the screen
    /// would have put it. Checking the tree shape instead would pass for a
    /// version that flips axes without reordering children or mirroring ratios —
    /// which is exactly what kettle used to do.
    #[test]
    fn rotating_the_layout_moves_every_pane_where_turning_the_screen_would() {
        // Nested and lopsided on purpose: a shape-only check can't tell a real
        // rotation from a flip, and an even ratio can't tell a mirrored ratio
        // from an unmirrored one.
        let tree = || Node::Split {
            dir: Dir::Vertical,
            ratio: 0.25,
            a: Box::new(Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.4,
                a: Box::new(Node::Leaf(1)),
                b: Box::new(Node::Leaf(2)),
            }),
            b: Box::new(Node::Leaf(3)),
        };
        // Square area so a rotated rect stays inside it and the coordinates are
        // directly comparable.
        let side = 800.0_f32;
        let rects = |root: &Node| {
            let mut out = Vec::new();
            root.layout((0.0, 0.0, side, side), &mut out);
            out.sort_by_key(|(id, _)| *id);
            out
        };

        let before = rects(&tree());
        let mut turned = tree();
        rotate_tree(&mut turned, true);
        let after = rects(&turned);

        // Turning the picture clockwise sends (x, y) to (side - y - h, x).
        for ((id, (x, y, w, h)), (rid, (rx, ry, rw, rh))) in before.iter().zip(&after) {
            assert_eq!(id, rid, "pane order changed");
            assert!(
                (rx - (side - y - h)).abs() <= 1.0
                    && (ry - x).abs() <= 1.0
                    && (rw - h).abs() <= 1.0
                    && (rh - w).abs() <= 1.0,
                "pane {id} was ({x},{y},{w},{h}), turned to ({rx},{ry},{rw},{rh})"
            );
        }

        // Clockwise then counter-clockwise is the identity, and so is four
        // turns the same way. Neither held before: counter-clockwise used to be
        // "flip the axis and don't swap", which is not the inverse of anything.
        let mut round_trip = tree();
        rotate_tree(&mut round_trip, true);
        rotate_tree(&mut round_trip, false);
        assert_eq!(rects(&round_trip), before, "cw then ccw must be a no-op");

        let mut four = tree();
        for _ in 0..4 {
            rotate_tree(&mut four, true);
        }
        assert_eq!(rects(&four), before, "four clockwise turns must be a no-op");
    }

    /// Terminator rotates every pane in the visible tab, not just the split the
    /// focused pane happens to sit in, and it leaves zoom on the way so the user
    /// can see what happened.
    #[test]
    fn rotate_turns_the_whole_tab_and_leaves_zoom() {
        let mut mux = Mux::new();
        push_tab(
            &mut mux,
            Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Split {
                    dir: Dir::Horizontal,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(1)),
                    b: Box::new(Node::Leaf(2)),
                }),
                b: Box::new(Node::Leaf(3)),
            },
            1,
        );
        mux.tabs[0].zoomed = true;

        assert!(mux.rotate_layout(true));
        assert!(!mux.tabs[0].zoomed, "rotation must leave zoom");
        // Both splits turned — the focused pane's parent AND the one above it.
        match &mux.tabs[0].root {
            Node::Split { dir, b, .. } => {
                assert_eq!(*dir, Dir::Horizontal, "outer split must turn too");
                assert!(
                    matches!(
                        b.as_ref(),
                        Node::Split {
                            dir: Dir::Vertical,
                            ..
                        }
                    ),
                    "inner split must turn as well"
                );
            }
            Node::Leaf(_) => panic!("root should still be a split"),
        }

        // A tab with nothing to rotate says so rather than reporting work.
        let mut solo = Mux::new();
        push_tab(&mut solo, Node::Leaf(1), 1);
        assert!(!solo.rotate_layout(true));
    }

    #[test]
    fn tab_title_falls_back_to_cwd_basename() {
        // The fallback only kicks in when the pane's title is the
        // initial placeholder "kettle" (or empty) — once a real shell
        // sets `\e]2;…\007`, that title wins. This is a small pure
        // test of the path-basename logic since the full title path
        // requires a real Terminal/PTY.
        let path = "/home/user/Repos/kettle";
        let basename = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str());
        assert_eq!(basename, Some("kettle"));
        // Trailing slash: `file_name` returns None on root-like paths
        // — those should fall through to "tab N" by the same code.
        assert_eq!(std::path::Path::new("/").file_name(), None);
        // Edge: empty path / no-cwd case (Terminal::current_dir = None)
        // also routes to "tab N" naturally.
    }

    #[test]
    fn initial_pane_title_seeds_ssh_with_target_else_kettle() {
        // Plain shell (or empty argv) → "kettle" placeholder; the
        // cwd-basename fallback fills it in once OSC 7 arrives.
        assert_eq!(initial_pane_title(&[]), "kettle");
        assert_eq!(initial_pane_title(&["bash".into()]), "kettle");
        assert_eq!(initial_pane_title(&["zsh".into(), "-i".into()]), "kettle");
        // Path-qualified shell is still treated as a shell (basename match).
        assert_eq!(initial_pane_title(&["/bin/bash".into()]), "kettle");
        assert_eq!(initial_pane_title(&["/usr/bin/fish".into()]), "kettle");
        // Windows shells too — names differ from POSIX so list them explicitly.
        assert_eq!(initial_pane_title(&["pwsh.exe".into()]), "kettle");
        assert_eq!(initial_pane_title(&["cmd.exe".into()]), "kettle");
        // SSH: surface the target so the tab is identifiable while
        // connecting. `-t`/`-A`/etc are skipped to find the host.
        assert_eq!(
            initial_pane_title(&["ssh".into(), "-t".into(), "me@example.com".into()]),
            "ssh me@example.com"
        );
        assert_eq!(initial_pane_title(&["ssh".into(), "box".into()]), "ssh box");
        // `ssh` with no positional arg → just "ssh" (rare but defined).
        assert_eq!(initial_pane_title(&["ssh".into(), "-V".into()]), "ssh");
        // Explicit `-e PROG` for non-shells uses the program basename, so
        // `kettle -e htop` doesn't show the generic "kettle" forever
        // (htop never emits OSC 2 and has no useful cwd to back-fill from).
        assert_eq!(initial_pane_title(&["htop".into()]), "htop");
        assert_eq!(initial_pane_title(&["/usr/bin/htop".into()]), "htop");
        assert_eq!(initial_pane_title(&["vim".into(), "file.rs".into()]), "vim");
        assert_eq!(
            initial_pane_title(&["python3".into(), "script.py".into()]),
            "python3"
        );
        assert_eq!(initial_pane_title(&["tmux".into()]), "tmux");
    }

    #[test]
    fn engine_cursor_shape_maps_config_to_engine() {
        // Block / Underline are 1:1. `Bar` (kettle config name) → `Beam`
        // (engine name) — same thin vertical stroke. The engine also has
        // `HollowBlock` and `Hidden` but those only ever arrive via
        // DECSCUSR/DEC?25 from a running program, never as a seed.
        assert_eq!(engine_cursor_shape(CursorStyle::Block), CursorShape::Block);
        assert_eq!(
            engine_cursor_shape(CursorStyle::Underline),
            CursorShape::Underline
        );
        assert_eq!(engine_cursor_shape(CursorStyle::Bar), CursorShape::Beam);
    }

    #[test]
    fn modify_other_keys_config_controls_only_the_enter_fallback() {
        assert!(!unnegotiated_modified_enter(ModifyOtherKeysMode::Auto));
        assert!(unnegotiated_modified_enter(ModifyOtherKeysMode::Always));
        assert!(!unnegotiated_modified_enter(ModifyOtherKeysMode::Off));
    }

    #[test]
    fn automatic_modified_enter_requires_a_known_foreground_composer() {
        use kettle_core::ShellActivity;

        let auto = ModifyOtherKeysMode::Auto;
        // Nested shells and readline/libedit REPLs can be noncanonical too.
        // They must receive CR, not the tail of an unsolicited xterm sequence.
        assert!(!modified_enter_fallback(
            auto,
            ModifiedEnterContext::UnixPty {
                canonical: Some(false),
                foreground_program: Some(false),
            }
        ));
        assert!(modified_enter_fallback(
            auto,
            ModifiedEnterContext::UnixPty {
                canonical: Some(false),
                foreground_program: Some(true),
            }
        ));
        assert!(!modified_enter_fallback(
            auto,
            ModifiedEnterContext::UnixPty {
                canonical: Some(true),
                foreground_program: Some(true),
            }
        ));
        assert!(!modified_enter_fallback(
            auto,
            ModifiedEnterContext::UnixPty {
                canonical: None,
                foreground_program: Some(true),
            }
        ));
        assert!(!modified_enter_fallback(
            auto,
            ModifiedEnterContext::UnixPty {
                canonical: Some(false),
                foreground_program: None,
            }
        ));
        assert!(modified_enter_fallback(
            auto,
            ModifiedEnterContext::WindowsShell {
                activity: ShellActivity::Running,
                foreground_program: Some(true),
                launch_program: false,
            }
        ));
        assert!(!modified_enter_fallback(
            auto,
            ModifiedEnterContext::WindowsShell {
                activity: ShellActivity::Running,
                foreground_program: Some(false),
                launch_program: false,
            }
        ));
        assert!(modified_enter_fallback(
            auto,
            ModifiedEnterContext::WindowsShell {
                activity: ShellActivity::Unknown,
                foreground_program: None,
                launch_program: true,
            }
        ));
        for activity in [ShellActivity::Idle, ShellActivity::Unknown] {
            assert!(!modified_enter_fallback(
                auto,
                ModifiedEnterContext::WindowsShell {
                    activity,
                    foreground_program: Some(true),
                    launch_program: false,
                }
            ));
        }
        assert!(modified_enter_fallback(
            ModifyOtherKeysMode::Always,
            ModifiedEnterContext::Unsupported
        ));
        assert!(!modified_enter_fallback(
            ModifyOtherKeysMode::Off,
            ModifiedEnterContext::UnixPty {
                canonical: Some(false),
                foreground_program: Some(true),
            }
        ));
    }

    #[test]
    fn automatic_program_detection_is_a_narrow_allowlist() {
        assert!(argv_accepts_unnegotiated_modified_enter(&["codex".into()]));
        assert!(argv_accepts_unnegotiated_modified_enter(&[
            "C:\\Users\\me\\bin\\Codex.exe".into()
        ]));
        assert!(argv_accepts_unnegotiated_modified_enter(&[
            "/usr/local/bin/claude".into()
        ]));
        assert!(argv_accepts_unnegotiated_modified_enter(&[
            "node".into(),
            "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js".into(),
        ]));
        for program in [
            "htop",
            "python",
            "python3",
            "node",
            "psql",
            "sqlite3",
            "gdb",
            "lldb",
            "zsh",
            "pwsh.exe",
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "ssh",
            "autossh",
            "telnet",
            "wsl.exe",
            "tmux",
            "screen",
            "mosh",
            "zellij",
            "byobu",
            "dtach",
            "abduco",
            "script",
            "sudo",
            "doas",
            "pkexec",
            "su",
            "runuser",
            "setpriv",
            "login",
            "env",
            "nix-shell",
            "chroot",
            "nsenter",
            "/usr/bin/unshare",
            "setsid",
            "systemd-run",
            "bwrap",
            "firejail",
            "proot",
            "docker",
            "podman.exe",
            "nerdctl",
            "kubectl",
            "distrobox",
            "distrobox-enter",
            "toolbox",
            "lxc",
            "machinectl",
            "FLATPAK-SPAWN",
        ] {
            assert!(
                !argv_accepts_unnegotiated_modified_enter(&[program.into()]),
                "{program}"
            );
        }
        assert!(!argv_accepts_unnegotiated_modified_enter(&[]));
        assert!(!argv_accepts_unnegotiated_modified_enter(&[
            "python".into(),
            "codex".into(),
        ]));
        assert!(!argv_accepts_unnegotiated_modified_enter(&[
            "node".into(),
            "/tmp/server.js".into(),
            "codex".into(),
        ]));
        assert!(!argv_accepts_unnegotiated_modified_enter(&[
            "node".into(),
            "/home/me/src/codex/scripts/repl.js".into(),
        ]));
    }

    #[test]
    fn unix_foreground_matching_rejects_stale_process_snapshots() {
        let codex = kettle_remote::ForegroundProcess {
            pid: 20,
            argv: vec!["codex".into()],
        };
        let python = kettle_remote::ForegroundProcess {
            pid: 21,
            argv: vec!["python3".into()],
        };

        assert_eq!(
            unix_foreground_program_acceptance(Some(20), Some(10), &["zsh".into()], Some(&codex)),
            Some(true)
        );
        assert_eq!(
            unix_foreground_program_acceptance(Some(21), Some(10), &["zsh".into()], Some(&python)),
            Some(false)
        );
        assert_eq!(
            unix_foreground_program_acceptance(Some(22), Some(10), &["zsh".into()], Some(&codex)),
            None,
            "a snapshot for the previous foreground pid must fail closed"
        );
        assert_eq!(
            unix_foreground_program_acceptance(Some(10), Some(10), &["codex".into()], None),
            Some(true),
            "a directly launched composer is identified by its immutable child pid"
        );
        assert_eq!(
            unix_foreground_program_acceptance(None, Some(10), &["codex".into()], Some(&codex)),
            None
        );
    }

    #[test]
    fn argv_is_wsl_detects_launcher_by_basename() {
        assert!(argv_is_wsl(&["wsl".to_string()]));
        assert!(argv_is_wsl(&["wsl.exe".to_string()]));
        assert!(argv_is_wsl(&["WSL.EXE".to_string()]));
        assert!(argv_is_wsl(&[
            "C:\\Windows\\System32\\wsl.exe".to_string(),
            "-d".to_string()
        ]));
        assert!(!argv_is_wsl(&["pwsh.exe".to_string()]));
        assert!(!argv_is_wsl(&["bash".to_string()]));
        assert!(!argv_is_wsl(&[]));
    }

    #[test]
    fn direct_agent_editor_launches_split_to_shell() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        assert!(direct_launch_splits_to_shell(&s(&["codex"])));
        assert!(direct_launch_splits_to_shell(&s(&["/usr/bin/claude"])));
        assert!(direct_launch_splits_to_shell(&s(&[
            "C:\\Users\\me\\bin\\CODEX.EXE"
        ])));
        assert!(direct_launch_splits_to_shell(&s(&["nvim", "file.rs"])));
        assert!(direct_launch_splits_to_shell(&s(&["vim", "file.rs"])));

        // Shell/session launchers remain exact split templates.
        assert!(!direct_launch_splits_to_shell(&s(&["bash"])));
        assert!(!direct_launch_splits_to_shell(&s(&["zsh", "-l"])));
        assert!(!direct_launch_splits_to_shell(&s(&["wsl.exe"])));
        assert!(!direct_launch_splits_to_shell(&s(&["ssh", "box"])));
        // Ordinary explicit commands keep the pre-existing split clone behavior.
        assert!(!direct_launch_splits_to_shell(&s(&["htop"])));
        assert!(!direct_launch_splits_to_shell(&s(&[
            "python3",
            "script.py"
        ])));
        assert!(!direct_launch_splits_to_shell(&[]));
    }

    /// Splitting/duplicating clones the focused pane's command;
    /// for WSL the dir is carried via `wsl --cd` (a Windows spawn can't `cd`
    /// into the Linux path WSL reports). Guards the pure decision.
    #[test]
    fn launch_cwd_routes_wsl_dir_through_cd_flag() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let tmp = std::env::temp_dir().to_string_lossy().into_owned();

        // Non-WSL + a real Windows dir → inherited as the spawn cwd, argv as-is.
        let (argv, cwd) = launch_cwd(s(&["pwsh.exe"]), Some(tmp.clone()));
        assert_eq!(argv, s(&["pwsh.exe"]));
        assert_eq!(cwd, Some(tmp));

        // Non-WSL + a non-directory (e.g. a Linux path) → no spawn cwd.
        let (argv, cwd) = launch_cwd(s(&["pwsh.exe"]), Some("/mnt/c/nope-xyz".into()));
        assert_eq!(argv, s(&["pwsh.exe"]));
        assert_eq!(cwd, None);

        // WSL + a reported (Linux) dir → carried via `--cd`, inserted in the
        // option section right after the launcher, no spawn cwd.
        let (argv, cwd) = launch_cwd(
            s(&["wsl.exe", "-d", "Ubuntu"]),
            Some("/mnt/c/Users/me/proj".into()),
        );
        assert_eq!(
            argv,
            s(&["wsl.exe", "--cd", "/mnt/c/Users/me/proj", "-d", "Ubuntu"])
        );
        assert_eq!(cwd, None);

        // WSL carrying a command after `--`. `--cd` MUST
        // land before the `--` separator so it reaches WSL, not the command.
        // Appending at the end (the old bug) put it after `bash -l`.
        let (argv, cwd) = launch_cwd(
            s(&["wsl.exe", "-d", "Ubuntu", "--", "bash", "-l"]),
            Some("/home/me/proj".into()),
        );
        assert_eq!(
            argv,
            s(&[
                "wsl.exe",
                "--cd",
                "/home/me/proj",
                "-d",
                "Ubuntu",
                "--",
                "bash",
                "-l"
            ])
        );
        assert_eq!(cwd, None);

        // WSL with a bare command positional (no `--`). `--cd`
        // still goes first so it isn't consumed as an argument to the command.
        let (argv, _) = launch_cwd(s(&["wsl.exe", "htop"]), Some("/home/me".into()));
        assert_eq!(argv, s(&["wsl.exe", "--cd", "/home/me", "htop"]));

        // WSL + no reported dir → unchanged argv, no spawn cwd.
        let (argv, cwd) = launch_cwd(s(&["wsl"]), None);
        assert_eq!(argv, s(&["wsl"]));
        assert_eq!(cwd, None);

        // WSL already specifying --cd → not double-injected.
        let (argv, _) = launch_cwd(
            s(&["wsl.exe", "--cd", "/home/me"]),
            Some("/mnt/c/other".into()),
        );
        assert_eq!(argv, s(&["wsl.exe", "--cd", "/home/me"]));
    }

    #[test]
    fn shell_kind_for_argv_classifies_families() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(shell_kind_for_argv(&s(&["wsl.exe"])), PaneShellKind::Posix);
        assert_eq!(
            shell_kind_for_argv(&s(&["wsl", "-d", "Ubuntu"])),
            PaneShellKind::Posix
        );
        assert_eq!(shell_kind_for_argv(&s(&["bash"])), PaneShellKind::Posix);
        assert_eq!(
            shell_kind_for_argv(&s(&[r"C:\Windows\System32\pwsh.exe"])),
            PaneShellKind::PowerShell
        );
        assert_eq!(
            shell_kind_for_argv(&s(&["powershell.exe"])),
            PaneShellKind::PowerShell
        );
        assert_eq!(shell_kind_for_argv(&s(&["cmd.exe"])), PaneShellKind::Cmd);
        // Unknown program → POSIX (portable default).
        assert_eq!(shell_kind_for_argv(&s(&["fish"])), PaneShellKind::Posix);
    }

    #[test]
    fn windows_path_to_wsl_translates_drive_and_unc() {
        use std::path::Path;
        assert_eq!(
            windows_path_to_wsl(Path::new(r"C:\Users\me\v.mp4")).as_deref(),
            Some("/mnt/c/Users/me/v.mp4")
        );
        // Lowercased drive, forward-slash input also accepted.
        assert_eq!(
            windows_path_to_wsl(Path::new("D:/data/x")).as_deref(),
            Some("/mnt/d/data/x")
        );
        // WSL UNC share → in-distro absolute path (distro component dropped).
        assert_eq!(
            windows_path_to_wsl(Path::new(r"\\wsl.localhost\Ubuntu\home\me\v.mp4")).as_deref(),
            Some("/home/me/v.mp4")
        );
        assert_eq!(
            windows_path_to_wsl(Path::new(r"\\wsl$\Debian\etc\hosts")).as_deref(),
            Some("/etc/hosts")
        );
        // Already-POSIX / unrecognized → None (caller keeps the original).
        assert_eq!(windows_path_to_wsl(Path::new("/home/me/v.mp4")), None);
    }

    #[test]
    fn quote_path_for_escapes_per_shell() {
        // POSIX: wrap in single quotes, embedded ' → '\''
        assert_eq!(
            quote_path_for(PaneShellKind::Posix, "/foo bar/baz.txt"),
            "'/foo bar/baz.txt'"
        );
        assert_eq!(
            quote_path_for(PaneShellKind::Posix, "/foo'bar"),
            r"'/foo'\''bar'"
        );
        // PowerShell: single quotes, embedded ' doubled to ''
        assert_eq!(
            quote_path_for(PaneShellKind::PowerShell, "/foo'bar"),
            "'/foo''bar'"
        );
        // cmd: double quotes (paths can't contain ").
        assert_eq!(
            quote_path_for(PaneShellKind::Cmd, r"C:\a b\c.txt"),
            "\"C:\\a b\\c.txt\""
        );
        // Multibyte survives, still quoted.
        assert_eq!(
            quote_path_for(PaneShellKind::Posix, "/路径/file.txt"),
            "'/路径/file.txt'"
        );
    }

    #[test]
    fn format_paths_for_paste_translates_and_quotes_by_pane() {
        use std::path::PathBuf;
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // WSL pane: Windows path → /mnt/c and POSIX-quoted.
        assert_eq!(
            format_paths_for_paste(&s(&["wsl.exe"]), &[PathBuf::from(r"C:\Users\me\clip.mp4")]),
            "'/mnt/c/Users/me/clip.mp4'"
        );
        // Native PowerShell pane: no translation, PowerShell quoting.
        assert_eq!(
            format_paths_for_paste(&s(&["pwsh.exe"]), &[PathBuf::from(r"C:\Users\me\clip.mp4")]),
            "'C:\\Users\\me\\clip.mp4'"
        );
        // Multiple paths (CF_HDROP multi-select) → space-joined.
        assert_eq!(
            format_paths_for_paste(
                &s(&["bash"]),
                &[PathBuf::from("/a/one.txt"), PathBuf::from("/a/two.txt")]
            ),
            "'/a/one.txt' '/a/two.txt'"
        );
    }

    #[test]
    fn path_paste_fanout_formats_every_target_shell_independently() {
        use std::path::PathBuf;

        let powershell = vec!["pwsh.exe".to_string()];
        let wsl = vec!["wsl.exe".to_string()];
        let formatted = format_paths_for_targets(
            [(10, powershell.as_slice()), (20, wsl.as_slice())],
            &[PathBuf::from(r"C:\Users\me\a b.txt")],
            true,
        );
        assert_eq!(
            formatted,
            [
                (10, "'C:\\Users\\me\\a b.txt' ".to_string()),
                (20, "'/mnt/c/Users/me/a b.txt' ".to_string()),
            ]
        );
    }

    #[test]
    fn usable_cwd_keeps_only_existing_dirs() {
        // An existing directory is kept (new tab/split opens here).
        assert_eq!(usable_cwd(Some("/".to_string())), Some("/".to_string()));
        let tmp = std::env::temp_dir();
        assert_eq!(
            usable_cwd(Some(tmp.to_string_lossy().into_owned())),
            Some(tmp.to_string_lossy().into_owned())
        );
        // A since-deleted path or a file → fall back to the default.
        assert_eq!(usable_cwd(Some("/no/such/kettle/xyz".to_string())), None);
        assert_eq!(usable_cwd(None), None);
    }

    #[test]
    fn split_layout_tiles_without_gaps_or_overlap() {
        let mut n = Node::Leaf(1);
        assert!(n.split_leaf(1, 2, Dir::Horizontal));
        let mut rects = Vec::new();
        n.layout((0.0, 0.0, 100.0, 40.0), &mut rects);
        assert_eq!(rects.len(), 2);
        let (_, a) = rects[0];
        let (_, b) = rects[1];
        assert_eq!(a.2 + b.2, 100.0); // widths sum to full
        assert_eq!(a.0, 0.0);
        assert_eq!(b.0, a.2); // b starts where a ends
        assert_eq!(a.3, 40.0);
    }

    #[test]
    fn remove_leaf_collapses_parent() {
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, Dir::Vertical);
        assert!(n.contains(2));
        // Removing one child of a 2-leaf split collapses to the sibling,
        // signalled as `Err(Some(sibling))` to the parent.
        match n.remove_leaf(2) {
            Err(Some(Node::Leaf(1))) => {}
            _ => panic!("removing one child should collapse to the sibling"),
        }
    }

    #[test]
    fn leaf_ids_walks_dfs_order() {
        // Same DFS-order traversal that nth_leaf / leaf_index_of /
        // session-save use, so any caller switching between these
        // helpers gets a consistent enumeration. Used by broadcast_write
        // to scope broadcast input to a single tab.
        let single = Node::Leaf(7);
        assert_eq!(single.leaf_ids(), vec![7]);
        // Build:  Split(a=Leaf(1), b=Split(a=Leaf(2), b=Leaf(3)))
        // DFS:    [1, 2, 3]
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal);
        n.split_leaf(2, 3, Dir::Vertical);
        assert_eq!(n.leaf_ids(), vec![1, 2, 3]);
        // Symmetric with nth_leaf for the same positions.
        for (i, id) in n.leaf_ids().iter().enumerate() {
            assert_eq!(n.nth_leaf(i), *id);
        }
    }

    #[test]
    fn nested_splits_keep_all_leaves() {
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal);
        n.split_leaf(2, 3, Dir::Vertical);
        let mut rects = Vec::new();
        n.layout((0.0, 0.0, 200.0, 100.0), &mut rects);
        let ids: Vec<u64> = rects.iter().map(|(i, _)| *i).collect();
        assert!(ids.contains(&1) && ids.contains(&2) && ids.contains(&3));
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn move_active_tab_relocates_and_clamps() {
        // Build a 4-tab mux without spawning real terminals; use the leaf
        // ids as a fingerprint so we can verify the WHOLE bar, not just the
        // tab that moved.
        //
        // Asserting only the dragged tab is what let the `swap` bug ship:
        // the old test checked the moved tab's new slot and never looked at
        // the others, so it stayed green under both semantics. Every case
        // below compares the entire order.
        let mut m = Mux::new();
        for id in 1..=4u64 {
            m.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        let order = |m: &Mux| -> Vec<u64> {
            m.tabs
                .iter()
                .map(|tab| match tab.root {
                    Node::Leaf(id) => id,
                    _ => u64::MAX,
                })
                .collect()
        };
        assert_eq!(order(&m), vec![1, 2, 3, 4]);

        // One place right is the case where relocating and swapping agree.
        m.active = 1;
        assert!(m.move_active_tab(1));
        assert_eq!(m.active, 2);
        assert_eq!(order(&m), vec![1, 3, 2, 4]);

        // MORE than one place is where they diverge, and it is what
        // drag-to-reorder actually produces: the handler passes
        // `target_index - active`, one coalesced mouse move can cross several
        // segments, and an overshoot clamps to the last one. Everything the
        // dragged tab passes slides back by one; nothing teleports.
        m.active = 0;
        assert!(m.move_active_tab(3));
        assert_eq!(m.active, 3);
        assert_eq!(
            order(&m),
            vec![3, 2, 4, 1],
            "a multi-step move must slide the passed tabs, not swap the ends"
        );

        // Clamps past the right edge, and reports no-ops honestly.
        assert!(!m.move_active_tab(0));
        assert!(!m.move_active_tab(5));
        assert_eq!(m.active, 3);
        assert_eq!(order(&m), vec![3, 2, 4, 1]);

        // Move left clamps at 0, again sliding rather than swapping.
        assert!(m.move_active_tab(-100));
        assert_eq!(m.active, 0);
        assert_eq!(order(&m), vec![1, 3, 2, 4]);
        // With < 2 tabs the move is a no-op (clamp still leaves us put).
        let mut single = Mux::new();
        single.tabs.push(Tab {
            root: Node::Leaf(1),
            focus: 1,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        assert!(!single.move_active_tab(1));
    }

    /// The keys wrap, the drag clamps. Both behaviours are wanted, which is why
    /// they are two entry points rather than one — a `move_tab_right` on the
    /// last tab that does nothing feels broken (Terminator brings it round to
    /// the front), while a *drag* that wrapped would fling the tab across the
    /// bar the instant the cursor overshot the last segment.
    #[test]
    fn the_tab_move_keys_wrap_where_a_drag_clamps() {
        let mut m = Mux::new();
        for id in 1..=4u64 {
            m.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        let order = |m: &Mux| -> Vec<u64> {
            m.tabs
                .iter()
                .map(|tab| match tab.root {
                    Node::Leaf(id) => id,
                    _ => u64::MAX,
                })
                .collect()
        };

        // Right from the last tab comes round to the front, taking focus along.
        m.active = 3;
        assert!(m.nudge_active_tab(1));
        assert_eq!(m.active, 0);
        assert_eq!(order(&m), vec![4, 1, 2, 3]);

        // Left from the first goes to the end, and undoes the wrap exactly.
        assert!(m.nudge_active_tab(-1));
        assert_eq!(m.active, 3);
        assert_eq!(order(&m), vec![1, 2, 3, 4]);

        // The drag path over the same edge does not wrap.
        m.active = 3;
        assert!(!m.move_active_tab(1));
        assert_eq!(order(&m), vec![1, 2, 3, 4]);
        m.active = 0;
        assert!(!m.move_active_tab(-1));
        assert_eq!(order(&m), vec![1, 2, 3, 4]);

        // A lone tab has nowhere to wrap to.
        let mut single = Mux::new();
        single.tabs.push(Tab {
            root: Node::Leaf(1),
            focus: 1,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });
        assert!(!single.nudge_active_tab(1));
        assert!(!single.nudge_active_tab(-1));
    }

    #[test]
    fn close_tab_at_keeps_active_valid() {
        // Build a 3-tab mux without spawning real terminals.
        let mut m = Mux::new();
        for id in 1..=3u64 {
            m.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        m.active = 2; // third tab
        // Close the first tab → active shifts left to stay on the same tab.
        assert!(!m.close_tab_at(0));
        assert_eq!(m.tabs.len(), 2);
        assert_eq!(m.active, 1);
        // Close the (now) last tab while it's active → clamps.
        m.active = 1;
        assert!(!m.close_tab_at(1));
        assert_eq!(m.active, 0);
        // Closing the final tab reports "empty".
        assert!(m.close_tab_at(0));
        assert!(m.tabs.is_empty());
    }

    /// The whole reason the `ask-before-closing` prompt names a tab by a pane
    /// instead of by an index: the prompt can sit on screen for as long as the
    /// user takes to answer, and a shell exiting in the meantime drops its tab
    /// and shifts every index after it down one. A remembered index would then
    /// resolve to somebody else's tab — confirming "close this tab" would close
    /// a different one.
    #[test]
    fn a_tab_anchor_outlives_the_tabs_being_renumbered() {
        let mut m = Mux::new();
        for id in 1..=3u64 {
            m.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }

        // Take an anchor on the third tab, the way the ✕ button does.
        let anchor = m.tab_anchor_panes(2);
        assert_eq!(anchor, vec![3], "the third tab's only pane");
        assert_eq!(m.tab_index_of_any_pane(&anchor), Some(2));

        // A shell exits in the first tab while the prompt is still up.
        assert!(!m.close_tab_at(0));
        // The raw index 2 is now out of bounds, but the anchor still finds the
        // tab the user actually pointed at.
        assert_eq!(
            m.tab_index_of_any_pane(&anchor),
            Some(1),
            "the anchored tab moved, it did not become a different tab"
        );

        // Now the anchored tab itself goes away before the answer arrives.
        assert!(!m.close_tab_at(1));
        assert_eq!(
            m.tab_index_of_any_pane(&anchor),
            None,
            "a tab that is already gone must resolve to nothing, never to a \
             surviving tab — pane ids are process-global and never reused, so \
             this cannot alias"
        );

        // Anchoring past the end is simply no tab.
        assert!(m.tab_anchor_panes(99).is_empty());
    }

    /// An `ask-before-closing` prompt names the pane it was raised for, and
    /// the answer must act on THAT pane.
    ///
    /// The prompt can sit on screen indefinitely. In that time the target's own
    /// shell can exit, which promotes a sibling into focus — so "close the
    /// focused pane" no longer means what it meant when the prompt went up.
    /// Confirming then closed the sibling, which can be a tmux or agent session
    /// the user never selected.
    #[test]
    fn a_confirmed_pane_close_acts_on_the_pane_it_was_raised_for() {
        let mut m = Mux::new();
        let mut root = Node::Leaf(10);
        assert!(root.split_leaf(10, 20, Dir::Horizontal));
        m.tabs.push(Tab {
            root,
            focus: 10,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });

        // The prompt was raised for pane 10; focus then moves to 20.
        m.tabs[0].focus = 20;
        assert_eq!(m.active_focus(), Some(20));

        // Re-focusing by id is what makes the confirmed close act on the
        // original target rather than on whatever is focused now — and the
        // close must then actually take pane 10, not its sibling. Asserting
        // only the refocus let an implementation that refocused 10 and closed
        // 20 pass.
        assert!(m.focus_pane(10), "the target is still present");
        assert_eq!(m.active_focus(), Some(10));
        assert!(!m.close_focused(), "the tab survives its sibling");
        assert!(
            !m.panes.contains_key(&10),
            "pane 10 — the prompt's target — is gone"
        );
        assert_eq!(
            m.active_focus(),
            Some(20),
            "and the sibling the user did NOT select is still here"
        );

        // A target that is already gone reports so, and the caller closes
        // nothing rather than falling back to the current focus.
        assert!(!m.focus_pane(999), "a vanished target must not resolve");
    }

    /// A split tab must be anchorable by any pane it holds, not just its first
    /// leaf — closing a pane inside the tab rewrites the tree and can change
    /// which leaf comes first.
    #[test]
    fn a_tab_anchor_resolves_from_any_pane_in_a_split() {
        let mut m = Mux::new();
        let mut root = Node::Leaf(10);
        assert!(root.split_leaf(10, 20, Dir::Horizontal));
        m.tabs.push(Tab {
            root,
            focus: 10,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });
        assert_eq!(m.tab_index_of_any_pane(&[10]), Some(0));
        assert_eq!(m.tab_index_of_any_pane(&[20]), Some(0), "the sibling too");
        assert_eq!(m.tab_index_of_any_pane(&[30]), None, "a pane in no tab");

        // The point of anchoring on the WHOLE tab: remove the first pane and
        // the anchor must still find the tab. A resolver that only consulted
        // `panes[0]` passed every assertion above and failed exactly here.
        let anchor = m.tab_anchor_panes(0);
        assert_eq!(anchor, vec![10, 20]);
        m.focus_pane(10);
        assert!(!m.close_focused(), "the tab survives");
        assert_eq!(
            m.tab_index_of_any_pane(&anchor),
            Some(0),
            "pane 20 still names the tab after pane 10 exited"
        );
        // The surviving pane is now the whole tab.
        assert_eq!(m.tab_anchor_panes(0), vec![20]);
    }

    #[test]
    fn close_focused_promotes_sibling_in_two_pane_split() {
        // Repro for the `Ctrl+Shift+E` then `Ctrl+Shift+W` regression:
        // `match Err(_)` used to conflate two distinct `Node::remove_leaf`
        // results — `Err(None)` (the focused leaf was the only one, close
        // the tab) and `Err(Some(sibling))` (the focused leaf had a
        // sibling, promote it). The wrong arm fired for the second case
        // and closed the whole tab on what should have been a per-pane
        // close. Pin the contract here so a future refactor that
        // re-conflates them fails CI rather than re-introducing the bug.
        let mut m = Mux::new();
        let mut root = Node::Leaf(10);
        assert!(root.split_leaf(10, 20, Dir::Horizontal));
        m.tabs.push(Tab {
            root,
            focus: 10,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        m.active = 0;
        // Close the focused (left) pane → tab survives with the right
        // pane promoted to root.
        assert!(!m.close_focused(), "tab should NOT be reported empty");
        assert_eq!(m.tabs.len(), 1, "tab should still exist");
        assert_eq!(m.active, 0);
        assert!(
            matches!(m.tabs[0].root, Node::Leaf(20)),
            "sibling (id=20) should be the new root after closing the focused leaf"
        );
        assert_eq!(
            m.tabs[0].focus, 20,
            "focus should move to the promoted sibling, not linger on the closed leaf"
        );
        // Closing the now-last pane drains the tab.
        assert!(m.close_focused(), "last-pane close should report empty");
        assert!(m.tabs.is_empty());
    }

    /// User-reported bug. When the user splits many times
    /// and then closes a pane deep in the tree, focus jumps back to
    /// the leftmost (first focused) pane instead of the deeper
    /// neighbor of the closed pane.
    ///
    /// Repro: build tree
    ///
    ///     Split{Horiz,
    ///         a: Leaf(10),
    ///         b: Split{Vert,
    ///             a: Leaf(20),
    ///             b: Split{Horiz,
    ///                 a: Leaf(30),
    ///                 b: Leaf(40)}}}
    ///
    /// User focuses Leaf(40) and closes it. Before the fix:
    /// `tab.root.first_leaf()` returns 10 (the leftmost of the WHOLE
    /// tree). Expected: focus moves to Leaf(30) — the immediate
    /// neighbor that took 40's slot in the deepest split.
    #[test]
    fn close_focused_picks_nearest_neighbor_not_leftmost_root() {
        let mut m = Mux::new();
        // Build the 4-leaf nested tree by hand (testing the Node logic
        // directly; bypasses the Pane/PTY infra which split_leaf would
        // touch in the full Mux::split flow).
        let root = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(10)),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(20)),
                b: Box::new(Node::Split {
                    dir: Dir::Horizontal,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(30)),
                    b: Box::new(Node::Leaf(40)),
                }),
            }),
        };
        m.tabs.push(Tab {
            root,
            focus: 40,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        m.active = 0;
        assert!(!m.close_focused(), "tab still has 3 panes after closing 40");
        assert_eq!(
            m.tabs[0].focus, 30,
            "focus must move to the *nearest neighbor* (30), not jump back \
             to the leftmost-leaf-of-the-tab (10) — that's the \
             focus-jumped-to-leftmost-leaf bug"
        );
    }

    /// `exit-action = hold` survival. Before the fix `reap` removed any
    /// child-exited pane, so Hold behaved like Close. `is_reapable` now keeps a
    /// held pane after its drained exit event until it is explicitly closed.
    #[test]
    fn is_reapable_waits_for_the_drained_exit_event_and_honors_hold() {
        use super::is_reapable;
        // Live pane (no drained exit event) — never reaped.
        assert!(!is_reapable(false, false, false));
        // Default (Close): drained exit observed, not held -> reaped.
        assert!(is_reapable(false, false, true));
        // Hold: exit observed but held -> NOT reaped (the fix above).
        assert!(!is_reapable(false, true, true));
        // Explicit close (ClosePane / Restart set `closed`) always reaps, even
        // a held pane — so the user can still dismiss a held dead shell.
        assert!(is_reapable(true, true, true));
        assert!(is_reapable(true, false, false));
    }

    #[test]
    fn reap_never_consumes_child_status_ahead_of_the_pty_exit_event() {
        let source = production_source();
        let body = source
            .split("pub fn reap(&mut self) -> bool {")
            .nth(1)
            .and_then(|rest| rest.split("\n    pub(crate) fn reap_tabs").next())
            .expect("Mux::reap body");
        assert!(
            body.contains("p.exit_observed") && !body.contains("child_exited()"),
            "Mux::reap must wait for the drained PTY exit event instead of \
             consuming process status before final output and exit policy"
        );
    }

    #[test]
    fn interactive_panes_opt_into_direct_child_observation() {
        assert!(
            production_source().contains("observe_child_exit: true"),
            "only the windowed pane constructor should request the independent child-exit edge"
        );
    }

    #[test]
    fn held_panes_retry_status_collection_only_after_ordered_exit() {
        let source = production_source();
        let body = source
            .split("pub fn poll_held_child_statuses(&mut self) -> bool {")
            .nth(1)
            .and_then(|rest| rest.split("\n    pub(crate) fn reap_tabs").next())
            .expect("held-child status poll body");
        assert!(
            body.contains("pane.held && pane.exit_observed && !pane.held_child_reaped")
                && body.contains("pane.term.child_exit_code().is_some()"),
            "Hold must collect a status that lagged PTY EOF, but only after the ordered exit event"
        );
    }

    /// Companion to the close_focused neighbor-promotion fix —
    /// the PTY-died-while-focused path through `reap_tabs` had
    /// the same `tab.root.first_leaf()` anti-pattern. When the
    /// user runs `exit` in the focused pane (or its process
    /// crashes), focus should land on the immediate neighbor,
    /// not jump back to the leftmost leaf of the whole tab.
    ///
    /// Same 4-leaf tree as
    /// `close_focused_picks_nearest_neighbor_not_leftmost_root`'s
    /// test: focus = 40, reap dead leaf 40. Before the fix: focus = 10
    /// (leftmost). Post-fix: focus = 30 (the immediate neighbor of 40).
    #[test]
    fn reap_tabs_promotes_neighbor_when_focused_pane_dies() {
        let mut tabs = vec![Tab {
            root: Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(10)),
                b: Box::new(Node::Split {
                    dir: Dir::Vertical,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(20)),
                    b: Box::new(Node::Split {
                        dir: Dir::Horizontal,
                        ratio: 0.5,
                        a: Box::new(Node::Leaf(30)),
                        b: Box::new(Node::Leaf(40)),
                    }),
                }),
            },
            focus: 40,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        }];
        let mut active = 0;
        // Pane 40's PTY exits → reap it.
        Mux::reap_tabs(&mut tabs, &mut active, &[40]);
        assert_eq!(tabs.len(), 1, "tab survives with 3 panes");
        assert_eq!(
            tabs[0].focus, 30,
            "focus must move to the *nearest neighbor* (30), not jump back \
             to the leftmost-leaf-of-the-tab (10) — that's the \
             focus-jumped-to-leftmost-leaf bug"
        );
    }

    /// The EXISTING `reap_tabs` match arm
    /// conflated `Err(None)` (tab is empty) with `Err(Some(sibling))`
    /// (focused leaf was a direct child of root and the sibling
    /// was promoted). For a 2-pane tab where one pane's PTY exits,
    /// `remove_leaf` returns `Err(Some(surviving_sibling))` — and
    /// the pre-fix `Err(_) => tabs.remove(ti)` arm then deleted
    /// the WHOLE tab, losing the surviving sibling along with it.
    ///
    /// Latent bug surfaced by the broader audit of `reap_tabs`: any
    /// 2-pane tab + `exit` in either pane = both panes vanish.
    /// Reachable in production after `Mux::reap` consumes the pane's drained
    /// PTY exit event.
    #[test]
    fn reap_tabs_preserves_tab_when_2_pane_split_has_one_pane_exit() {
        let mut tabs = vec![Tab {
            root: Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(10)),
                b: Box::new(Node::Leaf(20)),
            },
            focus: 10,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        }];
        let mut active = 0;
        // Pane 20's PTY exits. Pre-fix: tab is removed — the
        // surviving Leaf(10) goes with it. Post-fix: tab survives
        // with root collapsed to Leaf(10).
        Mux::reap_tabs(&mut tabs, &mut active, &[20]);
        assert_eq!(
            tabs.len(),
            1,
            "tab must survive a 2-pane sibling promotion (pre-fix this \
             was 0 — `Err(_) => tabs.remove(ti)` ate the surviving pane)"
        );
        assert!(matches!(tabs[0].root, Node::Leaf(10)));
        assert_eq!(tabs[0].focus, 10);
    }

    /// Negative case: if the dying pane is NOT the
    /// focused one, focus must stay put — the existing
    /// `contains(focus)` guard already covers this, so this test
    /// catches a regression where the neighbor-capture logic in
    /// `reap_tabs` accidentally triggers for non-focused dyings.
    #[test]
    fn reap_tabs_keeps_focus_when_dying_pane_is_not_focused() {
        let mut tabs = vec![Tab {
            root: Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(10)),
                b: Box::new(Node::Leaf(20)),
            },
            // Focus is on 10; pane 20's PTY dies.
            focus: 10,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        }];
        let mut active = 0;
        Mux::reap_tabs(&mut tabs, &mut active, &[20]);
        assert_eq!(
            tabs[0].focus, 10,
            "focus on 10 must survive — pane 20's death shouldn't move focus"
        );
    }

    /// Companion to the test above. The `neighbor_of`
    /// helper drives the focus-restoration. Asserts the contract
    /// directly so a future refactor of `close_focused` that
    /// stops calling `neighbor_of` (or breaks the helper) fails
    /// the gauntlet rather than re-introducing the user-reported
    /// bug.
    #[test]
    fn node_neighbor_of_finds_sibling_subtree_first_leaf() {
        // Same shape as the close-focused repro above.
        let root = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(10)),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(20)),
                b: Box::new(Node::Split {
                    dir: Dir::Horizontal,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(30)),
                    b: Box::new(Node::Leaf(40)),
                }),
            }),
        };
        // Neighbor of the deepest right leaf is its split-mate (30).
        assert_eq!(root.neighbor_of(40), Some(30));
        // Neighbor of 30 is 40 (the other side of the deepest split).
        assert_eq!(root.neighbor_of(30), Some(40));
        // Neighbor of 20 is the first leaf of its sibling subtree
        // (Split{30, 40}) → 30.
        assert_eq!(root.neighbor_of(20), Some(30));
        // Neighbor of 10 is the first leaf of its sibling subtree
        // (the deeper Split) → 20.
        assert_eq!(root.neighbor_of(10), Some(20));
        // Leaf id not in the tree → None.
        assert_eq!(root.neighbor_of(999), None);
        // Single-leaf tree → no neighbor.
        let lonely = Node::Leaf(1);
        assert_eq!(lonely.neighbor_of(1), None);
    }

    #[test]
    fn classify_tab_activity_picks_the_right_indicator() {
        use std::time::{Duration, Instant};
        let base = Instant::now();
        let earlier = base;
        let now = base + Duration::from_secs(5);
        let later = base + Duration::from_secs(10);
        // Default 10 s silence threshold matches the config default;
        // the existing transitions still fire under it.
        let silence = Duration::from_secs(10);

        // Active tab → always Normal, regardless of output / bell. The
        // focused-tab accent + window-title already telegraph "you're
        // here" so adding a dot would be redundant.
        assert_eq!(
            classify_tab_activity(true, true, Some(later), Some(earlier), now, silence),
            TabActivity::Normal
        );
        assert_eq!(
            classify_tab_activity(true, false, Some(later), Some(earlier), now, silence),
            TabActivity::Normal
        );

        // Inactive tab + bell → Bell, regardless of output state.
        // Bell is the stronger signal (the focused program explicitly
        // asked for attention) so it wins over plain output activity.
        assert_eq!(
            classify_tab_activity(false, true, None, None, now, silence),
            TabActivity::Bell
        );
        assert_eq!(
            classify_tab_activity(false, true, Some(later), Some(earlier), now, silence),
            TabActivity::Bell
        );

        // Inactive tab + output after last-seen → Output (fresh, hasn't
        // exceeded silence threshold yet).
        assert_eq!(
            classify_tab_activity(false, false, Some(later), Some(earlier), now, silence),
            TabActivity::Output
        );

        // Inactive tab + output BEFORE the user last looked → Normal.
        // The user already saw this output; no need to nudge again.
        assert_eq!(
            classify_tab_activity(false, false, Some(earlier), Some(later), now, silence),
            TabActivity::Normal
        );

        // First-output edge: no last_seen_at yet → Output (the user
        // has never been on this tab and something happened on it).
        assert_eq!(
            classify_tab_activity(false, false, Some(later), None, now, silence),
            TabActivity::Output
        );

        // No activity recorded at all → Normal.
        assert_eq!(
            classify_tab_activity(false, false, None, None, now, silence),
            TabActivity::Normal
        );
        assert_eq!(
            classify_tab_activity(false, false, None, Some(earlier), now, silence),
            TabActivity::Normal
        );
    }

    #[test]
    fn classify_tab_activity_transitions_to_silent_after_threshold() {
        // Output → Silent transition once the last unseen
        // output is older than the silence threshold. The test fakes
        // a clock by passing `now` explicitly — same trick the
        // primary classifier test uses, keeping the function pure.
        use std::time::{Duration, Instant};
        let base = Instant::now();
        let silence = Duration::from_secs(10);
        // Tab last looked at 60 s ago; output arrived at 30 s ago
        // (so unseen — output > seen).
        let last_seen = base;
        let last_out = base + Duration::from_secs(30);
        // Just-after-output: 5 s elapsed since output, below the 10 s
        // threshold → Output.
        let now_fresh = last_out + Duration::from_secs(5);
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_fresh,
                silence
            ),
            TabActivity::Output,
            "5 s after output should still be Output (threshold = 10 s)"
        );
        // Exactly at threshold: 10 s elapsed → Silent (the `>=` arm).
        let now_at_threshold = last_out + silence;
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_at_threshold,
                silence
            ),
            TabActivity::Silent,
            "elapsed = threshold should be Silent (inclusive boundary)"
        );
        // Well past threshold: 30 s elapsed → Silent.
        let now_late = last_out + Duration::from_secs(30);
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_late,
                silence
            ),
            TabActivity::Silent
        );
        // Bell still beats Silent — explicit attention wins over
        // implicit "things stopped" signal.
        assert_eq!(
            classify_tab_activity(
                false,
                true,
                Some(last_out),
                Some(last_seen),
                now_late,
                silence
            ),
            TabActivity::Bell
        );
        // Backward clock (now < last_out — should only happen with a
        // bug or clock-skew adjustment between calls): treat as fresh
        // Output rather than triggering Silent on a saturating-zero
        // duration.
        let now_before = base + Duration::from_secs(29);
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_before,
                silence
            ),
            TabActivity::Output,
            "backward clock should NOT trigger Silent"
        );
    }

    #[test]
    fn closed_tab_ring_bounded_and_lifo() {
        // The snapshot ring is bounded at `CLOSED_TAB_RING_CAP`
        // and pops LIFO (most-recent first). Builds a fake ring
        // directly so we don't need to spawn real PTYs.
        let mut m = Mux::new();
        for i in 0..(super::CLOSED_TAB_RING_CAP + 3) {
            if m.closed_tabs.len() >= super::CLOSED_TAB_RING_CAP {
                m.closed_tabs.pop_front();
            }
            m.closed_tabs.push_back(super::ClosedTab {
                original_index: i,
                argv: vec![format!("argv-{i}")],
                cwd: Some(format!("/tmp/{i}")),
            });
        }
        // Cap honored: oldest 3 entries fell off the front.
        assert_eq!(m.closed_tabs.len(), super::CLOSED_TAB_RING_CAP);
        assert_eq!(m.closed_tabs.front().unwrap().original_index, 3);
        assert_eq!(
            m.closed_tabs.back().unwrap().original_index,
            super::CLOSED_TAB_RING_CAP + 2
        );
        // LIFO: pop_back gives the most-recently-closed snapshot.
        let last = m.closed_tabs.pop_back().unwrap();
        assert_eq!(last.original_index, super::CLOSED_TAB_RING_CAP + 2);
        assert_eq!(
            last.argv,
            vec![format!("argv-{}", super::CLOSED_TAB_RING_CAP + 2)]
        );
    }

    #[test]
    fn reap_tabs_keeps_active_pointed_at_the_same_tab() {
        // `reap` used to handle only the "active
        // tab was the last one and the list shrunk" case via the
        // trailing clamp, missing the much more common "a tab BEFORE
        // active died" case which silently shifted what `active`
        // pointed to. Each scenario builds a fresh `tabs` Vec where
        // we can recognize each tab by its single leaf id, then
        // calls `reap_tabs` with the dead set and asserts which
        // leaf id `active` now indexes.
        fn tab(id: u64) -> Tab {
            Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            }
        }
        // Scenario 1 (the active-index-drift bug described above): focused on the middle tab
        // (B); the leftmost tab (A) dies. Pre-fix: active stayed 1
        // and now indexed C — focus silently jumped past B. Post-
        // fix: active decrements to 0 so it still points at B.
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 1; // B
        Mux::reap_tabs(&mut tabs, &mut active, &[1]); // A dies
        assert_eq!(tabs.len(), 2);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 2, "still focused on B"),
            _ => panic!("expected leaf"),
        }
        // Scenario 2: focused on the rightmost (C); leftmost (A) dies.
        // Pre-fix: trailing-clamp didn't fire (active was still in
        // bounds), so active=2 became C's new neighbor — wrong.
        // Post-fix: decrements 2→1, still C.
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 2;
        Mux::reap_tabs(&mut tabs, &mut active, &[1]);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 3, "still focused on C"),
            _ => panic!("expected leaf"),
        }
        // Scenario 3: the active tab itself dies. Focus should fall
        // on its right neighbor (matches every modern terminal's
        // close-current-tab behavior).
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 1; // B
        Mux::reap_tabs(&mut tabs, &mut active, &[2]); // B dies
        assert_eq!(tabs.len(), 2);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 3, "active falls on right neighbor"),
            _ => panic!("expected leaf"),
        }
        // Scenario 4: active is the LAST tab and dies — trailing-clamp
        // brings active back to the new last tab (the existing
        // behavior; regression guard).
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 2;
        Mux::reap_tabs(&mut tabs, &mut active, &[3]);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 2, "active clamped to new last"),
            _ => panic!("expected leaf"),
        }
        // Scenario 5: multiple dead. focused on C (index 2); A and B
        // both die.
        let mut tabs = vec![tab(1), tab(2), tab(3), tab(4)];
        let mut active = 2; // C
        Mux::reap_tabs(&mut tabs, &mut active, &[1, 2]); // A + B die
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 3, "still focused on C"),
            _ => panic!("expected leaf"),
        }
    }

    #[test]
    fn close_window_drops_every_tab_and_pane() {
        // close_window is *not* an alias for close_tab.
        // Build a multi-tab, multi-pane mux and verify everything is
        // gone after close_window, including the active index reset.
        let mut m = Mux::new();
        for id in 1..=3u64 {
            // Each tab is a 2-pane split so we also confirm both
            // panes-per-tab get reaped (not just the focused leaf).
            let mut root = Node::Leaf(id * 10);
            root.split_leaf(id * 10, id * 10 + 1, Dir::Horizontal);
            m.tabs.push(Tab {
                root,
                focus: id * 10,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        m.active = 1;
        // Sanity: pre-state has tabs (panes map is empty in this test
        // because we didn't spawn real Pane records — we only need to
        // observe the tab + active-index reset).
        assert_eq!(m.tabs.len(), 3);
        assert_eq!(m.active, 1);

        let empty = m.close_window();
        assert!(empty, "close_window always reports the mux empty");
        assert!(m.tabs.is_empty(), "all tabs gone");
        assert!(m.panes.is_empty(), "all panes gone");
        assert_eq!(m.active, 0, "active reset to 0");
    }

    #[test]
    fn insert_split_exits_zoom_and_focuses_new_pane() {
        // With a single-leaf tab zoomed (one pane
        // visible), splitting should produce a 2-leaf tab, focus the
        // new pane, and exit zoom so the user sees both halves —
        // matching tmux / WezTerm. Before this fix, zoom stayed on and the
        // old half silently hid.
        let mut tab = Tab {
            root: Node::Leaf(1),
            focus: 1,
            zoomed: true, // already zoomed before the split
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        super::insert_split(&mut tab, 2, Dir::Horizontal);
        assert_eq!(tab.focus, 2, "focus moves to the new pane");
        assert!(!tab.zoomed, "zoom is exited so both halves render");
        // Tree now contains both leaves.
        let mut rects = Vec::new();
        tab.root.layout((0.0, 0.0, 100.0, 50.0), &mut rects);
        assert_eq!(rects.len(), 2, "split produced two leaves");

        // Unzoomed → unzoomed (no-op on the zoom flag).
        let mut tab = Tab {
            root: Node::Leaf(1),
            focus: 1,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        super::insert_split(&mut tab, 2, Dir::Vertical);
        assert!(!tab.zoomed);
        assert_eq!(tab.focus, 2);
    }

    /// The stale-focus retry. When `tab.focus`
    /// points at a leaf NOT in the tree (a focus-desync), `split_leaf` no-ops on
    /// the stale id; `insert_split` must repair focus to `first_leaf()`, retry,
    /// graft the new pane, and return true — instead of the old silent no-op that
    /// orphaned the just-spawned pane (a leaked PTY). The existing test always
    /// has focus on a valid leaf, so it never exercised this branch.
    #[test]
    fn insert_split_repairs_stale_focus_and_grafts() {
        let mut tab = Tab {
            root: Node::Leaf(1),
            focus: 99, // stale: not a leaf in the tree
            zoomed: true,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        assert!(
            super::insert_split(&mut tab, 2, Dir::Horizontal),
            "stale focus must be repaired + the split grafted (returns true)"
        );
        assert_eq!(tab.focus, 2, "focus moves to the newly-grafted pane");
        assert!(!tab.zoomed, "zoom exited");
        let mut rects = Vec::new();
        tab.root.layout((0.0, 0.0, 100.0, 50.0), &mut rects);
        let ids: Vec<u64> = rects.iter().map(|(id, _)| *id).collect();
        assert!(
            ids.contains(&1) && ids.contains(&2),
            "both leaves present: {ids:?}"
        );
    }

    /// Drift guard: every split caller that may graft a
    /// freshly-spawned pane must REAP it (`self.panes.remove(&new_id)`) if the
    /// graft fails, instead of leaking the PTY/child. There are three such
    /// callers (`split`, `split_with`, `duplicate_focused_pane`); `>= 3` lets a
    /// future fourth variant be added without silently skipping the reap (it
    /// would have to add the reap to keep the count, or fail this guard).
    #[test]
    fn split_callers_reap_orphaned_pane_on_graft_failure() {
        // Counted over production only. Searching the whole file counted this
        // test's own literal as a fourth site, so the guard was one short of
        // what it claimed to require.
        let src = production_source();
        let reaps = src.matches("self.panes.remove(&new_id)").count();
        assert!(
            reaps >= 3,
            "expected >= 3 orphan-reap sites (split / split_with / duplicate_focused_pane); found {reaps}"
        );
    }

    #[test]
    fn zoom_collapses_layout_to_focused_pane() {
        let mut m = Mux::new();
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Dir::Horizontal);
        m.tabs.push(Tab {
            root,
            focus: 2,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        m.active = 0;
        assert_eq!(m.layout(0, (0.0, 0.0, 100.0, 50.0)).len(), 2);
        m.toggle_zoom();
        let z = m.layout(0, (0.0, 0.0, 100.0, 50.0));
        assert_eq!(z.len(), 1);
        assert_eq!(z[0], (2, (0.0, 0.0, 100.0, 50.0)));
        m.toggle_zoom();
        assert_eq!(m.layout(0, (0.0, 0.0, 100.0, 50.0)).len(), 2);
    }

    #[test]
    fn serialize_tab_handles_out_of_range_idx() {
        // Drift guard. Out-of-range index returns
        // None without panic.
        let m = Mux::new();
        assert!(m.serialize_tab(0).is_none());
        assert!(m.serialize_tab(99).is_none());
    }

    #[test]
    fn mux_new_starts_with_broadcast_off() {
        // Drift guard. A fresh Mux MUST start with
        // broadcast disabled. An earlier bug seeded broadcast=true
        // from `broadcast_default = group` (the default), so every
        // kettle window started broadcasting input across all panes
        // in the active tab — users typing in one pane saw the
        // input mirrored everywhere.
        //
        // The fix removed the bad seeding in App::new;
        // this guard pins the Mux::new contract so a future App-
        // side re-introduction of broadcast-on-startup gets caught
        // by the App-side construction path being out of sync with
        // this baseline.
        let m = Mux::new();
        assert!(
            !m.is_broadcast_on(),
            "Mux::new must start with broadcast disabled; \
             enabling at startup mirrors keystrokes across panes \
             without the user opting in"
        );
        assert_eq!(m.broadcast, BroadcastScope::Off);
    }

    #[test]
    fn extract_and_insert_tab_roundtrip() {
        // Drift guard. extract_tab → insert_tab
        // reproduces the same tab + the active idx tracks
        // correctly across the operation.
        let mut m = Mux::new();
        let mk = |id: u64| Tab {
            root: Node::Leaf(id),
            focus: id,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        };
        m.tabs.push(mk(1));
        m.tabs.push(mk(2));
        m.tabs.push(mk(3));
        m.active = 2; // focus on tab 3.
        let extracted = m.extract_tab(1).expect("extract 1");
        // Tab 2 removed; remaining tabs are [1, 3].
        assert_eq!(m.tabs.len(), 2);
        // active=2 was past the removed idx; clamped to 1.
        assert_eq!(m.active, 1);
        // Insert the extracted tab back at the head.
        m.insert_tab(0, extracted);
        // Tabs are now [2, 1, 3]; active=0 (insert_tab sets
        // active to the new position so the moved tab is focused).
        assert_eq!(m.tabs.len(), 3);
        assert_eq!(m.active, 0);
        // Out-of-range extract returns None.
        assert!(m.extract_tab(99).is_none());
        // Out-of-range insert clamps to end.
        m.insert_tab(99, mk(4));
        assert_eq!(m.tabs.len(), 4);
        assert_eq!(m.active, 3);
    }

    #[test]
    fn detach_attach_tab_moves_between_muxes() {
        // C2 (multi-window) drift guard: detach_tab → attach_tab moves a tab
        // from one Mux to another with the same index semantics as the
        // extract/insert pair it composes, does NOT snapshot to closed_tabs
        // (the tab is moving, not closing), and the source's active index
        // stays valid.
        let mk = |id: u64| Tab {
            root: Node::Leaf(id),
            focus: id,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        };
        let mut src = Mux::new();
        src.tabs.push(mk(101));
        src.tabs.push(mk(102));
        src.tabs.push(mk(103));
        src.active = 1; // detach the active tab itself
        let dt = src.detach_tab(1).expect("detach");
        assert_eq!(dt.tab.focus, 102);
        // Panes vec is empty here (no real PTYs in this fixture) — the pane
        // transfer itself is exercised by the C5 live-move e2e.
        assert!(dt.panes.is_empty());
        assert_eq!(src.tabs.len(), 2);
        // Removing the active tab keeps focus position (right neighbor
        // slides in), clamped — extract_tab semantics.
        assert_eq!(src.active, 1);
        assert!(
            src.closed_tabs.is_empty(),
            "a moved tab must not appear in the undo-close ring"
        );

        let mut dst = Mux::new();
        dst.tabs.push(mk(201));
        let at = dst.attach_tab(dt, None);
        assert_eq!(at, 1, "None appends");
        assert_eq!(dst.tabs.len(), 2);
        assert_eq!(dst.active, 1, "the attached tab becomes active");
        assert_eq!(dst.tabs[1].focus, 102);

        // Out-of-range detach is None; attach at an oversized index clamps.
        assert!(src.detach_tab(99).is_none());
        let dt2 = src.detach_tab(0).expect("detach head");
        let at2 = dst.attach_tab(dt2, Some(99));
        assert_eq!(at2, 2, "oversized attach index clamps to append");
    }

    #[test]
    fn pane_id_allocator_is_process_global() {
        // C2 drift guard: pane ids come from the shared NEXT_PANE_ID static
        // (never a per-Mux counter), so ids stay unique across every window's
        // Mux — the agent API, Lua hooks, and pending_runs address panes by
        // bare id, and a live tab move carries ids into another Mux. If this
        // stops compiling because NEXT_PANE_ID is gone, per-Mux ids came back.
        let a = NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let b = NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(b > a, "monotonic process-wide allocation");
    }

    #[test]
    fn completion_capability_overrides_a_spoofed_user_env_value() {
        let mut config = Config::default();
        config
            .env
            .push(("KETTLE_COMPLETION_OVERLAY".to_string(), "stale".to_string()));
        let environment = super::pane_environment(&config);
        assert_eq!(environment.last().unwrap().1, "1");

        config.completion_overlay = kettle_config::CompletionOverlayMode::Off;
        let environment = super::pane_environment(&config);
        assert_eq!(environment.last().unwrap().1, "0");
    }

    #[test]
    fn completion_visibility_is_snapshotted_with_the_shell_capability() {
        let src = production_source();
        let spawn = src
            .split("self.panes.insert(")
            .nth(1)
            .and_then(|rest| rest.split("Ok(id)").next())
            .expect("pane insertion");
        assert!(
            spawn.contains("completion_overlay: cfg.completion_overlay"),
            "a config reload must not hide the card from a shell whose wrapper still owns Tab"
        );
    }
}
