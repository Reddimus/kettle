//! Cycle 927 (agent-first A2): the blocking control-plane client.
//!
//! Connects to a running kettle's control server (discovered via the registry
//! or named by pid/endpoint), issues correlated `call(method, params)`
//! requests, and—after `subscribe`—iterates the event stream. Used by
//! `kettle ctl` and the `kettle mcp` bridge.

use std::io::{BufRead, BufReader, Write};

use serde_json::Value;

use crate::discovery;
use crate::protocol::{Event, PROTOCOL_VERSION, Request, Response};
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
    /// that pid's server; otherwise pick the newest live entry, pruning dead
    /// ones as it probes.
    pub fn discover(pid: Option<u32>) -> Result<Self, CtlError> {
        let dir = discovery::registry_dir();
        let entries = discovery::list(&dir);
        if entries.is_empty() {
            return Err(CtlError::NoServer);
        }
        let candidates: Vec<_> = match pid {
            Some(p) => entries.into_iter().filter(|e| e.pid == p).collect(),
            None => entries,
        };
        for e in candidates {
            match Self::connect_endpoint(&e.endpoint) {
                Ok(c) => return Ok(c),
                Err(_) => {
                    // Dead/stale entry — prune and try the next.
                    discovery::prune(&dir, e.pid);
                }
            }
        }
        Err(CtlError::NoServer)
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
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                return Err(CtlError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "server closed the connection",
                )));
            }
            let trimmed = buf.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            // A response has an `id`; an event has an `event` field.
            if let Ok(resp) = serde_json::from_str::<Response>(trimmed) {
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

    /// Read the next event from the stream (after a successful `subscribe`).
    /// Returns `None` on clean EOF.
    pub fn next_event(&mut self) -> Result<Option<Event>, CtlError> {
        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                return Ok(None);
            }
            let trimmed = buf.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                return Ok(Some(ev));
            }
            // Skip non-event lines (e.g. a late response).
        }
    }
}
