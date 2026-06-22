//! Cycle 927 (agent-first A2): the blocking control-plane client.
//!
//! Connects to a running kettle's control server (discovered via the registry
//! or named by pid/endpoint), issues correlated `call(method, params)`
//! requests, and—after `subscribe`—iterates the event stream. Used by
//! `kettle ctl` and the `kettle mcp` bridge.

use std::io::{BufRead, BufReader, Write};

use serde_json::Value;

use crate::discovery;
use crate::protocol::{Event, MAX_LINE_BYTES, PROTOCOL_VERSION, Request, Response};
use crate::transport::{self, CtlStream};

/// A client error: a transport failure, or a structured server error.
#[derive(Debug)]
pub enum CtlError {
    /// No running server was found in the registry.
    NoServer,
    /// An I/O / transport failure.
    Io(std::io::Error),
    /// The server returned an error response.
    Server { code: String, message: String },
    /// The server's reply was not parseable.
    Protocol(String),
}

impl std::fmt::Display for CtlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CtlError::NoServer => write!(
                f,
                "no running kettle control server found (start kettle with `agent-server = full` or `--agent-server full`)"
            ),
            CtlError::Io(e) => write!(f, "control I/O error: {e}"),
            CtlError::Server { code, message } => write!(f, "server error [{code}]: {message}"),
            CtlError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for CtlError {}

impl From<std::io::Error> for CtlError {
    fn from(e: std::io::Error) -> Self {
        CtlError::Io(e)
    }
}

/// A blocking client over one control connection.
pub struct Client {
    writer: CtlStream,
    reader: BufReader<CtlStream>,
    next_id: u64,
}

impl Client {
    /// Connect to a specific endpoint (socket path / pipe name).
    pub fn connect_endpoint(endpoint: &str) -> Result<Self, CtlError> {
        let stream = transport::connect(endpoint)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            writer: stream,
            reader,
            next_id: 1,
        })
    }

    /// Discover a running server and connect. If `pid` is `Some`, connect to
    /// that pid's server; otherwise pick the newest live entry.
    ///
    /// Robustness (audit): an entry is only *pruned* when its owning process is
    /// genuinely dead (`presence::pid_alive` says so). A `connect_endpoint`
    /// failure can also come from a *client-side* hiccup (a `try_clone` /
    /// BufReader error while the server is alive and already connected) or a
    /// transient transport error — pruning on those would permanently delete a
    /// healthy server's entry, since the server `register`s exactly once at
    /// start (no heartbeat). When the connect fails but the pid is still alive,
    /// we leave the entry in place and remember the error, surfacing it rather
    /// than masking every failure as a blanket `NoServer`.
    pub fn discover(pid: Option<u32>) -> Result<Self, CtlError> {
        let dir = discovery::registry_dir();
        Self::discover_in(
            &dir,
            pid,
            Self::connect_endpoint,
            crate::presence::pid_alive,
        )
    }

    /// The dependency-injected core of [`discover`], split out so the
    /// prune-gating invariant (a connect failure against a *live* pid must NOT
    /// prune the entry) is unit-testable without the real registry/transport.
    /// `connect` opens an endpoint; `pid_alive` reports whether a pid's owning
    /// process is still running.
    fn discover_in(
        dir: &std::path::Path,
        pid: Option<u32>,
        connect: impl Fn(&str) -> Result<Self, CtlError>,
        pid_alive: impl Fn(u32) -> bool,
    ) -> Result<Self, CtlError> {
        // `list_live` already drops + prunes pid-dead entries, so the loop only
        // probes endpoints whose owner is alive at enumeration time.
        let entries: Vec<_> = discovery::list(dir)
            .into_iter()
            .filter(|e| {
                if pid_alive(e.pid) {
                    true
                } else {
                    // Dead owner — its server can't return under this pid.
                    discovery::prune(dir, e.pid);
                    false
                }
            })
            .collect();
        if entries.is_empty() {
            return Err(CtlError::NoServer);
        }
        let candidates: Vec<_> = match pid {
            Some(p) => entries.into_iter().filter(|e| e.pid == p).collect(),
            None => entries,
        };
        if candidates.is_empty() {
            return Err(CtlError::NoServer);
        }
        let mut last_err: Option<CtlError> = None;
        for e in candidates {
            match connect(&e.endpoint) {
                Ok(c) => return Ok(c),
                Err(err) => {
                    // Only prune a TRULY dead server. If the owning process is
                    // still alive the failure is client-side (a try_clone /
                    // BufReader hiccup while the server is alive and already
                    // connected) or a transient transport error — do NOT delete
                    // a healthy entry; remember the error instead.
                    if !pid_alive(e.pid) {
                        discovery::prune(dir, e.pid);
                    }
                    last_err = Some(err);
                }
            }
        }
        // Surface the real reason we couldn't connect rather than a blanket
        // NoServer, unless every candidate vanished without an error.
        Err(last_err.unwrap_or(CtlError::NoServer))
    }

    /// Issue a request and return its result value (or a structured error).
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, CtlError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            v: PROTOCOL_VERSION,
            id,
            method: method.to_string(),
            params,
        };
        let line = serde_json::to_string(&req).map_err(|e| CtlError::Protocol(e.to_string()))?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        // Read response lines until we get the one matching `id` (events that
        // arrive before the response — if already subscribed — are skipped).
        loop {
            let Some(trimmed) = self.read_capped_line()? else {
                return Err(CtlError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "server closed the connection",
                )));
            };
            if trimmed.is_empty() {
                continue;
            }
            // A response has an `id`; an event has an `event` field.
            if let Ok(resp) = serde_json::from_str::<Response>(&trimmed) {
                if resp.v > PROTOCOL_VERSION {
                    return Err(CtlError::Protocol(format!(
                        "server response protocol v{} > supported v{PROTOCOL_VERSION}",
                        resp.v
                    )));
                }
                if resp.id != id {
                    continue;
                }
                if resp.ok {
                    return Ok(resp.result);
                }
                let err = resp.error.unwrap_or_else(|| crate::protocol::RpcError {
                    code: "internal".into(),
                    message: "error response without payload".into(),
                });
                return Err(CtlError::Server {
                    code: err.code,
                    message: err.message,
                });
            }
            // Not a response (likely an event) — keep reading.
        }
    }

    /// Read the next *meaningful* event from the stream (after a successful
    /// `subscribe`). Returns `None` on clean EOF.
    ///
    /// `ping` keepalives are consumed and skipped internally — they exist only
    /// so the server can detect a dead peer on write, and carry no payload for
    /// consumers. This is the single forward-compat seam for that filtering, so
    /// every caller need not re-discover it. Non-event lines (e.g. a late
    /// response) are likewise skipped.
    pub fn next_event(&mut self) -> Result<Option<Event>, CtlError> {
        loop {
            let Some(trimmed) = self.read_capped_line()? else {
                return Ok(None);
            };
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Event>(&trimmed) {
                if ev.v > PROTOCOL_VERSION {
                    return Err(CtlError::Protocol(format!(
                        "server event protocol v{} > supported v{PROTOCOL_VERSION}",
                        ev.v
                    )));
                }
                // Swallow keepalives — they're a transport-liveness mechanism,
                // not a consumer event.
                if ev.event == "ping" {
                    continue;
                }
                return Ok(Some(ev));
            }
            // Skip non-event lines (e.g. a late response) and keep reading.
        }
    }

    /// Read one NDJSON line, enforcing the protocol's `MAX_LINE_BYTES` cap so a
    /// hostile/buggy server can't make the client buffer without bound. Returns
    /// the trimmed line, or `None` on clean EOF.
    fn read_capped_line(&mut self) -> Result<Option<String>, CtlError> {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut bytes)?;
        if n == 0 {
            return Ok(None);
        }
        if bytes.len() > MAX_LINE_BYTES && bytes.last() != Some(&b'\n') {
            return Err(CtlError::Protocol("server line exceeds 1 MiB".into()));
        }
        Ok(Some(String::from_utf8_lossy(&bytes).trim_end().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;
    use crate::transport::CtlListener;

    /// Spin up a loopback listener whose server side writes `lines` (each is a
    /// raw NDJSON message, no trailing newline needed) then closes, and return a
    /// `Client` connected to it. Mirrors `transport::tests` so the Windows
    /// named-pipe leg is exercised on CI too.
    fn client_fed(lines: Vec<String>) -> Client {
        let pid = std::process::id();
        let tag = format!("{pid}-{:p}", &lines);
        #[cfg(unix)]
        let endpoint = std::env::temp_dir()
            .join(format!("kettle-ctl-evt-{tag}.sock"))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\kettle-ctl-evt-{tag}");

        let listener = CtlListener::bind(&endpoint).expect("bind");
        let ep = endpoint.clone();
        std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            for line in lines {
                conn.write_all(line.as_bytes()).expect("write");
                conn.write_all(b"\n").expect("write nl");
            }
            conn.flush().ok();
            // Drop closes the connection → the client sees clean EOF.
        });
        Client::connect_endpoint(&ep).expect("connect")
    }

    use crate::discovery::{self, RegistryEntry};

    fn reg_entry(pid: u32, endpoint: &str, started: u64) -> RegistryEntry {
        RegistryEntry {
            v: 1,
            kind: "gui".into(),
            pid,
            endpoint: endpoint.into(),
            version: "x".into(),
            started_unix: started,
        }
    }

    /// Fix 1 invariant: when `connect_endpoint` fails but the owning pid is
    /// still ALIVE (a client-side `try_clone`/BufReader hiccup or a transient
    /// transport error — the server `register`s exactly once, no heartbeat), the
    /// healthy entry MUST NOT be pruned, and the real transport error must be
    /// surfaced rather than masked as a blanket `NoServer`.
    #[test]
    fn discover_does_not_prune_live_pid_on_connect_failure() {
        let dir = std::env::temp_dir().join(format!("kettle-ctl-disc-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pid = 4242;
        discovery::register(&dir, &reg_entry(pid, "ep-flaky", 100)).unwrap();

        // Connect always fails with a transport error; pid is reported alive.
        let connect = |_ep: &str| -> Result<Client, CtlError> {
            Err(CtlError::Io(std::io::Error::other(
                "transient transport hiccup",
            )))
        };
        let res = Client::discover_in(&dir, None, connect, |_p| true);

        // The error is surfaced (not masked as NoServer)…
        match res {
            Err(CtlError::Io(_)) => {}
            Err(other) => panic!("expected the transport Io error to surface, got {other:?}"),
            Ok(_) => panic!("connect closure always errs; discover must not succeed"),
        }
        // …and crucially the live entry is STILL on disk (not pruned).
        assert!(
            discovery::list(&dir).iter().any(|e| e.pid == pid),
            "a live server's entry must survive a transient connect failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Conversely, a connect failure whose pid is DEAD does prune the entry
    /// (the complementary half of the gate).
    #[test]
    fn discover_prunes_dead_pid_on_connect_failure() {
        let dir = std::env::temp_dir().join(format!("kettle-ctl-disc-dead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pid = 4243;
        discovery::register(&dir, &reg_entry(pid, "ep-dead", 100)).unwrap();

        let connect = |_ep: &str| -> Result<Client, CtlError> {
            Err(CtlError::Io(std::io::Error::other("x")))
        };
        // pid reported dead → enumeration filter prunes it before any connect,
        // so discovery yields NoServer and the entry is gone.
        let res = Client::discover_in(&dir, None, connect, |_p| false);
        assert!(matches!(res, Err(CtlError::NoServer)));
        assert!(
            !discovery::list(&dir).iter().any(|e| e.pid == pid),
            "a dead server's entry must be pruned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_event_skips_ping_keepalives() {
        // A ping, then a real event: next_event must yield only the real one.
        let ping = serde_json::to_string(&Event::new("ping", None, Value::Null)).unwrap();
        let output =
            serde_json::to_string(&Event::new("output", Some(3), serde_json::json!("hi"))).unwrap();
        let mut client = client_fed(vec![ping, output]);

        let ev = client.next_event().expect("ok").expect("an event");
        assert_eq!(ev.event, "output", "ping was leaked instead of skipped");
        assert_eq!(ev.pane, Some(3));
    }

    #[test]
    fn next_event_skips_runs_of_pings_then_returns_eof() {
        // Several consecutive pings with no real event → clean EOF (None), never
        // a ping handed back to the caller.
        let ping = serde_json::to_string(&Event::new("ping", None, Value::Null)).unwrap();
        let mut client = client_fed(vec![ping.clone(), ping.clone(), ping]);

        assert!(
            client.next_event().expect("ok").is_none(),
            "only pings were sent — next_event should reach EOF without yielding one"
        );
    }
}
