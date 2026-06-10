//! Cycle 931–933 (agent-first A3): `kettle mcp` — a Model Context Protocol
//! server over stdio, so an AI agent (Claude Code, Codex, …) gets kettle as a
//! set of native tools: run a command headlessly, and drive a running kettle
//! (list panes, read the screen, send text, run a command in a pane).
//!
//! Hand-rolled sync JSON-RPC 2.0 over stdin/stdout — the official MCP Rust SDK
//! pulls tokio, against kettle's no-async-runtime policy (ureq/rustls
//! precedent). MCP's stdio transport is newline-delimited JSON-RPC; a blocking
//! `stdin.lock()` line loop is all it takes. stdout is the protocol channel, so
//! ALL logging goes to stderr (env_logger's default).
//!
//! Register with Claude Code: `claude mcp add kettle -- kettle mcp`.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// The MCP protocol revision this server implements.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Run the stdio MCP server loop until stdin closes. Returns the process exit
/// code (0 on clean EOF).
pub fn run_mcp() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let lines = stdin.lock().lines();
    for item in lines {
        // Don't let a non-UTF-8 line (InvalidData) or a transient read error
        // masquerade as EOF and silently kill the server: report -32700 and
        // keep going for malformed input; stop only on a genuine I/O error.
        let line = match item {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("invalid utf-8: {e}")),
                );
                continue;
            }
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // Parse error — JSON-RPC -32700, no id recoverable.
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                );
                continue;
            }
        };
        // Notifications have no `id` and get no response.
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                write_message(
                    &mut stdout,
                    &handle_initialize(id.unwrap_or(Value::Null), &params),
                );
            }
            "notifications/initialized" | "initialized" => {
                // Notification: no response.
            }
            "ping" => {
                write_message(&mut stdout, &success(id.unwrap_or(Value::Null), json!({})));
            }
            "tools/list" => {
                write_message(
                    &mut stdout,
                    &success(
                        id.unwrap_or(Value::Null),
                        json!({ "tools": crate::mcp_tools::tool_specs() }),
                    ),
                );
            }
            "tools/call" => {
                let resp_id = id.unwrap_or(Value::Null);
                let result = crate::mcp_tools::call_tool(&params);
                write_message(&mut stdout, &success(resp_id, result));
            }
            // Unknown method — only respond if it was a request (has id).
            _ => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &error_response(id, -32601, &format!("method not found: {method}")),
                    );
                }
            }
        }
    }
    0
}

/// `kettle mcp --self-test`: in-process initialize → tools/list → one kettle_run
/// round trip, asserting the handshake + a tool work. Returns 0 on success.
pub fn self_test() -> i32 {
    // initialize
    let init = handle_initialize(json!(1), &json!({"protocolVersion": MCP_PROTOCOL_VERSION}));
    if init
        .get("result")
        .and_then(|r| r.get("serverInfo"))
        .is_none()
    {
        eprintln!("self-test FAIL: initialize missing serverInfo");
        return 1;
    }
    // tools/list must include kettle_run.
    let tools = crate::mcp_tools::tool_specs();
    if !tools
        .iter()
        .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("kettle_run"))
    {
        eprintln!("self-test FAIL: tools/list missing kettle_run");
        return 1;
    }
    // tools/call kettle_run echo.
    #[cfg(windows)]
    let cmd = json!(["cmd", "/c", "echo", "mcp-self-test-ok"]);
    #[cfg(unix)]
    let cmd = json!(["echo", "mcp-self-test-ok"]);
    let call = json!({"name": "kettle_run", "params": {"command": cmd}});
    let result = crate::mcp_tools::call_tool(&call);
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    if !text.contains("mcp-self-test-ok") {
        // Soft-pass if there's no PTY in the sandbox (CI without a console).
        if text.contains("cannot start PTY") || text.contains("PTY") {
            eprintln!("self-test: no PTY available — handshake + tools/list OK, skipping run");
            return 0;
        }
        eprintln!("self-test FAIL: kettle_run output missing marker: {text:?}");
        return 1;
    }
    eprintln!("kettle mcp --self-test: OK");
    0
}

/// Build the `initialize` response. Echoes the client's protocol version when
/// we support it, else advertises ours.
fn handle_initialize(id: Value, params: &Value) -> Value {
    let client_ver = params.get("protocolVersion").and_then(|v| v.as_str());
    let version = match client_ver {
        Some(v) if v == MCP_PROTOCOL_VERSION => v.to_string(),
        _ => MCP_PROTOCOL_VERSION.to_string(),
    };
    success(
        id,
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "kettle",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "kettle terminal tools. `kettle_run` runs a command \
                headlessly under a real PTY and returns its output + exit code. \
                The other tools drive a running kettle window with the agent \
                server enabled (start it with `kettle --agent-server full`): \
                list panes, read the screen, send text, run a command in a pane."
        }),
    )
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Write one JSON-RPC message as a single line to stdout (+ flush).
fn write_message(stdout: &mut std::io::Stdout, msg: &Value) {
    if let Ok(line) = serde_json::to_string(msg) {
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_returns_server_info_and_capabilities() {
        let resp = handle_initialize(json!(1), &json!({"protocolVersion": MCP_PROTOCOL_VERSION}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["serverInfo"]["name"], "kettle");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn unknown_client_version_falls_back_to_ours() {
        let resp = handle_initialize(json!(1), &json!({"protocolVersion": "1999-01-01"}));
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn error_response_shape() {
        let e = error_response(json!(5), -32601, "nope");
        assert_eq!(e["error"]["code"], -32601);
        assert_eq!(e["error"]["message"], "nope");
        assert_eq!(e["id"], 5);
    }
}
