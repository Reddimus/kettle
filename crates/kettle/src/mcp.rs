//! `kettle mcp`: a bounded Model Context Protocol server over stdio.
//!
//! MCP uses one JSON-RPC 2.0 object per UTF-8 line. Stdout is exclusively the
//! protocol channel; diagnostics remain on stderr. Tool calls run on a bounded
//! worker pool so one PTY command cannot block ping, cancellation, or unrelated
//! control reads.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{Value, json};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_COMPAT_VERSION: &str = "2025-06-18";
const MAX_MCP_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_MCP_RESPONSE_BYTES: usize = 768 * 1024;
const TOOL_WORKERS: usize = 4;
const TOOL_QUEUE_CAPACITY: usize = 16;
const SERVER_NOT_INITIALIZED: i64 = -32002;
const SERVER_BUSY: i64 = -32003;

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
}

type Pending = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

/// Run the stdio MCP server loop until stdin closes. Returns zero on clean EOF.
pub fn run_mcp() -> i32 {
    let (responses_tx, responses_rx) =
        crossbeam_channel::bounded::<Value>(TOOL_QUEUE_CAPACITY + TOOL_WORKERS + 8);
    let writer = match std::thread::Builder::new()
        .name("kettle-mcp-writer".into())
        .spawn(move || {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            while let Ok(message) = responses_rx.recv() {
                if write_message(&mut stdout, &message).is_err() {
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

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut lifecycle = Lifecycle::Uninitialized;
    loop {
        let line = match read_capped_line(&mut input) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(ReadLineError::TooLarge) => {
                let _ = responses_tx.send(error_response(
                    Value::Null,
                    -32700,
                    "JSON-RPC line exceeds 1 MiB",
                ));
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
                let _ = responses_tx.send(error_response(
                    Value::Null,
                    -32700,
                    &format!("invalid UTF-8: {error}"),
                ));
                continue;
            }
        };
        let message: Value = match serde_json::from_str(text) {
            Ok(message) => message,
            Err(error) => {
                let _ = responses_tx.send(error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
                continue;
            }
        };
        dispatch_message(message, &mut lifecycle, &jobs_tx, &responses_tx, &pending);
    }
    // EOF is an orderly stdio shutdown. Drain already-accepted jobs so clients
    // that write a request batch and close stdin still receive every response.
    drop(jobs_tx);
    for worker in workers {
        let _ = worker.join();
    }
    drop(responses_tx);
    let _ = writer.join();
    0
}

fn dispatch_message(
    message: Value,
    lifecycle: &mut Lifecycle,
    jobs: &crossbeam_channel::Sender<ToolJob>,
    responses: &crossbeam_channel::Sender<Value>,
    pending: &Pending,
) {
    let Some(object) = message.as_object() else {
        let _ = responses.send(error_response(Value::Null, -32600, "invalid request"));
        return;
    };
    let id = object.get("id").cloned();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        let _ = responses.send(error_response(
            valid_error_id(id),
            -32600,
            "jsonrpc must be '2.0'",
        ));
        return;
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        let _ = responses.send(error_response(
            valid_error_id(id),
            -32600,
            "method must be a string",
        ));
        return;
    };
    if let Some(id) = &id
        && !is_request_id(id)
    {
        let _ = responses.send(error_response(
            Value::Null,
            -32600,
            "id must be a string or integer",
        ));
        return;
    }
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        if let Some(id) = id {
            let _ = responses.send(error_response(id, -32602, "params must be an object"));
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

    if method == "initialize" {
        if *lifecycle != Lifecycle::Uninitialized {
            let _ = responses.send(error_response(id, -32600, "server is already initialized"));
            return;
        }
        match handle_initialize(id, &params) {
            Ok(response) => {
                *lifecycle = Lifecycle::Initializing;
                let _ = responses.send(response);
            }
            Err(response) => {
                let _ = responses.send(response);
            }
        }
        return;
    }

    // Ping is the lifecycle exception: clients may use it while completing the
    // initialize handshake, and receivers must answer it promptly.
    if method == "ping" {
        let _ = responses.send(success(id, json!({})));
        return;
    }

    if *lifecycle != Lifecycle::Ready {
        let _ = responses.send(error_response(
            id,
            SERVER_NOT_INITIALIZED,
            "server is not initialized",
        ));
        return;
    }

    match method {
        "tools/list" => {
            let _ = responses.send(success(
                id,
                json!({"tools": crate::mcp_tools::tool_specs()}),
            ));
        }
        "tools/call" => schedule_tool(id, params, jobs, responses, pending),
        _ => {
            let _ = responses.send(error_response(
                id,
                -32601,
                &format!("method not found: {method}"),
            ));
        }
    }
}

fn schedule_tool(
    id: Value,
    params: Value,
    jobs: &crossbeam_channel::Sender<ToolJob>,
    responses: &crossbeam_channel::Sender<Value>,
    pending: &Pending,
) {
    if let Err(message) = crate::mcp_tools::validate_tool_call(&params) {
        let _ = responses.send(error_response(id, -32602, &message));
        return;
    }
    let key = request_key(&id);
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut requests) = pending.lock() else {
            let _ = responses.send(error_response(
                id,
                -32603,
                "request registry is unavailable",
            ));
            return;
        };
        if requests.contains_key(&key) {
            let _ = responses.send(error_response(id, -32600, "duplicate in-flight request id"));
            return;
        }
        requests.insert(key.clone(), cancelled.clone());
    }
    let job = ToolJob {
        id: id.clone(),
        key: key.clone(),
        params,
        cancelled,
    };
    if jobs.try_send(job).is_err() {
        if let Ok(mut requests) = pending.lock() {
            requests.remove(&key);
        }
        let _ = responses.send(error_response(
            id,
            SERVER_BUSY,
            "tool queue is full; retry later",
        ));
    }
}

fn tool_worker(
    jobs: crossbeam_channel::Receiver<ToolJob>,
    responses: crossbeam_channel::Sender<Value>,
    pending: Pending,
) {
    while let Ok(job) = jobs.recv() {
        let response = if job.cancelled.load(Ordering::Acquire) {
            None
        } else {
            let result = crate::mcp_tools::call_tool_cancellable(&job.params, &job.cancelled);
            if job.cancelled.load(Ordering::Acquire) {
                None
            } else {
                Some(bounded_tool_success(job.id.clone(), result))
            }
        };
        let completed_before_cancellation =
            finish_pending_request(&pending, &job.key, &job.cancelled);
        // MCP cancellation is advisory and fire-and-forget: once cancellation
        // is observed, cease work and do not emit a response the client has
        // already declared it will ignore.
        if completed_before_cancellation
            && response.is_some_and(|response| responses.send(response).is_err())
        {
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
    let version = match params.protocol_version.as_str() {
        MCP_PROTOCOL_VERSION => MCP_PROTOCOL_VERSION,
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

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn bounded_tool_success(id: Value, result: Value) -> Value {
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
        let pending = Arc::new(Mutex::new(HashMap::new()));
        schedule_tool(
            json!(7),
            json!({"name":"kettle_run","arguments":{"command":["echo","queued"]}}),
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        let original = pending.lock().unwrap().get("n:7").unwrap().clone();
        schedule_tool(
            json!(7),
            json!({"name":"kettle_run","arguments":{"command":["echo","queued"]}}),
            &jobs_tx,
            &responses_tx,
            &pending,
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
        let pending = Arc::new(Mutex::new(HashMap::new()));

        schedule_tool(
            json!(3),
            json!({"name":"unknown","arguments":{}}),
            &jobs_tx,
            &responses_tx,
            &pending,
        );
        schedule_tool(
            json!(4),
            json!({"name":"kettle_run","arguments":null}),
            &jobs_tx,
            &responses_tx,
            &pending,
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
        let pending = Arc::new(Mutex::new(HashMap::new()));
        schedule_tool(
            json!(5),
            json!({"name":"kettle_run","arguments":{"command":"not-an-array"}}),
            &jobs_tx,
            &responses_tx,
            &pending,
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
        let response = bounded_tool_success(json!(17), result);
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
