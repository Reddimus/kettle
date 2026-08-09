//! Spawn the real `kettle mcp` stdio server and speak JSON-RPC over pipes.
//!
//! Unit tests cover the handler functions, and `kettle mcp --self-test` covers
//! the in-process path. This test pins the agent-facing process boundary Claude
//! Code / Codex use: newline-delimited JSON-RPC on stdin/stdout plus a real
//! `tools/call` round trip.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn kettle() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kettle"))
}

fn run_mcp_stdio(messages: &[Value]) -> (i32, String, String) {
    let mut child = kettle()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kettle mcp");

    {
        let mut stdin = child.stdin.take().expect("mcp stdin");
        for msg in messages {
            writeln!(stdin, "{msg}").expect("write mcp message");
        }
    } // EOF tells the server to shut down after processing queued requests.

    let mut out = String::new();
    let mut err = String::new();
    child
        .stdout
        .take()
        .expect("mcp stdout")
        .read_to_string(&mut out)
        .expect("read mcp stdout");
    child
        .stderr
        .take()
        .expect("mcp stderr")
        .read_to_string(&mut err)
        .expect("read mcp stderr");
    let status = child.wait().expect("wait mcp");
    (status.code().unwrap_or(-1), out, err)
}

fn parse_responses(out: &str) -> Vec<Value> {
    out.lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON line {line:?}: {e}"))
        })
        .collect()
}

#[test]
fn mcp_stdio_initialize_list_and_kettle_run() {
    #[cfg(windows)]
    let command = json!(["cmd", "/c", "echo", "mcp-stdio-marker-42"]);
    #[cfg(unix)]
    let command = json!(["echo", "mcp-stdio-marker-42"]);

    let messages = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "kettle-test", "version": "1"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "kettle_run",
                "arguments": {
                    "command": command,
                    "timeout_s": 5,
                    "strip_ansi": true
                }
            }
        }),
    ];

    let (code, out, err) = run_mcp_stdio(&messages);
    assert_eq!(code, 0, "mcp process failed; stderr: {err}");
    let responses = parse_responses(&out);
    assert_eq!(responses.len(), 3, "stdout was: {out:?}");

    let init = responses
        .iter()
        .find(|r| r["id"] == 1)
        .expect("init response");
    assert_eq!(init["result"]["serverInfo"]["name"], "kettle");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let tools = responses
        .iter()
        .find(|r| r["id"] == 2)
        .expect("tools response");
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"kettle_run"));
    assert!(names.contains(&"kettle_send_keys"));
    assert!(names.contains(&"kettle_dispatch_ui_key"));
    assert!(names.contains(&"kettle_wait_for"));

    let call = responses
        .iter()
        .find(|r| r["id"] == 3)
        .expect("call response");
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    if text.contains("cannot start PTY") || text.contains("PTY") {
        eprintln!("skipping kettle_run assertion: no PTY available");
        return;
    }
    assert_eq!(
        call["result"].get("isError").and_then(Value::as_bool),
        Some(false),
        "kettle_run should not be an MCP error: {text}"
    );
    assert!(text.contains("exit code: 0"), "tool text was: {text:?}");
    assert!(
        text.contains("mcp-stdio-marker-42"),
        "tool text was: {text:?}"
    );
}

/// A modern client sends no `initialize`. Before dual-era support that meant
/// every call it made came back `-32002 server is not initialized`, which is
/// the compatibility matrix's "Modern client + Legacy server: Fails" in the
/// one form a user would actually see.
#[test]
fn mcp_stdio_serves_a_modern_client_that_never_handshakes() {
    #[cfg(windows)]
    let command = json!(["cmd", "/c", "echo", "modern-marker-7"]);
    #[cfg(unix)]
    let command = json!(["echo", "modern-marker-7"]);

    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "kettle-test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover",
               "params": {"_meta": meta}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list",
               "params": {"_meta": meta}}),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "_meta": meta,
                "name": "kettle_run",
                "arguments": {"command": command, "timeout_s": 5, "strip_ansi": true}
            }
        }),
    ];
    let (code, stdout, stderr) = run_mcp_stdio(&messages);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let responses = parse_responses(&stdout);
    assert_eq!(responses.len(), 3, "{stdout}");

    let discover = &responses[0]["result"];
    assert_eq!(discover["resultType"], "complete", "{stdout}");
    assert!(
        discover["supportedVersions"]
            .as_array()
            .expect("supportedVersions")
            .contains(&json!("2026-07-28")),
        "discover must advertise the modern revision: {stdout}"
    );
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "kettle",
        "{stdout}"
    );

    assert!(
        !responses[1]["result"]["tools"]
            .as_array()
            .expect("tools")
            .is_empty(),
        "a modern tools/list must work without a handshake: {stdout}"
    );
    for response in &responses {
        assert!(
            response.get("error").is_none(),
            "no modern request may be refused for want of an initialize: {stdout}"
        );
    }
    assert!(
        serde_json::to_string(&responses[2])
            .unwrap()
            .contains("modern-marker-7"),
        "the tool ran and its output came back: {stdout}"
    );
}

/// The two eras negotiate differently, and conflating them breaks one of them.
///
/// A MODERN request declaring a version kettle does not speak is refused with
/// `UnsupportedProtocolVersion` (-32022) naming what it does speak, so the
/// client can retry.
///
/// A LEGACY `initialize` is not. 2025-11-25 says the server "MUST respond with
/// another protocol version it supports", and the client disconnects if it
/// cannot speak that. Returning -32022 there — which an earlier draft of this
/// change did, calling the correct behaviour a bug — turns a conforming
/// handshake into a hard failure.
#[test]
fn mcp_stdio_negotiates_an_unknown_version_per_era() {
    let messages = [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        }),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "initialize",
            "params": {
                "protocolVersion": "1900-01-01",
                "capabilities": {},
                "clientInfo": {"name": "kettle-test", "version": "1"}
            }
        }),
    ];
    let (_code, stdout, stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(responses.len(), 2, "stdout={stdout}\nstderr={stderr}");

    let modern = &responses[0];
    assert_eq!(modern["error"]["code"], -32022, "{stdout}");
    let supported = modern["error"]["data"]["supported"]
        .as_array()
        .unwrap_or_else(|| panic!("supported list: {stdout}"));
    assert!(supported.contains(&json!("2026-07-28")), "{stdout}");
    assert!(supported.contains(&json!("2025-11-25")), "{stdout}");
    assert_eq!(
        modern["error"]["data"]["requested"], "1900-01-01",
        "{stdout}"
    );

    let legacy = &responses[1];
    assert!(
        legacy.get("error").is_none(),
        "a legacy initialize must succeed with a supported version, not error: {stdout}"
    );
    assert_eq!(
        legacy["result"]["protocolVersion"], "2025-11-25",
        "and that version must be one kettle actually speaks: {stdout}"
    );
}

/// Both fields the specification marks required are required. Filling in a
/// default for a missing one would let a server answer a request it cannot
/// actually characterize.
#[test]
fn mcp_stdio_rejects_a_modern_request_missing_required_meta() {
    let messages = [json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}
    })];
    let (_code, stdout, _stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(responses[0]["error"]["code"], -32602, "{stdout}");
}

/// The legacy era still works, unchanged: "Legacy client + Dual-era server:
/// Works." A legacy result must NOT grow `resultType`, which a legacy client
/// has no reason to expect.
#[test]
fn mcp_stdio_still_serves_a_legacy_client_unchanged() {
    let messages = [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "kettle-test", "version": "1"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    let (_code, stdout, _stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(
        responses[0]["result"]["protocolVersion"], "2025-11-25",
        "{stdout}"
    );
    assert!(
        responses[1]["result"].get("resultType").is_none(),
        "a legacy result must stay legacy-shaped: {stdout}"
    );
    assert!(
        !responses[1]["result"]["tools"]
            .as_array()
            .expect("tools")
            .is_empty(),
        "{stdout}"
    );
}

/// A modern client with a small bug — the version sent as a number, or `_meta`
/// sent as something other than an object — must be told what is wrong. Falling
/// through to the legacy path would answer "server is not initialized", which
/// points at a handshake the client is right not to be sending.
#[test]
fn mcp_stdio_names_a_malformed_modern_envelope_instead_of_blaming_the_handshake() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
               "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": 20260728}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list",
               "params": {"_meta": "2026-07-28"}}),
    ];
    let (_code, stdout, _stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(responses.len(), 2, "{stdout}");
    for response in &responses {
        assert_eq!(response["error"]["code"], -32602, "{stdout}");
        let message = response["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("_meta"),
            "the error must point at the envelope, not the handshake: {stdout}"
        );
    }
}
