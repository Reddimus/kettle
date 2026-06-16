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
            "params": {"protocolVersion": "2025-06-18"}
        }),
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
