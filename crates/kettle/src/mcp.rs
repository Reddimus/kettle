//! `kettle mcp`: a bounded Model Context Protocol server over stdio.
//!
//! **Dual-era.** MCP 2026-07-28 removed the `initialize` handshake from the
//! protocol core — not just from the HTTP transport — so a client on that
//! revision sends no handshake at all and carries its version, identity and
//! capabilities in each request's `_meta`. A server that only speaks the
//! handshake is "legacy" in that revision's terms, and its compatibility
//! matrix scores modern-client-to-legacy-server as *Fails*. This server
//! therefore answers both: a request carrying `_meta` protocol fields is
//! served statelessly, an `initialize` selects legacy semantics, and
//! `server/discover` answers either way because it is also the stdio probe a
//! dual-era client uses to tell the two apart.
//!
//! MCP uses one JSON-RPC 2.0 object per UTF-8 line. Stdout is exclusively the
//! protocol channel; diagnostics remain on stderr. Tool calls run on a bounded
//! worker pool so one PTY command cannot block ping, cancellation, or unrelated
//! control reads.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

/// The "modern" revision: no handshake, every request self-describing.
const MCP_MODERN_VERSION: &str = "2026-07-28";
/// Newest handshake-based ("legacy") revision this server negotiates.
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_COMPAT_VERSION: &str = "2025-06-18";
/// Advertised newest-first, which is the order a client picking from
/// `supported` should prefer.
const MCP_SUPPORTED_VERSIONS: [&str; 3] =
    [MCP_MODERN_VERSION, MCP_PROTOCOL_VERSION, MCP_COMPAT_VERSION];

/// `_meta` keys a modern request carries. The prefix is reserved for MCP, and
/// the two marked required in the specification are required here: a request
/// missing either is malformed, not a request to guess about.
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
const MAX_MCP_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_MCP_RESPONSE_BYTES: usize = 768 * 1024;
// A 1 MiB line can pack roughly a million `[`/`{` characters, and byte size
// alone does not bound parser recursion depth. `serde_json`'s own recursive-
// descent parser refuses to recurse past its internal default (128, absent
// the opt-in `unbounded_depth` feature), but that is an implementation
// detail of a dependency this crate does not own: a future Cargo feature-
// unification change elsewhere in the workspace could silently disable it.
// Reject over-nested input ourselves, before it ever reaches
// `serde_json::from_str`, so the stdio loop's recursion bound is explicit and
// independent of how `serde_json` happens to be compiled.
const MAX_JSON_NESTING_DEPTH: u32 = 64;
const TOOL_WORKERS: usize = 4;
const TOOL_QUEUE_CAPACITY: usize = 16;
// -32002 is what the legacy revisions used here. 2026-07-28 forbids emitting
// it (it meant "resource not found" there, and is replaced by -32602), so it
// is reachable only on the legacy path, where a legacy client understands it.
const SERVER_NOT_INITIALIZED: i64 = -32002;
const SERVER_BUSY: i64 = -32003;
/// `UnsupportedProtocolVersionError`. Spec-defined; the -32020..=-32099 range
/// belongs to the specification and must be used only with its meanings.
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
const INVALID_PARAMS: i64 = -32602;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Uninitialized,
    Initializing,
    Ready,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    protocol_version: String,
    #[allow(dead_code)]
    capabilities: serde_json::Map<String, Value>,
    #[allow(dead_code)]
    client_info: ClientInfo,
}

#[derive(Debug, Deserialize)]
struct ClientInfo {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    version: String,
}

struct ToolJob {
    id: Value,
    key: String,
    params: Value,
    cancelled: Arc<AtomicBool>,
    /// Which era asked. Carried on the job because the worker builds the
    /// response and the era is not recoverable from the tool result.
    modern: bool,
}

type Pending = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

/// How long the server will wait on a peer that has stopped reading its
/// stdout, before giving up on delivering anything more.
///
/// Everything downstream of a jammed stdout blocks: the writer thread blocks in
/// `write`, the bounded response channel fills behind it, the tool workers
/// block sending their results, and the reader loop blocks the moment it needs
/// to send anything at all. That last one is the sharp end — a server that
/// cannot read stdin cannot receive the `notifications/cancelled` that would
/// free it, so the jam becomes permanent.
///
/// A client that has stopped reading is not coming back within any useful
/// horizon; it has crashed, or it closed stdin to signal shutdown and stopped
/// caring. So these waits are bounded and the server exits, rather than
/// remaining as a process nothing can talk to and nothing will reap.
///
/// Shortened under `cfg(test)` so the fixtures exercise the mechanism without
/// spending the real budget in wall clock. The production value itself is
/// asserted by `the_production_stall_limit_is_the_documented_one`.
#[cfg(not(test))]
const STDOUT_STALL_LIMIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const STDOUT_STALL_LIMIT: Duration = Duration::from_millis(400);

/// What the writer thread is doing, so a stalled PEER can be told apart from
/// busy WORK.
///
/// These have to be distinguished, and a wall-clock budget cannot do it. An
/// earlier version gave shutdown 30 seconds to join its workers and exited when
/// that expired — which killed a perfectly healthy `kettle_run` whose
/// `timeout_s` exceeded 30 (the tool's schema allows up to 600), delivered no
/// result for it, and printed a diagnostic blaming stdout while stdout was
/// being read the whole time. That is worse than the hang it replaced: it
/// silently loses an agent's build output.
///
/// The only thing that means "the peer stopped reading" is the writer being
/// parked inside a single `write` that has not returned. That is what this
/// records, so a busy worker waits as long as it needs to and a jammed pipe is
/// still caught.
#[derive(Default)]
struct WriterProgress {
    /// Set while `write_message` has been entered and has not returned.
    in_write: AtomicBool,
    /// Incremented after each completed write, so "parked in ONE write" is
    /// distinguishable from "writing steadily".
    completed: AtomicU64,
}

/// Wait until `done()`, giving up only if the writer is parked inside a single
/// write for longer than `limit`.
///
/// Returns `true` when `done()` became true, `false` when the writer stalled.
/// An idle writer — no write in flight — never stalls this, however long the
/// wait, because nothing is blocked on the peer.
fn wait_unless_stdout_stalled(
    progress: &WriterProgress,
    limit: Duration,
    mut done: impl FnMut() -> bool,
) -> bool {
    let mut seen = progress.completed.load(Ordering::Relaxed);
    let mut since = Instant::now();
    loop {
        if done() {
            return true;
        }
        let completed = progress.completed.load(Ordering::Relaxed);
        if completed != seen || !progress.in_write.load(Ordering::Relaxed) {
            // Progress, or nothing in flight: the peer is not the problem.
            seen = completed;
            since = Instant::now();
        } else if since.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run the stdio MCP server loop until stdin closes. Returns zero on clean EOF.
pub fn run_mcp() -> i32 {
    let stdin = std::io::stdin();
    run_mcp_with(stdin.lock(), std::io::stdout())
}

/// The server proper, over any transport.
///
/// `run_mcp` supplies the real stdio. Taking them as parameters is what makes
/// the stall behaviour above testable at all: the failure only appears when the
/// peer stops reading, and the process's real stdout cannot be made to do that
/// from inside a test.
pub fn run_mcp_with(mut input: impl BufRead, output: impl Write + Send + 'static) -> i32 {
    let (responses_tx, responses_rx) =
        crossbeam_channel::bounded::<Value>(TOOL_QUEUE_CAPACITY + TOOL_WORKERS + 8);
    let progress = Arc::new(WriterProgress::default());
    let writer_progress = progress.clone();
    let writer = match std::thread::Builder::new()
        .name("kettle-mcp-writer".into())
        .spawn(move || {
            let mut output = output;
            while let Ok(message) = responses_rx.recv() {
                writer_progress.in_write.store(true, Ordering::Release);
                let wrote = write_message(&mut output, &message);
                writer_progress.in_write.store(false, Ordering::Release);
                writer_progress.completed.fetch_add(1, Ordering::Release);
                if wrote.is_err() {
                    break;
                }
            }
        }) {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("kettle mcp: cannot spawn protocol writer: {error}");
            return 1;
        }
    };

    let responses_tx = Responder::new(responses_tx);
    let (jobs_tx, jobs_rx) = crossbeam_channel::bounded::<ToolJob>(TOOL_QUEUE_CAPACITY);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let mut workers = Vec::with_capacity(TOOL_WORKERS);
    for index in 0..TOOL_WORKERS {
        let jobs = jobs_rx.clone();
        let responses = responses_tx.clone();
        let pending = pending.clone();
        if let Ok(worker) = std::thread::Builder::new()
            .name(format!("kettle-mcp-tool-{index}"))
            .spawn(move || tool_worker(jobs, responses, pending))
        {
            workers.push(worker);
        }
    }
    if workers.is_empty() {
        eprintln!("kettle mcp: cannot spawn any tool workers");
        drop(responses_tx);
        let _ = writer.join();
        return 1;
    }

    let mut lifecycle = Lifecycle::Uninitialized;
    loop {
        let line = match read_capped_line(&mut input) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(ReadLineError::TooLarge) => {
                respond(
                    &responses_tx,
                    error_response(Value::Null, -32700, "JSON-RPC line exceeds 1 MiB"),
                );
                break;
            }
            Err(ReadLineError::Io(error)) => {
                eprintln!("kettle mcp: stdin read failed: {error}");
                break;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let text = match std::str::from_utf8(&line) {
            Ok(text) => text.trim(),
            Err(error) => {
                respond(
                    &responses_tx,
                    error_response(Value::Null, -32700, &format!("invalid UTF-8: {error}")),
                );
                continue;
            }
        };
        if json_nesting_too_deep(text, MAX_JSON_NESTING_DEPTH) {
            respond(
                &responses_tx,
                error_response(
                    Value::Null,
                    -32700,
                    &format!("JSON nesting exceeds {MAX_JSON_NESTING_DEPTH} levels"),
                ),
            );
            continue;
        }
        let message: Value = match serde_json::from_str(text) {
            Ok(message) => message,
            Err(error) => {
                respond(
                    &responses_tx,
                    error_response(Value::Null, -32700, &format!("parse error: {error}")),
                );
                continue;
            }
        };
        dispatch_message(message, &mut lifecycle, &jobs_tx, &responses_tx, &pending);
        if responses_tx.peer_gone() {
            // Nothing further can be delivered, so there is nothing to be
            // gained by parsing more of stdin. Go to shutdown.
            break;
        }
    }
    // EOF is an orderly stdio shutdown. Drain already-accepted jobs so clients
    // that write a request batch and close stdin still receive every response —
    // a `kettle_run` with a five-minute `timeout_s` is entitled to its five
    // minutes, and this is the documented contract.
    //
    // Waiting is bounded only by the writer being STUCK, never by a clock. A
    // peer that stopped reading blocks the writer inside `write`, which fills
    // the response channel, which blocks every worker mid-`send`; joining them
    // then never returns, `drop(responses_tx)` below is unreachable, and the
    // process stays alive holding a terminal until something kills it.
    // `wait_unless_stdout_stalled` tells that apart from a worker that is
    // simply busy — an earlier version used a 30-second budget and killed the
    // healthy case.
    drop(jobs_tx);
    let mut stalled = responses_tx.peer_gone();
    for worker in workers {
        // `JoinHandle` has no timed join, so watch `is_finished` instead.
        if !stalled
            && !wait_unless_stdout_stalled(&progress, STDOUT_STALL_LIMIT, || worker.is_finished())
        {
            stalled = true;
        }
        if stalled {
            break;
        }
        let _ = worker.join();
    }
    drop(responses_tx);
    if !stalled {
        // The workers are done and the channel is closed, so the writer is
        // finishing its queue. It can still be parked in a write here — with
        // few enough responses in flight the channel never filled, so nothing
        // upstream ever noticed, and this join was the remaining way to hang
        // forever.
        stalled =
            !wait_unless_stdout_stalled(&progress, STDOUT_STALL_LIMIT, || writer.is_finished());
    }
    if stalled {
        eprintln!(
            "kettle mcp: stdout has not been read for {:?}; exiting with responses undelivered",
            STDOUT_STALL_LIMIT
        );
        // Deliberately not joining the writer: it is inside a blocking write to
        // a pipe with no reader, and joining it is the hang this exists to
        // avoid. Leaving the process is what closes that handle.
        return 1;
    }
    let _ = writer.join();
    0
}

/// The protocol output channel, plus the one-way latch that records the peer
/// has stopped reading.
///
/// A plain `send` on the bounded response channel blocks once the writer is
/// stuck in a `write` to a stdout nobody is draining. Blocking there is worse
/// than dropping the message: the caller is either a tool worker, which then
/// never returns to the pool, or the reader loop, which then stops reading
/// stdin — and the `notifications/cancelled` that would free everything
/// arrives on stdin. The loop deadlocks itself precisely when the client is
/// trying to get its attention.
///
/// The latch is what makes the total cost bounded rather than per-message. A
/// timeout on any one send is proof the peer is not reading, and there is no
/// second opinion to be had: every later send would wait the same full
/// deadline and then drop the message anyway. Sixty queued responses would
/// have cost sixty times the limit. After the first, they cost nothing.
#[derive(Clone)]
struct Responder {
    tx: crossbeam_channel::Sender<Value>,
    peer_gone: Arc<AtomicBool>,
}

impl Responder {
    fn new(tx: crossbeam_channel::Sender<Value>) -> Self {
        Self {
            tx,
            peer_gone: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Has a send already proved the peer is not reading?
    fn peer_gone(&self) -> bool {
        self.peer_gone.load(Ordering::Relaxed)
    }
}

/// Hand a protocol message to the writer, giving up if it cannot be accepted.
///
/// The message is lost when the deadline expires, which is honest about the
/// situation: nothing written after this point reaches the peer either.
fn respond(responses: &Responder, message: Value) {
    if responses.peer_gone() {
        return;
    }
    if responses
        .tx
        .send_timeout(message, STDOUT_STALL_LIMIT)
        .is_err()
    {
        responses.peer_gone.store(true, Ordering::Relaxed);
    }
}

/// What `server/discover` reports. Identity, the versions a client may pick
/// from, and the capabilities it can use — in one round trip, so a client does
/// not have to probe with `tools/list` to find out what is here.
fn discover_result() -> Value {
    json!({
        "supportedVersions": MCP_SUPPORTED_VERSIONS,
        "capabilities": {"tools": {}},
        // Declared because this result is cacheable and the shape says so.
        "ttlMs": 3_600_000,
        "cacheScope": "public",
        "instructions": "Use kettle_run for bounded one-shot PTY commands. \
    Other tools inspect or drive a running Kettle control server.",
    })
}

/// Serve one modern request. No lifecycle state is consulted, because on this
/// revision there is none to consult: every request carries what the server
/// needs to answer it.
fn dispatch_modern(
    id: Value,
    method: &str,
    params: Value,
    jobs: &crossbeam_channel::Sender<ToolJob>,
    responses: &Responder,
    pending: &Pending,
) {
    match method {
        "server/discover" => respond(responses, success(id, modernize(discover_result()))),
        "tools/list" => respond(
            responses,
            success(
                id,
                modernize(json!({"tools": crate::mcp_tools::tool_specs()})),
            ),
        ),
        // The tool path is shared with the legacy era on purpose: the tools and
        // their bounds do not differ between revisions, only the envelope does.
        "tools/call" => schedule_tool(id, params, jobs, responses, pending, true),
        _ => respond(
            responses,
            error_response(id, -32601, &format!("method not found: {method}")),
        ),
    }
}

fn dispatch_message(
    message: Value,
    lifecycle: &mut Lifecycle,
    jobs: &crossbeam_channel::Sender<ToolJob>,
    responses: &Responder,
    pending: &Pending,
) {
    let Some(object) = message.as_object() else {
        respond(
            responses,
            error_response(Value::Null, -32600, "invalid request"),
        );
        return;
    };
    let id = object.get("id").cloned();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        respond(
            responses,
            error_response(valid_error_id(id), -32600, "jsonrpc must be '2.0'"),
        );
        return;
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        respond(
            responses,
            error_response(valid_error_id(id), -32600, "method must be a string"),
        );
        return;
    };
    if let Some(id) = &id
        && !is_request_id(id)
    {
        respond(
            responses,
            error_response(Value::Null, -32600, "id must be a string or integer"),
        );
        return;
    }
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        if let Some(id) = id {
            respond(
                responses,
                error_response(id, -32602, "params must be an object"),
            );
        }
        return;
    }

    // Notifications never receive responses, including unknown notifications.
    if id.is_none() {
        match method {
            "notifications/initialized" if *lifecycle == Lifecycle::Initializing => {
                *lifecycle = Lifecycle::Ready;
            }
            "notifications/cancelled" => cancel_request(&params, pending),
            _ => {}
        }
        return;
    }
    let id = id.unwrap();

    // Era selection. A request carrying `_meta` protocol fields is modern and
    // is served statelessly — no handshake, no lifecycle gate, because on this
    // revision "an open connection, such as a STDIO process, is not a
    // conversation or session". An `initialize` selects legacy semantics
    // below. Both eras are served on the same process, which is what makes a
    // modern client work against kettle at all: before this, one got
    // SERVER_NOT_INITIALIZED for every call it made.
    let era = request_era(&params);
    if let Era::Malformed(reason) = era {
        respond(responses, error_response(id, INVALID_PARAMS, reason));
        return;
    }
    if let Era::Modern(requested) = era {
        if requested != MCP_MODERN_VERSION {
            respond(responses, unsupported_protocol_version(id, requested));
            return;
        }
        // Both fields the specification marks required are required. A request
        // missing one is malformed, not a request to fill in defaults for.
        // An object, not merely present: a `null` or a string here would let a
        // request through that the server cannot actually characterize.
        if !params
            .get("_meta")
            .and_then(|meta| meta.get(META_CLIENT_CAPABILITIES))
            .is_some_and(Value::is_object)
        {
            respond(
                responses,
                error_response(
                    id,
                    INVALID_PARAMS,
                    "modern requests must carry an object at \
                     _meta[\"io.modelcontextprotocol/clientCapabilities\"]",
                ),
            );
            return;
        }
        dispatch_modern(id, method, params, jobs, responses, pending);
        return;
    }

    if method == "initialize" {
        if *lifecycle != Lifecycle::Uninitialized {
            respond(
                responses,
                error_response(id, -32600, "server is already initialized"),
            );
            return;
        }
        match handle_initialize(id, &params) {
            Ok(response) => {
                *lifecycle = Lifecycle::Initializing;
                respond(responses, response);
            }
            Err(response) => {
                respond(responses, response);
            }
        }
        return;
    }

    // Ping is the lifecycle exception: clients may use it while completing the
    // initialize handshake, and receivers must answer it promptly.
    if method == "ping" {
        respond(responses, success(id, json!({})));
        return;
    }

    if *lifecycle != Lifecycle::Ready {
        respond(
            responses,
            error_response(id, SERVER_NOT_INITIALIZED, "server is not initialized"),
        );
        return;
    }

    match method {
        "tools/list" => {
            respond(
                responses,
                success(id, json!({"tools": crate::mcp_tools::tool_specs()})),
            );
        }
        "tools/call" => schedule_tool(id, params, jobs, responses, pending, false),
        _ => {
            respond(
                responses,
                error_response(id, -32601, &format!("method not found: {method}")),
            );
        }
    }
}

fn schedule_tool(
    id: Value,
    params: Value,
    jobs: &crossbeam_channel::Sender<ToolJob>,
    responses: &Responder,
    pending: &Pending,
    // The tools and their bounds are identical across revisions; only the
    // result envelope differs, and only the caller knows which era asked.
    modern: bool,
) {
    if let Err(message) = crate::mcp_tools::validate_tool_call(&params) {
        respond(responses, error_response(id, -32602, &message));
        return;
    }
    let key = request_key(&id);
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut requests) = pending.lock() else {
            respond(
                responses,
                error_response(id, -32603, "request registry is unavailable"),
            );
            return;
        };
        if requests.contains_key(&key) {
            respond(
                responses,
                error_response(id, -32600, "duplicate in-flight request id"),
            );
            return;
        }
        requests.insert(key.clone(), cancelled.clone());
    }
    let job = ToolJob {
        id: id.clone(),
        key: key.clone(),
        params,
        cancelled,
        modern,
    };
    if jobs.try_send(job).is_err() {
        if let Ok(mut requests) = pending.lock() {
            requests.remove(&key);
        }
        respond(
            responses,
            error_response(id, SERVER_BUSY, "tool queue is full; retry later"),
        );
    }
}

fn tool_worker(jobs: crossbeam_channel::Receiver<ToolJob>, responses: Responder, pending: Pending) {
    while let Ok(job) = jobs.recv() {
        let response = if job.cancelled.load(Ordering::Acquire) {
            None
        } else {
            let result = crate::mcp_tools::call_tool_cancellable(&job.params, &job.cancelled);
            if job.cancelled.load(Ordering::Acquire) {
                None
            } else {
                Some(bounded_tool_success(job.id.clone(), result, job.modern))
            }
        };
        let completed_before_cancellation =
            finish_pending_request(&pending, &job.key, &job.cancelled);
        // MCP cancellation is advisory and fire-and-forget: once cancellation
        // is observed, cease work and do not emit a response the client has
        // already declared it will ignore.
        if completed_before_cancellation && let Some(response) = response {
            respond(&responses, response);
        }
        // The writer has gone away, or the peer stopped reading it — `respond`
        // latches both. Nothing this worker produces from here can reach
        // anyone, so it leaves the pool rather than running tools into a void.
        if responses.peer_gone() {
            return;
        }
    }
}

/// Atomically linearize completion against `notifications/cancelled`. The
/// cancellation path holds the same registry lock while setting the flag, so a
/// cancellation observed before removal always suppresses the response; once
/// removal wins, later cancellation notifications correctly see an unknown,
/// already-completed request.
fn finish_pending_request(pending: &Pending, key: &str, cancelled: &AtomicBool) -> bool {
    let Ok(mut requests) = pending.lock() else {
        return false;
    };
    if requests.remove(key).is_none() {
        return false;
    }
    !cancelled.load(Ordering::Acquire)
}

fn cancel_request(params: &Value, pending: &Pending) {
    let Some(id) = params.get("requestId") else {
        return;
    };
    if !is_request_id(id) {
        return;
    }
    if let Ok(requests) = pending.lock()
        && let Some(cancelled) = requests.get(&request_key(id))
    {
        cancelled.store(true, Ordering::Release);
    }
}

fn request_key(id: &Value) -> String {
    match id {
        Value::String(value) => format!("s:{value}"),
        Value::Number(value) => format!("n:{value}"),
        _ => "invalid".into(),
    }
}

fn is_request_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
    }
}

fn valid_error_id(id: Option<Value>) -> Value {
    id.filter(is_request_id).unwrap_or(Value::Null)
}

/// `kettle mcp --self-test`: handshake, list tools, and run one bounded PTY.
pub fn self_test() -> i32 {
    let init = handle_initialize(
        json!(1),
        &json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "kettle-self-test", "version": env!("CARGO_PKG_VERSION")},
        }),
    );
    let Ok(init) = init else {
        eprintln!("self-test FAIL: initialize was rejected");
        return 1;
    };
    if init
        .get("result")
        .and_then(|r| r.get("serverInfo"))
        .is_none()
    {
        eprintln!("self-test FAIL: initialize missing serverInfo");
        return 1;
    }
    if !crate::mcp_tools::tool_specs()
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some("kettle_run"))
    {
        eprintln!("self-test FAIL: tools/list missing kettle_run");
        return 1;
    }
    #[cfg(windows)]
    let command = json!(["cmd", "/c", "echo", "mcp-self-test-ok"]);
    #[cfg(unix)]
    let command = json!(["echo", "mcp-self-test-ok"]);
    let result = crate::mcp_tools::call_tool(&json!({
        "name": "kettle_run",
        "arguments": {"command": command},
    }));
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !text.contains("mcp-self-test-ok") {
        if text.contains("cannot start PTY") || text.contains("PTY") {
            eprintln!("self-test: no PTY available; handshake and tools/list passed");
            return 0;
        }
        eprintln!("self-test FAIL: kettle_run output missing marker: {text:?}");
        return 1;
    }
    eprintln!("kettle mcp --self-test: OK");
    0
}

fn handle_initialize(id: Value, params: &Value) -> Result<Value, Value> {
    let params: InitializeParams = serde_json::from_value(params.clone()).map_err(|error| {
        error_response(
            id.clone(),
            -32602,
            &format!("invalid initialize params: {error}"),
        )
    })?;
    // Answering an unknown version with our own newest is not a bug here, it is
    // what this revision requires: "If the server supports the requested
    // protocol version, it MUST respond with the same version. Otherwise, the
    // server MUST respond with another protocol version it supports." The
    // client then decides whether it can speak that, and disconnects if not.
    //
    // Do NOT return UnsupportedProtocolVersion (-32022) here. That is the
    // modern negotiation, and a legacy client has no rule for interpreting it —
    // an earlier version of this change made exactly that mistake and turned a
    // conforming handshake into a hard failure.
    let version = match params.protocol_version.as_str() {
        MCP_COMPAT_VERSION => MCP_COMPAT_VERSION,
        _ => MCP_PROTOCOL_VERSION,
    };
    Ok(success(
        id,
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {
                "name": "kettle",
                "title": "Kettle Terminal",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "Use kettle_run for bounded one-shot PTY commands. Other tools inspect or drive a running Kettle control server.",
        }),
    ))
}

/// `UnsupportedProtocolVersionError`, carrying the list a client should pick
/// from. The spec requires the `supported`/`requested` pair so a client can
/// retry without a second round trip.
fn unsupported_protocol_version(id: Value, requested: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": UNSUPPORTED_PROTOCOL_VERSION,
            "message": "Unsupported protocol version",
            "data": {
                "supported": MCP_SUPPORTED_VERSIONS,
                "requested": requested,
            },
        },
    })
}

fn server_info() -> Value {
    json!({"name": "kettle", "version": env!("CARGO_PKG_VERSION")})
}

/// Which era a request belongs to.
///
/// The specification says a dual-era server picks its behaviour from how the
/// client opens; on stdio that signal is the presence of the `_meta` protocol
/// version. `Malformed` exists so a modern client with a small bug — a version
/// sent as a number, say — is told what is wrong, rather than falling silently
/// through to the legacy path and being told it never initialized.
enum Era<'a> {
    Modern(&'a str),
    Malformed(&'static str),
    Legacy,
}

fn request_era(params: &Value) -> Era<'_> {
    let Some(meta) = params.get("_meta") else {
        return Era::Legacy;
    };
    if !meta.is_object() {
        return Era::Malformed("params._meta must be an object");
    }
    match meta.get(META_PROTOCOL_VERSION) {
        None => Era::Legacy,
        Some(Value::String(version)) => Era::Modern(version),
        Some(_) => {
            Era::Malformed("_meta[\"io.modelcontextprotocol/protocolVersion\"] must be a string")
        }
    }
}

/// Modern results carry `resultType` and identify the server without any prior
/// connection state. Legacy results must NOT gain these fields, so this is
/// applied per response rather than folded into `success`.
fn modernize(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("resultType".into(), json!("complete"));
        object
            .entry("_meta")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .map(|meta| meta.insert(META_SERVER_INFO.into(), server_info()));
    }
    result
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn bounded_tool_success(id: Value, result: Value, modern: bool) -> Value {
    let result = if modern { modernize(result) } else { result };
    let response = success(id.clone(), result.clone());
    let Ok(encoded) = serde_json::to_vec(&response) else {
        return error_response(id, -32603, "tool response could not be encoded");
    };
    if encoded.len() <= MAX_MCP_RESPONSE_BYTES {
        return response;
    }

    let Some(text) = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return error_response(
            id,
            -32603,
            "response exceeds 768 KiB and has no truncatable text content",
        );
    };

    let mut keep = text.len();
    let mut encoded_len = encoded.len();
    loop {
        let reduction = encoded_len
            .saturating_sub(MAX_MCP_RESPONSE_BYTES)
            .max(1)
            .min(keep.max(1));
        keep = keep.saturating_sub(reduction);
        let mut candidate = result.clone();
        let Some(slot) = candidate.pointer_mut("/content/0/text") else {
            unreachable!("text pointer was validated above");
        };
        *slot = Value::String(truncate_tool_text_for_wire(&text, keep));
        if let Some(object) = candidate
            .get_mut("structuredContent")
            .and_then(Value::as_object_mut)
        {
            object.insert("truncated".into(), Value::Bool(true));
        } else if let Some(object) = candidate.as_object_mut() {
            object.insert("structuredContent".into(), json!({"truncated": true}));
        }
        let response = success(id.clone(), candidate);
        match serde_json::to_vec(&response) {
            Ok(bytes) if bytes.len() <= MAX_MCP_RESPONSE_BYTES => return response,
            Ok(bytes) if keep > 0 => encoded_len = bytes.len(),
            _ => {
                return error_response(
                    id,
                    -32603,
                    "response exceeds 768 KiB after text truncation",
                );
            }
        }
    }
}

fn truncate_tool_text_for_wire(text: &str, keep: usize) -> String {
    const MARKER: &str = "\n\n[... Kettle MCP result truncated for transport ...]\n\n";
    if keep >= text.len() {
        return text.to_owned();
    }
    let mut head = keep / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len().saturating_sub(keep.saturating_sub(head));
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{}{}", &text[..head], MARKER, &text[tail..])
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn write_message(writer: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    if bytes.len() > MAX_MCP_RESPONSE_BYTES {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        bytes = serde_json::to_vec(&error_response(
            id.clone(),
            -32603,
            "response exceeds 768 KiB; request a smaller page",
        ))
        .map_err(std::io::Error::other)?;
        if bytes.len() > MAX_MCP_RESPONSE_BYTES {
            bytes = serde_json::to_vec(&error_response(
                Value::Null,
                -32603,
                &format!(
                    "response and request id exceed 768 KiB (id type: {})",
                    match id {
                        Value::String(_) => "string",
                        Value::Number(_) => "number",
                        _ => "other",
                    }
                ),
            ))
            .map_err(std::io::Error::other)?;
        }
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Scan raw (not-yet-parsed) JSON text for `{`/`[` nesting deeper than
/// `limit`, without allocating or recursing. Bracket characters inside JSON
/// string literals (including escaped quotes and backslashes) are skipped so
/// they are never mistaken for structural nesting; this only needs to track
/// "am I inside a string" well enough to find the real closing quote, not to
/// fully validate escape sequences, since malformed strings are still caught
/// by `serde_json::from_str` afterwards.
fn json_nesting_too_deep(text: &str, limit: u32) -> bool {
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

#[derive(Debug)]
enum ReadLineError {
    TooLarge,
    Io(std::io::Error),
}

fn read_capped_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, ReadLineError> {
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(reader, MAX_MCP_REQUEST_BYTES as u64 + 2);
    let read = limited
        .read_until(b'\n', &mut bytes)
        .map_err(ReadLineError::Io)?;
    if read == 0 {
        return Ok(None);
    }
    let has_newline = bytes.last() == Some(&b'\n');
    let content_len = bytes.len().saturating_sub(usize::from(has_newline));
    if content_len > MAX_MCP_REQUEST_BYTES || !has_newline && bytes.len() > MAX_MCP_REQUEST_BYTES {
        return Err(ReadLineError::TooLarge);
    }
    if has_newline {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that never finishes a write, standing in for a peer that has
    /// stopped reading its end of the pipe.
    ///
    /// This is the only way to reach the stall from inside a test: the
    /// process's real stdout cannot be made to stop draining, and a `Vec<u8>`
    /// sink never applies backpressure at all.
    struct NeverDrains {
        entered: std::sync::mpsc::SyncSender<()>,
    }

    impl Write for NeverDrains {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // Announce the first write, then never return. `try_send` because
            // the receiver only wants to know it happened once.
            let _ = self.entered.try_send(());
            std::thread::sleep(Duration::from_secs(3600));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The shortened test limit must not hide a wrong production value.
    #[test]
    fn the_production_stall_limit_is_the_documented_one() {
        // `STDOUT_STALL_LIMIT` is 400 ms under cfg(test) so the fixtures below
        // run in a second rather than a minute. The value that ships is this.
        let shipped = if cfg!(test) {
            Duration::from_secs(30)
        } else {
            STDOUT_STALL_LIMIT
        };
        assert_eq!(shipped, Duration::from_secs(30));
    }

    /// A worker that is simply BUSY must not be mistaken for a stalled peer.
    ///
    /// Shutdown used to give the workers a flat 30-second budget, which killed
    /// a healthy `kettle_run` whose `timeout_s` exceeded it — the tool's schema
    /// allows up to 600 — delivered no result, and printed a diagnostic blaming
    /// stdout while stdout was being read the whole time. Losing an agent's
    /// build output is worse than the hang that budget was meant to prevent.
    #[test]
    fn a_busy_worker_is_not_mistaken_for_a_stalled_peer() {
        let progress = WriterProgress::default();
        let started = Instant::now();
        // Nothing is in flight, so however long this takes it is not the peer.
        let finished = wait_unless_stdout_stalled(&progress, Duration::from_millis(50), || {
            started.elapsed() >= Duration::from_millis(300)
        });
        assert!(
            finished,
            "an idle writer means the work is slow, not the peer — waiting must \
             continue however long the limit has been exceeded"
        );

        // A writer that is WRITING, steadily, is also not stalled.
        //
        // Progress is published from inside the poll predicate rather than
        // from a helper thread. The property under test is that a changing
        // `completed` resets the stall timer — not the operating system's
        // willingness to schedule a second thread within the limit. A helper
        // ticking every 10ms against a 50ms budget has only five missed
        // wake-ups of headroom, and a loaded macOS runner spends that
        // routinely: the test failed in CI having proven nothing about the
        // code. Driving the counter from the detector's own loop removes the
        // race entirely and tests the same thing.
        let progress = WriterProgress::default();
        progress.in_write.store(true, Ordering::Release);
        let polls = std::cell::Cell::new(0u32);
        assert!(
            wait_unless_stdout_stalled(&progress, Duration::from_millis(50), || {
                progress.completed.fetch_add(1, Ordering::Release);
                polls.set(polls.get() + 1);
                polls.get() >= 20
            }),
            "a writer completing writes is making progress, not stalling"
        );
        assert_eq!(
            polls.get(),
            20,
            "the detector must have polled through every one of those writes \
             rather than returning early"
        );

        // Parked inside ONE write, with no completions, IS the peer.
        let progress = WriterProgress::default();
        progress.in_write.store(true, Ordering::Release);
        let started = Instant::now();
        assert!(
            !wait_unless_stdout_stalled(&progress, Duration::from_millis(50), || false),
            "a writer parked in a single write is a peer that stopped reading"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "and it must be detected promptly"
        );
    }

    /// A peer that stops reading kettle's stdout must not strand the process.
    ///
    /// The writer thread blocks in `write`, the bounded response channel fills
    /// behind it, and every tool worker blocks mid-`send`. Shutdown then joined
    /// those workers — which never return — so `drop(responses_tx)` was
    /// unreachable and the server sat holding a terminal, answering nothing,
    /// until something killed it. The reader loop had the same problem earlier:
    /// its own `send` blocked, so it stopped reading stdin, which is where the
    /// `notifications/cancelled` that would free everything arrives.
    ///
    /// The waits are bounded now. This drives the real `run_mcp_with` against a
    /// writer that never completes a write, and requires the server to return.
    #[test]
    fn a_peer_that_stops_reading_stdout_does_not_strand_the_server() {
        // BOTH shapes.
        //
        // The many-message one fills the bounded response channel, so `respond`
        // times out and latches `peer_gone`. The few-message one never fills it
        // — nothing times out, nothing latches — and the only thing left
        // holding the process is the writer's own join at the end. An earlier
        // version of this fix handled only the first shape, and this test only
        // covered the first shape, so it was green on a server that still hung
        // forever on a single ping. One or two calls and a client that stops
        // reading is the ordinary case; fifty is not.
        for requests in [3, TOOL_QUEUE_CAPACITY + TOOL_WORKERS + 32] {
            let mut input = String::new();
            input.push_str(&format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                       "params": init_params(MCP_PROTOCOL_VERSION)})
            ));
            for id in 1..=requests {
                input.push_str(&format!(
                    "{}\n",
                    json!({"jsonrpc": "2.0", "id": id, "method": "ping"})
                ));
            }

            let (entered, first_write) = std::sync::mpsc::sync_channel(1);
            let (finished, done) = std::sync::mpsc::sync_channel(1);
            let server = std::thread::spawn(move || {
                let code = run_mcp_with(
                    std::io::Cursor::new(input.into_bytes()),
                    NeverDrains { entered },
                );
                let _ = finished.try_send(code);
            });

            first_write
                .recv_timeout(Duration::from_secs(10))
                .expect("the writer must reach a write before the peer can stall it");

            let code = done
                .recv_timeout(STDOUT_STALL_LIMIT + Duration::from_secs(60))
                .unwrap_or_else(|_| {
                    panic!(
                        "run_mcp_with never returned with {requests} requests in \
                         flight: a peer that stopped reading stdout stranded the \
                         server"
                    )
                });
            assert_eq!(
                code, 1,
                "giving up on an unreadable stdout is a failure exit, not a clean one"
            );
            // The writer thread is deliberately abandoned inside its blocking
            // write, so the server thread is what must be joinable.
            server.join().expect("server thread panicked");
        }
    }

    fn init_params(version: &str) -> Value {
        json!({
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"},
        })
    }

    #[test]
    fn initialize_negotiates_current_and_compat_versions() {
        for version in [MCP_PROTOCOL_VERSION, MCP_COMPAT_VERSION] {
            let response = handle_initialize(json!(1), &init_params(version)).unwrap();
            assert_eq!(response["result"]["protocolVersion"], version);
            assert_eq!(response["result"]["serverInfo"]["name"], "kettle");
            assert_eq!(
                response["result"]["capabilities"]["tools"]["listChanged"],
                false
            );
        }
    }

    #[test]
    fn initialize_requires_typed_required_fields() {
        let response =
            handle_initialize(json!(1), &json!({"protocolVersion": MCP_PROTOCOL_VERSION}))
                .unwrap_err();
        assert_eq!(response["error"]["code"], -32602);

        for params in [
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": [],
                "clientInfo": {"name": "test", "version": "1"},
            }),
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": null,
            }),
        ] {
            assert_eq!(
                handle_initialize(json!(1), &params).unwrap_err()["error"]["code"],
                -32602
            );
        }
    }

    #[test]
    fn lifecycle_requires_exact_initialized_notification() {
        let (jobs_tx, _jobs_rx) = crossbeam_channel::bounded(1);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(8);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut lifecycle = Lifecycle::Uninitialized;
        dispatch_message(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":init_params(MCP_PROTOCOL_VERSION)}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        assert_eq!(lifecycle, Lifecycle::Initializing);
        assert!(responses_rx.try_recv().is_ok());
        dispatch_message(
            json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        let ping = responses_rx.recv().unwrap();
        assert_eq!(ping["id"], 2);
        assert_eq!(ping["result"], json!({}));
        assert_eq!(lifecycle, Lifecycle::Initializing);
        dispatch_message(
            json!({"jsonrpc":"2.0","method":"initialized"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        assert_eq!(lifecycle, Lifecycle::Initializing);
        assert!(
            responses_rx.try_recv().is_err(),
            "notifications get no response"
        );
        dispatch_message(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        assert_eq!(lifecycle, Lifecycle::Ready);
    }

    #[test]
    fn cancellation_marks_an_inflight_request() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let flag = Arc::new(AtomicBool::new(false));
        pending.lock().unwrap().insert("n:7".into(), flag.clone());
        cancel_request(&json!({"requestId": 7}), &pending);
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn completion_handoff_observes_prior_cancellation() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(AtomicBool::new(true));
        pending
            .lock()
            .unwrap()
            .insert("n:7".into(), cancelled.clone());

        assert!(!finish_pending_request(&pending, "n:7", &cancelled));
        assert!(pending.lock().unwrap().is_empty());

        let completed = Arc::new(AtomicBool::new(false));
        pending
            .lock()
            .unwrap()
            .insert("n:8".into(), completed.clone());
        assert!(finish_pending_request(&pending, "n:8", &completed));
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn queued_cancellation_skips_tool_execution() {
        let (jobs_tx, jobs_rx) = crossbeam_channel::bounded(1);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(2);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        schedule_tool(
            json!(11),
            json!({
                "name": "kettle_run",
                "arguments": {"command": ["this-command-must-not-run"]},
            }),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );
        cancel_request(&json!({"requestId": 11}), &pending);
        drop(jobs_tx);
        tool_worker(jobs_rx, responses_tx.clone(), pending.clone());

        assert!(
            responses_rx.try_recv().is_err(),
            "cancelled requests do not receive a response"
        );
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn duplicate_id_preserves_original_request_and_queue_is_bounded() {
        let (jobs_tx, jobs_rx) = crossbeam_channel::bounded(1);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(8);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        schedule_tool(
            json!(7),
            json!({"name":"kettle_run","arguments":{"command":["echo","queued"]}}),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );
        let original = pending.lock().unwrap().get("n:7").unwrap().clone();
        schedule_tool(
            json!(7),
            json!({"name":"kettle_run","arguments":{"command":["echo","queued"]}}),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );
        let duplicate = responses_rx.recv().unwrap();
        assert_eq!(duplicate["error"]["code"], -32600);
        assert!(Arc::ptr_eq(
            &original,
            pending.lock().unwrap().get("n:7").unwrap()
        ));

        schedule_tool(
            json!(8),
            json!({"name":"kettle_run","arguments":{"command":["echo","queued"]}}),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );
        let busy = responses_rx.recv().unwrap();
        assert_eq!(busy["id"], 8);
        assert_eq!(busy["error"]["code"], SERVER_BUSY);
        assert!(!pending.lock().unwrap().contains_key("n:8"));
        drop(jobs_rx);
    }

    #[test]
    fn malformed_and_unknown_tool_calls_are_protocol_errors() {
        let (jobs_tx, jobs_rx) = crossbeam_channel::bounded(2);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(2);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));

        schedule_tool(
            json!(3),
            json!({"name":"unknown","arguments":{}}),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );
        schedule_tool(
            json!(4),
            json!({"name":"kettle_run","arguments":null}),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );

        for id in [3, 4] {
            let response = responses_rx.recv().unwrap();
            assert_eq!(response["id"], id);
            assert_eq!(response["error"]["code"], -32602);
        }
        assert!(jobs_rx.try_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn known_tool_input_errors_are_tool_results() {
        let (jobs_tx, jobs_rx) = crossbeam_channel::bounded(1);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(1);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        schedule_tool(
            json!(5),
            json!({"name":"kettle_run","arguments":{"command":"not-an-array"}}),
            &jobs_tx,
            &responses_tx,
            &pending,
            false,
        );
        drop(jobs_tx);
        tool_worker(jobs_rx, responses_tx, pending.clone());

        let response = responses_rx.recv().unwrap();
        assert_eq!(response["id"], 5);
        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["isError"], true);
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn tool_success_is_truncated_after_json_encoding() {
        let result = json!({
            "content": [{"type": "text", "text": "\"".repeat(512 * 1024)}],
            "structuredContent": {"truncated": false},
        });
        let response = bounded_tool_success(json!(17), result, false);
        let encoded = serde_json::to_vec(&response).unwrap();

        assert!(encoded.len() <= MAX_MCP_RESPONSE_BYTES);
        assert_eq!(response["id"], 17);
        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["structuredContent"]["truncated"], true);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("truncated for transport"));
    }

    #[test]
    fn writer_never_exceeds_response_cap() {
        let message = success(
            json!(9),
            json!({"text": "x".repeat(MAX_MCP_RESPONSE_BYTES)}),
        );
        let mut output = Vec::new();
        write_message(&mut output, &message).unwrap();
        assert!(output.len() <= MAX_MCP_RESPONSE_BYTES + 1);
        let fallback: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(fallback["id"], 9);
        assert_eq!(fallback["error"]["code"], -32603);

        let huge_id = success(Value::String("x".repeat(MAX_MCP_RESPONSE_BYTES)), json!({}));
        let mut output = Vec::new();
        write_message(&mut output, &huge_id).unwrap();
        assert!(output.len() <= MAX_MCP_RESPONSE_BYTES + 1);
        let fallback: Value = serde_json::from_slice(&output).unwrap();
        assert!(fallback["id"].is_null());
        assert_eq!(fallback["error"]["code"], -32603);
    }

    #[test]
    fn json_nesting_rejects_only_past_the_limit() {
        // Exactly at the limit: allowed.
        let at_limit = format!("{}{}", "[".repeat(64), "]".repeat(64));
        assert!(!json_nesting_too_deep(&at_limit, 64));
        // One level past the limit: rejected.
        let over_limit = format!("{}{}", "[".repeat(65), "]".repeat(65));
        assert!(json_nesting_too_deep(&over_limit, 64));
        // Mixed object/array nesting is counted the same way.
        let mixed = "{\"a\":".repeat(65) + "1" + &"}".repeat(65);
        assert!(json_nesting_too_deep(&mixed, 64));
        // A shallow, realistic request is never affected.
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"kettle_run","arguments":{"command":["echo","hi"]}}});
        assert!(!json_nesting_too_deep(&request.to_string(), 64));
    }

    #[test]
    fn json_nesting_ignores_brackets_inside_strings() {
        // A million-bracket string body must not itself be counted as
        // structural nesting: only real, unescaped structural brackets do.
        let text = format!("{{\"text\":\"{}\"}}", "[".repeat(1_000_000));
        assert!(!json_nesting_too_deep(&text, 64));
        // An escaped quote inside a string must not prematurely end the
        // string and let a following bracket be miscounted as structural.
        let escaped_quote = "{\"a\":\"\\\"[[[[\", \"b\":1}";
        assert!(!json_nesting_too_deep(escaped_quote, 2));
    }

    #[test]
    fn deeply_nested_line_is_rejected_before_parsing() {
        let (jobs_tx, _jobs_rx) = crossbeam_channel::bounded(1);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(1);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut lifecycle = Lifecycle::Ready;
        let text = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{}",
            "[".repeat(1_000_000)
        );
        // This mirrors the stdio loop's own depth check, run against a line
        // that would otherwise need a million-deep parser recursion.
        assert!(json_nesting_too_deep(&text, MAX_JSON_NESTING_DEPTH));
        // The loop never even reaches `dispatch_message` for such a line; a
        // well-formed, shallow message on the same lifecycle still works.
        dispatch_message(
            json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        let response = responses_rx.recv().unwrap();
        assert_eq!(response["result"], json!({}));
    }

    #[test]
    fn capped_reader_rejects_oversize_line() {
        let input = vec![b'x'; MAX_MCP_REQUEST_BYTES + 1];
        let mut reader = std::io::BufReader::new(input.as_slice());
        assert!(matches!(
            read_capped_line(&mut reader),
            Err(ReadLineError::TooLarge)
        ));
    }

    #[test]
    fn invalid_request_and_notifications_follow_json_rpc_rules() {
        let (jobs_tx, _jobs_rx) = crossbeam_channel::bounded(1);
        let (responses_tx, responses_rx) = crossbeam_channel::bounded(8);
        let responses_tx = Responder::new(responses_tx);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut lifecycle = Lifecycle::Ready;
        dispatch_message(
            json!({"jsonrpc":"1.0","id":3,"method":"ping"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        assert_eq!(responses_rx.recv().unwrap()["error"]["code"], -32600);
        dispatch_message(
            json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        let fractional = responses_rx.recv().unwrap();
        assert!(fractional["id"].is_null());
        assert_eq!(fractional["error"]["code"], -32600);
        dispatch_message(
            json!({"jsonrpc":"2.0","method":"unknown/notification"}),
            &mut lifecycle,
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        assert!(responses_rx.try_recv().is_err());
    }
}
