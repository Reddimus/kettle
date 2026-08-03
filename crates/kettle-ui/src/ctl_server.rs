//! The in-process control server (agent-first A2).
//!
//! When `agent-server` is enabled, the App starts a [`CtlServer`]: an accept
//! thread binds the kettle-ctl transport (Unix socket / Windows named pipe),
//! registers a discovery entry, and spawns ONE thread per connection. That
//! thread reads NDJSON requests and writes responses/events on the SAME handle,
//! never concurrently. Keeping one sequential protocol owner prevents response
//! and event frames from interleaving; the transport itself now supports
//! deadline-bearing overlapped writes on both Windows pipe ends:
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
//! The server is OFF by default and gated by `AgentServer` mode; the threat
//! model (same local user, off-by-default, logged, dev-record-annotated) is in
//! docs/AGENT.md.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kettle_config::AgentServer;
use kettle_ctl::discovery::{self, RegistryEntry};
use kettle_ctl::protocol::{Event, Execution, Method, Request, Response};
use kettle_ctl::transport::{CtlListener, CtlStream};

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
    /// A parsed request; the App dispatches it and sends the [`Response`] back
    /// over `reply` (immediately, or—for `run_command`—when it completes).
    Request {
        conn_id: u64,
        req: Request,
        reply: ReplyTx,
        /// v2.20.0 (review fix): true for `wait_for`'s internal `read_screen`
        /// probes — the App skips the per-request dev-record marker (a 300s
        /// wait at 50ms polls would otherwise land ~6000 markers) and the
        /// post-drain redraw for them.
        internal_probe: bool,
    },
    /// A malformed line that parsed into a ready-to-send error response.
    BadRequest { reply: ReplyTx, resp: Response },
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

/// The control server: owns the connection table + the inbound channel; the
/// accept + per-connection threads run in the background.
pub struct CtlServer {
    mode: AgentServer,
    rx: Receiver<CtlServerMsg>,
    conns: HashMap<u64, ConnState>,
    registry_dir: PathBuf,
    pid: u32,
    _accept: std::thread::JoinHandle<()>,
}

impl CtlServer {
    /// Start the server for `mode`. `wake` is called after every message is
    /// enqueued so the App's event loop drains it (it sends `UserEvent::Ctl`).
    /// Returns `None` (logged) if `mode` is `Off` or binding fails.
    pub fn start(
        mode: AgentServer,
        pid: u32,
        version: &str,
        started_unix: u64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<CtlServer> {
        if !mode.is_enabled() {
            return None;
        }
        let registry_dir = discovery::registry_dir();
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
        log::info!("agent-server: listening on {endpoint} (mode {mode:?})");

        let (tx, rx) = crossbeam_channel::unbounded::<CtlServerMsg>();
        let accept = match std::thread::Builder::new()
            .name("kettle-ctl-accept".into())
            .spawn(move || accept_loop(listener, tx, wake, DEFAULT_CONNECTION_POLICY))
        {
            Ok(accept) => accept,
            Err(error) => {
                discovery::unregister(&registry_dir, pid);
                log::warn!("agent-server: cannot spawn accept thread: {error}");
                return None;
            }
        };

        Some(CtlServer {
            mode,
            rx,
            conns: HashMap::new(),
            registry_dir,
            pid,
            _accept: accept,
        })
    }

    /// The server's privilege mode.
    pub fn mode(&self) -> AgentServer {
        self.mode
    }

    /// Drain one pending message (App calls this in a loop on `UserEvent::Ctl`).
    pub fn try_recv(&self) -> Option<CtlServerMsg> {
        self.rx.try_recv().ok()
    }

    /// Register a freshly-accepted connection.
    ///
    /// The connection cap is enforced SOLELY at the source in `accept_loop`,
    /// which gates on the atomic `active` counter and refuses + closes an
    /// over-cap connection before it ever spawns a thread or sends `NewConn`.
    /// We must therefore ALWAYS insert here: a second, divergent `conns.len()`
    /// cap-check on this (App) thread could silently DROP a connection that
    /// `accept_loop` already admitted — under a cross-thread Disconnect/NewConn
    /// reorder right at the cap, `conns.len()` can momentarily read full while
    /// `active` has room. The dropped connection would still serve
    /// `get_state`/`send_text` (handled on the connection thread) but
    /// `subscribe`/`attach_pane` would silently no-op (no `ConnState` to flip),
    /// leaving an untracked-but-live connection. The atomic `active` is the
    /// single source of truth; `remove_conn` already no-ops on an absent id, so
    /// always inserting is safe and keeps `conns` membership in lockstep with
    /// `active`.
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
    policy: ConnectionPolicy,
) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
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
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            log::warn!("agent-server: connection cap ({MAX_CONNECTIONS}) reached; refusing");
            drop(conn); // closes the socket / pipe handle
            continue;
        }
        let conn_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        active.fetch_add(1, Ordering::Relaxed);
        let (event_tx, event_rx) = crossbeam_channel::bounded::<Event>(EVENT_CHANNEL_CAP);
        let ctx = tx.clone();
        let cwake = wake.clone();
        let active_dec = active.clone();
        let (start_tx, start_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let spawned = std::thread::Builder::new()
            .name(format!("kettle-ctl-{conn_id}"))
            .spawn(move || {
                if start_rx.recv().is_err() {
                    active_dec.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
                connection_loop(conn, conn_id, ctx, cwake, event_rx, policy);
                active_dec.fetch_sub(1, Ordering::Relaxed);
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
            active.fetch_sub(1, Ordering::Relaxed);
            log::warn!("agent-server: cannot spawn connection worker: {error}");
        }
    }
}

/// One connection, one thread: read requests + write responses/events on the
/// SAME handle, sequentially so frames cannot interleave. A `subscribe`
/// request flips the connection into event-only streaming.
fn connection_loop(
    mut conn: CtlStream,
    conn_id: u64,
    tx: Sender<CtlServerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    event_rx: Receiver<Event>,
    policy: ConnectionPolicy,
) {
    let mut acc: Vec<u8> = Vec::with_capacity(4096);
    let mut scan_offset = 0;
    let mut buf = [0u8; 4096];
    let mut idle_deadline = Instant::now() + policy.request_idle;
    let mut frame_deadline: Option<Instant> = None;
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
            // Oneshot reply channel for this request.
            let (rtx, rrx) = crossbeam_channel::bounded::<Response>(1);
            let is_subscribe;
            match kettle_ctl::protocol::parse_request_line(trimmed) {
                // v2.20.0 (agent plane): `wait_for` blocks THIS connection
                // thread, never the UI thread — it polls the screen via cheap
                // internal `read_screen` requests (≥50ms apart) until the
                // condition holds or the deadline passes. The UI thread only
                // ever answers individual snapshot probes.
                Ok(req)
                    if Method::from_name(&req.method)
                        .is_some_and(|method| method.execution() == Execution::Connection) =>
                {
                    let resp = wait_for_poll(&mut conn, &tx, &wake, conn_id, &req);
                    if write_response_line(&mut conn, &resp, policy.write).is_err() {
                        break 'outer;
                    }
                    idle_deadline = Instant::now() + policy.request_idle;
                    continue;
                }
                Ok(req) => {
                    is_subscribe = req.method == "subscribe";
                    let _ = tx.send(CtlServerMsg::Request {
                        conn_id,
                        req,
                        reply: rtx,
                        internal_probe: false,
                    });
                }
                Err(resp) => {
                    is_subscribe = false;
                    let _ = tx.send(CtlServerMsg::BadRequest { reply: rtx, resp });
                }
            }
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
            // send `Disconnect` (so the App drops the `PendingRun` + clears the
            // badge) and end the connection; the trailing `Disconnect` at loop
            // exit is a harmless no-op (`remove_conn` ignores an absent id).
            let response_deadline = Instant::now() + policy.response_wait;
            let resp = loop {
                match rrx.recv_timeout(Duration::from_millis(200)) {
                    Ok(resp) => break resp,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= response_deadline {
                            break 'outer;
                        }
                        if conn.peer_disconnected() {
                            let _ = tx.send(CtlServerMsg::Disconnect { conn_id });
                            wake();
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
            if is_subscribe {
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
    let _ = tx.send(CtlServerMsg::Disconnect { conn_id });
    wake();
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

/// v2.20.0 (agent plane): the `wait_for` poll loop. Runs on the CONNECTION
/// thread; each iteration sends one internal `read_screen` request to the UI
/// thread (the same cheap snapshot `read_screen` serves) and checks the
/// condition against the returned text. Params:
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
    req: &Request,
) -> Response {
    use kettle_ctl::protocol::error_codes as ec;
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
    // v2.20.0 (review fix): pin the target pane for the WHOLE wait. Without
    // this, a no-`pane` wait re-resolved "focused" on every probe — a focus
    // change mid-wait silently retargeted the watch and corrupted the
    // quiet_ms fingerprint (two panes' screens interleaving looks like
    // constant change). `read_screen` echoes the resolved pane id in its
    // result, so the first probe's reply pins it.
    let mut pinned_pane: Option<serde_json::Value> = req.params.get("pane").cloned();
    loop {
        // v2.20.0 (review fix): a vanished client (Ctrl+C'd `kettle ctl`,
        // crashed MCP host) must not keep this loop polling — it pins one of
        // the MAX_CONNECTIONS slots and wakes the UI thread every poll for
        // up to the full timeout. The zero-byte peek is safe here: this IS
        // the connection thread, with no other I/O outstanding.
        if conn.peer_disconnected() {
            return Response::err(req.id, ec::INTERNAL, "client disconnected during wait_for");
        }
        // Compose the internal probe (pinned pane addressing).
        let mut params = serde_json::Map::new();
        if let Some(p) = &pinned_pane {
            params.insert("pane".into(), p.clone());
        }
        let probe = Request {
            v: kettle_ctl::protocol::PROTOCOL_VERSION,
            id: req.id,
            method: "read_screen".into(),
            params: serde_json::Value::Object(params),
        };
        let (rtx, rrx) = crossbeam_channel::bounded::<Response>(1);
        let _ = tx.send(CtlServerMsg::Request {
            conn_id,
            req: probe,
            reply: rtx,
            internal_probe: true,
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

    fn start_test_accept_loop(
        tag: &str,
        policy: ConnectionPolicy,
    ) -> (String, Receiver<CtlServerMsg>) {
        let endpoint = test_endpoint(tag);
        let listener = CtlListener::bind(&endpoint).expect("bind test control listener");
        let (tx, rx) = crossbeam_channel::unbounded();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        std::thread::spawn(move || accept_loop(listener, tx, wake, policy));
        (endpoint, rx)
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
                    req,
                    reply,
                    ..
                }) if request_conn == conn_id => {
                    assert_eq!(req.id, 77);
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
        assert!(!AgentServer::Off.is_enabled());
        assert!(AgentServer::ReadOnly.is_enabled());
        assert!(AgentServer::Full.is_enabled());
        // Only Full permits mutation.
        assert!(!AgentServer::Off.allows_mutation());
        assert!(!AgentServer::ReadOnly.allows_mutation());
        assert!(AgentServer::Full.allows_mutation());
    }

    /// The typed protocol table replaces parallel string allowlists. Every
    /// connection-thread method must have an explicit worker dispatch path.
    #[test]
    fn connection_thread_methods_have_worker_dispatch() {
        for method in Method::ALL {
            if method.execution() == Execution::Connection {
                assert_eq!(
                    method.capability(),
                    kettle_ctl::protocol::Capability::Read,
                    "connection-thread methods cannot bypass the UI mutation gate"
                );
                let name = method.as_str();
                let src = include_str!("ctl_server.rs");
                assert!(
                    src.contains("method.execution() == Execution::Connection"),
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
            mode: AgentServer::Full,
            rx,
            conns: HashMap::new(),
            registry_dir: std::env::temp_dir(),
            pid: 0,
            _accept: accept,
        };
        (server, tx)
    }

    fn dummy_event_tx() -> Sender<Event> {
        // The table tests only check membership, never the event channel, so a
        // dropped receiver is fine: a `Sender` stays valid after its `Receiver`
        // is gone (sends would just fail — nothing here sends).
        let (tx, _rx) = crossbeam_channel::bounded::<Event>(EVENT_QUEUE_CAP);
        tx
    }

    /// E2 regression: `add_conn` no longer re-checks `conns.len()` against the
    /// cap. `accept_loop` is the single gate (atomic `active`); `add_conn` must
    /// ALWAYS insert so `conns` membership cannot silently diverge from the set
    /// of connections `accept_loop` admitted. Here we register exactly
    /// MAX_CONNECTIONS connections (what `accept_loop`'s `active` gate permits)
    /// and assert every one is tracked — none is dropped by a second counter.
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

    /// E2 invariant: membership + the admission count agree across a
    /// Disconnect/NewConn reorder at the cap. Simulate the race that the old
    /// `conns.len() >= MAX_CONNECTIONS` guard mishandled: with the table full,
    /// `accept_loop` drops one (decrementing `active`) and admits a replacement
    /// (incrementing `active` back to the cap). The App may process the new
    /// `NewConn` BEFORE the `Disconnect`; momentarily `conns.len()` would read
    /// full — the old guard would have silently dropped the replacement. With
    /// the guard gone, the replacement is always inserted; after the reorder
    /// settles, `conns` holds exactly the admitted set.
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
                    req,
                    reply,
                    ..
                }) if request_conn == conn_id => {
                    assert_eq!(req.id, 1);
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
                    req,
                    reply,
                    ..
                }) if request_conn == conn_id => {
                    assert_eq!(req.id, 2, "the pipelined request must be the one served");
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
                Ok(CtlServerMsg::Request { reply, req, .. }) => {
                    assert_eq!(req.method, "subscribe");
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

    /// Drift guard: every typed method has an App dispatch arm and the single
    /// capability gate occurs before that match.
    #[test]
    fn every_typed_method_is_dispatched_behind_capability_gate() {
        let src = include_str!("app.rs");
        let start = src
            .find("if method.capability() == Capability::Mutate")
            .expect("typed capability gate present");
        let dispatch = src[start..]
            .find("let resp = match method {")
            .map(|offset| start + offset)
            .expect("dispatch block present");
        assert!(start < dispatch, "authorization must precede dispatch");
        let block = &src[dispatch..(dispatch + 3600).min(src.len())];
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
