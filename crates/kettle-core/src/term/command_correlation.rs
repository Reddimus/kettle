use std::sync::{Arc, Mutex};
use std::time::Instant;

use kettle_vt::{CommandShell, CommandToken, PromptKind};

use super::{CommandFinished, CorrelatedCommandFinished};

#[derive(Default)]
pub(super) struct ShellCapability(std::sync::atomic::AtomicU8);

const AWAITING_PROMPT: u8 = 0x80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandCorrelationCapability {
    Unavailable,
    /// A previously supported shell has not announced its next prompt yet.
    AwaitingPrompt,
    Supported(CommandShell),
}

impl ShellCapability {
    pub(super) fn announce(&self, prompt: PromptKind) {
        let code = match prompt {
            PromptKind::IntegratedPromptStart(shell) => shell as u8,
            PromptKind::CorrelationDisabledPromptStart => 0,
            PromptKind::PromptStart
                if self.get() == CommandCorrelationCapability::AwaitingPrompt =>
            {
                0
            }
            // Fish emits an unversioned A after our versioned prompt hook.
            // Preserve duplicates within that already announced prompt.
            _ => return,
        };
        self.0.store(code, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn command_started(&self) {
        if let CommandCorrelationCapability::Supported(shell) = self.get() {
            self.0.store(
                AWAITING_PROMPT | shell as u8,
                std::sync::atomic::Ordering::Release,
            );
        }
    }

    pub(super) fn get(&self) -> CommandCorrelationCapability {
        let code = self.0.load(std::sync::atomic::Ordering::Acquire);
        if code & AWAITING_PROMPT != 0 {
            CommandCorrelationCapability::AwaitingPrompt
        } else if let Some(shell) = CommandShell::from_code(code) {
            CommandCorrelationCapability::Supported(shell)
        } else {
            CommandCorrelationCapability::Unavailable
        }
    }
}

pub(super) enum CommandBoundary {
    Begin(CommandToken),
    End(CommandToken, u64, Option<i32>),
}

pub(super) struct CommandMarker {
    correlation: Arc<Mutex<CommandCorrelation>>,
    boundary: CommandBoundary,
    at: Instant,
}

impl CommandMarker {
    pub(super) fn record(
        correlation: &Arc<Mutex<CommandCorrelation>>,
        boundary: CommandBoundary,
        at: Instant,
        processor: &mut super::Processor,
        term: &super::SharedTerm,
        deferred: &mut super::DeferredGraphicsJournal,
        proxy: &crate::event::EventProxy,
    ) {
        let token = match boundary {
            CommandBoundary::Begin(token) | CommandBoundary::End(token, ..) => token,
        };
        if !correlation
            .lock()
            .is_ok_and(|state| state.expected == Some(token))
        {
            return;
        }
        let marker = Self {
            correlation: Arc::clone(correlation),
            boundary,
            at,
        };
        // Share the graphics journal's offsets and bounds. Both the begin
        // anchor and completion must follow text buffered by DEC 2026, while
        // duration still measures receipt of the shell's original markers.
        if processor.sync_timeout().sync_timeout().is_some() {
            deferred.defer_output(processor, super::DeferredOutput::Command(marker));
        } else if let Ok(term) = term.lock() {
            marker.apply(&term, proxy);
        }
    }

    pub(super) fn apply(
        self,
        term: &super::Term<crate::event::EventProxy>,
        proxy: &crate::event::EventProxy,
    ) {
        use alacritty_terminal::grid::Dimensions;
        match self.boundary {
            CommandBoundary::Begin(token) => {
                let grid = term.grid();
                let line = grid
                    .history_origin()
                    .saturating_add(grid.history_size() as u64)
                    .saturating_add(grid.cursor.point.line.0.max(0) as u64);
                if let Ok(mut state) = self.correlation.lock() {
                    state.start(token, self.at, line);
                }
            }
            CommandBoundary::End(token, generation, code) => {
                finish_command(
                    &self.correlation,
                    proxy,
                    Some(token),
                    generation,
                    code,
                    self.at,
                );
            }
        }
    }
}

#[derive(Default)]
pub(super) struct CommandCorrelation {
    expected: Option<CommandToken>,
    started: Option<(Instant, u64)>,
    finished: Vec<CorrelatedCommandFinished>,
}

impl CommandCorrelation {
    pub(super) fn arm(&mut self, token: CommandToken) {
        self.expected = Some(token);
        self.started = None;
    }

    pub(super) fn cancel(&mut self, token: CommandToken) {
        if self.expected == Some(token) {
            self.expected = None;
            self.started = None;
        }
        self.finished.retain(|event| event.token != token);
    }

    pub(super) fn start_line(&self, token: CommandToken) -> Option<u64> {
        if self.expected == Some(token) {
            self.started.map(|(_, line)| line)
        } else {
            self.finished
                .iter()
                .find(|event| event.token == token)
                .map(|event| event.start_line)
        }
    }

    pub(super) fn start(&mut self, token: CommandToken, at: Instant, line: u64) -> bool {
        if self.expected != Some(token) || self.started.is_some() {
            return false;
        }
        self.started = Some((at, line));
        true
    }

    /// Record only the expected token's completion, independently of legacy events.
    pub(super) fn finish(
        &mut self,
        token: Option<CommandToken>,
        generation: u64,
        exit_code: Option<i32>,
        at: Instant,
    ) -> bool {
        let Some(token) = token else {
            return false;
        };
        if self.expected != Some(token) {
            return false;
        }
        let Some((started, start_line)) = self.started.take() else {
            return false;
        };
        self.expected = None;
        if self.finished.len() == 32 {
            self.finished.remove(0);
        }
        self.finished.push(CorrelatedCommandFinished {
            token,
            start_line,
            command: CommandFinished {
                generation,
                exit_code,
                duration: at.saturating_duration_since(started),
            },
        });
        true
    }

    pub(super) fn drain(&mut self) -> Vec<CorrelatedCommandFinished> {
        std::mem::take(&mut self.finished)
    }
}

pub(super) fn finish_command(
    correlation: &std::sync::Mutex<CommandCorrelation>,
    proxy: &crate::event::EventProxy,
    token: Option<CommandToken>,
    generation: u64,
    exit_code: Option<i32>,
    at: Instant,
) -> bool {
    let finished = correlation
        .lock()
        .map(|mut state| state.finish(token, generation, exit_code, at))
        .unwrap_or(false);
    if finished {
        proxy.wake_owner();
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn token(byte: u8) -> CommandToken {
        CommandToken::parse(&[byte; 32]).unwrap()
    }

    #[test]
    fn explicit_withdrawal_survives_legacy_duplicate_prompt_marks_at_every_split() {
        for end in ["\x07", "\x1b\\"] {
            let wire = format!("\x1b]133;A;kettle=1;shell=fish{end}\x1b]133;A;click_events=1{end}");
            for split in 0..=wire.len() {
                let capability = ShellCapability::default();
                let mut extractor = kettle_vt::Extractor::new();
                let mut feed = |bytes: &[u8]| {
                    for chunk in extractor.feed(bytes) {
                        if let kettle_vt::Chunk::Prompt(prompt) = chunk {
                            capability.announce(prompt);
                        }
                    }
                };
                feed(&wire.as_bytes()[..split]);
                feed(&wire.as_bytes()[split..]);
                assert_eq!(
                    capability.get(),
                    CommandCorrelationCapability::Supported(CommandShell::Fish)
                );
                let withdrawal = format!("\x1b]133;A;kettle=0{end}");
                for byte in withdrawal.as_bytes() {
                    feed(std::slice::from_ref(byte));
                }
                assert_eq!(capability.get(), CommandCorrelationCapability::Unavailable);
                feed(format!("\x1b]133;A;kettle=1;shell=zsh{end}").as_bytes());
                assert_eq!(
                    capability.get(),
                    CommandCorrelationCapability::Supported(CommandShell::Zsh)
                );
                capability.command_started();
                assert_eq!(
                    capability.get(),
                    CommandCorrelationCapability::AwaitingPrompt
                );
            }
        }
    }

    #[test]
    fn supported_commands_wait_for_the_next_prompt_without_changing_legacy_fallback() {
        use CommandCorrelationCapability::{AwaitingPrompt, Supported, Unavailable};
        let capability = ShellCapability::default();
        capability.command_started();
        assert_eq!(capability.get(), Unavailable);
        capability.announce(PromptKind::PromptStart);
        assert_eq!(capability.get(), Unavailable);
        capability.announce(PromptKind::IntegratedPromptStart(CommandShell::Fish));
        capability.command_started();
        assert_eq!(capability.get(), AwaitingPrompt);
        // Another C cannot prematurely authorize legacy input in the D-to-A gap.
        capability.command_started();
        assert_eq!(capability.get(), AwaitingPrompt);
        capability.announce(PromptKind::IntegratedPromptStart(CommandShell::Fish));
        capability.announce(PromptKind::PromptStart);
        assert_eq!(capability.get(), Supported(CommandShell::Fish));
        capability.command_started();
        capability.announce(PromptKind::PromptStart);
        assert_eq!(
            capability.get(),
            Unavailable,
            "exec into a legacy integration"
        );
        capability.announce(PromptKind::IntegratedPromptStart(CommandShell::Zsh));
        capability.command_started();
        capability.announce(PromptKind::CorrelationDisabledPromptStart);
        assert_eq!(capability.get(), Unavailable);
    }

    #[test]
    fn synchronized_command_boundaries_follow_their_text_offsets() {
        use super::super::fuzzing::SyncGraphicsHarness;
        for terminator in ["\x07", "\x1b\\"] {
            let mut harness = SyncGraphicsHarness::new();
            let wanted = token(b'a');
            harness.correlation.lock().unwrap().arm(wanted);
            harness.feed(b"\x1b[?2026hBEFORE\r\n");
            harness.feed(format!("\x1b]133;C;kettle={}{terminator}", wanted.as_str()).as_bytes());
            harness.feed(b"TARGET\r\n");
            harness.feed(format!("\x1b]133;D;7;kettle={}{terminator}", wanted.as_str()).as_bytes());
            assert!(
                harness.correlation.lock().unwrap().drain().is_empty(),
                "completion must not precede the synchronized text commit"
            );
            harness.feed(b"\x1b[?2026l");
            let completed = harness.correlation.lock().unwrap().drain();
            assert_eq!(completed.len(), 1);
            assert_eq!(completed[0].start_line, 1);
            assert_eq!(completed[0].command.exit_code, Some(7));
            let term = harness.term.lock().unwrap();
            use alacritty_terminal::index::{Column, Line, Point};
            assert_eq!(term.grid()[Point::new(Line(1), Column(0))].c, 'T');
        }
    }

    #[test]
    fn forced_sync_flush_preserves_marker_times_and_cancellation() {
        use super::super::fuzzing::SyncGraphicsHarness;
        for cancel in [false, true] {
            let mut harness = SyncGraphicsHarness::new();
            let wanted = token(b'a');
            harness.correlation.lock().unwrap().arm(wanted);
            harness.feed(b"\x1b[?2026hBEFORE\r\n");
            let now = Instant::now();
            for (boundary, at) in [
                (CommandBoundary::Begin(wanted), now),
                (
                    CommandBoundary::End(wanted, 9, Some(7)),
                    now + Duration::from_secs(7),
                ),
            ] {
                CommandMarker::record(
                    &harness.correlation,
                    boundary,
                    at,
                    &mut harness.processor,
                    &harness.term,
                    &mut harness.deferred,
                    &harness.proxy,
                );
                harness.feed(b"TARGET\r\n");
            }
            assert!(harness.correlation.lock().unwrap().drain().is_empty());
            if cancel {
                harness.correlation.lock().unwrap().cancel(wanted);
                harness.correlation.lock().unwrap().arm(token(b'b'));
            }
            harness.flush_sync();
            let events = harness.correlation.lock().unwrap().drain();
            if cancel {
                assert!(events.is_empty());
            } else {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].start_line, 1);
                assert_eq!(events[0].command.duration, Duration::from_secs(7));
            }
        }
    }

    #[test]
    fn synchronized_marker_overflow_cannot_publish_a_partial_completion() {
        use super::super::fuzzing::SyncGraphicsHarness;
        let mut harness = SyncGraphicsHarness::new();
        let wanted = token(b'a');
        harness.correlation.lock().unwrap().arm(wanted);
        harness.feed(b"\x1b[?2026h");
        let begin = format!("\x1b]133;C;kettle={}\x07", wanted.as_str());
        for _ in 0..=super::super::SYNC_MARKER_CAPACITY {
            harness.feed(begin.as_bytes());
        }
        harness.feed(format!("\x1b]133;D;0;kettle={}\x07", wanted.as_str()).as_bytes());
        harness.flush_sync();
        assert!(harness.correlation.lock().unwrap().drain().is_empty());
        harness.correlation.lock().unwrap().cancel(wanted);
        harness.correlation.lock().unwrap().arm(wanted);
        harness.feed(begin.as_bytes());
        harness.feed(format!("\x1b]133;D;0;kettle={}\x07", wanted.as_str()).as_bytes());
        assert_eq!(harness.correlation.lock().unwrap().drain().len(), 1);
    }

    #[test]
    fn a_matched_completion_wakes_a_quiescent_owner_once_without_paint() {
        use crate::event::{EventProxy, OutputWakeGate, Waker};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&wakes);
        let waker: Waker = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let output_wake = Arc::new(OutputWakeGate::new(Arc::clone(&waker)));
        output_wake.set_enabled(false);
        let (tx, _rx) = crossbeam_channel::bounded(1);
        let proxy = EventProxy::with_output_wake(tx, waker, output_wake);
        let lifecycle = proxy.lifecycle_pending();
        let wanted = token(b'a');
        let state = Mutex::new(CommandCorrelation::default());
        let now = Instant::now();
        state.lock().unwrap().arm(wanted);
        assert!(state.lock().unwrap().start(wanted, now, 7));
        assert!(!finish_command(
            &state,
            &proxy,
            Some(token(b'b')),
            1,
            Some(0),
            now
        ));
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
        assert!(finish_command(
            &state,
            &proxy,
            Some(wanted),
            1,
            Some(0),
            now
        ));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(lifecycle.swap(false, Ordering::AcqRel));
        assert!(!finish_command(
            &state,
            &proxy,
            Some(wanted),
            1,
            Some(0),
            now
        ));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(!lifecycle.load(Ordering::Acquire));
        assert_eq!(state.lock().unwrap().drain().len(), 1);
    }

    #[test]
    fn queued_commands_and_stale_or_duplicate_markers_cannot_complete_a_run() {
        let (wanted, stale) = (token(b'a'), token(b'b'));
        let mut state = CommandCorrelation::default();
        let now = Instant::now();
        state.arm(wanted);
        assert!(!state.finish(None, 1, Some(0), now));
        assert!(state.drain().is_empty());
        assert!(!state.start(stale, now, 12));
        assert!(!state.finish(Some(wanted), 2, Some(9), now));
        assert!(state.start(wanted, now, 42));
        assert!(!state.start(wanted, now + Duration::from_secs(5), 80));
        assert!(!state.start(stale, now, 90));
        assert!(!state.finish(None, 3, Some(1), now));
        assert!(!state.finish(Some(stale), 4, Some(2), now));
        assert!(state.finish(Some(wanted), 5, Some(7), now + Duration::from_secs(9)));
        let events = state.drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token, wanted);
        assert_eq!(events[0].start_line, 42);
        assert_eq!(events[0].command.duration, Duration::from_secs(9));
        assert_eq!(events[0].command.exit_code, Some(7));
        assert!(!state.finish(Some(wanted), 5, Some(7), now));
        assert!(state.drain().is_empty());
    }

    #[test]
    fn replacing_or_cancelling_an_expectation_retires_its_old_markers() {
        let (first, second) = (token(b'a'), token(b'b'));
        let mut state = CommandCorrelation::default();
        let now = Instant::now();
        state.arm(first);
        assert!(state.start(first, now, 10));
        state.arm(second);
        state.cancel(first);
        assert!(!state.start(first, now, 11));
        assert!(!state.finish(Some(first), 1, Some(0), now));
        assert!(state.start(second, now, 12));
        state.cancel(second);
        assert!(!state.finish(Some(second), 2, Some(0), now));
        assert!(!state.finish(None, 3, Some(0), now));
        assert!(state.drain().is_empty());
    }

    #[test]
    fn a_finished_begin_anchor_remains_available_until_the_ui_drains_or_cancels() {
        let token = token(b'a');
        let mut state = CommandCorrelation::default();
        let now = Instant::now();
        state.arm(token);
        assert_eq!(state.start_line(token), None);
        assert!(state.start(token, now, 42));
        assert_eq!(state.start_line(token), Some(42));
        assert!(state.finish(Some(token), 1, Some(0), now));
        assert_eq!(state.start_line(token), Some(42));
        state.cancel(token);
        assert_eq!(state.start_line(token), None);
        assert!(state.drain().is_empty());
    }

    #[test]
    fn completion_storage_is_bounded_even_without_a_drain() {
        let mut state = CommandCorrelation::default();
        let token = token(b'a');
        let now = Instant::now();
        for generation in 0..100 {
            state.arm(token);
            assert!(state.start(token, now, generation));
            assert!(state.finish(Some(token), generation, Some(0), now));
        }
        let events = state.drain();
        assert_eq!(events.len(), 32);
        assert_eq!(events[0].command.generation, 68);
        assert_eq!(events[31].command.generation, 99);
    }
}
