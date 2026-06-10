//! Cycle 932 (agent-first A3): the MCP tool registry.
//!
//! `kettle_run` runs a command headlessly via the A1 exec engine in-process.
//! The other tools (`kettle_list_panes`, `kettle_read_screen`, `kettle_send_text`,
//! `kettle_run_command`) drive a running kettle via the A2 control client; when
//! no server is discoverable they return an `isError` result with actionable
//! text (start `kettle --agent-server full`).

use serde_json::{Value, json};

use crate::exec::{ExecOpts, OutputMode, run_exec_capture};
use kettle_ctl::Client;

/// The tool specifications for `tools/list` (name + description + JSON Schema).
pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "kettle_run",
            "description": "Run a command headlessly under a real PTY (no window) and return its \
                output and exit code. Use for one-shot commands; the child gets a real terminal \
                so colored/TUI-aware programs behave normally. Output is ANSI-stripped by default.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "array", "items": {"type": "string"}, "description": "argv, e.g. [\"ls\",\"-la\"]"},
                    "cols": {"type": "integer", "description": "terminal width (default 80)"},
                    "rows": {"type": "integer", "description": "terminal height (default 24)"},
                    "cwd": {"type": "string", "description": "working directory"},
                    "timeout_s": {"type": "number", "description": "kill + report timeout after N seconds"},
                    "strip_ansi": {"type": "boolean", "description": "strip ANSI escapes (default true)"}
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "kettle_list_panes",
            "description": "List the panes of a running kettle (id, tab, title, cwd, size, focus). \
                Requires kettle running with `--agent-server full` (or read-only).",
            "inputSchema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "kettle_read_screen",
            "description": "Read the visible text (and optional scrollback) of a kettle pane. \
                Requires the agent server.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "scrollback_lines": {"type": "integer", "description": "extra history lines to include"}
                }
            }
        }),
        json!({
            "name": "kettle_send_text",
            "description": "Type text into a kettle pane's terminal (append \\n to submit). \
                Requires the agent server in `full` mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "text": {"type": "string"}
                },
                "required": ["text"]
            }
        }),
        json!({
            "name": "kettle_run_command",
            "description": "Run a command in a kettle pane and wait for it to finish, returning the \
                exit code (if the shell has OSC 133 integration), duration, and output. \
                Requires the agent server in `full` mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "command": {"type": "string"},
                    "timeout_s": {"type": "number", "description": "give up waiting after N seconds (default 15)"}
                },
                "required": ["command"]
            }
        }),
    ]
}

/// Dispatch a `tools/call`. `params` is `{name, params|arguments}`. Returns an
/// MCP tool result (`{content: [...], isError?}`).
pub fn call_tool(params: &Value) -> Value {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    // MCP uses `arguments`; accept `params` too for convenience.
    let args = params
        .get("arguments")
        .or_else(|| params.get("params"))
        .cloned()
        .unwrap_or(Value::Null);
    match name {
        "kettle_run" => tool_kettle_run(&args),
        "kettle_list_panes" => ctl_call("list_panes", json!({})),
        "kettle_read_screen" => {
            let mut p = serde_json::Map::new();
            if let Some(pane) = args.get("pane") {
                p.insert("pane".into(), pane.clone());
            }
            if let Some(sb) = args.get("scrollback_lines") {
                p.insert("scrollback_lines".into(), sb.clone());
            }
            ctl_call("read_screen", Value::Object(p))
        }
        "kettle_send_text" => {
            let Some(text) = args.get("text").and_then(|t| t.as_str()) else {
                return error_result("kettle_send_text requires a 'text' string");
            };
            let mut p = serde_json::Map::new();
            p.insert("text".into(), json!(text));
            if let Some(pane) = args.get("pane") {
                p.insert("pane".into(), pane.clone());
            }
            ctl_call("send_text", Value::Object(p))
        }
        "kettle_run_command" => {
            let Some(cmd) = args.get("command").and_then(|c| c.as_str()) else {
                return error_result("kettle_run_command requires a 'command' string");
            };
            let mut p = serde_json::Map::new();
            p.insert("command".into(), json!(cmd));
            if let Some(pane) = args.get("pane") {
                p.insert("pane".into(), pane.clone());
            }
            if let Some(t) = args.get("timeout_s") {
                p.insert("timeout_s".into(), t.clone());
            }
            ctl_call("run_command", Value::Object(p))
        }
        other => error_result(&format!("unknown tool '{other}'")),
    }
}

/// `kettle_run`: run a command headlessly + capture output.
fn tool_kettle_run(args: &Value) -> Value {
    let Some(command) = args.get("command").and_then(|c| c.as_array()) else {
        return error_result("kettle_run requires a 'command' string array");
    };
    let argv: Vec<String> = command
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if argv.is_empty() {
        return error_result("kettle_run 'command' must be a non-empty string array");
    }
    let cols = args.get("cols").and_then(|c| c.as_u64()).unwrap_or(80) as u16;
    let rows = args.get("rows").and_then(|r| r.as_u64()).unwrap_or(24) as u16;
    let strip = args
        .get("strip_ansi")
        .and_then(|s| s.as_bool())
        .unwrap_or(true);
    let opts = ExecOpts {
        argv,
        cols,
        rows,
        cwd: args
            .get("cwd")
            .and_then(|c| c.as_str())
            .map(std::path::PathBuf::from),
        timeout: args
            .get("timeout_s")
            .and_then(|t| t.as_f64())
            .map(std::time::Duration::from_secs_f64),
        mode: if strip {
            OutputMode::StripAnsi
        } else {
            OutputMode::Raw
        },
        record: None,
        forward_stdin: false,
    };
    let (code, output) = run_exec_capture(opts);
    let text = format!("exit code: {code}\n\n{output}");
    json!({ "content": [{ "type": "text", "text": text }], "isError": code != 0 })
}

/// Call a control-server method via the A2 client; render the result/ error as
/// an MCP tool result.
fn ctl_call(method: &str, params: Value) -> Value {
    let mut client = match Client::discover(None) {
        Ok(c) => c,
        Err(e) => {
            return error_result(&format!(
                "{e}\n(start kettle with `kettle --agent-server full` for this tool)"
            ));
        }
    };
    match client.call(method, params) {
        Ok(result) => {
            let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        Err(e) => error_result(&format!("{method}: {e}")),
    }
}

/// An MCP error tool-result (isError = true).
fn error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_have_required_shape() {
        let specs = tool_specs();
        assert!(specs.len() >= 5);
        for s in &specs {
            assert!(s["name"].is_string(), "tool missing name: {s}");
            assert!(s["description"].is_string(), "tool missing description");
            assert_eq!(s["inputSchema"]["type"], "object", "schema not an object");
        }
        // kettle_run requires `command`.
        let run = specs
            .iter()
            .find(|s| s["name"] == "kettle_run")
            .expect("kettle_run present");
        assert_eq!(run["inputSchema"]["required"][0], "command");
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let r = call_tool(&json!({"name": "nope", "arguments": {}}));
        assert_eq!(r["isError"], true);
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    #[test]
    fn kettle_run_rejects_empty_command() {
        let r = call_tool(&json!({"name": "kettle_run", "arguments": {"command": []}}));
        assert_eq!(r["isError"], true);
    }

    #[test]
    fn ctl_tool_without_server_is_actionable_error() {
        // No server running in the unit-test environment → actionable isError.
        let r = call_tool(&json!({"name": "kettle_list_panes", "arguments": {}}));
        assert_eq!(r["isError"], true);
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("agent-server"),
            "error should point at --agent-server"
        );
    }
}
