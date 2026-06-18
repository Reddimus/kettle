//! Cycle 928–930 (agent-first A2): the in-process control server.
//!
//! When `agent-server` is enabled, the App starts a [`CtlServer`]: an accept
//! thread binds the kettle-ctl transport (Unix socket / Windows named pipe),
//! registers a discovery entry, and spawns ONE thread per connection. That
//! thread reads NDJSON requests and writes responses/events on the SAME handle,
//! never concurrently — a hard requirement on Windows, where writing to a
//! named-pipe handle while a `try_clone`d sibling has a blocking read pending
//! fails cross-process with ERROR_NO_DATA. So each connection is sequential:
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
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use kettle_config::AgentServer;
use kettle_ctl::discovery::{self, RegistryEntry};
use kettle_ctl::protocol::{Event, Request, Response};
use kettle_ctl::transport::{CtlListener, CtlStream};

/// Max concurrent connections; excess are dropped immediately.
const MAX_CONNECTIONS: usize = 8;
/// Per-connection event queue cap (subscribers only). On overflow we drop +
/// flag `lag` so a slow client can't make the App allocate without bound.
const EVENT_QUEUE_CAP: usize = 1024;

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
        let entry = RegistryEntry {
            v: 1,
            kind: "gui".into(),
            pid,
            endpoint: endpoint.clone(),
            version: version.into(),
            started_unix,
        };
        if let Err(e) = discovery::register(&registry_dir, &entry) {
            log::warn!("agent-server: cannot write discovery entry: {e}");
        }
        log::info!("agent-server: listening on {endpoint} (mode {mode:?})");

        let (tx, rx) = crossbeam_channel::unbounded::<CtlServerMsg>();
        let accept = std::thread::Builder::new()
            .name("kettle-ctl-accept".into())
            .spawn(move || accept_loop(listener, tx, wake))
            .ok()?;

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
    pub fn add_conn(&mut self, conn_id: u64, event_tx: Sender<Event>) {
        if self.conns.len() >= MAX_CONNECTIONS {
            log::warn!("agent-server: connection cap reached; dropping conn {conn_id}");
            return;
        }
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
        for conn in self.conns.values() {
            if !conn.subscribed {
                continue;
            }
            if conn.event_tx.try_send(ev.clone()).is_err() {
                let _ = conn.event_tx.try_send(Event::new(
                    "lag",
                    None,
                    serde_json::json!({"dropped": 1}),
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
fn accept_loop(listener: CtlListener, tx: Sender<CtlServerMsg>, wake: Arc<dyn Fn() + Send + Sync>) {
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
        // Hard connection cap: refuse (and close) once MAX_CONNECTIONS are live.
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            log::warn!("agent-server: connection cap ({MAX_CONNECTIONS}) reached; refusing");
            drop(conn); // closes the socket / pipe handle
            continue;
        }
        let conn_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        active.fetch_add(1, Ordering::Relaxed);
        let (event_tx, event_rx) = crossbeam_channel::bounded::<Event>(EVENT_QUEUE_CAP);
        let _ = tx.send(CtlServerMsg::NewConn { conn_id, event_tx });
        wake();
        let ctx = tx.clone();
        let cwake = wake.clone();
        let active_dec = active.clone();
        std::thread::Builder::new()
            .name(format!("kettle-ctl-{conn_id}"))
            .spawn(move || {
                connection_loop(conn, conn_id, ctx, cwake, event_rx);
                active_dec.fetch_sub(1, Ordering::Relaxed);
            })
            .ok();
    }
}

/// One connection, one thread: read requests + write responses/events on the
/// SAME handle, sequentially (never a concurrent read+write — the Windows
/// named-pipe ERROR_NO_DATA constraint). A `subscribe` request flips the
/// connection into event-only streaming.
fn connection_loop(
    mut conn: CtlStream,
    conn_id: u64,
    tx: Sender<CtlServerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    event_rx: Receiver<Event>,
) {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    'outer: loop {
        // Extract a complete line if we have one.
        if let Some(pos) = acc.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = acc.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line);
            let trimmed = s.trim_end();
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
                Ok(req) if req.method == "wait_for" => {
                    let resp = wait_for_poll(&mut conn, &tx, &wake, conn_id, &req);
                    if write_line(&mut conn, &resp).is_err() {
                        break 'outer;
                    }
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
            // Block until the App replies (run_command can take its timeout),
            // then write the response on this handle.
            match rrx.recv() {
                Ok(resp) => {
                    if write_line(&mut conn, &resp).is_err() {
                        break 'outer;
                    }
                }
                // App dropped the reply sender (shutdown) — end the connection.
                Err(_) => break 'outer,
            }
            if is_subscribe {
                // Switch to event-only streaming for the rest of the
                // connection's life (no more requests read on this handle). Use
                // a bounded recv so an IDLE subscriber whose client vanished is
                // detected within the keepalive window: on timeout we write a
                // harmless `ping` event; a failed write means the peer is gone,
                // so we disconnect (bounding the ConnState+thread leak to the
                // timeout rather than "until the next real event").
                loop {
                    match event_rx.recv_timeout(std::time::Duration::from_secs(20)) {
                        Ok(ev) => {
                            if write_line(&mut conn, &ev).is_err() {
                                break 'outer;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            let ping = Event::new("ping", None, serde_json::Value::Null);
                            if write_line(&mut conn, &ping).is_err() {
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
            let _ = write_line(&mut conn, &resp);
            break;
        }
        match conn.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
    }
    let _ = tx.send(CtlServerMsg::Disconnect { conn_id });
    wake();
}

/// Serialize `value` and write it + a newline + flush.
fn write_line<T: serde::Serialize>(conn: &mut CtlStream, value: &T) -> std::io::Result<()> {
    let line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    conn.write_all(line.as_bytes())?;
    conn.write_all(b"\n")?;
    conn.flush()
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

/// The set of mutating methods. A drift-guard test pins that every method the
/// server dispatches is classified here, and that `handle_ctl_request` gates
/// the mutating ones on `agent-server = full`. (Used by the drift-guard tests.)
#[cfg_attr(not(test), allow(dead_code))]
pub const MUTATING_METHODS: &[&str] = &[
    "send_text",
    "send_keys",
    "send_mouse",
    "resize_window",
    "perform_action",
    "run_command",
];

/// The read-only methods (allowed in `read-only` mode).
#[cfg_attr(not(test), allow(dead_code))]
pub const READ_ONLY_METHODS: &[&str] = &[
    "get_state",
    "list_tabs",
    "list_panes",
    "read_screen",
    "read_cells",
    "ui_geometry",
    "screenshot",
    "subscribe",
];

/// v2.20.0: methods handled entirely on the CONNECTION thread (never reach
/// `handle_ctl_request`). `wait_for` is read-only by construction — it only
/// ever issues `read_screen` probes — so it works in `read-only` mode; it is
/// listed separately because the dispatch-block drift guard below scans the
/// UI-thread match, which these never appear in.
#[cfg_attr(not(test), allow(dead_code))]
pub const CONN_THREAD_METHODS: &[&str] = &["wait_for"];

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Drift guard: the method classification must stay disjoint + cover every
    /// method `handle_ctl_request` dispatches (the app.rs match). Mutating
    /// methods MUST be gated; read-only MUST NOT overlap them. If a new method
    /// is added to the dispatch without classifying it here, the source-scan
    /// guard in app.rs catches it.
    #[test]
    fn method_classification_is_disjoint() {
        for m in MUTATING_METHODS {
            assert!(
                !READ_ONLY_METHODS.contains(m),
                "method {m} is both mutating and read-only"
            );
            assert!(
                !CONN_THREAD_METHODS.contains(m),
                "method {m} is both mutating and connection-thread"
            );
        }
        for m in CONN_THREAD_METHODS {
            assert!(
                !READ_ONLY_METHODS.contains(m),
                "method {m} is both connection-thread and read-only"
            );
            // Connection-thread methods must be special-cased in
            // connection_loop, BEFORE the UI-thread forward.
            let src = include_str!("ctl_server.rs");
            assert!(
                src.contains(&format!("req.method == \"{m}\"")),
                "conn-thread method {m} has no connection_loop arm"
            );
        }
    }

    /// Drift guard: every method arm in `App::handle_ctl_request` must appear in
    /// exactly one of the classification lists, and every mutating method must
    /// be guarded by `require_full`. Reads the app.rs source.
    #[test]
    fn every_dispatched_method_is_classified() {
        let src = include_str!("app.rs");
        // The dispatch block is between these markers. (Window widened in
        // v2.20.0 when the send_keys arm landed; the `other =>` fallback arm
        // bounds the real block well inside it.)
        let start = src
            .find("let resp = match req.method.as_str() {")
            .expect("dispatch block present");
        let block = &src[start..start + 3600];
        for m in READ_ONLY_METHODS.iter().chain(MUTATING_METHODS) {
            assert!(
                block.contains(&format!("\"{m}\"")),
                "method {m} classified but not dispatched in handle_ctl_request"
            );
        }
        // Each mutating method's arm must reference the `require_full` gate.
        for m in MUTATING_METHODS {
            let arm = format!("\"{m}\" =>");
            let pos = block
                .find(&arm)
                .unwrap_or_else(|| panic!("{m} arm missing"));
            let arm_body = &block[pos..(pos + 200).min(block.len())];
            assert!(
                arm_body.contains("require_full"),
                "mutating method {m} must be gated by require_full"
            );
        }
    }
}
