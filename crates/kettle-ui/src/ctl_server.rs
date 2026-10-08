//! The in-process control server (agent-first A2).
//!
//! When `agent-server` is enabled, the App starts a [`CtlServer`]: an accept
//! thread binds the kettle-ctl transport (Unix socket / Windows named pipe),
//! registers a discovery entry, and spawns ONE thread per connection. That
//! thread reads NDJSON requests and writes responses/events on the SAME handle,
//! never concurrently, so response and event frames cannot interleave. Writes
//! carry a deadline. On Windows, the transport enforces it with overlapped I/O
//! on both pipe ends.
//!
//!   - request → the App dispatches it on the main thread (the only place
//!     `self.mux` is touched) and sends the [`Response`] back over a per-request
//!     reply channel; the connection thread writes it. `run_command` defers its
//!     reply until the OSC-133 completion (the App holds the reply sender).
//!   - `subscribe` → after the ok reply, the connection switches to
//!     event-streaming: it drains its event channel and writes events until the
//!     client disconnects (so a streaming client uses a dedicated connection,
//!     matching `kettle ctl events`).
//!
//! The server is OFF by default. A [`CtlPolicy`] (`agent-server` plus
//! `agent-display`) decides whether it runs and what each connection may do;
//! the connection thread checks every request against it before any
//! dispatch. The threat model (same local user, off-by-default, logged,
//! dev-record-annotated) is in docs/AGENT.md.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kettle_ctl::discovery::{self, RegistryEntry};
use kettle_ctl::identity::{PeerCapture, UnverifiedReason};
use kettle_ctl::process::ProcessIdentity;
use kettle_ctl::protocol::{Event, Execution, Method, PeerClaim, Request, Response};
use kettle_ctl::transport::{CtlListener, CtlStream};
use kettle_ctl::{CtlPolicy, SharedCtlPolicy};

/// Max concurrent connections; excess are dropped immediately.
const MAX_CONNECTIONS: usize = 8;
/// Per-connection event queue cap (subscribers only). On overflow we drop +
/// flag `lag` so a slow client can't make the App allocate without bound.
const EVENT_QUEUE_CAP: usize = 256;
/// One extra channel slot is reserved for a lag notice after the data budget is
/// full. Without it, enqueueing the notice into the already-full queue can
/// never succeed, leaving a slow subscriber unaware that events were dropped.
const EVENT_CHANNEL_CAP: usize = EVENT_QUEUE_CAP + 1;
/// Tighter than the wire response cap so a full subscriber queue remains
/// bounded to roughly 16 MiB rather than hundreds of MiB.
const MAX_EVENT_BYTES: usize = 64 * 1024;

/// A request connection must complete a non-empty frame at least this often.
/// Long-running methods use their own documented deadlines once dispatched.
const REQUEST_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Once the server starts waiting for the rest of a partial frame, slow-drip
/// input has this absolute budget to deliver its newline. Individual bytes do
/// not extend it. The budget measures time spent waiting on the client, so it
/// is armed at the wait and cleared when the frame completes: a request the
/// server answers slowly — `wait_for` blocks its own connection thread by
/// design — must not spend the budget belonging to a pipelined successor.
const REQUEST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Every server response/event write is cancelled at this deadline so a peer
/// which stopped reading cannot pin a connection worker.
const SERVER_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// UI-dispatched methods normally reply immediately; `run_command` is allowed
/// up to 600 seconds, so give it a small teardown margin but never let a lost
/// reply sender pin a slot forever.
const SERVER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(610);
/// Subscribers are intentionally long-lived. A periodic bounded write both
/// keeps the stream observable and eventually backpressures an unread peer.
const SUBSCRIBER_KEEPALIVE: Duration = Duration::from_secs(20);

#[derive(Clone, Copy)]
struct ConnectionPolicy {
    request_idle: Duration,
    frame_assembly: Duration,
    write: Duration,
    response_wait: Duration,
    subscriber_keepalive: Duration,
}

const DEFAULT_CONNECTION_POLICY: ConnectionPolicy = ConnectionPolicy {
    request_idle: REQUEST_IDLE_TIMEOUT,
    frame_assembly: REQUEST_FRAME_TIMEOUT,
    write: SERVER_WRITE_TIMEOUT,
    response_wait: SERVER_RESPONSE_TIMEOUT,
    subscriber_keepalive: SUBSCRIBER_KEEPALIVE,
};

/// A reply channel for one request (a 1-slot oneshot).
pub type ReplyTx = Sender<Response>;

/// A message from a connection thread to the App's main-thread drain.
pub enum CtlServerMsg {
    /// A new client connected; `event_tx` is how the App pushes events to it
    /// (only after it subscribes).
    NewConn {
        conn_id: u64,
        event_tx: Sender<Event>,
    },
    /// An admitted request; the App dispatches it and sends the [`Response`]
    /// back over `reply` (immediately, or—for `run_command`—when it
    /// completes).
    Request {
        conn_id: u64,
        request: AdmittedRequest,
        reply: ReplyTx,
    },
    /// The connection closed.
    Disconnect { conn_id: u64 },
}

/// Per-connection state the App owns (main thread only).
pub struct ConnState {
    /// Push events here; the connection thread writes them once subscribed.
    event_tx: Sender<Event>,
    pub subscribed: bool,
    /// Panes this connection has targeted (for the agent badge + cleanup).
    pub attached_panes: HashSet<u64>,
}

/// Time a connection thread may spend checking who sent one request.
const CALLER_CHECK_BUDGET: Duration = Duration::from_millis(250);

/// What the connection thread learned about who sent a request: the
/// caller's checked ancestry, nearest first, for the App to match against
/// its panes. Only the connection thread makes one, and only `get_state`
/// carries one today.
#[derive(Debug, Clone, Default)]
pub struct CallerEvidence {
    checked: Option<CheckedCaller>,
}

#[derive(Debug, Clone)]
struct CheckedCaller {
    chain: Result<Vec<ProcessIdentity>, UnverifiedReason>,
    hint: Option<PaneHint>,
}

/// The pane and Kettle a caller's environment names. A hint is never proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneHint {
    pub pane: u64,
    pub kettle_pid: u32,
}

impl CallerEvidence {
    /// The caller's ancestry, nearest first, or why it has none.
    pub fn chain(&self) -> Option<Result<&[ProcessIdentity], UnverifiedReason>> {
        self.checked
            .as_ref()
            .map(|checked| checked.chain.as_deref().map_err(|reason| *reason))
    }

    pub fn hint(&self) -> Option<PaneHint> {
        self.checked.as_ref().and_then(|checked| checked.hint)
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        chain: Result<Vec<ProcessIdentity>, UnverifiedReason>,
        hint: Option<PaneHint>,
    ) -> Self {
        Self {
            checked: Some(CheckedCaller { chain, hint }),
        }
    }

    /// Check `claim` against `capture` and walk the caller's ancestry. Runs
    /// on the connection thread, never the UI thread.
    fn check(claim: Result<PeerClaim, UnverifiedReason>, capture: &PeerCapture) -> Self {
        let hint = claim.ok().and_then(|claim| {
            Some(PaneHint {
                pane: claim.pane_hint?,
                kettle_pid: claim.pid_hint?.get(),
            })
        });
        let chain = claim.and_then(|claim| {
            kettle_ctl::identity::verify_chain(
                capture,
                &claim,
                std::process::id(),
                Instant::now() + CALLER_CHECK_BUDGET,
            )
        });
        Self {
            checked: Some(CheckedCaller { chain, hint }),
        }
    }
}

/// A connection's claim, fixed by its first nonblank frame. A frame that
/// is not a valid request fixes "invalid"; a later request claiming another
/// process fixes "changed". Neither can be undone on that connection.
#[derive(Debug, Default)]
struct ClaimLatch(Option<Result<PeerClaim, UnverifiedReason>>);

impl ClaimLatch {
    fn invalid_frame(&mut self) {
        self.0.get_or_insert(Err(UnverifiedReason::InvalidClaim));
    }

    fn frame(&mut self, caller: Option<PeerClaim>) {
        match self.0 {
            None => self.0 = Some(caller.ok_or(UnverifiedReason::MissingClaim)),
            Some(Ok(first)) if caller.is_some_and(|claim| claim != first) => {
                self.0 = Some(Err(UnverifiedReason::ClaimChanged));
            }
            Some(_) => {}
        }
    }

    fn claim(&self) -> Result<PeerClaim, UnverifiedReason> {
        self.0.unwrap_or(Err(UnverifiedReason::MissingClaim))
    }
}

/// A request the control policy allowed. The App dispatches only these, and
/// only the connection thread can make one: [`admit`] for a client request,
/// [`AdmittedRequest::read_screen_probe`] for `wait_for`'s screen probes.
#[derive(Debug)]
pub struct AdmittedRequest {
    req: Request,
    method: Method,
    internal_probe: bool,
    caller: CallerEvidence,
}

impl AdmittedRequest {
    pub fn request(&self) -> &Request {
        &self.req
    }

    pub fn method(&self) -> Method {
        self.method
    }

    /// True for `wait_for`'s internal `read_screen` probes. The App skips the
    /// per-request dev-record marker (a 300s wait at 50ms polls would
    /// otherwise land ~6000 markers) and the post-drain redraw for them.
    pub fn internal_probe(&self) -> bool {
        self.internal_probe
    }

    /// Who sent this request, as far as the connection thread could check.
    pub fn caller(&self) -> &CallerEvidence {
        &self.caller
    }

    /// One `read_screen` probe for an admitted `wait_for`. It carries the
    /// wait's authority, which is Read for both, and nothing more: the method
    /// is fixed and only the pane address varies.
    fn read_screen_probe(&self, pane: Option<&serde_json::Value>) -> AdmittedRequest {
        debug_assert_eq!(self.method, Method::WaitFor);
        debug_assert_eq!(self.method.capability(), Method::ReadScreen.capability());
        let mut params = serde_json::Map::new();
        if let Some(pane) = pane {
            params.insert("pane".into(), pane.clone());
        }
        AdmittedRequest {
            req: Request {
                v: kettle_ctl::protocol::PROTOCOL_VERSION,
                id: self.req.id,
                method: Method::ReadScreen.as_str().into(),
                params: serde_json::Value::Object(params),
                caller: None,
            },
            method: Method::ReadScreen,
            internal_probe: true,
            caller: self.caller.clone(),
        }
    }
}

/// Admit `req` under `policy`, or answer it. The policy gate comes first, so a
/// refused request learns nothing about its parameters; then the parameter
/// shape every method shares.
fn admit(policy: CtlPolicy, req: Request) -> Result<AdmittedRequest, Response> {
    use kettle_ctl::protocol::error_codes as ec;
    let Some(method) = Method::from_name(&req.method) else {
        return Err(Response::err(
            req.id,
            ec::UNKNOWN_METHOD,
            format!("unknown method '{}'", req.method),
        ));
    };
    if let Err(error) = policy.check(method.capability()) {
        return Err(Response::err(req.id, &error.code, error.message));
    }
    if !req.params.is_null() && !req.params.is_object() {
        return Err(Response::err(
            req.id,
            ec::BAD_PARAMS,
            "params must be an object",
        ));
    }
    Ok(AdmittedRequest {
        req,
        method,
        internal_probe: false,
        caller: CallerEvidence::default(),
    })
}

/// The control server: owns the connection table + the inbound channel; the
/// accept + per-connection threads run in the background.
pub struct CtlServer {
    policy: SharedCtlPolicy,
    rx: Receiver<CtlServerMsg>,
    conns: HashMap<u64, ConnState>,
    registry_dir: PathBuf,
    /// Where this server left an alias to its entry, if anywhere.
    alias_dir: Option<PathBuf>,
    pid: u32,
    endpoint: String,
    _accept: std::thread::JoinHandle<()>,
}

impl CtlServer {
    /// Start the server under `policy`. `wake` is called after every message
    /// is enqueued so the App's event loop drains it (it sends
    /// `UserEvent::Ctl`). Returns `None` if the policy allows nothing, or
    /// (logged) if binding, registering the discovery entry, or spawning the
    /// accept thread fails.
    pub fn start(
        policy: CtlPolicy,
        pid: u32,
        version: &str,
        started_unix: u64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<CtlServer> {
        if !policy.runs_server() {
            return None;
        }
        let registry_dir = discovery::registry_dir();
        // Held until the entry is written, so no other process removes this
        // socket between its bind and its registration.
        let registration = discovery::begin_registration(&registry_dir);
        let endpoint = discovery::default_endpoint(&registry_dir, pid);
        let listener = match CtlListener::bind(&endpoint) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("agent-server: cannot bind control endpoint {endpoint}: {e}");
                return None;
            }
        };
        let entry = RegistryEntry::registering("gui", pid, endpoint.clone(), version, started_unix);
        if let Err(e) = discovery::register(&registry_dir, &entry) {
            log::warn!("agent-server: cannot write discovery entry: {e}");
            return None;
        }
        drop(registration);
        // Where the OS puts the registry, independent of this environment, so
        // a client started with a stripped environment still finds us.
        let alias_dir = discovery::canonical_registry_dir()
            .filter(|alias| *alias != registry_dir)
            .filter(
                |alias| match discovery::publish_alias(alias, &registry_dir, pid) {
                    Ok(()) => true,
                    Err(e) => {
                        log::warn!("agent-server: cannot write discovery alias: {e}");
                        false
                    }
                },
            );
        log::info!(
            "agent-server: listening on {endpoint} (mode {:?}, display {})",
            policy.server(),
            policy.display()
        );
        let policy = SharedCtlPolicy::new(policy);
        let access = policy.clone();

        let (tx, rx) = crossbeam_channel::unbounded::<CtlServerMsg>();
        let accept = match std::thread::Builder::new()
            .name("kettle-ctl-accept".into())
            .spawn(move || accept_loop(listener, tx, wake, access, DEFAULT_CONNECTION_POLICY))
        {
            Ok(accept) => accept,
            Err(error) => {
                discovery::unregister(&registry_dir, pid);
                if let Some(alias) = &alias_dir {
                    discovery::withdraw_alias(alias, pid);
                }
                log::warn!("agent-server: cannot spawn accept thread: {error}");
                return None;
            }
        };

        Some(CtlServer {
            policy,
            rx,
            conns: HashMap::new(),
            registry_dir,
            alias_dir,
            pid,
            endpoint,
            _accept: accept,
        })
    }

    /// What clients may do right now.
    pub fn policy(&self) -> CtlPolicy {
        self.policy.current()
    }

    /// Allow display for every connection, including ones already open.
    /// Turning display off takes effect at the next launch.
    pub fn enable_display(&self) {
        self.policy.enable_display();
    }

    /// Drain one pending message (App calls this in a loop on `UserEvent::Ctl`).
    pub fn try_recv(&self) -> Option<CtlServerMsg> {
        self.rx.try_recv().ok()
    }

    /// Register a freshly-accepted connection.
    ///
    /// `accept_loop` alone enforces the connection cap. It checks the atomic
    /// `active` counter and closes an over-cap connection before spawning a
    /// thread or sending `NewConn`. So this ALWAYS inserts. A second
    /// `conns.len()` check here could drop a connection `accept_loop` already
    /// admitted, because a Disconnect/NewConn reorder at the cap can make
    /// `conns` read full while `active` has room. That connection would stay
    /// live but untracked. The App still serves its requests, but `subscribe`
    /// and `attach_pane` silently no-op (no `ConnState` to flip). `remove_conn`
    /// ignores an absent id, so always inserting is safe and keeps `conns` in
    /// lockstep with `active`.
    pub fn add_conn(&mut self, conn_id: u64, event_tx: Sender<Event>) {
        self.conns.insert(
            conn_id,
            ConnState {
                event_tx,
                subscribed: false,
                attached_panes: HashSet::new(),
            },
        );
    }

    /// Remove a closed connection; returns the panes it had attached.
    pub fn remove_conn(&mut self, conn_id: u64) -> HashSet<u64> {
        self.conns
            .remove(&conn_id)
            .map(|c| c.attached_panes)
            .unwrap_or_default()
    }

    /// Mark `conn_id` subscribed to the event stream.
    pub fn set_subscribed(&mut self, conn_id: u64) {
        if let Some(c) = self.conns.get_mut(&conn_id) {
            c.subscribed = true;
        }
    }

    /// Record that `conn_id` attached to `pane`. Returns true if this is a new
    /// attachment for the pane across ALL connections.
    pub fn attach_pane(&mut self, conn_id: u64, pane: u64) -> bool {
        let already = self.pane_is_attached(pane);
        // Only report a NEW attachment if we actually recorded one: an untracked
        // connection (e.g. dropped at the cap) must never light a badge it can't
        // later clear on disconnect.
        let Some(c) = self.conns.get_mut(&conn_id) else {
            return false;
        };
        c.attached_panes.insert(pane);
        !already
    }

    /// Whether ANY connection has `pane` attached.
    pub fn pane_is_attached(&self, pane: u64) -> bool {
        self.conns
            .values()
            .any(|c| c.attached_panes.contains(&pane))
    }

    /// Broadcast an event to every subscribed connection. Overflowing a slow
    /// connection's queue drops the event for it + sends a one-line `lag` notice.
    pub fn broadcast(&self, ev: &Event) {
        let outgoing = match kettle_ctl::protocol::to_json_vec_bounded(ev, MAX_EVENT_BYTES) {
            Ok(_) => ev.clone(),
            _ => Event::new(
                "lag",
                ev.pane,
                serde_json::json!({"dropped": 1, "reason": "event_too_large"}),
            ),
        };
        for conn in self.conns.values() {
            if !conn.subscribed {
                continue;
            }
            if conn.event_tx.len() >= EVENT_QUEUE_CAP {
                let _ = conn.event_tx.try_send(Event::new(
                    "lag",
                    None,
                    serde_json::json!({"dropped": 1, "reason": "queue_full"}),
                ));
                continue;
            }
            if conn.event_tx.try_send(outgoing.clone()).is_err() {
                let _ = conn.event_tx.try_send(Event::new(
                    "lag",
                    None,
                    serde_json::json!({"dropped": 1, "reason": "queue_full"}),
                ));
            }
        }
    }

    /// True if any connection is subscribed (lets the App skip event work).
    pub fn has_subscribers(&self) -> bool {
        self.conns.values().any(|c| c.subscribed)
    }
}

impl Drop for CtlServer {
    fn drop(&mut self) {
        discovery::unregister(&self.registry_dir, self.pid);
        if let Some(alias) = &self.alias_dir {
            discovery::withdraw_alias(alias, self.pid);
        }
        discovery::remove_own_endpoint(&self.endpoint);
    }
}

/// The accept loop: assign a connection id, create its event channel, and spawn
/// ONE thread per connection that reads + writes on the same handle. A shared
/// atomic counter enforces `MAX_CONNECTIONS` at the source — an over-cap
/// connection is closed immediately (the socket/pipe handle dropped) rather
/// than spawning a live thread that would sit idle holding the endpoint.
fn accept_loop(
    listener: CtlListener,
    tx: Sender<CtlServerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    access: SharedCtlPolicy,
    policy: ConnectionPolicy,
) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    accept_loop_counting(listener, tx, wake, access, policy, active);
}

/// [`accept_loop`] counting its live connections in `active`, which tests
/// read.
fn accept_loop_counting(
    listener: CtlListener,
    tx: Sender<CtlServerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    access: SharedCtlPolicy,
    policy: ConnectionPolicy,
    active: Arc<std::sync::atomic::AtomicUsize>,
) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let mut consecutive_errors = 0u32;
    loop {
        let conn = match listener.accept() {
            Ok(s) => {
                consecutive_errors = 0;
                s
            }
            Err(e) => {
                // A single bad/abandoned client must not kill the accept thread.
                // Tolerate transient errors with a short backoff; give up only
                // after many consecutive failures (the listener is truly gone).
                consecutive_errors += 1;
                if consecutive_errors > 32 {
                    log::warn!("agent-server: accept loop ending after repeated errors: {e}");
                    return;
                }
                log::debug!("agent-server: accept error (transient): {e}");
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
        };
        match conn.peer_is_same_user() {
            Ok(true) => {}
            Ok(false) => {
                log::warn!("agent-server: refusing control connection from another user");
                continue;
            }
            Err(e) => {
                log::warn!("agent-server: cannot verify control peer credentials: {e}");
                continue;
            }
        }
        // Hard connection cap: refuse (and close) once MAX_CONNECTIONS are live.
        if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            log::warn!("agent-server: connection cap ({MAX_CONNECTIONS}) reached; refusing");
            drop(conn); // closes the socket / pipe handle
            continue;
        }
        // Who connected, read before any request bytes: the identity a
        // first-request claim must match.
        let capture = PeerCapture::capture(&conn);
        let conn_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        active.fetch_add(1, Ordering::Relaxed);
        let (event_tx, event_rx) = crossbeam_channel::bounded::<Event>(EVENT_CHANNEL_CAP);
        let ctx = tx.clone();
        let cwake = wake.clone();
        let active_dec = active.clone();
        let caccess = access.clone();
        let (start_tx, start_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let spawned = std::thread::Builder::new()
            .name(format!("kettle-ctl-{conn_id}"))
            .spawn(move || {
                if start_rx.recv().is_err() {
                    active_dec.fetch_sub(1, Ordering::Release);
                    return;
                }
                connection_loop(
                    conn, capture, conn_id, ctx, cwake, event_rx, caccess, policy, active_dec,
                );
            });
        finish_worker_spawn(spawned, conn_id, event_tx, &tx, &wake, start_tx, &active);
    }
}

/// Publish a connection only after its worker exists. On spawn failure the
/// admission count is rolled back and no `NewConn` can reach the App.
fn finish_worker_spawn(
    spawned: std::io::Result<std::thread::JoinHandle<()>>,
    conn_id: u64,
    event_tx: Sender<Event>,
    tx: &Sender<CtlServerMsg>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    start_tx: std::sync::mpsc::SyncSender<()>,
    active: &Arc<std::sync::atomic::AtomicUsize>,
) {
    match spawned {
        Ok(_) => {
            if tx.send(CtlServerMsg::NewConn { conn_id, event_tx }).is_ok() {
                wake();
                let _ = start_tx.send(());
            }
        }
        Err(error) => {
            active.fetch_sub(1, Ordering::Release);
            log::warn!("agent-server: cannot spawn connection worker: {error}");
        }
    }
}

/// Ends a connection, however [`connection_loop`] returns, in the order its
/// observers need. First its place under `MAX_CONNECTIONS` is freed, then the
/// App hears `Disconnect`, and only then does the handle close (a parameter,
/// it drops after this guard, a local). A client that reconnects on seeing
/// EOF, or a test that does on seeing `Disconnect`, therefore finds the place
/// free; announcing first let such a reconnect reach the cap check while the
/// old place was still counted, and at the cap it was refused.
struct ConnectionExit {
    active: Arc<std::sync::atomic::AtomicUsize>,
    conn_id: u64,
    tx: Sender<CtlServerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for ConnectionExit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Release);
        let _ = self.tx.send(CtlServerMsg::Disconnect {
            conn_id: self.conn_id,
        });
        (self.wake)();
    }
}

/// One connection, one thread: read requests + write responses/events on the
/// SAME handle, sequentially so frames cannot interleave. Every request is
/// checked against `access` before anything else happens for it. A
/// `subscribe` the App accepts flips the connection into event-only
/// streaming. `active` counts this connection until it ends (see
/// [`ConnectionExit`]).
#[allow(clippy::too_many_arguments)]
fn connection_loop(
    mut conn: CtlStream,
    capture: PeerCapture,
    conn_id: u64,
    tx: Sender<CtlServerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    event_rx: Receiver<Event>,
    access: SharedCtlPolicy,
    policy: ConnectionPolicy,
    active: Arc<std::sync::atomic::AtomicUsize>,
) {
    let _exit = ConnectionExit {
        active,
        conn_id,
        tx: tx.clone(),
        wake: wake.clone(),
    };
    let mut acc: Vec<u8> = Vec::with_capacity(4096);
    let mut scan_offset = 0;
    let mut buf = [0u8; 4096];
    let mut idle_deadline = Instant::now() + policy.request_idle;
    let mut frame_deadline: Option<Instant> = None;
    let mut claim = ClaimLatch::default();
    'outer: loop {
        // Extract a complete line if we have one.
        if let Some(pos) = kettle_ctl::protocol::find_newline(&acc, &mut scan_offset) {
            if pos > kettle_ctl::protocol::MAX_LINE_BYTES {
                let response = Response::err(
                    0,
                    kettle_ctl::protocol::error_codes::BAD_REQUEST,
                    "request line exceeds 1 MiB",
                );
                let _ = write_response_line(&mut conn, &response, policy.write);
                break;
            }
            let line: Vec<u8> = acc.drain(..=pos).collect();
            scan_offset = 0;
            // This frame is complete, so it owes nothing more. Any pipelined
            // remainder is armed where the server actually starts waiting for
            // the rest of it, not here: answering this request can take as long
            // as `wait_for` needs, and charging that to the next frame would
            // disconnect a well-behaved client for the server's own work.
            frame_deadline = None;
            let trimmed = match std::str::from_utf8(&line) {
                Ok(line) => line.trim_end(),
                Err(error) => {
                    claim.invalid_frame();
                    let response = Response::err(
                        0,
                        kettle_ctl::protocol::error_codes::BAD_REQUEST,
                        format!("request line is not UTF-8: {error}"),
                    );
                    if write_response_line(&mut conn, &response, policy.write).is_err() {
                        break;
                    }
                    idle_deadline = Instant::now() + policy.request_idle;
                    continue;
                }
            };
            if trimmed.is_empty() {
                continue;
            }
            // Admission is the one policy gate, and it comes before every
            // dispatch: a malformed, unknown or refused request is answered
            // here, does no work, and reaches no other thread.
            let parsed = kettle_ctl::protocol::parse_request_line(trimmed);
            match &parsed {
                Ok(req) => claim.frame(req.caller),
                Err(_) => claim.invalid_frame(),
            }
            let request = match parsed.and_then(|req| admit(access.current(), req)) {
                // Only `get_state` reports its caller today, so only it pays
                // for the check.
                Ok(mut request) if request.method() == Method::GetState => {
                    request.caller = CallerEvidence::check(claim.claim(), &capture);
                    request
                }
                Ok(request) => request,
                Err(resp) => {
                    if write_response_line(&mut conn, &resp, policy.write).is_err() {
                        break 'outer;
                    }
                    idle_deadline = Instant::now() + policy.request_idle;
                    continue;
                }
            };
            // `wait_for` blocks THIS connection thread, never the UI thread.
            // It polls the screen via cheap internal `read_screen` requests
            // (>=50ms apart) until the condition holds or the deadline
            // passes. The UI thread only ever answers individual snapshot
            // probes.
            if request.method().execution() == Execution::Connection {
                let resp = wait_for_poll(&mut conn, &tx, &wake, conn_id, &request);
                if write_response_line(&mut conn, &resp, policy.write).is_err() {
                    break 'outer;
                }
                idle_deadline = Instant::now() + policy.request_idle;
                continue;
            }
            let is_subscribe = request.method() == Method::Subscribe;
            // Oneshot reply channel for this request.
            let (rtx, rrx) = crossbeam_channel::bounded::<Response>(1);
            let _ = tx.send(CtlServerMsg::Request {
                conn_id,
                request,
                reply: rtx,
            });
            wake();
            // Block until the App replies (a deferred `run_command` can take up
            // to its full `timeout_s`, e.g. 600s), then write the response on
            // this handle. We must NOT block UNBOUNDED on `rrx.recv()`: while the
            // App holds a deferred `run_command` reply, this thread is parked
            // OUTSIDE `conn.read()`, so a client that vanishes mid-run (Ctrl+C'd
            // `kettle ctl`, crashed MCP host) is never observed — the
            // MAX_CONNECTIONS slot, the agent badge, and the per-pane
            // `PendingRun` (which makes new runs on that pane return BUSY) stay
            // pinned until the command deadline. Mirror `wait_for_poll`: poll the
            // reply on a short interval and probe `conn.peer_disconnected()` on
            // each timeout. The zero-byte peek is safe here — this IS the
            // connection thread with no other I/O outstanding. On a gone peer we
            // end the connection, and `ConnectionExit` sends `Disconnect` (so the
            // App drops the `PendingRun` + clears the badge).
            let response_deadline = Instant::now() + policy.response_wait;
            let resp = loop {
                match rrx.recv_timeout(Duration::from_millis(200)) {
                    Ok(resp) => break resp,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= response_deadline {
                            break 'outer;
                        }
                        if conn.peer_disconnected() {
                            break 'outer;
                        }
                    }
                    // App dropped the reply sender (shutdown) — end the connection.
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break 'outer,
                }
            };
            if write_response_line(&mut conn, &resp, policy.write).is_err() {
                break 'outer;
            }
            idle_deadline = Instant::now() + policy.request_idle;
            if is_subscribe && resp.ok {
                // Switch to event-only streaming for the rest of the
                // connection's life (no more requests read on this handle). Use
                // a bounded recv so an IDLE subscriber whose client vanished is
                // detected within the keepalive window: on timeout we write a
                // harmless `ping` event; a failed write means the peer is gone,
                // so we disconnect (bounding the ConnState+thread leak to the
                // timeout rather than "until the next real event").
                loop {
                    match event_rx.recv_timeout(policy.subscriber_keepalive) {
                        Ok(ev) => {
                            if write_event_line(&mut conn, &ev, policy.write).is_err() {
                                break 'outer;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            let ping = Event::new("ping", None, serde_json::Value::Null);
                            if write_event_line(&mut conn, &ping, policy.write).is_err() {
                                break 'outer;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break 'outer,
                    }
                }
            }
            continue;
        }
        // Need more bytes. Enforce the 1 MiB line cap incrementally so a client
        // that never sends a newline can't grow `acc` without bound (DoS) — the
        // protocol's MAX_LINE_BYTES is otherwise only checkable post-assembly.
        if acc.len() > kettle_ctl::protocol::MAX_LINE_BYTES {
            let resp = Response::err(
                0,
                kettle_ctl::protocol::error_codes::BAD_REQUEST,
                "request line exceeds 1 MiB",
            );
            let _ = write_response_line(&mut conn, &resp, policy.write);
            break;
        }
        // Arm the assembly budget where the server begins waiting on the
        // client, and only once per frame: `is_none` means a drip of one byte
        // per interval cannot keep pushing the deadline out. A frame that
        // completes clears it above, so each partial frame gets exactly one
        // budget measured from when we started waiting for its remainder.
        if !acc.is_empty() && frame_deadline.is_none() {
            frame_deadline = Some(Instant::now() + policy.frame_assembly);
        }
        let read_deadline = frame_deadline
            .map(|deadline| deadline.min(idle_deadline))
            .unwrap_or(idle_deadline);
        let now = Instant::now();
        if now >= read_deadline {
            break;
        }
        match conn.wait_readable(read_deadline.saturating_duration_since(now)) {
            Ok(true) => {}
            Ok(false) | Err(_) => break,
        }
        let remaining = (kettle_ctl::protocol::MAX_LINE_BYTES + 1).saturating_sub(acc.len());
        if remaining == 0 {
            let resp = Response::err(
                0,
                kettle_ctl::protocol::error_codes::BAD_REQUEST,
                "request line exceeds 1 MiB",
            );
            let _ = write_response_line(&mut conn, &resp, policy.write);
            break;
        }
        let read_len = remaining.min(buf.len());
        match conn.read(&mut buf[..read_len]) {
            Ok(0) | Err(_) => break,
            // The budget is armed before the wait above, so arriving bytes
            // never refresh it — that is what bounds a slow drip.
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
    }
}

/// Serialize and write a response within the server amplification budget.
fn write_response_line(
    conn: &mut CtlStream,
    value: &Response,
    timeout: Duration,
) -> std::io::Result<()> {
    let line = serialized_response_line(value)?;
    write_serialized_line(conn, line, timeout)
}

fn serialized_response_line(value: &Response) -> std::io::Result<Vec<u8>> {
    use kettle_ctl::protocol::{BoundedJsonError, MAX_RESPONSE_LINE_BYTES};

    match kettle_ctl::protocol::to_json_vec_bounded(value, MAX_RESPONSE_LINE_BYTES) {
        Ok(line) => return Ok(line),
        Err(BoundedJsonError::Serialize(error)) => return Err(std::io::Error::other(error)),
        Err(BoundedJsonError::Limit { .. }) => {}
    }
    kettle_ctl::protocol::to_json_vec_bounded(
        &Response::err(
            value.id,
            kettle_ctl::protocol::error_codes::RESPONSE_TOO_LARGE,
            format!(
                "response exceeds {} bytes; use cursor/limit paging",
                MAX_RESPONSE_LINE_BYTES
            ),
        ),
        MAX_RESPONSE_LINE_BYTES,
    )
    .map_err(std::io::Error::other)
}

/// Events share the response budget. Oversize event payloads become a bounded
/// lag notice rather than closing every subscriber.
fn write_event_line(conn: &mut CtlStream, value: &Event, timeout: Duration) -> std::io::Result<()> {
    use kettle_ctl::protocol::{BoundedJsonError, MAX_RESPONSE_LINE_BYTES};

    let line = match kettle_ctl::protocol::to_json_vec_bounded(value, MAX_RESPONSE_LINE_BYTES) {
        Ok(line) => line,
        Err(BoundedJsonError::Serialize(error)) => return Err(std::io::Error::other(error)),
        Err(BoundedJsonError::Limit { .. }) => kettle_ctl::protocol::to_json_vec_bounded(
            &Event::new(
                "lag",
                value.pane,
                serde_json::json!({"dropped": 1, "reason": "event_too_large"}),
            ),
            MAX_RESPONSE_LINE_BYTES,
        )
        .map_err(std::io::Error::other)?,
    };
    write_serialized_line(conn, line, timeout)
}

fn write_serialized_line(
    conn: &mut CtlStream,
    mut line: Vec<u8>,
    timeout: Duration,
) -> std::io::Result<()> {
    line.push(b'\n');
    conn.write_all_until(&line, Instant::now() + timeout, None)
}

/// The `wait_for` poll loop. Runs on the CONNECTION thread; each iteration
/// sends one internal `read_screen` request to the UI thread (the same cheap
/// snapshot `read_screen` serves) and checks the condition against the
/// returned text. Params:
///
/// - `pane?: u64`      — target pane (default: focused)
/// - `text?: string`   — substring that must appear on screen
/// - `regex?: string`  — regex that must match the screen text
/// - `quiet_ms?: u64`  — additionally require the screen to have been
///   UNCHANGED for this long (output settled — TUI finished painting)
/// - `timeout_ms?: u64`— overall deadline (default 30 000, capped 300 000)
/// - `poll_ms?: u64`   — poll interval (default 100, floor 50 so a tight
///   caller can't hammer the UI thread)
///
/// Multiple conditions AND together. Returns `{matched, elapsed_ms, polls}`
/// — a timeout is an `ok` response with `matched: false` (the agent decides
/// what a non-appearance means; it is not a transport error).
fn wait_for_poll(
    conn: &mut CtlStream,
    tx: &Sender<CtlServerMsg>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    conn_id: u64,
    wait: &AdmittedRequest,
) -> Response {
    use kettle_ctl::protocol::error_codes as ec;
    let req = wait.request();
    let text = req
        .params
        .get("text")
        .and_then(|v| v.as_str())
        .map(String::from);
    let regex = match req.params.get("regex").and_then(|v| v.as_str()) {
        Some(src) => match regex::Regex::new(src) {
            Ok(re) => Some(re),
            Err(e) => return Response::err(req.id, ec::BAD_PARAMS, format!("bad regex: {e}")),
        },
        None => None,
    };
    let quiet_ms = req.params.get("quiet_ms").and_then(|v| v.as_u64());
    if text.is_none() && regex.is_none() && quiet_ms.is_none() {
        return Response::err(
            req.id,
            ec::BAD_PARAMS,
            "wait_for needs at least one of 'text', 'regex', 'quiet_ms'",
        );
    }
    let timeout_ms = req
        .params
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000)
        .min(300_000);
    let poll_ms = req
        .params
        .get("poll_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(100)
        .clamp(50, 5_000);
    let start = std::time::Instant::now();
    let mut last_change = std::time::Instant::now();
    let mut last_fingerprint: Option<u64> = None;
    let mut polls = 0u64;
    // Pin the target pane for the WHOLE wait. Otherwise a no-`pane` wait
    // re-resolves "focused" on every probe, so a focus change mid-wait would
    // retarget the watch and corrupt the quiet_ms fingerprint (two panes'
    // screens interleaving looks like constant change). `read_screen` echoes
    // the resolved pane id in its result, so the first probe's reply pins it.
    let mut pinned_pane: Option<serde_json::Value> = req.params.get("pane").cloned();
    loop {
        // A vanished client (Ctrl+C'd `kettle ctl`, crashed MCP host) must not
        // keep this loop polling. It would pin one of the MAX_CONNECTIONS slots
        // and wake the UI thread every poll for up to the full timeout. The
        // zero-byte peek is safe because this IS the connection thread, with no
        // other I/O outstanding.
        if conn.peer_disconnected() {
            return Response::err(req.id, ec::INTERNAL, "client disconnected during wait_for");
        }
        // Compose the internal probe (pinned pane addressing).
        let (rtx, rrx) = crossbeam_channel::bounded::<Response>(1);
        let _ = tx.send(CtlServerMsg::Request {
            conn_id,
            request: wait.read_screen_probe(pinned_pane.as_ref()),
            reply: rtx,
        });
        wake();
        polls += 1;
        let resp = match rrx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(r) => r,
            Err(_) => {
                return Response::err(
                    req.id,
                    ec::INTERNAL,
                    "wait_for: the UI thread did not answer a screen probe",
                );
            }
        };
        // Propagate probe errors (no_such_pane, …) verbatim under our id.
        if let Some(err) = &resp.error {
            return Response::err(req.id, &err.code, err.message.clone());
        }
        // Pin the resolved pane after the first successful probe.
        if pinned_pane.is_none() {
            pinned_pane = resp.result.get("pane").cloned();
        }
        let screen = resp
            .result
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Change detection for quiet_ms: text + cursor + history fingerprint.
        if quiet_ms.is_some() {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            screen.hash(&mut h);
            resp.result
                .get("cursor")
                .map(|c| c.to_string())
                .hash(&mut h);
            resp.result
                .get("history_size")
                .and_then(|v| v.as_u64())
                .hash(&mut h);
            let fp = h.finish();
            if last_fingerprint != Some(fp) {
                last_fingerprint = Some(fp);
                last_change = std::time::Instant::now();
            }
        }
        let content_hit = text.as_deref().is_none_or(|t| screen.contains(t))
            && regex.as_ref().is_none_or(|re| re.is_match(screen));
        let quiet_hit = quiet_ms.is_none_or(|q| {
            // The first poll has no baseline; require at least one interval.
            polls > 1 && last_change.elapsed().as_millis() as u64 >= q
        });
        let elapsed = start.elapsed().as_millis() as u64;
        if content_hit && quiet_hit {
            return Response::ok(
                req.id,
                serde_json::json!({
                    "matched": true,
                    "elapsed_ms": elapsed,
                    "polls": polls,
                    "pane": resp.result.get("pane").cloned().unwrap_or(serde_json::Value::Null),
                }),
            );
        }
        if elapsed >= timeout_ms {
            return Response::ok(
                req.id,
                serde_json::json!({
                    "matched": false,
                    "timed_out": true,
                    "elapsed_ms": elapsed,
                    "polls": polls,
                }),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production source of this file, excluding test-only items.
    fn production_source() -> String {
        let production = kettle_test_support::production_source(include_str!("ctl_server.rs"));
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

    fn test_endpoint(tag: &str) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        #[cfg(unix)]
        return std::env::temp_dir()
            .join(format!(
                "kettle-ctl-ui-{tag}-{}-{unique}.sock",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        return format!(
            r"\\.\pipe\kettle-ctl-ui-{tag}-{}-{unique}",
            std::process::id()
        );
    }

    fn full_access() -> SharedCtlPolicy {
        SharedCtlPolicy::new(CtlPolicy::new(kettle_config::AgentServer::Full, false))
    }

    fn start_test_accept_loop(
        tag: &str,
        policy: ConnectionPolicy,
    ) -> (String, Receiver<CtlServerMsg>) {
        start_test_accept_loop_with(tag, full_access(), policy)
    }

    fn start_test_accept_loop_with(
        tag: &str,
        access: SharedCtlPolicy,
        policy: ConnectionPolicy,
    ) -> (String, Receiver<CtlServerMsg>) {
        let endpoint = test_endpoint(tag);
        let listener = CtlListener::bind(&endpoint).expect("bind test control listener");
        let (tx, rx) = crossbeam_channel::unbounded();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        std::thread::spawn(move || accept_loop(listener, tx, wake, access, policy));
        (endpoint, rx)
    }

    /// A connection that ends frees its place under `MAX_CONNECTIONS` before
    /// the App hears `Disconnect`, and the App hears it before the client sees
    /// EOF. A client reconnecting on either signal must never find its old
    /// place still counted: at the cap the server refuses it.
    #[test]
    fn an_ended_connection_frees_its_place_before_anyone_hears_it_ended() {
        let policy = ConnectionPolicy {
            request_idle: Duration::from_millis(100),
            frame_assembly: Duration::from_millis(200),
            write: Duration::from_millis(200),
            response_wait: Duration::from_secs(1),
            subscriber_keepalive: Duration::from_millis(200),
        };
        let endpoint = test_endpoint("exit-order");
        let listener = CtlListener::bind(&endpoint).expect("bind test control listener");
        let (tx, rx) = crossbeam_channel::unbounded();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // `wake` runs right after each message is sent, on the sending
        // thread: after `NewConn` on the accept thread, after `Disconnect` on
        // the connection's own.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let (active, seen) = (active.clone(), seen.clone());
            Arc::new(move || seen.lock().unwrap().push(active.load(Ordering::Acquire)))
        };
        let counted = active.clone();
        std::thread::spawn(move || {
            accept_loop_counting(listener, tx, wake, full_access(), policy, counted)
        });

        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect control peer");
        let (conn_id, _) = recv_new_conn(&rx, Duration::from_secs(2));
        // The server closes the idle connection; wait for its EOF.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut byte = [0u8; 1];
        loop {
            assert!(
                Instant::now() < deadline,
                "the idle connection never closed"
            );
            match client.wait_readable(Duration::from_millis(50)) {
                Ok(false) => {}
                Ok(true) => match std::io::Read::read(&mut client, &mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                },
                // A Windows named pipe reports the server's close as a failed
                // peek (ERROR_BROKEN_PIPE), not as readable.
                Err(_) => break,
            }
        }
        assert!(
            matches!(
                rx.try_recv(),
                Ok(CtlServerMsg::Disconnect { conn_id: ended }) if ended == conn_id
            ),
            "the client saw EOF before the App heard Disconnect"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "Disconnect never woke the App");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [1, 0],
            "the connection was counted as it was admitted, and no longer counted \
             when the App heard it ended"
        );
    }

    fn recv_new_conn(rx: &Receiver<CtlServerMsg>, timeout: Duration) -> (u64, Sender<Event>) {
        let deadline = Instant::now() + timeout;
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(CtlServerMsg::NewConn { conn_id, event_tx }) => {
                    return (conn_id, event_tx);
                }
                Ok(CtlServerMsg::Disconnect { conn_id }) => {
                    panic!("connection {conn_id} expired before all peers were admitted");
                }
                Ok(_) => {}
                Err(error) => panic!("timed out waiting for NewConn: {error}"),
            }
        }
    }

    fn prove_fresh_request_is_served(endpoint: &str, rx: &Receiver<CtlServerMsg>) {
        let mut client = kettle_ctl::transport::connect(endpoint).expect("connect fresh client");
        let (conn_id, _event_tx) = recv_new_conn(rx, Duration::from_secs(2));
        client
            .write_all_until(
                br#"{"v":1,"id":77,"method":"get_state","params":{}}
"#,
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .expect("send fresh request");

        let reply = loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(CtlServerMsg::Request {
                    conn_id: request_conn,
                    request,
                    reply,
                    ..
                }) if request_conn == conn_id => {
                    assert_eq!(request.request().id, 77);
                    break reply;
                }
                Ok(_) => {}
                Err(error) => panic!("fresh request was not dispatched: {error}"),
            }
        };
        reply
            .send(Response::ok(77, serde_json::json!({"served": true})))
            .expect("reply to fresh request");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut response = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                client
                    .wait_readable(remaining)
                    .expect("wait for fresh response"),
                "fresh response never became readable"
            );
            let mut chunk = [0u8; 128];
            let read = client.read(&mut chunk).expect("read fresh response");
            assert_ne!(read, 0, "fresh response closed before its newline");
            response.extend_from_slice(&chunk[..read]);
            if response.ends_with(b"\n") {
                break;
            }
            assert!(response.len() < 512, "fresh response exceeded fixture cap");
        }
        let response: Response =
            serde_json::from_slice(response.strip_suffix(b"\n").expect("response newline"))
                .expect("parse fresh response");
        assert!(response.ok);
        assert_eq!(response.result["served"], true);
    }

    #[test]
    fn off_mode_is_disabled_others_enabled() {
        use kettle_config::AgentServer;
        assert!(!AgentServer::Off.is_enabled());
        assert!(AgentServer::ReadOnly.is_enabled());
        assert!(AgentServer::Full.is_enabled());
        // Only Full permits mutation.
        assert!(!AgentServer::Off.allows_mutation());
        assert!(!AgentServer::ReadOnly.allows_mutation());
        assert!(AgentServer::Full.allows_mutation());
    }

    /// With both settings off there is nothing to serve: no socket, no
    /// discovery entry.
    #[test]
    fn a_policy_that_allows_nothing_binds_nothing() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        assert!(
            CtlServer::start(
                CtlPolicy::new(kettle_config::AgentServer::Off, false),
                std::process::id(),
                "test",
                0,
                wake,
            )
            .is_none()
        );
    }

    /// Read one newline-terminated response from `client`.
    fn read_response(client: &mut CtlStream, timeout: Duration) -> Response {
        let deadline = Instant::now() + timeout;
        let mut line = Vec::new();
        while !line.ends_with(b"\n") {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                client.wait_readable(remaining).expect("wait for response"),
                "no response within {timeout:?}"
            );
            let mut chunk = [0u8; 512];
            let read = client.read(&mut chunk).expect("read response");
            assert_ne!(read, 0, "connection closed before a full response");
            line.extend_from_slice(&chunk[..read]);
        }
        serde_json::from_slice(&line).expect("parse response")
    }

    fn send_request(client: &mut CtlStream, id: u64, method: &str) {
        let line = format!("{{\"v\":1,\"id\":{id},\"method\":\"{method}\",\"params\":{{}}}}\n");
        client
            .write_all_until(
                line.as_bytes(),
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .expect("send request");
    }

    fn quick_policy() -> ConnectionPolicy {
        ConnectionPolicy {
            request_idle: Duration::from_secs(5),
            frame_assembly: Duration::from_secs(1),
            write: Duration::from_secs(1),
            response_wait: Duration::from_secs(2),
            subscriber_keepalive: Duration::from_secs(5),
        }
    }

    /// A display-only connection is refused every read and mutation on its
    /// own thread, `wait_for` included: nothing reaches the App and no
    /// connection-thread work starts.
    #[test]
    fn display_only_connection_refuses_reads_and_mutations_before_dispatch() {
        use kettle_ctl::protocol::{Capability, error_codes};
        let access = SharedCtlPolicy::new(CtlPolicy::new(kettle_config::AgentServer::Off, true));
        let (endpoint, rx) = start_test_accept_loop_with("display-only", access, quick_policy());
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
        let (conn_id, _event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
        let mut refused = 0;
        for (index, method) in Method::ALL.iter().enumerate() {
            let expected = match method.capability() {
                Capability::Read => error_codes::DISPLAY_ONLY,
                Capability::Mutate => error_codes::READ_ONLY,
                Capability::Display => continue,
            };
            let id = index as u64 + 1;
            send_request(&mut client, id, method.as_str());
            let response = read_response(&mut client, Duration::from_secs(2));
            assert_eq!(response.id, id);
            assert!(!response.ok, "{method:?} was allowed");
            assert_eq!(
                response.error.expect("refusal").code,
                expected,
                "{method:?}"
            );
            refused += 1;
        }
        assert!(
            refused >= Method::ALL.len() - 1,
            "every existing method was tried"
        );
        while let Ok(message) = rx.try_recv() {
            assert!(
                !matches!(message, CtlServerMsg::Request { conn_id: c, .. } if c == conn_id),
                "a refused request reached the App"
            );
        }
    }

    fn send_line(client: &mut CtlStream, line: &str) {
        client
            .write_all_until(
                format!("{line}\n").as_bytes(),
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .expect("send line");
    }

    fn assert_nothing_dispatched(rx: &Receiver<CtlServerMsg>, conn_id: u64) {
        while let Ok(message) = rx.try_recv() {
            assert!(
                !matches!(message, CtlServerMsg::Request { conn_id: c, .. } if c == conn_id),
                "a request that admission answered reached the App"
            );
        }
    }

    /// Under `read-only`, every mutation is refused on its connection thread
    /// with the policy's text.
    #[test]
    fn read_only_connection_refuses_mutations_before_dispatch() {
        use kettle_ctl::protocol::Capability;
        let access =
            SharedCtlPolicy::new(CtlPolicy::new(kettle_config::AgentServer::ReadOnly, false));
        let (endpoint, rx) = start_test_accept_loop_with("read-only", access, quick_policy());
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
        let (conn_id, _event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
        for (index, method) in Method::ALL
            .iter()
            .filter(|method| method.capability() == Capability::Mutate)
            .enumerate()
        {
            let id = index as u64 + 1;
            send_request(&mut client, id, method.as_str());
            let response = read_response(&mut client, Duration::from_secs(2));
            assert_eq!(response.id, id);
            let error = response.error.expect("refusal");
            assert_eq!(error.code, kettle_ctl::protocol::error_codes::READ_ONLY);
            assert_eq!(error.message, kettle_ctl::policy::READ_ONLY_MESSAGE);
        }
        assert_nothing_dispatched(&rx, conn_id);
    }

    /// A refused request learns nothing about its parameters, and a request
    /// the policy allows is still checked for shape before any dispatch.
    #[test]
    fn authorization_precedes_parameter_validation() {
        use kettle_ctl::protocol::error_codes;
        let display_only =
            SharedCtlPolicy::new(CtlPolicy::new(kettle_config::AgentServer::Off, true));
        let (endpoint, rx) =
            start_test_accept_loop_with("refusal-first", display_only, quick_policy());
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
        let (conn_id, _event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
        for (id, line) in [
            (1, r#"{"v":1,"id":1,"method":"wait_for","params":5}"#),
            (
                2,
                r#"{"v":1,"id":2,"method":"wait_for","params":{"regex":"("}}"#,
            ),
            (3, r#"{"v":1,"id":3,"method":"read_screen","params":"x"}"#),
        ] {
            send_line(&mut client, line);
            let response = read_response(&mut client, Duration::from_secs(2));
            assert_eq!(response.id, id);
            assert_eq!(
                response.error.expect("refusal").code,
                error_codes::DISPLAY_ONLY
            );
        }
        assert_nothing_dispatched(&rx, conn_id);

        let (endpoint, rx) = start_test_accept_loop("shape-before-dispatch", quick_policy());
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
        let (conn_id, _event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
        send_line(
            &mut client,
            r#"{"v":1,"id":4,"method":"read_screen","params":7}"#,
        );
        let response = read_response(&mut client, Duration::from_secs(2));
        assert_eq!(response.id, 4);
        assert_eq!(
            response.error.expect("bad params").code,
            error_codes::BAD_PARAMS
        );
        send_request(&mut client, 5, "no_such_method");
        let response = read_response(&mut client, Duration::from_secs(2));
        assert_eq!(response.id, 5);
        assert_eq!(
            response.error.expect("unknown").code,
            error_codes::UNKNOWN_METHOD
        );
        assert_nothing_dispatched(&rx, conn_id);
    }

    /// A malformed `subscribe` is answered by admission and the connection
    /// keeps answering requests.
    #[test]
    fn a_malformed_subscribe_never_starts_streaming() {
        let (endpoint, rx) = start_test_accept_loop("malformed-subscribe", quick_policy());
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
        let (conn_id, event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
        send_line(
            &mut client,
            r#"{"v":1,"id":1,"method":"subscribe","params":[]}"#,
        );
        assert!(!read_response(&mut client, Duration::from_secs(2)).ok);
        event_tx
            .try_send(Event::new(
                "output",
                None,
                serde_json::json!({"leak": true}),
            ))
            .expect("queue an event");
        send_request(&mut client, 2, "get_state");
        let reply = loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(CtlServerMsg::Request {
                    conn_id: c,
                    request,
                    reply,
                }) if c == conn_id => {
                    assert_eq!(request.method(), Method::GetState);
                    break reply;
                }
                Ok(_) => {}
                Err(error) => panic!("get_state was not dispatched: {error}"),
            }
        };
        reply
            .send(Response::ok(2, serde_json::json!({"served": true})))
            .expect("reply");
        let response = read_response(&mut client, Duration::from_secs(2));
        assert_eq!(response.id, 2);
        assert_eq!(response.result["served"], true);
    }

    /// `wait_for`'s probes are fixed `read_screen` requests under the wait's
    /// own id, marked internal, and address only the pinned pane.
    #[test]
    fn wait_for_probes_carry_only_read_screen_authority() {
        let wait = admit(
            CtlPolicy::new(kettle_config::AgentServer::ReadOnly, false),
            Request {
                v: 1,
                id: 9,
                method: "wait_for".into(),
                params: serde_json::json!({"text": "$", "pane": 3, "regex": "x"}),
                caller: None,
            },
        )
        .expect("read-only admits wait_for");
        assert!(!wait.internal_probe());
        let probe = wait.read_screen_probe(Some(&serde_json::json!(3)));
        assert_eq!(probe.method(), Method::ReadScreen);
        assert!(probe.internal_probe());
        assert_eq!(probe.request().id, 9);
        assert_eq!(probe.request().method, "read_screen");
        assert_eq!(probe.request().params, serde_json::json!({"pane": 3}));
        let unpinned = wait.read_screen_probe(None);
        assert_eq!(unpinned.request().params, serde_json::json!({}));
        assert_eq!(
            Method::WaitFor.capability(),
            Method::ReadScreen.capability(),
            "a probe must not carry more authority than its wait"
        );
    }

    /// The first nonblank frame fixes the connection's claim; nothing after
    /// it can upgrade a missing or invalid one or switch to another process.
    #[test]
    fn the_first_frame_fixes_the_claim() {
        let claim = |pid: u32| PeerClaim {
            pid: std::num::NonZeroU32::new(pid).unwrap(),
            start_token: Some(kettle_ctl::protocol::StartToken(5)),
            pane_hint: None,
            pid_hint: None,
        };
        let mut latch = ClaimLatch::default();
        assert_eq!(latch.claim(), Err(UnverifiedReason::MissingClaim));
        latch.frame(Some(claim(7)));
        latch.frame(None);
        latch.frame(Some(claim(7)));
        assert_eq!(latch.claim(), Ok(claim(7)), "repeats and omissions keep it");
        latch.frame(Some(claim(8)));
        assert_eq!(latch.claim(), Err(UnverifiedReason::ClaimChanged));
        latch.frame(Some(claim(7)));
        assert_eq!(
            latch.claim(),
            Err(UnverifiedReason::ClaimChanged),
            "permanent"
        );

        let mut missing = ClaimLatch::default();
        missing.frame(None);
        missing.frame(Some(claim(7)));
        assert_eq!(missing.claim(), Err(UnverifiedReason::MissingClaim));

        let mut invalid = ClaimLatch::default();
        invalid.invalid_frame();
        invalid.frame(Some(claim(7)));
        assert_eq!(invalid.claim(), Err(UnverifiedReason::InvalidClaim));

        let mut late_invalid = ClaimLatch::default();
        late_invalid.frame(Some(claim(7)));
        late_invalid.invalid_frame();
        assert_eq!(
            late_invalid.claim(),
            Ok(claim(7)),
            "a later bad frame changes nothing"
        );
    }

    /// Over a real connection, `get_state` reaches the App carrying the
    /// check of the connection's own claim, and no other method pays for one.
    #[test]
    fn get_state_carries_the_connections_caller_check() {
        let (endpoint, rx) = start_test_accept_loop("caller-check", quick_policy());
        let me = kettle_ctl::process::current()
            .expect("inspect self")
            .identity;
        let claimed = format!(
            r#""caller":{{"pid":{},"start_token":"{}"}}"#,
            me.pid(),
            me.start()
        );
        let dispatched = |line: String| {
            let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
            let (conn_id, _event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
            send_line(&mut client, &line);
            loop {
                match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(CtlServerMsg::Request {
                        conn_id: c,
                        request,
                        reply,
                    }) if c == conn_id => {
                        reply
                            .send(Response::ok(request.request().id, serde_json::json!({})))
                            .ok();
                        let _ = read_response(&mut client, Duration::from_secs(2));
                        // Kept open: a closed connection's Disconnect would
                        // reach the next connection's admission first.
                        break (request, client);
                    }
                    Ok(_) => {}
                    Err(error) => panic!("not dispatched: {error}"),
                }
            }
        };
        let claim_errors = [
            UnverifiedReason::MissingClaim,
            UnverifiedReason::InvalidClaim,
            UnverifiedReason::ClaimChanged,
            UnverifiedReason::ClaimMismatch,
        ];
        let (state, _first) = dispatched(format!(
            r#"{{"v":1,"id":1,"method":"get_state",{claimed}}}"#
        ));
        match state.caller().chain().expect("get_state is checked") {
            // This process is its own peer, so its chain starts with itself.
            Ok(chain) => assert_eq!(chain[0], me),
            // Above the test runner the walk may meet another user's process.
            Err(reason) => assert!(!claim_errors.contains(&reason), "{reason:?}"),
        }
        let (bare, _second) = dispatched(r#"{"v":1,"id":2,"method":"get_state"}"#.to_string());
        assert_eq!(
            bare.caller().chain().map(|chain| chain.err()),
            Some(Some(UnverifiedReason::MissingClaim))
        );
        let (other, _third) = dispatched(format!(
            r#"{{"v":1,"id":3,"method":"list_panes",{claimed}}}"#
        ));
        assert!(
            other.caller().chain().is_none(),
            "only get_state is checked"
        );
    }

    /// Streaming starts only after the App accepts `subscribe`. A refused
    /// subscribe leaves the connection answering requests, and events queued
    /// for it are never written.
    #[test]
    fn a_refused_subscribe_never_starts_streaming() {
        let (endpoint, rx) = start_test_accept_loop("refused-subscribe", quick_policy());
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect");
        let (conn_id, event_tx) = recv_new_conn(&rx, Duration::from_secs(2));
        let answer = |id: u64, response: Response| loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(CtlServerMsg::Request {
                    conn_id: c,
                    request,
                    reply,
                }) if c == conn_id => {
                    assert_eq!(request.request().id, id);
                    reply.send(response).expect("reply");
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("request {id} was not dispatched: {error}"),
            }
        };
        send_request(&mut client, 1, "subscribe");
        answer(
            1,
            Response::err(1, kettle_ctl::protocol::error_codes::INTERNAL, "refused"),
        );
        assert!(!read_response(&mut client, Duration::from_secs(2)).ok);
        event_tx
            .try_send(Event::new(
                "output",
                None,
                serde_json::json!({"leak": true}),
            ))
            .expect("queue an event");
        send_request(&mut client, 2, "get_state");
        answer(2, Response::ok(2, serde_json::json!({"served": true})));
        let response = read_response(&mut client, Duration::from_secs(2));
        assert_eq!(response.id, 2);
        assert_eq!(response.result["served"], true);
    }

    /// The typed protocol table replaces parallel string allowlists. Every
    /// connection-thread method must have an explicit worker dispatch path.
    /// Admission precedes that route, so any capability may use it.
    #[test]
    fn connection_thread_methods_have_worker_dispatch() {
        for method in Method::ALL {
            if method.execution() == Execution::Connection {
                let name = method.as_str();
                let src = production_source();
                assert!(
                    src.contains("if request.method().execution() == Execution::Connection {\n                let resp = wait_for_poll("),
                    "connection-thread method {name} has no connection_loop dispatch"
                );
            }
        }
    }

    /// Build a bare `CtlServer` for table-level unit tests: no listener thread,
    /// an empty conn table, a temp registry dir (so `Drop`'s `unregister`
    /// remove_file is a harmless no-op). Returns the server plus the inbound
    /// channel's sender, kept alive so the receiver in `rx` stays open.
    fn test_server() -> (CtlServer, Sender<CtlServerMsg>) {
        let (tx, rx) = crossbeam_channel::unbounded::<CtlServerMsg>();
        let accept = std::thread::Builder::new()
            .spawn(|| {})
            .expect("spawn noop");
        let server = CtlServer {
            policy: full_access(),
            rx,
            conns: HashMap::new(),
            registry_dir: std::env::temp_dir(),
            alias_dir: None,
            pid: 0,
            endpoint: String::new(),
            _accept: accept,
        };
        (server, tx)
    }

    /// The listener lives on the accept thread, which is still blocked when
    /// the process exits, so the server itself must unlink its socket or every
    /// exit leaves one behind in the registry directory.
    #[cfg(unix)]
    #[test]
    fn dropping_the_server_unlinks_its_socket() {
        let path =
            std::env::temp_dir().join(format!("kettle-ctl-drop-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Still listening, as the accept thread's listener is at exit.
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind test socket");
        let (mut server, _tx) = test_server();
        server.endpoint = path.to_string_lossy().into_owned();
        drop(server);
        assert!(!path.exists(), "the server's socket must not outlive it");
        drop(listener);
    }

    fn dummy_event_tx() -> Sender<Event> {
        // The table tests only check membership, never the event channel, so a
        // dropped receiver is fine: a `Sender` stays valid after its `Receiver`
        // is gone (sends would just fail — nothing here sends).
        let (tx, _rx) = crossbeam_channel::bounded::<Event>(EVENT_QUEUE_CAP);
        tx
    }

    /// `accept_loop` is the single cap gate (atomic `active`), so `add_conn`
    /// must ALWAYS insert or `conns` could diverge from the connections
    /// `accept_loop` admitted. Register exactly MAX_CONNECTIONS connections
    /// (what the `active` gate permits) and assert every one is tracked.
    #[test]
    fn add_conn_always_inserts_up_to_cap() {
        let (mut server, _tx) = test_server();
        for id in 0..MAX_CONNECTIONS as u64 {
            server.add_conn(id, dummy_event_tx());
        }
        // `active` (the source-of-truth counter) admitted MAX_CONNECTIONS; the
        // conn table must agree exactly — no admitted connection went untracked.
        assert_eq!(server.conns.len(), MAX_CONNECTIONS);
        for id in 0..MAX_CONNECTIONS as u64 {
            assert!(
                server.conns.contains_key(&id),
                "conn {id} admitted by accept_loop must be tracked by add_conn"
            );
        }
    }

    /// Membership and the admission count agree across a Disconnect/NewConn
    /// reorder at the cap. With the table full, `accept_loop` drops one
    /// connection (decrementing `active`) and admits a replacement
    /// (incrementing `active` back to the cap). The App may process the new
    /// `NewConn` BEFORE the `Disconnect`, while `conns.len()` still reads full.
    /// A `conns.len() >= MAX_CONNECTIONS` guard would drop the replacement;
    /// `add_conn` always inserts it, so once the reorder settles `conns` holds
    /// exactly the admitted set.
    #[test]
    fn add_conn_survives_disconnect_newconn_reorder_at_cap() {
        let (mut server, _tx) = test_server();
        for id in 0..MAX_CONNECTIONS as u64 {
            server.add_conn(id, dummy_event_tx());
        }
        assert_eq!(server.conns.len(), MAX_CONNECTIONS);
        // Reordered: the replacement (id == cap) is admitted by accept_loop's
        // `active` gate and inserted here while the table still reads full...
        let replacement = MAX_CONNECTIONS as u64;
        server.add_conn(replacement, dummy_event_tx());
        assert!(
            server.conns.contains_key(&replacement),
            "replacement admitted at the cap must NOT be silently dropped"
        );
        // ...then the lagging Disconnect for the evicted connection (id 0)
        // lands; remove_conn no-ops if already gone, so this is always safe.
        server.remove_conn(0);
        // Net: exactly MAX_CONNECTIONS tracked — one in, one out — matching the
        // atomic `active` count accept_loop maintains.
        assert_eq!(server.conns.len(), MAX_CONNECTIONS);
        assert!(!server.conns.contains_key(&0));
        assert!(server.conns.contains_key(&replacement));
    }

    #[test]
    fn spawn_failure_rolls_back_without_registering_connection() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (event_tx, _event_rx) = crossbeam_channel::bounded(1);
        let (start_tx, _start_rx) = std::sync::mpsc::sync_channel(0);
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let spawned: std::io::Result<std::thread::JoinHandle<()>> =
            Err(std::io::Error::other("injected spawn failure"));

        finish_worker_spawn(spawned, 7, event_tx, &tx, &wake, start_tx, &active);

        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert!(rx.try_recv().is_err(), "failed worker must not register");
    }

    #[test]
    fn eight_idle_peers_expire_and_a_fresh_request_is_served() {
        let policy = ConnectionPolicy {
            request_idle: Duration::from_millis(750),
            frame_assembly: Duration::from_millis(200),
            write: Duration::from_millis(200),
            response_wait: Duration::from_secs(1),
            subscriber_keepalive: Duration::from_millis(200),
        };
        let (endpoint, rx) = start_test_accept_loop("idle-cap", policy);
        let mut stalled = Vec::with_capacity(MAX_CONNECTIONS);
        for _ in 0..MAX_CONNECTIONS {
            stalled.push(
                kettle_ctl::transport::connect(&endpoint).expect("connect idle control peer"),
            );
            recv_new_conn(&rx, Duration::from_secs(1));
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut disconnected = HashSet::new();
        while disconnected.len() < MAX_CONNECTIONS {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(CtlServerMsg::Disconnect { conn_id }) => {
                    disconnected.insert(conn_id);
                }
                Ok(_) => {}
                Err(error) => panic!("idle slots were not reclaimed: {error}"),
            }
        }
        assert_eq!(disconnected.len(), MAX_CONNECTIONS);
        prove_fresh_request_is_served(&endpoint, &rx);
        drop(stalled);
    }

    /// The frame budget bounds time the server spends *waiting on the client*,
    /// which is not the same as wall-clock since the bytes arrived. A client is
    /// allowed to pipeline the head of its next request behind one the server
    /// answers slowly — `wait_for` deliberately blocks its own connection
    /// thread — and those pipelined bytes must not have their budget consumed
    /// by the server's own work. Anchoring the next frame at true arrival time
    /// instead would disconnect a well-behaved client the instant a legitimate
    /// long request outlived the assembly budget.
    #[test]
    fn a_slow_reply_does_not_consume_the_next_frames_budget() {
        let policy = ConnectionPolicy {
            request_idle: Duration::from_secs(5),
            frame_assembly: Duration::from_millis(200),
            write: Duration::from_secs(1),
            response_wait: Duration::from_secs(5),
            subscriber_keepalive: Duration::from_secs(5),
        };
        let (endpoint, rx) = start_test_accept_loop("pipelined-behind-slow", policy);
        let mut client =
            kettle_ctl::transport::connect(&endpoint).expect("connect pipelining peer");
        let (conn_id, _) = recv_new_conn(&rx, Duration::from_secs(1));

        // One write: a complete request, plus the first byte of the next one.
        client
            .write_all_until(
                b"{\"v\":1,\"id\":1,\"method\":\"get_state\",\"params\":{}}\n{",
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .expect("send request with a pipelined partial frame");

        let reply = loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(CtlServerMsg::Request {
                    conn_id: request_conn,
                    request,
                    reply,
                    ..
                }) if request_conn == conn_id => {
                    assert_eq!(request.request().id, 1);
                    break reply;
                }
                Ok(CtlServerMsg::Disconnect { conn_id: gone }) if gone == conn_id => {
                    panic!("peer was dropped before its first request was dispatched")
                }
                Ok(_) => {}
                Err(error) => panic!("first request was not dispatched: {error}"),
            }
        };

        // Answer well after the assembly budget would have expired, as a real
        // `wait_for` does. The pipelined `{` arrived before this delay began.
        std::thread::sleep(Duration::from_millis(600));
        reply
            .send(Response::ok(1, serde_json::json!({"served": true})))
            .expect("reply to the slow request");

        // Finish the second frame. It must still be accepted.
        client
            .write_all_until(
                b"\"v\":1,\"id\":2,\"method\":\"get_state\",\"params\":{}}\n",
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .expect("complete the pipelined frame");

        loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(CtlServerMsg::Request {
                    conn_id: request_conn,
                    request,
                    reply,
                    ..
                }) if request_conn == conn_id => {
                    assert_eq!(
                        request.request().id,
                        2,
                        "the pipelined request must be the one served"
                    );
                    let _ = reply.send(Response::ok(2, serde_json::json!({"served": true})));
                    break;
                }
                Ok(CtlServerMsg::Disconnect { conn_id: gone }) if gone == conn_id => {
                    panic!("a slow reply consumed the pipelined frame's assembly budget")
                }
                Ok(_) => {}
                Err(error) => panic!("pipelined request was not dispatched: {error}"),
            }
        }
    }

    #[test]
    fn slow_drip_cannot_extend_the_absolute_frame_deadline() {
        let policy = ConnectionPolicy {
            request_idle: Duration::from_secs(2),
            frame_assembly: Duration::from_millis(200),
            write: Duration::from_millis(200),
            response_wait: Duration::from_secs(1),
            subscriber_keepalive: Duration::from_millis(200),
        };
        let (endpoint, rx) = start_test_accept_loop("slow-drip", policy);
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect slow peer");
        let (conn_id, _) = recv_new_conn(&rx, Duration::from_secs(1));
        let started = Instant::now();
        for byte in b"{    " {
            if client
                .write_all_until(
                    std::slice::from_ref(byte),
                    Instant::now() + Duration::from_millis(100),
                    None,
                )
                .is_err()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(CtlServerMsg::Disconnect {
                    conn_id: disconnected,
                }) if disconnected == conn_id => break,
                Ok(_) => {}
                Err(error) => panic!("slow-drip peer retained its slot: {error}"),
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "per-byte activity extended the frame deadline: {:?}",
            started.elapsed()
        );
        prove_fresh_request_is_served(&endpoint, &rx);
    }

    #[test]
    fn unread_subscriber_write_times_out_and_releases_its_slot() {
        let policy = ConnectionPolicy {
            request_idle: Duration::from_secs(1),
            frame_assembly: Duration::from_millis(200),
            write: Duration::from_millis(100),
            response_wait: Duration::from_secs(1),
            subscriber_keepalive: Duration::from_secs(1),
        };
        let (endpoint, rx) = start_test_accept_loop("subscriber-backpressure", policy);
        let mut client = kettle_ctl::transport::connect(&endpoint).expect("connect subscriber");
        let (conn_id, event_tx) = recv_new_conn(&rx, Duration::from_secs(1));
        client
            .write_all_until(
                br#"{"v":1,"id":1,"method":"subscribe","params":{}}
"#,
                Instant::now() + Duration::from_secs(1),
                None,
            )
            .expect("send subscribe request");
        let reply = loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(CtlServerMsg::Request { reply, request, .. }) => {
                    assert_eq!(request.method(), Method::Subscribe);
                    break reply;
                }
                Ok(_) => {}
                Err(error) => panic!("subscribe was not dispatched: {error}"),
            }
        };
        reply
            .send(Response::ok(1, serde_json::json!({"subscribed": true})))
            .expect("reply to subscribe");

        let payload = "x".repeat(48 * 1024);
        for sequence in 0..EVENT_QUEUE_CAP {
            if event_tx
                .try_send(Event::new(
                    "output",
                    None,
                    serde_json::json!({"sequence": sequence, "text": payload.as_str()}),
                ))
                .is_err()
            {
                break;
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(CtlServerMsg::Disconnect {
                    conn_id: disconnected,
                }) if disconnected == conn_id => break,
                Ok(_) => {}
                Err(error) => panic!("unread subscriber pinned its writer: {error}"),
            }
        }
        prove_fresh_request_is_served(&endpoint, &rx);
    }

    #[test]
    fn oversize_response_becomes_bounded_structured_error() {
        let response = Response::ok(
            42,
            serde_json::json!({"text": "x".repeat(kettle_ctl::protocol::MAX_RESPONSE_LINE_BYTES)}),
        );
        let line = serialized_response_line(&response).unwrap();
        assert!(line.len() <= kettle_ctl::protocol::MAX_RESPONSE_LINE_BYTES);
        let response: Response = serde_json::from_slice(&line).unwrap();
        assert_eq!(response.id, 42);
        assert_eq!(
            response.error.unwrap().code,
            kettle_ctl::protocol::error_codes::RESPONSE_TOO_LARGE
        );
    }

    #[test]
    fn oversize_event_is_replaced_before_it_enters_subscriber_queue() {
        let (mut server, _tx) = test_server();
        let (event_tx, event_rx) = crossbeam_channel::bounded(EVENT_QUEUE_CAP);
        server.add_conn(1, event_tx);
        server.set_subscribed(1);
        server.broadcast(&Event::new(
            "output",
            Some(9),
            serde_json::json!("x".repeat(MAX_EVENT_BYTES)),
        ));
        let event = event_rx.recv().unwrap();
        assert_eq!(event.event, "lag");
        assert_eq!(event.data["reason"], "event_too_large");
    }

    #[test]
    fn saturated_subscriber_queue_retains_a_lag_notice() {
        let (mut server, _tx) = test_server();
        let (event_tx, event_rx) = crossbeam_channel::bounded(EVENT_CHANNEL_CAP);
        server.add_conn(1, event_tx.clone());
        server.set_subscribed(1);
        for seq in 0..EVENT_QUEUE_CAP {
            event_tx
                .try_send(Event::new("output", None, serde_json::json!({"seq": seq})))
                .unwrap();
        }

        server.broadcast(&Event::new(
            "output",
            None,
            serde_json::json!({"seq": "lost"}),
        ));

        assert_eq!(event_rx.len(), EVENT_CHANNEL_CAP);
        let events: Vec<_> = event_rx.try_iter().collect();
        let lag = events.last().expect("reserved lag event");
        assert_eq!(lag.event, "lag");
        assert_eq!(lag.data["reason"], "queue_full");
    }

    /// Drift guard: admission is the one policy gate and comes before every
    /// route a request can take, and the App dispatches every typed method
    /// without a gate of its own.
    #[test]
    fn every_typed_method_is_dispatched_behind_the_admission_gate() {
        let server = kettle_test_support::production_source(include_str!("ctl_server.rs"));
        let admit = server
            .split_once("fn admit(policy: CtlPolicy, req: Request)")
            .expect("admit present")
            .1;
        let admit = &admit[..admit.find("\n}\n").expect("end of admit")];
        let gate = admit
            .find("policy.check(method.capability())")
            .expect("admission checks the policy");
        let shape = admit
            .find("!req.params.is_object()")
            .expect("admission checks the params shape");
        assert!(gate < shape, "authorization must precede parameter checks");

        let connection = server
            .split_once("fn connection_loop(")
            .expect("connection_loop present")
            .1;
        let admitted = connection
            .find(".and_then(|req| admit(access.current(), req))")
            .expect("connection_loop admits each request");
        for route in [
            "request.method().execution() == Execution::Connection",
            "tx.send(CtlServerMsg::Request {",
        ] {
            let at = connection.find(route).unwrap_or_else(|| panic!("{route}"));
            assert!(admitted < at, "{route} must follow admission");
        }
        assert_eq!(
            server.matches("CtlServerMsg::Request {").count(),
            2,
            "client requests and wait_for probes are the only requests sent to the App"
        );

        let app = kettle_test_support::production_source(include_str!("app.rs"));
        let handler = app
            .split_once("fn handle_ctl_request(")
            .expect("handle_ctl_request present")
            .1;
        let dispatch = handler
            .find("let resp = match method {")
            .expect("dispatch block present");
        assert!(
            !handler[..dispatch].contains(".check("),
            "the App must not keep a second gate that can drift from admission"
        );
        let block = &handler[dispatch..(dispatch + 3600).min(handler.len())];
        for method in Method::ALL {
            let variant = format!("Method::{method:?}");
            assert!(
                block.contains(&variant),
                "typed method {} is not dispatched in handle_ctl_request",
                method.as_str()
            );
        }
    }
}
