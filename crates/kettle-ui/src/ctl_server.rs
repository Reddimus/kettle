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
        if let Some(c) = self.conns.get_mut(&conn_id) {
            c.attached_panes.insert(pane);
        }
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
/// ONE thread per connection that reads + writes on the same handle.
fn accept_loop(listener: CtlListener, tx: Sender<CtlServerMsg>, wake: Arc<dyn Fn() + Send + Sync>) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    loop {
        let conn = match listener.accept() {
            Ok(s) => s,
            Err(e) => {
                log::debug!("agent-server: accept ended: {e}");
                return;
            }
        };
        let conn_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (event_tx, event_rx) = crossbeam_channel::bounded::<Event>(EVENT_QUEUE_CAP);
        let _ = tx.send(CtlServerMsg::NewConn { conn_id, event_tx });
        wake();
        let ctx = tx.clone();
        let cwake = wake.clone();
        std::thread::Builder::new()
            .name(format!("kettle-ctl-{conn_id}"))
            .spawn(move || connection_loop(conn, conn_id, ctx, cwake, event_rx))
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
                Ok(req) => {
                    is_subscribe = req.method == "subscribe";
                    let _ = tx.send(CtlServerMsg::Request {
                        conn_id,
                        req,
                        reply: rtx,
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
                // connection's life (no more requests read on this handle).
                while let Ok(ev) = event_rx.recv() {
                    if write_line(&mut conn, &ev).is_err() {
                        break 'outer;
                    }
                }
                break 'outer;
            }
            continue;
        }
        // Need more bytes.
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

/// The set of mutating methods. A drift-guard test pins that every method the
/// server dispatches is classified here, and that `handle_ctl_request` gates
/// the mutating ones on `agent-server = full`. (Used by the drift-guard tests.)
#[cfg_attr(not(test), allow(dead_code))]
pub const MUTATING_METHODS: &[&str] = &["send_text", "run_command"];

/// The read-only methods (allowed in `read-only` mode).
#[cfg_attr(not(test), allow(dead_code))]
pub const READ_ONLY_METHODS: &[&str] = &[
    "get_state",
    "list_tabs",
    "list_panes",
    "read_screen",
    "subscribe",
];

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
        }
    }

    /// Drift guard: every method arm in `App::handle_ctl_request` must appear in
    /// exactly one of the classification lists, and every mutating method must
    /// be guarded by `require_full`. Reads the app.rs source.
    #[test]
    fn every_dispatched_method_is_classified() {
        let src = include_str!("app.rs");
        // The dispatch block is between these markers.
        let start = src
            .find("let resp = match req.method.as_str() {")
            .expect("dispatch block present");
        let block = &src[start..start + 1600];
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
