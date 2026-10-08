//! The blocking control-plane client.
//!
//! Connects to a running kettle's control server (discovered via the registry
//! or named by pid/endpoint), issues correlated `call(method, params)`
//! requests, and—after `subscribe`—iterates the event stream. Used by
//! `kettle ctl` and the `kettle mcp` bridge.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::discovery;
use crate::protocol::{
    BoundedJsonError, Event, MAX_LINE_BYTES, MAX_RESPONSE_LINE_BYTES, PROTOCOL_VERSION, PeerClaim,
    Request, Response, StartToken,
};
use crate::transport::{self, CtlStream};

/// A client error: a transport failure, or a structured server error.
#[derive(Debug)]
pub enum CtlError {
    /// No running server was found in the registry.
    NoServer,
    /// Display discovery found no Kettle this process runs in, by its
    /// ancestry or by the `KETTLE_PID` it inherited, and tries no other.
    NotInKettle,
    /// An I/O / transport failure.
    Io(std::io::Error),
    /// The server returned an error response.
    Server { code: String, message: String },
    /// The server's reply was not parseable.
    Protocol(String),
    /// The request did not receive a complete response before its deadline.
    TimedOut,
    /// The caller cancelled the request while waiting for its response.
    Cancelled,
    /// An earlier request was abandoned before its response was read, so this
    /// connection can no longer correlate one. The caller must reconnect.
    Unusable(String),
}

impl std::fmt::Display for CtlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CtlError::NoServer => write!(
                f,
                "no running kettle control server found (start kettle with `agent-server = full` or `--agent-server full`)"
            ),
            CtlError::NotInKettle => write!(
                f,
                "this session is not running inside a kettle whose agent previews are on"
            ),
            CtlError::Io(e) => write!(f, "control I/O error: {e}"),
            CtlError::Server { code, message } => write!(f, "server error [{code}]: {message}"),
            CtlError::Protocol(m) => write!(f, "protocol error: {m}"),
            // Both of these end the caller's wait, not the server's work: a
            // request already on the wire may be carried out regardless. Say
            // so here, where every renderer picks it up — `kettle ctl` and the
            // MCP bridge both print this Display and nothing else.
            CtlError::TimedOut => write!(
                f,
                "control request timed out; the server may already have performed it"
            ),
            CtlError::Cancelled => write!(
                f,
                "control request was cancelled; the server may already have performed it"
            ),
            CtlError::Unusable(reason) => write!(
                f,
                "control connection was abandoned after {reason}; open a new connection to issue further requests"
            ),
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
///
/// Correlation is positional: a call writes one request and reads frames until
/// the response carrying its id arrives. Any outcome that stops that read
/// early — a deadline, a cancellation, a bound on buffered events, malformed
/// data, a partially written request — leaves a response still travelling
/// towards us, so the connection is retired rather than reused; see
/// [`Client::is_usable`].
pub struct Client {
    /// The two halves of the connection, dropped together the moment it is
    /// retired: a retired client can never speak again, so holding the
    /// transport open would keep one of the server's bounded connection slots
    /// (and its worker) busy until the caller happened to drop this value.
    /// `None` therefore means exactly the same thing as `abandoned.is_some()`.
    writer: Option<CtlStream>,
    reader: Option<CtlStream>,
    read_buffer: Vec<u8>,
    read_scan_offset: usize,
    queued_events: VecDeque<(Event, usize)>,
    queued_event_bytes: usize,
    next_id: u64,
    /// Why this connection was retired, once it has been.
    abandoned: Option<String>,
    /// What this process says about itself, sent once with the first request
    /// that reaches the wire. Taken when the connection opened.
    claim: Option<PeerClaim>,
    claim_sent: bool,
    /// The process that opened the connection. A child that inherited this
    /// value across `fork()` must not speak with the parent's identity.
    owner_pid: u32,
}

enum ServerFrame {
    Response(Response),
    Event(Event),
}

const MAX_QUEUED_EVENTS_DURING_CALL: usize = 1024;
const MAX_QUEUED_EVENT_BYTES: usize = 8 * 1024 * 1024;

impl Client {
    /// Connect to a specific endpoint (socket path / pipe name).
    pub fn connect_endpoint(endpoint: &str) -> Result<Self, CtlError> {
        // Taken before connecting, so the claim names the process that opens
        // the connection.
        let claim = own_claim(|name| std::env::var(name).ok());
        let stream = transport::connect(endpoint)?;
        let reader = stream.try_clone()?;
        Ok(Self {
            writer: Some(stream),
            reader: Some(reader),
            read_buffer: Vec::new(),
            read_scan_offset: 0,
            queued_events: VecDeque::new(),
            queued_event_bytes: 0,
            next_id: 1,
            abandoned: None,
            claim,
            claim_sent: false,
            owner_pid: std::process::id(),
        })
    }

    /// Discover a running server and connect.
    ///
    /// The server is, in order: the one `pid` names; else the nearest Kettle
    /// this process runs inside, matched by pid and start time against its
    /// own ancestors; else the one `KETTLE_PID` names; else, for a caller in
    /// none, the newest live one. A server chosen by name or ancestry is the
    /// only one tried: its failure never falls through to another instance.
    /// Every registry location is read, including an alias the server left
    /// where the OS (not the environment) puts the registry, so a client with
    /// a stripped environment still finds it. Each connection is checked
    /// before any request is written: the kernel must name the entry's pid at
    /// the other end, and that process must still be the instance the entry
    /// recorded.
    ///
    /// An entry is only *pruned* when `presence::owner_alive` says its owning
    /// process is gone (dead, or its pid since handed to a stranger). A
    /// connect failure can also come from a client-side `try_clone` error or
    /// a transient transport error while the server is alive. Pruning on
    /// those would permanently delete a healthy server's entry, since the
    /// server `register`s exactly once at start (no heartbeat). So when the
    /// owner is still alive, the entry stays and discovery surfaces the
    /// connect error instead of a blanket `NoServer`.
    pub fn discover(pid: Option<u32>) -> Result<Self, CtlError> {
        Self::discover_with(pid, Fallback::Newest)
    }

    /// [`Client::discover`] for showing media: the Kettle `pid` names, else
    /// the one this process runs inside, else the one `KETTLE_PID` names,
    /// and nothing else, so media never lands in an unrelated Kettle. An
    /// entry named by `KETTLE_PID` must record its start time, so the server
    /// can be checked to be that instance. Finding none is
    /// [`CtlError::NotInKettle`].
    pub fn discover_display(pid: Option<u32>) -> Result<Self, CtlError> {
        Self::discover_with(pid, Fallback::Nothing)
    }

    fn discover_with(pid: Option<u32>, fallback: Fallback) -> Result<Self, CtlError> {
        let ancestry = crate::identity::current_ancestry(Instant::now() + ANCESTRY_BUDGET);
        Self::discover_in(
            &discovery::registry_locations(),
            pid,
            &ancestry,
            kettle_pid_hint(|name| std::env::var(name).ok()),
            fallback,
            Self::connect_authenticated,
            discovery::owner_alive,
        )
    }

    /// Connect to `entry`'s endpoint and check, before writing anything, that
    /// the server there is the process the entry names.
    fn connect_authenticated(entry: &discovery::RegistryEntry) -> Result<Self, CtlError> {
        let client = Self::connect_endpoint(&entry.endpoint)?;
        let server = client
            .writer
            .as_ref()
            .ok_or_else(Self::retired)?
            .peer_pid()?;
        let instance = match entry.start_token {
            // An entry from a build that predates start tokens names only a
            // pid; the kernel's peer pid is all there is to check.
            None => true,
            Some(token) => crate::process::identity(server).is_ok_and(|live| live.start() == token),
        };
        if server != entry.pid || !instance {
            return Err(CtlError::Protocol(format!(
                "the server at {} is not the kettle its registry entry names",
                entry.endpoint
            )));
        }
        Ok(client)
    }

    /// The dependency-injected core of [`discover`], so selection and the
    /// prune-gating invariant (a connect failure against a *live* owner must
    /// NOT prune the entry) are testable without the real registry, process
    /// table or transport. `ancestry` is this process's, nearest first;
    /// `owner_alive` reports whether the process instance that wrote an entry
    /// is still running — the entry, not just its pid, because a recycled pid
    /// is a stranger.
    fn discover_in(
        locations: &[std::path::PathBuf],
        pid: Option<u32>,
        ancestry: &[crate::process::ProcessIdentity],
        kettle_pid: Option<u32>,
        fallback: Fallback,
        connect: impl Fn(&discovery::RegistryEntry) -> Result<Self, CtlError>,
        owner_alive: impl Fn(&discovery::RegistryEntry) -> bool,
    ) -> Result<Self, CtlError> {
        // Enumeration drops and prunes entries with a dead owner, so only
        // endpoints whose owner is alive are probed.
        let candidates = discovery::live_candidates_by(locations, &owner_alive);
        let chosen = match pid {
            Some(pid) => Some(
                candidates
                    .iter()
                    .find(|(entry, _)| entry.pid == pid)
                    .ok_or(CtlError::NoServer)?,
            ),
            None => choose(&candidates, ancestry, kettle_pid, fallback),
        };
        let attempt = |entry: &discovery::RegistryEntry, dir: &std::path::Path| {
            connect(entry).inspect_err(|_| {
                // Only prune a TRULY dead server. If the owning process is
                // still alive the failure is client-side (a `try_clone`
                // hiccup) or a transient transport error, so do NOT delete a
                // healthy entry. Even for a dead owner the delete is
                // conditional, since the connect attempt takes real time and
                // this entry's pid may by now belong to a new kettle that
                // registered at the same path.
                if !owner_alive(entry) {
                    discovery::prune_stale(dir, entry);
                }
            })
        };
        if let Some((entry, dir)) = chosen {
            return attempt(entry, dir);
        }
        if fallback == Fallback::Nothing {
            return Err(CtlError::NotInKettle);
        }
        let mut last_err: Option<CtlError> = None;
        for (entry, dir) in &candidates {
            match attempt(entry, dir) {
                Ok(client) => return Ok(client),
                Err(error) => last_err = Some(error),
            }
        }
        // Surface the real reason we couldn't connect rather than a blanket
        // NoServer, unless there was no candidate at all.
        Err(last_err.unwrap_or(CtlError::NoServer))
    }

    /// Issue a request and return its result value (or a structured error).
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, CtlError> {
        let timeout = call_timeout(method, &params);
        self.call_inner(method, params, timeout, None)
    }

    /// Issue a request with an explicit overall response deadline.
    pub fn call_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CtlError> {
        self.call_inner(method, params, timeout, None)
    }

    /// Issue a request while observing an external cancellation flag.
    pub fn call_cancellable(
        &mut self,
        method: &str,
        params: Value,
        cancelled: &AtomicBool,
    ) -> Result<Value, CtlError> {
        let timeout = call_timeout(method, &params);
        self.call_inner(method, params, timeout, Some(cancelled))
    }

    /// Whether this connection may still be used.
    ///
    /// A client goes unusable after any request that ended without its
    /// response being read off the wire; every later call then fails with
    /// [`CtlError::Unusable`] instead of correlating against a frame that
    /// belongs to the abandoned request. Note that a cancelled or timed-out
    /// *mutating* request may still have been executed by the server — the
    /// caller learned nothing about its fate, only that this connection can no
    /// longer report it.
    pub fn is_usable(&self) -> bool {
        self.abandoned.is_none()
    }

    fn usable(&self) -> Result<(), CtlError> {
        match &self.abandoned {
            Some(reason) => Err(CtlError::Unusable(reason.clone())),
            None => Ok(()),
        }
    }

    /// Retire the connection: record why, close the transport, and release
    /// everything buffered for the abandoned exchange (up to the queued-event
    /// cap) since no caller can reach it again.
    ///
    /// Dropping the streams here is what makes retirement self-enforcing
    /// rather than advisory. A caller that follows the contract — notice
    /// [`is_usable`](Client::is_usable), reconnect — would otherwise hold this
    /// dead connection, and with it one of the server's bounded connection
    /// slots and a worker thread, for as long as it keeps the value around.
    fn abandon(&mut self, error: &CtlError) {
        if self.abandoned.is_none() {
            self.abandoned = Some(match error {
                CtlError::TimedOut => "a timed-out request".to_string(),
                CtlError::Cancelled => "a cancelled request".to_string(),
                CtlError::Protocol(message) => format!("a protocol failure ({message})"),
                CtlError::Io(error) => format!("an I/O failure ({error})"),
                // An exchange never produces these; keep the mapping total
                // rather than assert a shape a future caller could break.
                CtlError::NoServer
                | CtlError::NotInKettle
                | CtlError::Server { .. }
                | CtlError::Unusable(_) => "an incomplete request".to_string(),
            });
        }
        self.writer = None;
        self.reader = None;
        self.read_buffer = Vec::new();
        self.read_scan_offset = 0;
        self.queued_events.clear();
        self.queued_event_bytes = 0;
    }

    /// The error every path takes when it reaches the transport after
    /// retirement. Unreachable while [`usable`](Client::usable) guards each
    /// entry point, and kept total rather than a panic for the day it isn't.
    fn retired() -> CtlError {
        CtlError::Unusable("a retired connection".to_string())
    }

    fn call_inner(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Value, CtlError> {
        self.usable()?;
        if std::process::id() != self.owner_pid {
            return Err(CtlError::Unusable(
                "a connection inherited from another process".to_string(),
            ));
        }
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(CtlError::Cancelled);
        }
        if timeout.is_zero() {
            return Err(CtlError::TimedOut);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| CtlError::Protocol("request deadline is out of range".into()))?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| CtlError::Protocol("request id space is exhausted".into()))?;
        let req = Request {
            v: PROTOCOL_VERSION,
            id,
            method: method.to_string(),
            params,
            caller: if self.claim_sent { None } else { self.claim },
        };
        let mut frame = match crate::protocol::to_json_vec_bounded(&req, MAX_LINE_BYTES) {
            Ok(frame) => frame,
            Err(BoundedJsonError::Limit { .. }) => {
                return Err(CtlError::Protocol(format!(
                    "request line exceeds {MAX_LINE_BYTES} bytes"
                )));
            }
            Err(BoundedJsonError::Serialize(error)) => {
                return Err(CtlError::Protocol(error.to_string()));
            }
        };
        frame.push(b'\n');
        // Failures above this line never touch the wire, so they keep the
        // connection reusable. So does a failed write that sent no bytes (a
        // deadline or cancellation can land before the first one), since the
        // server never saw that request. Once a byte is out, the connection
        // stays in step only if this request's response is read.
        let writer = self.writer.as_mut().ok_or_else(Self::retired)?;
        let (written, write) = writer.write_all_until_counted(&frame, deadline, cancelled);
        // The server fixes a connection's claim from its first frame, so once
        // any byte of this one is out the claim is spent.
        self.claim_sent |= written > 0;
        if let Err(error) = write {
            let error = map_write_error(error);
            if written > 0 {
                self.abandon(&error);
            }
            return Err(error);
        }
        let result = self.read_response(id, deadline, cancelled);
        if let Err(error) = &result
            && !matches!(error, CtlError::Server { .. })
        {
            // A structured server error IS the response for `id`; anything
            // else means we stopped before reading it.
            self.abandon(error);
        }
        result
    }

    /// Read until the response for `id` arrives, queueing any events that
    /// precede it.
    fn read_response(
        &mut self,
        id: u64,
        deadline: Instant,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Value, CtlError> {
        loop {
            let Some(line) = self.read_capped_line(Some(deadline), cancelled)? else {
                return Err(CtlError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "server closed the connection",
                )));
            };
            if line.trim().is_empty() {
                continue;
            }
            match parse_server_frame(&line)? {
                ServerFrame::Event(event) => {
                    if event.event != "ping" {
                        if self.queued_events.len() >= MAX_QUEUED_EVENTS_DURING_CALL {
                            return Err(CtlError::Protocol(format!(
                                "more than {MAX_QUEUED_EVENTS_DURING_CALL} events arrived before the response"
                            )));
                        }
                        let frame_bytes = line.len();
                        if self.queued_event_bytes.saturating_add(frame_bytes)
                            > MAX_QUEUED_EVENT_BYTES
                        {
                            return Err(CtlError::Protocol(format!(
                                "queued event data exceeds {MAX_QUEUED_EVENT_BYTES} bytes before the response"
                            )));
                        }
                        self.queued_event_bytes += frame_bytes;
                        self.queued_events.push_back((event, frame_bytes));
                    }
                }
                ServerFrame::Response(resp) => {
                    if resp.id != id {
                        return Err(CtlError::Protocol(format!(
                            "response id {} does not match request id {id}",
                            resp.id
                        )));
                    }
                    if resp.ok {
                        if resp.error.is_some() {
                            return Err(CtlError::Protocol(
                                "successful response contains an error payload".into(),
                            ));
                        }
                        return Ok(resp.result);
                    }
                    let Some(err) = resp.error else {
                        return Err(CtlError::Protocol(
                            "error response is missing its error payload".into(),
                        ));
                    };
                    if !resp.result.is_null() {
                        return Err(CtlError::Protocol(
                            "error response contains a result payload".into(),
                        ));
                    }
                    return Err(CtlError::Server {
                        code: err.code,
                        message: err.message,
                    });
                }
            }
        }
    }

    /// Read the next *meaningful* event from the stream (after a successful
    /// `subscribe`). Returns `None` on clean EOF.
    ///
    /// `ping` keepalives are consumed and skipped internally — they exist only
    /// so the server can detect a dead peer on write, and carry no payload for
    /// consumers. This is the single forward-compat seam for that filtering, so
    /// every caller need not re-discover it. A response in the event stream is
    /// a protocol violation rather than something that can be silently lost.
    pub fn next_event(&mut self) -> Result<Option<Event>, CtlError> {
        self.usable()?;
        let result = self.next_event_inner();
        if let Err(error) = &result {
            // The stream stopped making sense mid-frame; a later read would
            // resume inside data we already failed to interpret.
            self.abandon(error);
        }
        result
    }

    fn next_event_inner(&mut self) -> Result<Option<Event>, CtlError> {
        loop {
            if let Some((event, frame_bytes)) = self.queued_events.pop_front() {
                self.queued_event_bytes = self.queued_event_bytes.saturating_sub(frame_bytes);
                return Ok(Some(event));
            }
            let Some(line) = self.read_capped_line(None, None)? else {
                return Ok(None);
            };
            if line.trim().is_empty() {
                continue;
            }
            match parse_server_frame(&line)? {
                ServerFrame::Event(event) => {
                    if event.event == "ping" {
                        continue;
                    }
                    return Ok(Some(event));
                }
                ServerFrame::Response(response) => {
                    return Err(CtlError::Protocol(format!(
                        "unexpected response id {} in event stream",
                        response.id
                    )));
                }
            }
        }
    }

    /// Read one NDJSON line, enforcing the response-line cap so a
    /// hostile/buggy server can't make the client buffer without bound. Returns
    /// the line without CR/LF framing, or `None` on clean EOF.
    fn read_capped_line(
        &mut self,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<String>, CtlError> {
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Err(CtlError::Cancelled);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(CtlError::TimedOut);
            }
            if let Some(newline) =
                crate::protocol::find_newline(&self.read_buffer, &mut self.read_scan_offset)
            {
                if newline > MAX_RESPONSE_LINE_BYTES {
                    return Err(line_too_large());
                }
                let mut bytes: Vec<u8> = self.read_buffer.drain(..=newline).collect();
                self.read_scan_offset = 0;
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                return decode_server_line(bytes).map(Some);
            }
            if self.read_buffer.len() > MAX_RESPONSE_LINE_BYTES {
                return Err(line_too_large());
            }
            if let Some(deadline) = deadline {
                let now = Instant::now();
                if now >= deadline {
                    return Err(CtlError::TimedOut);
                }
                let mut wait = deadline.saturating_duration_since(now);
                if cancelled.is_some() {
                    wait = wait.min(Duration::from_millis(50));
                }
                if !self
                    .reader
                    .as_ref()
                    .ok_or_else(Self::retired)?
                    .wait_readable(wait)?
                {
                    if Instant::now() >= deadline {
                        return Err(CtlError::TimedOut);
                    }
                    continue;
                }
            }

            let mut chunk = [0u8; 8192];
            let remaining = (MAX_RESPONSE_LINE_BYTES + 1).saturating_sub(self.read_buffer.len());
            if remaining == 0 {
                return Err(line_too_large());
            }
            let read_len = remaining.min(chunk.len());
            let read = self
                .reader
                .as_mut()
                .ok_or_else(Self::retired)?
                .read(&mut chunk[..read_len])?;
            if read == 0 {
                if self.read_buffer.is_empty() {
                    return Ok(None);
                }
                if self.read_buffer.len() > MAX_RESPONSE_LINE_BYTES {
                    return Err(line_too_large());
                }
                return decode_server_line(std::mem::take(&mut self.read_buffer)).map(Some);
            }
            self.read_buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

fn call_timeout(method: &str, params: &Value) -> Duration {
    match method {
        "run_command" => {
            let seconds = params
                .get("timeout_s")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
                .unwrap_or(15.0)
                .clamp(0.1, 600.0);
            Duration::from_secs_f64(seconds) + Duration::from_secs(5)
        }
        "wait_for" => {
            let millis = params
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(30_000)
                .min(300_000);
            Duration::from_millis(millis) + Duration::from_secs(12)
        }
        "screenshot" => Duration::from_secs(15),
        "show" => crate::show::SHOW_CALL_TIMEOUT,
        _ => Duration::from_secs(15),
    }
}

fn map_write_error(error: std::io::Error) -> CtlError {
    match error.kind() {
        std::io::ErrorKind::TimedOut => CtlError::TimedOut,
        std::io::ErrorKind::Interrupted => CtlError::Cancelled,
        _ => CtlError::Io(error),
    }
}

fn parse_server_frame(line: &str) -> Result<ServerFrame, CtlError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| CtlError::Protocol(format!("malformed server frame: {error}")))?;
    let Some(object) = value.as_object() else {
        return Err(CtlError::Protocol("server frame must be an object".into()));
    };
    let has_id = object.contains_key("id");
    let has_event = object.contains_key("event");
    if has_id && has_event {
        return Err(CtlError::Protocol(
            "server frame cannot contain both 'id' and 'event'".into(),
        ));
    }
    if has_id {
        let response: Response = serde_json::from_value(value)
            .map_err(|error| CtlError::Protocol(format!("malformed response: {error}")))?;
        if response.v != PROTOCOL_VERSION {
            return Err(CtlError::Protocol(format!(
                "server response protocol v{} is unsupported; expected v{PROTOCOL_VERSION}",
                response.v
            )));
        }
        return Ok(ServerFrame::Response(response));
    }
    if has_event {
        let event: Event = serde_json::from_value(value)
            .map_err(|error| CtlError::Protocol(format!("malformed event: {error}")))?;
        if event.v != PROTOCOL_VERSION {
            return Err(CtlError::Protocol(format!(
                "server event protocol v{} is unsupported; expected v{PROTOCOL_VERSION}",
                event.v
            )));
        }
        return Ok(ServerFrame::Event(event));
    }
    Err(CtlError::Protocol(
        "server frame has neither 'id' nor 'event'".into(),
    ))
}

fn decode_server_line(bytes: Vec<u8>) -> Result<String, CtlError> {
    String::from_utf8(bytes)
        .map_err(|error| CtlError::Protocol(format!("server line is not UTF-8: {error}")))
}

fn line_too_large() -> CtlError {
    CtlError::Protocol(format!(
        "server line exceeds {MAX_RESPONSE_LINE_BYTES} bytes"
    ))
}

/// Time a client may spend reading its own ancestry to find its Kettle.
const ANCESTRY_BUDGET: Duration = Duration::from_millis(250);

/// `KETTLE_PID` when it is a well-formed pid: the Kettle whose pane started
/// this process, unless something changed it.
/// The Kettle a client that names none uses: the nearest one it runs
/// inside, matched by pid and start, else the one `KETTLE_PID` names. Under
/// [`Fallback::Nothing`] that inherited name counts only when its entry
/// records a start, since a pid alone may by now be another Kettle.
fn choose<'a>(
    candidates: &'a [(discovery::RegistryEntry, std::path::PathBuf)],
    ancestry: &[crate::process::ProcessIdentity],
    kettle_pid: Option<u32>,
    fallback: Fallback,
) -> Option<&'a (discovery::RegistryEntry, std::path::PathBuf)> {
    ancestry
        .iter()
        .skip(1)
        .find_map(|ancestor| {
            candidates.iter().find(|(entry, _)| {
                entry.pid == ancestor.pid() && entry.start_token == Some(ancestor.start())
            })
        })
        .or_else(|| {
            let pid = kettle_pid?;
            candidates.iter().find(|(entry, _)| {
                entry.pid == pid && (fallback == Fallback::Newest || entry.start_token.is_some())
            })
        })
}

/// The pid of the Kettle display discovery would use for this process,
/// without connecting to it: whether this session runs inside a Kettle that
/// can show media.
pub fn display_target() -> Option<u32> {
    let ancestry = crate::identity::current_ancestry(Instant::now() + ANCESTRY_BUDGET);
    let candidates =
        discovery::live_candidates_by(&discovery::registry_locations(), discovery::owner_alive);
    choose(
        &candidates,
        &ancestry,
        kettle_pid_hint(|name| std::env::var(name).ok()),
        Fallback::Nothing,
    )
    .map(|(entry, _)| entry.pid)
}

/// What discovery may try when no Kettle is named and none is an ancestor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fallback {
    /// The newest running server, for control and reads.
    Newest,
    /// Nothing: media must not land in an unrelated Kettle.
    Nothing,
}

fn kettle_pid_hint(env: impl Fn(&str) -> Option<String>) -> Option<u32> {
    env("KETTLE_PID")
        .filter(|value| {
            !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && !value.starts_with('0')
        })
        .and_then(|value| value.parse().ok())
}

/// This process's claim: its own pid and start token, plus the pane and
/// Kettle it was started in when its environment names them. `None` when the
/// OS will not describe this process, in which case requests carry no claim
/// and the server treats the caller as unverified.
fn own_claim(env: impl Fn(&str) -> Option<String>) -> Option<PeerClaim> {
    let me = crate::process::current().ok()?;
    let pid = std::num::NonZeroU32::new(me.identity.pid())?;
    // Strict parses: a malformed hint is no hint.
    let decimal = |name: &str| {
        env(name).filter(|value| {
            !value.is_empty()
                && value.len() <= 20
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && !value.starts_with('0')
        })
    };
    Some(PeerClaim {
        pid,
        start_token: Some(StartToken(me.identity.start())),
        pane_hint: decimal("KETTLE_PANE_ID").and_then(|value| value.parse().ok()),
        pid_hint: decimal("KETTLE_PID").and_then(|value| value.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;
    use crate::transport::CtlListener;
    use serde_json::json;
    use std::io::Write as _;

    fn test_listener(tag: &str) -> (CtlListener, String) {
        static NEXT_ENDPOINT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let pid = std::process::id();
        let unique = NEXT_ENDPOINT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(unix)]
        let endpoint = std::env::temp_dir()
            .join(format!("kettle-ctl-{tag}-{pid}-{unique}.sock"))
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\kettle-ctl-{tag}-{pid}-{unique}");
        let listener = CtlListener::bind(&endpoint).expect("bind");
        (listener, endpoint)
    }

    /// Spin up a loopback listener whose server side writes `lines` (each is a
    /// raw NDJSON message, no trailing newline needed) then closes, and return a
    /// `Client` connected to it. Mirrors `transport::tests` so the Windows
    /// named-pipe leg is exercised on CI too.
    fn client_fed(lines: Vec<String>) -> Client {
        let (listener, endpoint) = test_listener("fed");
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

    fn client_replies(lines: Vec<String>) -> Client {
        let (listener, endpoint) = test_listener("reply");
        let ep = endpoint.clone();
        std::thread::spawn(move || {
            use std::io::BufRead as _;

            let mut conn = listener.accept().expect("accept");
            let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut request = String::new();
            reader.read_line(&mut request).expect("read request");
            for line in lines {
                if conn.write_all(line.as_bytes()).is_err() || conn.write_all(b"\n").is_err() {
                    return;
                }
            }
            conn.flush().ok();
        });
        Client::connect_endpoint(&ep).expect("connect")
    }

    /// Like [`client_replies`], but answers each successive request with the
    /// next batch of raw lines — so a test can observe what a *second* call
    /// does with the connection.
    fn client_replies_in_turn(batches: Vec<Vec<String>>) -> Client {
        let (listener, endpoint) = test_listener("turns");
        let ep = endpoint.clone();
        std::thread::spawn(move || {
            use std::io::BufRead as _;

            let mut conn = listener.accept().expect("accept");
            let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
            for batch in batches {
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    return;
                }
                for line in batch {
                    if conn.write_all(line.as_bytes()).is_err() || conn.write_all(b"\n").is_err() {
                        return;
                    }
                }
                conn.flush().ok();
            }
        });
        Client::connect_endpoint(&ep).expect("connect")
    }

    fn client_stalled(hold: Duration) -> Client {
        let (listener, endpoint) = test_listener("stalled");
        let ep = endpoint.clone();
        let (accepted, wait_for_accept) = std::sync::mpsc::sync_channel(0);
        std::thread::spawn(move || {
            let _conn = listener.accept().expect("accept");
            accepted.send(()).expect("signal accepted stalled client");
            std::thread::sleep(hold);
        });
        let client = Client::connect_endpoint(&ep).expect("connect");
        wait_for_accept
            .recv_timeout(Duration::from_secs(1))
            .expect("server accepted stalled client");
        client
    }

    /// A server that answers every request with `ok` and reports each
    /// request line it received.
    fn client_recorded() -> (Client, std::sync::mpsc::Receiver<String>) {
        let (listener, endpoint) = test_listener("recorded");
        let (lines_tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let mut conn = listener.accept().expect("accept");
            let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
            loop {
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    return;
                }
                let id = serde_json::from_str::<Value>(&request).unwrap()["id"].clone();
                lines_tx.send(request).ok();
                let reply = format!(r#"{{"v":1,"id":{id},"ok":true,"result":{{}}}}"#);
                if conn.write_all(reply.as_bytes()).is_err() || conn.write_all(b"\n").is_err() {
                    return;
                }
            }
        });
        (Client::connect_endpoint(&endpoint).expect("connect"), lines)
    }

    /// The claim rides only the first request that reaches the wire.
    #[test]
    fn only_the_first_written_request_carries_the_claim() {
        let (mut client, lines) = client_recorded();
        let me = crate::process::current().unwrap().identity;
        // A call that fails before writing anything keeps the claim.
        assert!(matches!(
            client.call_with_timeout("get_state", Value::Null, Duration::ZERO),
            Err(CtlError::TimedOut)
        ));
        client.call("get_state", Value::Null).expect("first call");
        client.call("list_panes", Value::Null).expect("second call");
        let first: Value = serde_json::from_str(&lines.recv().unwrap()).unwrap();
        let second: Value = serde_json::from_str(&lines.recv().unwrap()).unwrap();
        assert_eq!(first["id"], 1, "the failed call sent nothing");
        assert_eq!(first["caller"]["pid"], me.pid());
        assert_eq!(first["caller"]["start_token"], me.start().to_string());
        assert!(second.get("caller").is_none(), "{second}");
    }

    /// A client inherited by another process must not speak as its parent.
    #[test]
    fn a_client_owned_by_another_process_refuses_to_write() {
        let (mut client, lines) = client_recorded();
        client.owner_pid = client.owner_pid.wrapping_add(1);
        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Unusable(_))
        ));
        assert!(
            lines.recv_timeout(Duration::from_millis(200)).is_err(),
            "nothing reached the server"
        );
    }

    #[test]
    fn claim_hints_come_only_from_well_formed_environment_values() {
        let claim = |pane: Option<&str>, pid: Option<&str>| {
            own_claim(|name| match name {
                "KETTLE_PANE_ID" => pane.map(str::to_string),
                "KETTLE_PID" => pid.map(str::to_string),
                _ => None,
            })
            .expect("this process can describe itself")
        };
        let both = claim(Some("7"), Some("4242"));
        assert_eq!(both.pane_hint, Some(7));
        assert_eq!(both.pid_hint.map(std::num::NonZeroU32::get), Some(4242));
        assert_eq!(both.pid.get(), std::process::id());
        for bad in ["", "07", "-1", "7 ", "x", "0", "99999999999999999999999"] {
            let parsed = claim(Some(bad), Some(bad));
            assert_eq!(parsed.pane_hint, None, "{bad:?}");
            assert_eq!(parsed.pid_hint, None, "{bad:?}");
        }
        assert_eq!(claim(None, None).pane_hint, None);
    }

    use crate::discovery::{self, RegistryEntry};
    use crate::process::ProcessIdentity;

    fn scratch_registry(tag: &str) -> std::path::PathBuf {
        let dir = crate::test_scratch_root()
            .join(format!("kettle-ctl-select-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Register a fake server `pid` started at `started` with `token`.
    fn fake_server(dir: &std::path::Path, pid: u32, started: u64, token: u64) {
        let mut entry = reg_entry(dir, pid, started);
        entry.start_token = Some(token);
        discovery::register(dir, &entry).unwrap();
    }

    /// Run discovery over `dirs`, recording which servers it tried. Every
    /// connect fails, so the result names the last one tried.
    fn tried(
        dirs: &[std::path::PathBuf],
        pid: Option<u32>,
        ancestry: &[ProcessIdentity],
        hint: Option<u32>,
    ) -> Vec<u32> {
        tried_with(dirs, pid, ancestry, hint, Fallback::Newest).0
    }

    /// [`tried`] under `fallback`, with whether discovery ended in
    /// [`CtlError::NotInKettle`].
    fn tried_with(
        dirs: &[std::path::PathBuf],
        pid: Option<u32>,
        ancestry: &[ProcessIdentity],
        hint: Option<u32>,
        fallback: Fallback,
    ) -> (Vec<u32>, bool) {
        let attempts = std::cell::RefCell::new(Vec::new());
        let connect = |entry: &RegistryEntry| -> Result<Client, CtlError> {
            attempts.borrow_mut().push(entry.pid);
            Err(CtlError::Io(std::io::Error::other("refused")))
        };
        let outcome =
            Client::discover_in(dirs, pid, ancestry, hint, fallback, connect, |_entry| true);
        let outside = matches!(outcome, Err(CtlError::NotInKettle));
        (attempts.into_inner(), outside)
    }

    /// Display discovery tries the Kettle named or the one a client runs in,
    /// then the one `KETTLE_PID` names when its entry records a start, and
    /// never another: outside every Kettle it says so and connects nowhere.
    #[test]
    fn display_discovery_never_uses_an_unrelated_instance() {
        let dir = scratch_registry("display");
        fake_server(&dir, 10, 100, 1000);
        fake_server(&dir, 20, 200, 2000);
        // An entry from a build without start tokens names only a pid.
        discovery::register(&dir, &reg_entry(&dir, 30, 300)).unwrap();
        let me = ProcessIdentity::new(1, 5000);
        let dirs = [dir.clone()];
        let display = |pid, ancestry: &[ProcessIdentity], hint| {
            tried_with(&dirs, pid, ancestry, hint, Fallback::Nothing)
        };
        assert_eq!(display(None, &[me], None), (vec![], true));
        assert_eq!(display(None, &[me], Some(99)), (vec![], true));
        assert_eq!(display(None, &[me], Some(10)), (vec![10], false));
        assert_eq!(display(None, &[me], Some(30)), (vec![], true));
        let inside_twenty = [me, ProcessIdentity::new(20, 2000)];
        assert_eq!(display(None, &inside_twenty, Some(10)), (vec![20], false));
        assert_eq!(display(Some(30), &[me], None), (vec![30], false));
        // Control discovery still falls back, and takes a pid-only entry.
        assert_eq!(tried(&dirs, None, &[me], Some(30)), [30]);
        assert_eq!(tried(&dirs, None, &[me], None).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Kettle a client runs inside wins over a newer one, the nearest of
    /// nested ones wins, and its failure never falls through to another.
    #[test]
    fn the_enclosing_kettle_is_chosen_and_never_swapped() {
        let dir = scratch_registry("ancestor");
        fake_server(&dir, 10, 100, 1000);
        fake_server(&dir, 20, 200, 2000);
        let me = ProcessIdentity::new(1, 5000);
        let dirs = [dir.clone()];
        // Outside any Kettle: newest first, then the rest.
        assert_eq!(tried(&dirs, None, &[me], None), [20, 10]);
        // Inside 10 (older): only 10, even though it fails.
        let inside_ten = [
            me,
            ProcessIdentity::new(7, 4000),
            ProcessIdentity::new(10, 1000),
        ];
        assert_eq!(tried(&dirs, None, &inside_ten, None), [10]);
        // Nested: 20's pane inside 10's pane. The nearer one wins.
        let nested = [
            me,
            ProcessIdentity::new(20, 2000),
            ProcessIdentity::new(10, 1000),
        ];
        assert_eq!(tried(&dirs, None, &nested, None), [20]);
        // A matching pid with another start is a stranger that reused it.
        let reused = [me, ProcessIdentity::new(10, 1001)];
        assert_eq!(tried(&dirs, None, &reused, None), [20, 10]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kettle_pid_is_a_fallback_and_an_explicit_pid_is_exact() {
        let dir = scratch_registry("hint");
        fake_server(&dir, 10, 100, 1000);
        fake_server(&dir, 20, 200, 2000);
        let me = ProcessIdentity::new(1, 5000);
        let dirs = [dir.clone()];
        assert_eq!(tried(&dirs, None, &[me], Some(10)), [10]);
        // The ancestry beats the hint.
        let inside_twenty = [me, ProcessIdentity::new(20, 2000)];
        assert_eq!(tried(&dirs, None, &inside_twenty, Some(10)), [20]);
        // A hint naming no live server is ignored.
        assert_eq!(tried(&dirs, None, &[me], Some(99)), [20, 10]);
        // An explicit pid is the only one tried, and an unknown one is none.
        assert_eq!(tried(&dirs, Some(10), &inside_twenty, None), [10]);
        assert_eq!(tried(&dirs, Some(99), &[me], None), Vec::<u32>::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(kettle_pid_hint(|_| Some("4242".into())), Some(4242));
        for bad in ["", "0", "042", "-1", "x", "4242 "] {
            assert_eq!(kettle_pid_hint(|_| Some(bad.into())), None, "{bad:?}");
        }
    }

    /// A client whose environment points nowhere still finds a server through
    /// the alias left where the OS puts the registry; the alias only points,
    /// and a dead server's alias goes while its entry stays for its own
    /// registry to prune.
    #[test]
    fn an_alias_leads_a_stripped_client_to_the_real_entry() {
        let primary = scratch_registry("alias-primary");
        let canonical = scratch_registry("alias-canonical");
        fake_server(&primary, 30, 300, 3000);
        discovery::publish_alias(&canonical, &primary, 30).unwrap();
        assert_eq!(
            tried(std::slice::from_ref(&canonical), None, &[], None),
            [30]
        );
        // Both locations, in either order: one candidate, not two.
        assert_eq!(
            tried(&[canonical.clone(), primary.clone()], None, &[], None),
            [30]
        );
        assert_eq!(
            tried(&[primary.clone(), canonical.clone()], None, &[], None),
            [30]
        );
        // An alias to a registry without that entry leads nowhere.
        discovery::publish_alias(&canonical, &primary, 31).unwrap();
        assert_eq!(
            tried(std::slice::from_ref(&canonical), None, &[], None),
            [30]
        );
        let attempts = std::cell::RefCell::new(Vec::new());
        let _ = Client::discover_in(
            std::slice::from_ref(&canonical),
            None,
            &[],
            None,
            Fallback::Newest,
            |entry: &RegistryEntry| {
                attempts.borrow_mut().push(entry.pid);
                Err(CtlError::NoServer)
            },
            |_entry| false,
        );
        assert!(attempts.into_inner().is_empty());
        assert!(
            !canonical.join("30.alias.json").exists(),
            "a dead server's alias is withdrawn"
        );
        assert!(
            primary.join("30.json").exists(),
            "the entry belongs to its own registry"
        );
        let _ = std::fs::remove_dir_all(&primary);
        let _ = std::fs::remove_dir_all(&canonical);
    }

    /// Before any request bytes, the server at an entry's endpoint must be
    /// the process the entry names.
    #[test]
    fn a_connection_must_reach_the_process_its_entry_names() {
        let (listener, endpoint) = test_listener("authenticate");
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok(conn) = listener.accept() {
                held.push(conn);
            }
        });
        let me = crate::process::current().unwrap().identity;
        let entry = |pid: u32, token: Option<u64>| {
            let dir = std::path::Path::new(&endpoint).parent().unwrap();
            let mut entry = reg_entry(dir, pid, 1);
            entry.endpoint = endpoint.clone();
            entry.start_token = token;
            entry
        };
        assert!(Client::connect_authenticated(&entry(me.pid(), Some(me.start()))).is_ok());
        assert!(Client::connect_authenticated(&entry(me.pid(), None)).is_ok());
        for wrong in [
            entry(me.pid(), Some(me.start() + 1)),
            entry(me.pid().wrapping_add(1), Some(me.start())),
        ] {
            assert!(matches!(
                Client::connect_authenticated(&wrong),
                Err(CtlError::Protocol(_))
            ));
        }
    }

    fn reg_entry(dir: &std::path::Path, pid: u32, started: u64) -> RegistryEntry {
        RegistryEntry::registering(
            "gui",
            pid,
            discovery::default_endpoint(dir, pid),
            "x",
            started,
        )
    }

    /// When `connect_endpoint` fails but the owning pid is still ALIVE (a
    /// client-side `try_clone` hiccup or a transient transport error; the server
    /// `register`s exactly once, no heartbeat), the healthy entry MUST NOT be
    /// pruned, and the real transport error must surface rather than be masked
    /// as a blanket `NoServer`.
    #[test]
    fn discover_does_not_prune_live_pid_on_connect_failure() {
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-disc-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pid = 4242;
        discovery::register(&dir, &reg_entry(&dir, pid, 100)).unwrap();

        // Connect always fails with a transport error; pid is reported alive.
        let connect = |_entry: &RegistryEntry| -> Result<Client, CtlError> {
            Err(CtlError::Io(std::io::Error::other(
                "transient transport hiccup",
            )))
        };
        let res = Client::discover_in(
            std::slice::from_ref(&dir),
            None,
            &[],
            None,
            Fallback::Newest,
            connect,
            |_entry| true,
        );

        match res {
            Err(CtlError::Io(_)) => {}
            Err(other) => panic!("expected the transport Io error to surface, got {other:?}"),
            Ok(_) => panic!("connect closure always errs; discover must not succeed"),
        }
        assert!(
            discovery::list(&dir).iter().any(|e| e.pid == pid),
            "a live server's entry must survive a transient connect failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Conversely, an entry whose owner is DEAD is pruned (the complementary
    /// half of the gate).
    #[test]
    fn discover_prunes_dead_pid_on_connect_failure() {
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-disc-dead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pid = 4243;
        discovery::register(&dir, &reg_entry(&dir, pid, 100)).unwrap();

        let connect = |_entry: &RegistryEntry| -> Result<Client, CtlError> {
            Err(CtlError::Io(std::io::Error::other("x")))
        };
        // owner reported dead → enumeration filter prunes it before any
        // connect, so discovery yields NoServer and the entry is gone.
        let res = Client::discover_in(
            std::slice::from_ref(&dir),
            None,
            &[],
            None,
            Fallback::Newest,
            connect,
            |_entry| false,
        );
        assert!(matches!(res, Err(CtlError::NoServer)));
        assert!(
            !discovery::list(&dir).iter().any(|e| e.pid == pid),
            "a dead server's entry must be pruned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two tests above inject their liveness answers; this one runs the
    /// predicate the production path actually passes
    /// (`discovery::owner_alive`), so the wiring is covered rather than only
    /// the plumbing around it. An entry whose pid is live but whose process
    /// *instance* is gone must be pruned; the one written by this instance
    /// must not.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn discovery_prunes_a_recycled_pid_under_the_real_liveness_predicate() {
        let dir =
            crate::test_scratch_root().join(format!("kettle-ctl-disc-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = std::process::id();
        let connect = |_entry: &RegistryEntry| -> Result<Client, CtlError> {
            Err(CtlError::Io(std::io::Error::other(
                "transient transport hiccup",
            )))
        };

        let mut recycled = reg_entry(&dir, me, 100);
        recycled.start_token = Some(
            recycled
                .start_token
                .expect("a supported platform reports a start token")
                .wrapping_add(1),
        );
        discovery::register(&dir, &recycled).unwrap();
        assert!(matches!(
            Client::discover_in(
                std::slice::from_ref(&dir),
                None,
                &[],
                None,
                Fallback::Newest,
                connect,
                discovery::owner_alive
            ),
            Err(CtlError::NoServer)
        ));
        assert!(
            discovery::list(&dir).is_empty(),
            "a live pid running a different instance is not a live server"
        );

        discovery::register(&dir, &reg_entry(&dir, me, 200)).unwrap();
        assert!(matches!(
            Client::discover_in(
                std::slice::from_ref(&dir),
                None,
                &[],
                None,
                Fallback::Newest,
                connect,
                discovery::owner_alive
            ),
            Err(CtlError::Io(_)),
        ));
        assert!(
            discovery::list(&dir).iter().any(|e| e.pid == me),
            "this instance's own entry survives a failing connect"
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

    #[test]
    fn client_rejects_non_v1_response_and_event() {
        let response = serde_json::json!({
            "v": 0,
            "id": 1,
            "ok": true,
            "result": {},
        })
        .to_string();
        let mut client = client_replies(vec![response]);
        let error = client.call("get_state", Value::Null).unwrap_err();
        assert!(matches!(error, CtlError::Protocol(message) if message.contains("expected v1")));

        let event = serde_json::json!({"v": 0, "event": "output", "data": "x"}).to_string();
        let mut client = client_fed(vec![event]);
        let error = client.next_event().unwrap_err();
        assert!(matches!(error, CtlError::Protocol(message) if message.contains("expected v1")));
    }

    #[test]
    fn client_rejects_oversize_server_line() {
        let mut client = client_fed(vec!["x".repeat(MAX_RESPONSE_LINE_BYTES + 1)]);
        let error = client.next_event().unwrap_err();
        assert!(matches!(error, CtlError::Protocol(message) if message.contains("exceeds")));
    }

    #[test]
    fn call_rejects_malformed_and_mismatched_frames() {
        let mut malformed = client_replies(vec!["not-json".into()]);
        assert!(matches!(
            malformed.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("malformed server frame")
        ));

        let response = serde_json::to_string(&Response::ok(99, json!({}))).unwrap();
        let mut mismatched = client_replies(vec![response]);
        assert!(matches!(
            mismatched.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("does not match request id")
        ));
    }

    #[test]
    fn call_preserves_an_event_that_precedes_its_response() {
        let event = serde_json::to_string(&Event::new("output", Some(7), json!("ready"))).unwrap();
        let event_bytes = event.len();
        let response = serde_json::to_string(&Response::ok(1, json!({"state": "ok"}))).unwrap();
        let mut client = client_replies(vec![event, response]);

        assert_eq!(
            client.call("get_state", Value::Null).unwrap()["state"],
            "ok"
        );
        assert_eq!(client.queued_event_bytes, event_bytes);
        let queued = client.next_event().unwrap().expect("queued event");
        assert_eq!(queued.event, "output");
        assert_eq!(queued.pane, Some(7));
        assert_eq!(client.queued_event_bytes, 0);
    }

    #[test]
    fn call_bounds_events_that_precede_a_response() {
        let event = serde_json::to_string(&Event::new("output", Some(7), json!("x"))).unwrap();
        let mut lines = vec![event; MAX_QUEUED_EVENTS_DURING_CALL + 1];
        lines.push(serde_json::to_string(&Response::ok(1, json!({}))).unwrap());
        let mut client = client_replies(lines);

        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("events arrived before")
        ));
    }

    #[test]
    fn call_bounds_cumulative_event_bytes() {
        let event = serde_json::to_string(&Event::new(
            "output",
            Some(7),
            Value::String("x".repeat(128 * 1024)),
        ))
        .unwrap();
        let count = MAX_QUEUED_EVENT_BYTES / event.len() + 1;
        assert!(count < MAX_QUEUED_EVENTS_DURING_CALL);
        let mut lines = vec![event; count];
        lines.push(serde_json::to_string(&Response::ok(1, json!({}))).unwrap());
        let mut client = client_replies(lines);

        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("queued event data exceeds")
        ));
    }

    #[test]
    fn call_deadline_and_cancellation_are_bounded() {
        let mut timed = client_stalled(Duration::from_millis(250));
        let started = Instant::now();
        assert!(matches!(
            timed.call_with_timeout("get_state", Value::Null, Duration::from_millis(30)),
            Err(CtlError::TimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));

        let mut cancelled_client = client_stalled(Duration::from_millis(500));
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let setter = cancelled.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, Ordering::Release);
        });
        let started = Instant::now();
        assert!(matches!(
            cancelled_client.call_cancellable("get_state", Value::Null, &cancelled),
            Err(CtlError::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn call_fails_cleanly_when_request_ids_are_exhausted() {
        let response = serde_json::to_string(&Response::ok(u64::MAX - 1, json!({}))).unwrap();
        let mut client = client_replies(vec![response]);
        client.next_id = u64::MAX - 1;

        assert_eq!(client.call("get_state", Value::Null).unwrap(), json!({}));
        assert_eq!(client.next_id, u64::MAX);
        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("request id space is exhausted")
        ));
        assert_eq!(client.next_id, u64::MAX);
    }

    #[test]
    fn zero_timeout_does_not_consume_an_id_or_send_a_request() {
        let mut client = client_stalled(Duration::from_millis(50));

        assert!(matches!(
            client.call_with_timeout("send_text", json!({"text": "side effect"}), Duration::ZERO),
            Err(CtlError::TimedOut)
        ));
        assert_eq!(client.next_id, 1);
        assert!(
            client.is_usable(),
            "a request that never reached the wire leaves the connection in step"
        );
    }

    /// A response the caller stopped waiting for is still coming. Reusing the
    /// connection would read it as the NEXT call's response, and would put a
    /// second (possibly mutating) request onto a stream nobody can correlate.
    ///
    /// Three guards prevent that, and this test pins each one: the second
    /// request is refused locally, **no second request ever reaches the wire**,
    /// and retiring closes the transport while this client is still alive and
    /// un-dropped. The last two are checked at the server, not on the client's
    /// word.
    #[test]
    fn a_timed_out_call_retires_the_connection_before_a_late_response_lands() {
        let (listener, endpoint) = test_listener("late");
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        let (closed, wait_for_close) = std::sync::mpsc::channel();
        // Wait for the server to be INSIDE a connection before the client does
        // anything. On Windows the pipe instance exists from `bind`, so without
        // this the client could connect, write, hit its 40 ms deadline, retire,
        // and close its handle before the thread reaches `accept`. That `accept`
        // would then tear down the poisoned instance, create a fresh one, and
        // block forever on a client that never comes, so `closed` is never
        // sent. `client_stalled` above synchronizes the same way.
        let (accepted, wait_for_accept) = std::sync::mpsc::sync_channel(0);
        std::thread::spawn(move || {
            use std::io::BufRead as _;

            // Nothing is answered for the first request. If a second one ever
            // arrives, flush BOTH responses, the stale one first: exactly the
            // frame a reused client would mistake for its own.
            let mut conn = listener.accept().expect("accept");
            accepted.send(()).expect("signal the accepted connection");
            let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
            loop {
                let mut request = String::new();
                match reader.read_line(&mut request) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if counted.fetch_add(1, Ordering::Relaxed) == 0 {
                    continue;
                }
                for id in [1, 2] {
                    let line = serde_json::to_string(&Response::ok(id, json!({"late": id})))
                        .expect("serialize");
                    if conn.write_all(line.as_bytes()).is_err() || conn.write_all(b"\n").is_err() {
                        break;
                    }
                }
                conn.flush().ok();
            }
            let _ = closed.send(());
        });
        let mut client = Client::connect_endpoint(&endpoint).expect("connect");
        wait_for_accept
            .recv_timeout(Duration::from_secs(5))
            .expect("server accepted the connection");

        assert!(matches!(
            client.call_with_timeout("get_state", Value::Null, Duration::from_millis(40)),
            Err(CtlError::TimedOut)
        ));
        assert!(!client.is_usable());
        assert_eq!(client.next_id, 2, "the abandoned request consumed its id");

        assert!(matches!(
            client.call_with_timeout(
                "send_text",
                json!({"text": "side effect"}),
                Duration::from_millis(40),
            ),
            Err(CtlError::Unusable(reason)) if reason.contains("timed-out")
        ));
        assert_eq!(
            client.next_id, 2,
            "a refused request must not be written to the abandoned stream"
        );
        wait_for_close
            .recv_timeout(Duration::from_secs(5))
            .expect("retiring closed the transport rather than waiting for the client to drop");
        assert_eq!(
            requests.load(Ordering::Relaxed),
            1,
            "only the abandoned request ever reached the wire"
        );
        assert!(!client.is_usable(), "and the client is still alive here");
    }

    /// A deadline can expire before the first byte of a request goes out. The
    /// server never learned of that request, so there is no response in flight
    /// and nothing to correlate — retiring a healthy connection there would
    /// throw away the very thing the caller is about to reuse.
    #[test]
    fn a_deadline_that_beats_the_first_byte_leaves_the_connection_usable() {
        let answer = serde_json::to_string(&Response::ok(2, json!({"state": "ok"}))).unwrap();
        let mut client = client_replies(vec![answer]);

        assert!(matches!(
            client.call_with_timeout("send_text", json!({"text": "x"}), Duration::from_nanos(1)),
            Err(CtlError::TimedOut)
        ));
        assert!(
            client.is_usable(),
            "a request that put nothing on the wire leaves the connection in step"
        );
        assert_eq!(
            client.call("get_state", Value::Null).unwrap()["state"],
            "ok",
            "and the connection still serves the next call"
        );
    }

    /// Ending the *caller's* wait does not end the server's work: a request
    /// already on the wire may be carried out anyway. The caller learns that
    /// where it will actually read it — `kettle ctl` and the MCP bridge render
    /// this `Display` and nothing else — rather than only in a design doc.
    #[test]
    fn giving_up_on_a_request_says_the_server_may_still_have_run_it() {
        for error in [CtlError::TimedOut, CtlError::Cancelled] {
            let rendered = error.to_string();
            assert!(
                rendered.contains("may already have performed it"),
                "an agent reading {rendered:?} would think the request did not happen"
            );
        }
    }

    /// The same rule for cancellation: the caller gave up waiting, but the
    /// server may still be executing the request it already received.
    #[test]
    fn a_cancelled_call_retires_the_connection() {
        let mut client = client_stalled(Duration::from_millis(500));
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let setter = cancelled.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, Ordering::Release);
        });
        assert!(matches!(
            client.call_cancellable("get_state", Value::Null, &cancelled),
            Err(CtlError::Cancelled)
        ));
        assert!(!client.is_usable());
        assert_eq!(client.next_id, 2);

        assert!(matches!(
            client.call_with_timeout("send_text", json!({"text": "x"}), Duration::from_millis(30)),
            Err(CtlError::Unusable(reason)) if reason.contains("cancelled")
        ));
        assert_eq!(
            client.next_id, 2,
            "a refused request must not be written to the abandoned stream"
        );
    }

    /// Breaching the queued-event bound abandons the call mid-stream, so the
    /// events buffered for it are not later handed out as a subscription feed
    /// and the connection is not reused.
    #[test]
    fn exceeding_the_event_bound_retires_the_connection() {
        let event = serde_json::to_string(&Event::new("output", Some(7), json!("x"))).unwrap();
        let mut lines = vec![event; MAX_QUEUED_EVENTS_DURING_CALL + 1];
        lines.push(serde_json::to_string(&Response::ok(1, json!({}))).unwrap());
        let mut client = client_replies(lines);

        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("events arrived before")
        ));
        assert!(!client.is_usable());
        assert!(matches!(client.next_event(), Err(CtlError::Unusable(_))));
        assert!(matches!(
            client.call_with_timeout("get_state", Value::Null, Duration::from_millis(30)),
            Err(CtlError::Unusable(_))
        ));
    }

    /// Retiring also releases what the abandoned exchange had buffered. Those
    /// events were queued for a caller that can never collect them — up to
    /// `MAX_QUEUED_EVENT_BYTES` of them — and would otherwise sit in a client
    /// its owner is still holding.
    #[test]
    fn retiring_a_connection_releases_what_it_buffered() {
        let event = serde_json::to_string(&Event::new("output", Some(7), json!("x"))).unwrap();
        let mut lines = vec![event; MAX_QUEUED_EVENTS_DURING_CALL + 1];
        lines.push(serde_json::to_string(&Response::ok(1, json!({}))).unwrap());
        let mut client = client_replies(lines);

        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("events arrived before")
        ));
        assert!(
            client.queued_events.is_empty(),
            "events queued for the abandoned call are released, not held"
        );
        assert_eq!(client.queued_event_bytes, 0);
        assert!(
            client.read_buffer.is_empty(),
            "and so are the raw bytes read past the point of failure"
        );
        assert_eq!(client.read_scan_offset, 0);
    }

    /// Malformed data leaves the reader inside bytes it could not interpret,
    /// so the event stream is retired with the call that hit it.
    #[test]
    fn a_malformed_frame_retires_the_connection() {
        let mut client = client_replies(vec!["not-json".into()]);

        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Protocol(message)) if message.contains("malformed server frame")
        ));
        assert!(!client.is_usable());
        assert!(matches!(client.next_event(), Err(CtlError::Unusable(_))));
    }

    /// The complementary half: a structured server error IS this request's
    /// response, so the connection stays in step and keeps serving calls.
    #[test]
    fn a_server_error_response_leaves_the_connection_usable() {
        let refused =
            serde_json::to_string(&Response::err(1, "bad_params", "pane is required")).unwrap();
        let accepted = serde_json::to_string(&Response::ok(2, json!({"state": "ok"}))).unwrap();
        let mut client = client_replies_in_turn(vec![vec![refused], vec![accepted]]);

        assert!(matches!(
            client.call("get_state", Value::Null),
            Err(CtlError::Server { code, .. }) if code == "bad_params"
        ));
        assert!(client.is_usable());
        assert_eq!(
            client.call("get_state", Value::Null).unwrap()["state"],
            "ok"
        );
    }

    #[test]
    fn write_deadline_errors_keep_their_public_semantics() {
        assert!(matches!(
            map_write_error(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "deadline"
            )),
            CtlError::TimedOut
        ));
        assert!(matches!(
            map_write_error(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled"
            )),
            CtlError::Cancelled
        ));
        assert!(matches!(
            map_write_error(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "peer")),
            CtlError::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe
        ));
    }
}
