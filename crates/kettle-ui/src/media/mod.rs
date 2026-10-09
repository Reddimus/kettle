//! Media an agent or a command sends to the user with `show`, rendered by
//! the media worker onto the shelf of the pane it came from.
//!
//! [`MediaService`] is the App's one handle: it owns the render [`queue`]
//! and the lane, a thread started on first use that renders one job at a
//! time through the worker client. Each pane owns its [`Shelf`], so a shelf
//! moves with its tab into another window. A push is answered only after its
//! item is on the shelf; nothing a push does opens anything on screen.

mod cards;
mod external;
mod inline;
mod queue;
mod route;
mod shelf;
mod sightings;
mod tip;

use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender as ChannelSender};
use kettle_ctl::protocol::Response;
use kettle_media::client::{RenderControl, RenderError, WorkerClient};
use kettle_media::{FailureCode, Job, RenderOutput};

use crate::ctl_server::ReplyTx;

#[cfg(test)]
pub(crate) use cards::HARNESS_CARDS_PER_SECOND;
pub(crate) use cards::{CardLedger, CardRecord};
pub(crate) use external::{OpenFailure, Viewer, open as open_externally};
pub(crate) use inline::{card_caption, card_message, card_size};
pub(crate) use queue::Sender;
pub(crate) use route::{PaneRoot, Route, nearest_pane, route};
pub(crate) use shelf::{
    Provenance, Shelf, ShelfItem, UnverifiedSender, report as shelf_report, signer_name,
};
pub(crate) use sightings::{CardSightings, MAX_CARD_INSTANCE};
pub(crate) use tip::CardsTip;

use queue::{Admitted, Queue};

/// Longest title shown, in characters, after sanitizing.
const MAX_TITLE_CHARS: usize = 256;

/// Most failed previews kept for the user between two looks: the user has
/// one request waiting at a time, so a few is plenty.
const MAX_USER_FAILURES: usize = 8;

/// Who is waiting for a push's answer.
pub(crate) struct Origin {
    pub conn_id: u64,
    pub request_id: u64,
    pub reply: ReplyTx,
}

impl Origin {
    /// Answer with `response`. A client that left is not waiting.
    pub(crate) fn answer(self, response: Response) {
        let _ = self.reply.try_send(response);
    }

    pub(crate) fn refuse(self, failure: FailureCode) {
        let id = self.request_id;
        self.answer(kettle_ctl::show::show_failure(id, failure));
    }
}

/// Who a push answers: a control client waiting on its connection, or the
/// user, whose preview opens in the pane's lane when it is ready and who is
/// told when it cannot be.
pub(crate) enum Requester {
    Ctl(Origin),
    User,
}

impl Requester {
    /// Answer a control client with `failure`; for the user, hand it back to
    /// be shown.
    pub(crate) fn refuse(self, failure: FailureCode) -> Option<FailureCode> {
        match self {
            Self::Ctl(origin) => {
                origin.refuse(failure);
                None
            }
            Self::User => Some(failure),
        }
    }
}

/// What a push's shelf item will say about itself.
pub(crate) struct Draft {
    pub key: Option<String>,
    /// Display-ready (see [`display_title`]).
    pub title: String,
    pub provenance: Provenance,
    /// The inline card to register once the item is on the shelf.
    pub inline: Option<InlineDraft>,
}

/// An inline card a verified harness may have: who owns it and, when the
/// media came from a file, the name its caption shows.
pub(crate) struct InlineDraft {
    pub owner: kettle_ctl::process::ProcessIdentity,
    /// The harness that prints the card, which sets its place and size.
    pub harness: kettle_render::CardHarness,
    pub name: Option<String>,
}

/// One admitted `show`, from admission until it is answered.
pub(crate) struct Push {
    pub route: Route,
    pub draft: Draft,
    pub control: RenderControl,
    requester: Requester,
    /// The job, until the lane takes it.
    job: Option<Job>,
}

impl Push {
    pub(crate) fn new(
        requester: Requester,
        route: Route,
        draft: Draft,
        job: Job,
        control: RenderControl,
    ) -> Self {
        Self {
            route,
            draft,
            control,
            requester,
            job: Some(job),
        }
    }

    /// Whether the user asked for it.
    pub(crate) fn is_users(&self) -> bool {
        matches!(self.requester, Requester::User)
    }

    /// The control client it answers, if one does.
    fn conn_id(&self) -> Option<u64> {
        match &self.requester {
            Requester::Ctl(origin) => Some(origin.conn_id),
            Requester::User => None,
        }
    }

    /// Who waits for it, where it goes, and what its item will say.
    pub(crate) fn into_parts(self) -> (Requester, Route, Draft) {
        (self.requester, self.route, self.draft)
    }
}

/// A push whose render ended.
pub(crate) struct Finished {
    pub push: Push,
    pub result: Result<RenderOutput, RenderError>,
}

/// The render thread and its two channels.
struct Lane {
    work: ChannelSender<(Job, RenderControl)>,
    done: Receiver<Result<RenderOutput, RenderError>>,
}

impl Lane {
    /// Start the thread. `wake` runs after each render, on that thread,
    /// and once more when the thread ends, however it ends.
    fn start(client: Arc<WorkerClient>, wake: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let (work, jobs) = crossbeam_channel::bounded::<(Job, RenderControl)>(1);
        let (finished, done) = crossbeam_channel::bounded(1);
        std::thread::Builder::new()
            .name("kettle-media-lane".into())
            .spawn(move || {
                let exit = LaneExit {
                    finished: Some(finished),
                    wake,
                };
                for (job, control) in jobs {
                    let result = client.render_media_with_control(&job, &control);
                    // The source can be megabytes; it goes before the wait
                    // for the next job, not after.
                    drop(job);
                    if exit.send(result).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self { work, done })
    }
}

/// The lane's end of its result channel. Dropped, by a return or a panic,
/// it closes the channel and then wakes the App, which finds the lane gone
/// and fails the push it was rendering.
struct LaneExit {
    finished: Option<ChannelSender<Result<RenderOutput, RenderError>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl LaneExit {
    fn send(&self, result: Result<RenderOutput, RenderError>) -> Result<(), ()> {
        let finished = self.finished.as_ref().ok_or(())?;
        finished.send(result).map_err(drop)?;
        (self.wake)();
        Ok(())
    }
}

impl Drop for LaneExit {
    fn drop(&mut self) {
        drop(self.finished.take());
        (self.wake)();
    }
}

#[derive(Default)]
pub(crate) struct MediaService {
    queue: Queue<Push>,
    /// What the user asked to preview and could not have, by pane, for the
    /// App to tell them; bounded, as each failure comes from one request.
    user_failures: Vec<(u64, FailureCode)>,
    lane: Option<Lane>,
    /// The push being rendered.
    active: Option<Push>,
    /// The last item id handed out; ids are unique for the process's life.
    last_item: u64,
    /// Every registered inline card.
    pub(crate) cards: CardLedger,
}

impl MediaService {
    /// A fresh item id.
    pub(crate) fn next_item(&mut self) -> u64 {
        self.last_item += 1;
        self.last_item
    }

    /// Queue `push` for `sender`. A push it displaces, or the push itself
    /// when there is no room, is answered as busy. A preview the user asked
    /// for and then replaced with another goes without a word.
    pub(crate) fn admit(&mut self, sender: Sender, deadline: Instant, push: Push) {
        match self.queue.admit(sender, deadline, push) {
            Admitted::Queued => {}
            Admitted::Displaced(push) if push.is_users() => {}
            Admitted::Displaced(push) | Admitted::Busy(push) => {
                self.refuse(push, FailureCode::Busy);
            }
        }
    }

    /// End `push` with `failure`, answering its client or keeping it for
    /// the user.
    fn refuse(&mut self, push: Push, failure: FailureCode) {
        let pane = push.route.pane;
        if let Some(failure) = push.requester.refuse(failure)
            && self.user_failures.len() < MAX_USER_FAILURES
        {
            self.user_failures.push((pane, failure));
        }
    }

    /// The previews the user asked for that failed before rendering, by
    /// pane, since this was last asked.
    pub(crate) fn take_user_failures(&mut self) -> Vec<(u64, FailureCode)> {
        std::mem::take(&mut self.user_failures)
    }

    /// Answer every push that waited past its deadline as busy, then hand
    /// the next one to the lane if it is free. The lane starts on first use,
    /// rendering through `client`.
    pub(crate) fn pump(
        &mut self,
        client: Option<&Arc<WorkerClient>>,
        wake: impl FnOnce() -> Arc<dyn Fn() + Send + Sync>,
    ) {
        let now = Instant::now();
        for push in self.queue.expire(now) {
            self.refuse(push, FailureCode::Busy);
        }
        if self.queue.is_rendering() {
            return;
        }
        let Some(mut push) = self.queue.start_next(now) else {
            return;
        };
        let Some(job) = push.job.take() else {
            self.queue.finished();
            self.refuse(push, FailureCode::WorkerUnavailable);
            return;
        };
        if self.lane.is_none() {
            self.lane = client.and_then(|client| Lane::start(Arc::clone(client), wake()));
        }
        let sent = self
            .lane
            .as_ref()
            .is_some_and(|lane| lane.work.send((job, push.control.clone())).is_ok());
        if sent {
            self.active = Some(push);
        } else {
            self.lane = None;
            self.queue.finished();
            self.refuse(push, FailureCode::WorkerUnavailable);
        }
    }

    /// The push whose render just ended, if one did. A lane that died ends
    /// its push as unavailable and is started afresh for the next.
    pub(crate) fn take_finished(&mut self) -> Option<Finished> {
        let result = match self.lane.as_ref()?.done.try_recv() {
            Ok(result) => result,
            Err(crossbeam_channel::TryRecvError::Empty) => return None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.lane = None;
                Err(RenderError::Failure(FailureCode::WorkerUnavailable))
            }
        };
        self.finish(result)
    }

    /// End the active push with `result`. One cancelled after its render
    /// finished, by its client leaving or its pane going, is cancelled
    /// still: nothing it rendered is published.
    fn finish(&mut self, result: Result<RenderOutput, RenderError>) -> Option<Finished> {
        self.queue.finished();
        let push = self.active.take()?;
        let result = if push.control.is_cancelled() {
            Err(RenderError::Cancelled)
        } else {
            result
        };
        Some(Finished { push, result })
    }

    /// A connection closed: nothing it sent is rendered or answered.
    pub(crate) fn disconnected(&mut self, conn_id: u64) {
        drop(self.queue.cancel(|push| push.conn_id() == Some(conn_id)));
        if let Some(active) = self
            .active
            .as_ref()
            .filter(|push| push.conn_id() == Some(conn_id))
        {
            active.control.cancel();
        }
    }

    /// Pushes to panes `live` no longer lists are answered as having no
    /// pane, and one rendering is cancelled.
    pub(crate) fn retain_panes(&mut self, live: impl Fn(u64) -> bool) {
        // The user's own request for a pane that went needs no word.
        for push in self.queue.cancel(|push| !live(push.route.pane)) {
            if !push.is_users() {
                self.refuse(push, FailureCode::NotInKettlePane);
            }
        }
        if let Some(active) = self.active.as_ref().filter(|push| !live(push.route.pane)) {
            active.control.cancel();
        }
    }

    /// When a waiting push next expires, if one is waiting.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.queue.next_deadline()
    }

    /// Nothing waits or renders.
    pub(crate) fn is_idle(&self) -> bool {
        self.active.is_none() && self.queue.next_deadline().is_none()
    }
}

/// A title fit to display: placeholder sequences go, control characters,
/// line and paragraph separators and bidirectional formatting become
/// spaces, runs of spaces collapse, and it ends after [`MAX_TITLE_CHARS`]
/// characters. No title can carry a card's marks to the viewer, a listing
/// or assistive technology.
pub(crate) fn display_title(text: &str) -> String {
    let text = kettle_core::strip_placeholders(text);
    let mut title = String::new();
    let mut chars = 0;
    let mut space = false;
    for c in text.chars() {
        let c = if c.is_control()
            || matches!(c, '\u{2028}' | '\u{2029}' | '\u{061C}')
            || crate::app::is_bidi_format_char(c)
        {
            ' '
        } else {
            c
        };
        if c.is_whitespace() {
            space = !title.is_empty();
            continue;
        }
        if chars >= MAX_TITLE_CHARS {
            break;
        }
        if space {
            title.push(' ');
            chars += 1;
            space = false;
        }
        title.push(c);
        chars += 1;
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(conn_id: u64) -> (Push, crossbeam_channel::Receiver<Response>) {
        let (reply, answers) = crossbeam_channel::bounded(1);
        let push = push_for(Requester::Ctl(Origin {
            conn_id,
            request_id: 1,
            reply,
        }));
        (push, answers)
    }

    fn push_for(requester: Requester) -> Push {
        Push::new(
            requester,
            Route {
                pane: 3,
                window: 1,
                verified: true,
            },
            Draft {
                key: None,
                title: "t".into(),
                provenance: Provenance::Verified,
                inline: None,
            },
            Job {
                kind: kettle_media::JobKind::Svg,
                source: kettle_media::Source::Bytes(b"<svg/>".to_vec()),
                theme: kettle_media::Theme {
                    background: [0; 4],
                    foreground: [0; 4],
                    palette: [[0; 4]; 16],
                    accent: [0; 4],
                    is_dark: true,
                },
                canvas: kettle_media::Canvas::Theme,
                target: kettle_media::Target {
                    width: 1,
                    height: 1,
                    scale: 1.0,
                    crop: None,
                },
                fallback_fonts: Vec::new(),
            },
            RenderControl::default(),
        )
    }

    fn no_wake() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn output() -> RenderOutput {
        RenderOutput {
            kind: kettle_media::MediaKind::Svg,
            rendered: kettle_media::Rendered {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
                digest: kettle_media::content_digest(b"<svg/>", None).unwrap(),
                source_text: Vec::new(),
                fence_sources: Vec::new(),
                fence_count: 0,
                fence_index: None,
                uncovered_scripts: Vec::new(),
                warnings: Vec::new(),
            },
        }
    }

    /// A render that finished before its push was cancelled is not
    /// published: the cancellation stands.
    #[test]
    fn a_push_cancelled_after_its_render_finished_stays_cancelled() {
        let mut service = MediaService::default();
        let (active, _answers) = push(5);
        service.active = Some(active);
        service.disconnected(5);
        let finished = service.finish(Ok(output())).expect("the active push");
        assert!(matches!(finished.result, Err(RenderError::Cancelled)));
        let (active, _answers) = push(6);
        service.active = Some(active);
        let finished = service.finish(Ok(output())).expect("the active push");
        assert!(finished.result.is_ok(), "an uncancelled render stands");
    }

    /// What the user asked to preview and could not have is kept, by pane,
    /// for the App to tell them; a control client is answered instead. A
    /// request the user replaced, or whose pane went, ends without a word,
    /// and a client leaving never touches the user's.
    #[test]
    fn the_users_failed_previews_are_kept_to_be_told() {
        let mut service = MediaService::default();
        let later = Instant::now() + std::time::Duration::from_secs(60);
        service.admit(Sender::User, Instant::now(), push_for(Requester::User));
        let (client, answers) = push(1);
        service.admit(Sender::Pane(3), Instant::now(), client);
        service.pump(None, no_wake);
        assert_eq!(service.take_user_failures(), vec![(3, FailureCode::Busy)]);
        assert!(answers.try_recv().is_ok(), "the client is answered");
        assert!(service.take_user_failures().is_empty(), "told once");

        service.admit(Sender::User, later, push_for(Requester::User));
        service.admit(Sender::User, later, push_for(Requester::User));
        service.disconnected(1);
        assert_eq!(service.next_deadline(), Some(later));
        service.retain_panes(|_| false);
        assert_eq!(service.next_deadline(), None);
        assert!(service.take_user_failures().is_empty());

        // No worker to render it: the user hears so.
        service.admit(Sender::User, later, push_for(Requester::User));
        service.pump(None, no_wake);
        assert_eq!(
            service.take_user_failures(),
            vec![(3, FailureCode::WorkerUnavailable)]
        );

        for _ in 0..MAX_USER_FAILURES + 2 {
            service.admit(Sender::User, Instant::now(), push_for(Requester::User));
            service.pump(None, no_wake);
        }
        assert_eq!(service.take_user_failures().len(), MAX_USER_FAILURES);
    }

    /// The lane's end of the channel closes before the App is woken, by a
    /// return or a panic, so the App always finds the lane gone.
    #[test]
    fn a_lane_that_ends_closes_its_channel_then_wakes() {
        let (finished, done) = crossbeam_channel::bounded(1);
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let closed_first = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let woken = Arc::clone(&woken);
            let closed_first = Arc::clone(&closed_first);
            let done = done.clone();
            Arc::new(move || {
                woken.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if matches!(
                    done.try_recv(),
                    Err(crossbeam_channel::TryRecvError::Disconnected)
                ) {
                    closed_first.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            })
        };
        let lane = std::thread::spawn(move || {
            let _exit = LaneExit {
                finished: Some(finished),
                wake,
            };
            panic!("a lane that unwinds");
        });
        assert!(lane.join().is_err());
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(closed_first.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn titles_lose_controls_separators_and_bidi_and_are_bounded() {
        assert_eq!(display_title("  Build\u{7}\n\tplot  "), "Build plot");
        assert_eq!(display_title("a\u{202E}gnp.exe"), "a gnp.exe");
        assert_eq!(
            display_title("one\u{2028}two\u{2029}three"),
            "one two three"
        );
        assert_eq!(display_title("\u{2066}x\u{2069}\u{061C}"), "x");
        assert_eq!(display_title("\u{9b}31m"), "31m");
        let marked = format!(
            "plot{}",
            kettle_core::InlineMarker {
                row: 0,
                column: 1,
                nonce: kettle_core::InlineNonce::new([2, 3, 4, 5, 6, 7]).unwrap(),
            }
            .encode()
            .unwrap()
        );
        assert_eq!(display_title(&marked), "plot");
        assert_eq!(display_title(""), "");
        let long = "é".repeat(MAX_TITLE_CHARS + 10);
        assert_eq!(display_title(&long).chars().count(), MAX_TITLE_CHARS);
    }
}
