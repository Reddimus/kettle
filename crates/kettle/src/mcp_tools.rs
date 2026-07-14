//! Cycle 932 (agent-first A3): the MCP tool registry.
//!
//! `kettle_run` runs a command headlessly via the A1 exec engine in-process.
//! The other tools drive a running kettle via the A2 control client; when no
//! server is discoverable they return an `isError` result with actionable text
//! (start `kettle --agent-server full`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::exec::{ExecOpts, OutputMode, run_exec_capture, run_exec_capture_cancellable};
use kettle_ctl::Client;

const MAX_TOOL_TEXT_BYTES: usize = 512 * 1024;
const MAX_COMMAND_ARGS: usize = 256;
const MAX_COMMAND_ARG_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    arguments: Option<Value>,
    #[serde(default, rename = "_meta")]
    _meta: Value,
}

#[derive(Clone, Copy)]
enum ArgKind {
    String,
    Bool,
    Unsigned,
    Integer,
    Number,
    Strings,
}

/// The tool specifications for `tools/list` (name + description + JSON Schema).
pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "kettle_run",
            "description": "Run a command headlessly under a real PTY (no window) and return its \
                output and exit code. Use for one-shot commands; the child gets a real terminal \
                so colored/TUI-aware programs behave normally. Output is ANSI-stripped by default. \
                The child gets no stdin; a long-running or interactive program is killed at the \
                timeout (default 30s, max 600s) and reported as exit code 124.",
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
                "required": ["command"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_list_panes",
            "description": "List the panes of a running kettle (id, tab, title, cwd, size, focus). \
                Requires kettle running with `--agent-server full` (or read-only).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cursor": {"type": "string", "description": "continuation cursor from the prior page"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 4096},
                    "snapshot": {"type": "string", "description": "snapshot token from the prior page"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_read_screen",
            "description": "Read the visible text (and optional scrollback) of a kettle pane. \
                Requires the agent server.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "scrollback_lines": {"type": "integer", "description": "extra history lines to include"},
                    "include_selection": {"type": "boolean", "description": "include selected text (capped at 128 KiB)"},
                    "cursor": {"type": "string", "description": "continuation line cursor"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 4096},
                    "snapshot": {"type": "string", "description": "snapshot token from the prior page"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_read_cells",
            "description": "Read the visible cell grid plus selected attributes such as underline \
                and strikeout. Use for renderer diagnostics without OCR. Works in read-only mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "cursor": {"type": "string", "description": "continuation cell cursor"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 4096},
                    "snapshot": {"type": "string", "description": "snapshot token from the prior page"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_ui_geometry",
            "description": "Read live window UI geometry, including tab-bar segment rectangles, \
                pane titlebar rectangles, fitted title diagnostics, new-tab button bounds, \
                open context-menu rows, and tab drag state. Works in read-only mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "window": {"type": "integer", "description": "window seq (default: focused window)"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_screenshot",
            "description": "Save a live PNG screenshot from a running kettle. Defaults to the \
                focused pane crop; pass pane for a specific pane, full_window=true for the whole \
                window, and path to choose the output file. Requires the agent server in `full` \
                mode because saving the PNG mutates the filesystem.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused pane)"},
                    "full_window": {"type": "boolean", "description": "capture the whole target window instead of cropping to a pane"},
                    "path": {"type": "string", "description": "output PNG path (default: cache/kettle/shots/kettle-<time>-<pid>.png)"}
                },
                "additionalProperties": false
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
                "required": ["text"],
                "additionalProperties": false
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
                "required": ["command"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_send_keys",
            "description": "Press named keys / chords in a kettle pane — the way to drive \
                INTERACTIVE programs (vim, htop, fzf, tmux). Each token is one key: a name \
                (escape, enter, tab, backspace, delete, insert, space, up/down/left/right, \
                home/end, pageup/pagedown, f1–f12, plus/comma/minus/equal for the literal \
                characters), a chord (ctrl+c, alt+enter, shift+tab), or a single character \
                ('G' sends shift-g; multi-character text belongs in kettle_send_text). Keys \
                encode through the same path as real keystrokes, honoring the app's terminal \
                modes. Requires the agent server in `full` mode. Example: [\"escape\", \":\", \
                \"w\", \"q\", \"enter\"] saves and quits vim.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "keys": {"type": "array", "items": {"type": "string"}, "description": "key tokens, pressed in order"}
                },
                "required": ["keys"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_send_mouse",
            "description": "Send deterministic mouse input to a running kettle window for \
                interactive UI/TUI diagnostics. Coordinates are physical pixels from the \
                window's client-area top-left. Requires the agent server in `full` mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "window": {"type": "integer", "description": "window seq (default: focused window)"},
                    "event": {"type": "string", "enum": ["move", "press", "release", "click", "wheel"]},
                    "x": {"type": "number", "description": "x coordinate for move/press/release/click, or optional wheel cursor position"},
                    "y": {"type": "number", "description": "y coordinate for move/press/release/click, or optional wheel cursor position"},
                    "button": {"type": "string", "enum": ["left", "middle", "right", "back", "forward"], "description": "default left"},
                    "wheel_lines": {"type": "integer", "description": "signed terminal-scroll lines for wheel events"}
                },
                "required": ["event"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_resize_window",
            "description": "Request a live Kettle window client-area resize and let the normal \
                renderer/PTY resize path process it. Use for resize-overlay and split/grid \
                diagnostics. Requires the agent server in `full` mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "window": {"type": "integer", "description": "window seq (default: focused window)"},
                    "width": {"type": "integer", "description": "requested client-area width in physical pixels"},
                    "height": {"type": "integer", "description": "requested client-area height in physical pixels"}
                },
                "required": ["width", "height"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_perform_action",
            "description": "Dispatch a named Kettle app action such as start_search, \
                command_palette, or open_settings against the focused window. This drives \
                terminal chrome rather than writing bytes to the pane. Requires the agent \
                server in `full` mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "action name accepted by kettle keybinds, e.g. start_search"}
                },
                "required": ["action"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "kettle_wait_for",
            "description": "Wait until a kettle pane's screen matches a condition — replaces \
                sleep-and-pray when driving interactive apps. Conditions (AND when combined): \
                'text' (substring appears), 'regex' (pattern matches the screen), 'quiet_ms' \
                (screen unchanged for N ms — output settled). Returns {matched, elapsed_ms}; a \
                timeout returns matched=false rather than an error. Works in read-only mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused)"},
                    "text": {"type": "string", "description": "substring that must appear on screen"},
                    "regex": {"type": "string", "description": "regex the screen text must match"},
                    "quiet_ms": {"type": "integer", "description": "require the screen unchanged for N ms"},
                    "timeout_ms": {"type": "integer", "description": "overall deadline (default 30000, max 300000)"}
                },
                "additionalProperties": false
            }
        }),
    ]
}

/// Dispatch a `tools/call`. `params` is `{name, params|arguments}`. Returns an
/// MCP tool result (`{content: [...], isError?}`).
pub fn call_tool(params: &Value) -> Value {
    call_tool_inner(params, None)
}

/// Dispatch a tool while observing the owning JSON-RPC request's cancellation
/// flag. Local runs terminate their child, while control-backed calls stop
/// waiting and drop the connection so the server can release deferred work.
pub fn call_tool_cancellable(params: &Value, cancelled: &std::sync::atomic::AtomicBool) -> Value {
    call_tool_inner(params, Some(cancelled))
}

pub(crate) fn validate_tool_call(params: &Value) -> Result<(), String> {
    parse_tool_call(params).map(|_| ())
}

fn parse_tool_call(params: &Value) -> Result<ToolCallParams, String> {
    if params
        .get("arguments")
        .is_some_and(|arguments| !arguments.is_object())
    {
        return Err("tools/call 'arguments' must be an object".into());
    }
    let call: ToolCallParams = serde_json::from_value(params.clone())
        .map_err(|error| format!("invalid tools/call params: {error}"))?;
    if !is_known_tool(&call.name) {
        return Err(format!("unknown tool '{}'", call.name));
    }
    Ok(call)
}

fn is_known_tool(name: &str) -> bool {
    matches!(
        name,
        "kettle_run"
            | "kettle_list_panes"
            | "kettle_read_screen"
            | "kettle_read_cells"
            | "kettle_ui_geometry"
            | "kettle_screenshot"
            | "kettle_send_text"
            | "kettle_run_command"
            | "kettle_send_keys"
            | "kettle_send_mouse"
            | "kettle_resize_window"
            | "kettle_perform_action"
            | "kettle_wait_for"
    )
}

fn call_tool_inner(params: &Value, cancelled: Option<&std::sync::atomic::AtomicBool>) -> Value {
    let call = match parse_tool_call(params) {
        Ok(call) => call,
        Err(error) => return error_result(&error),
    };
    let args = call.arguments.unwrap_or_else(|| json!({}));
    if let Err(error) = validate_tool_arguments(&call.name, &args) {
        return error_result(&error);
    }
    match call.name.as_str() {
        "kettle_run" => tool_kettle_run(&args, cancelled),
        "kettle_list_panes" => ctl_call("list_panes", paging_params(&args, &[]), cancelled),
        "kettle_read_screen" => {
            let mut p = serde_json::Map::new();
            for key in [
                "pane",
                "scrollback_lines",
                "include_selection",
                "cursor",
                "limit",
                "snapshot",
            ] {
                if let Some(value) = args.get(key) {
                    p.insert(key.into(), value.clone());
                }
            }
            ctl_call("read_screen", Value::Object(p), cancelled)
        }
        "kettle_read_cells" => {
            let mut p = serde_json::Map::new();
            for key in ["pane", "cursor", "limit", "snapshot"] {
                if let Some(value) = args.get(key) {
                    p.insert(key.into(), value.clone());
                }
            }
            ctl_call("read_cells", Value::Object(p), cancelled)
        }
        "kettle_ui_geometry" => {
            let mut p = serde_json::Map::new();
            if let Some(window) = args.get("window") {
                p.insert("window".into(), window.clone());
            }
            ctl_call("ui_geometry", Value::Object(p), cancelled)
        }
        "kettle_screenshot" => {
            let mut p = serde_json::Map::new();
            for k in ["pane", "full_window", "path"] {
                if let Some(v) = args.get(k) {
                    p.insert(k.into(), v.clone());
                }
            }
            ctl_call("screenshot", Value::Object(p), cancelled)
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
            ctl_call("send_text", Value::Object(p), cancelled)
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
            ctl_call("run_command", Value::Object(p), cancelled)
        }
        // v2.20.0 (agent plane).
        "kettle_send_keys" => {
            let Some(keys) = args.get("keys").and_then(|k| k.as_array()) else {
                return error_result("kettle_send_keys requires a 'keys' string array");
            };
            if keys.is_empty() {
                return error_result("kettle_send_keys 'keys' must be non-empty");
            }
            let mut p = serde_json::Map::new();
            p.insert("keys".into(), Value::Array(keys.clone()));
            if let Some(pane) = args.get("pane") {
                p.insert("pane".into(), pane.clone());
            }
            ctl_call("send_keys", Value::Object(p), cancelled)
        }
        "kettle_send_mouse" => {
            let Some(event) = args.get("event").and_then(|e| e.as_str()) else {
                return error_result("kettle_send_mouse requires an 'event' string");
            };
            let mut p = serde_json::Map::new();
            p.insert("event".into(), json!(event));
            for k in ["window", "x", "y", "button", "wheel_lines"] {
                if let Some(v) = args.get(k) {
                    p.insert(k.into(), v.clone());
                }
            }
            ctl_call("send_mouse", Value::Object(p), cancelled)
        }
        "kettle_resize_window" => {
            let Some(width) = args.get("width") else {
                return error_result("kettle_resize_window requires a 'width' integer");
            };
            let Some(height) = args.get("height") else {
                return error_result("kettle_resize_window requires a 'height' integer");
            };
            let mut p = serde_json::Map::new();
            p.insert("width".into(), width.clone());
            p.insert("height".into(), height.clone());
            if let Some(window) = args.get("window") {
                p.insert("window".into(), window.clone());
            }
            ctl_call("resize_window", Value::Object(p), cancelled)
        }
        "kettle_perform_action" => {
            let Some(action) = args.get("action").and_then(|a| a.as_str()) else {
                return error_result("kettle_perform_action requires an 'action' string");
            };
            let mut p = serde_json::Map::new();
            p.insert("action".into(), json!(action));
            ctl_call("perform_action", Value::Object(p), cancelled)
        }
        "kettle_wait_for" => {
            let mut p = serde_json::Map::new();
            for k in ["pane", "text", "regex", "quiet_ms", "timeout_ms"] {
                if let Some(v) = args.get(k) {
                    p.insert(k.into(), v.clone());
                }
            }
            if !p.contains_key("text") && !p.contains_key("regex") && !p.contains_key("quiet_ms") {
                return error_result(
                    "kettle_wait_for needs at least one of 'text', 'regex', 'quiet_ms'",
                );
            }
            ctl_call("wait_for", Value::Object(p), cancelled)
        }
        other => error_result(&format!("unknown tool '{other}'")),
    }
}

/// `kettle_run`: run a command headlessly + capture output.
fn tool_kettle_run(args: &Value, cancelled: Option<&std::sync::atomic::AtomicBool>) -> Value {
    let Some(command) = args.get("command").and_then(|c| c.as_array()) else {
        return error_result("kettle_run requires a 'command' string array");
    };
    if command.len() > MAX_COMMAND_ARGS {
        return error_result("kettle_run 'command' has too many arguments");
    }
    let Some(argv) = command
        .iter()
        .map(|value| value.as_str().map(String::from))
        .collect::<Option<Vec<_>>>()
    else {
        return error_result("kettle_run 'command' must contain only strings");
    };
    if argv.is_empty() {
        return error_result("kettle_run 'command' must be a non-empty string array");
    }
    if argv.iter().any(|arg| arg.len() > MAX_COMMAND_ARG_BYTES) {
        return error_result("kettle_run command argument exceeds 64 KiB");
    }
    // Clamp before narrowing so an oversized value saturates instead of wrapping.
    let cols = args
        .get("cols")
        .and_then(|c| c.as_u64())
        .unwrap_or(80)
        .min(u16::MAX as u64) as u16;
    let rows = args
        .get("rows")
        .and_then(|r| r.as_u64())
        .unwrap_or(24)
        .min(u16::MAX as u64) as u16;
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
        // Always bound the run: the MCP server is single-threaded, so a child
        // that never exits (interactive prompt, daemon) would wedge it forever.
        // Default 30s, capped 0.1–600s (mirrors run_command). On expiry the
        // child is killed and exec reports 124.
        timeout: Some(std::time::Duration::from_secs_f64(
            args.get("timeout_s")
                .and_then(|t| t.as_f64())
                .unwrap_or(30.0)
                .clamp(0.1, 600.0),
        )),
        mode: if strip {
            OutputMode::StripAnsi
        } else {
            OutputMode::Raw
        },
        record: None,
        forward_stdin: false,
    };
    let (code, output) = match cancelled {
        Some(cancelled) => run_exec_capture_cancellable(opts, cancelled),
        None => run_exec_capture(opts),
    };
    let (text, truncated) = cap_tool_text(format!("exit code: {code}\n\n{output}"));
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": {"exit_code": code, "truncated": truncated},
        "isError": code != 0,
    })
}

/// Call a control-server method via the A2 client; render the result/ error as
/// an MCP tool result.
fn ctl_call(
    method: &str,
    params: Value,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Value {
    let mut client = match Client::discover(None) {
        Ok(c) => c,
        Err(e) => {
            return error_result(&format!(
                "{e}\n(start kettle with `kettle --agent-server full` for this tool)"
            ));
        }
    };
    let response = match cancelled {
        Some(cancelled) => client.call_cancellable(method, params, cancelled),
        None => client.call(method, params),
    };
    match response {
        Ok(result) => {
            let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
            let (text, truncated) = cap_tool_text(text);
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": {"truncated": truncated},
            })
        }
        Err(e) => error_result(&format!("{method}: {e}")),
    }
}

/// An MCP error tool-result (isError = true).
fn error_result(message: &str) -> Value {
    let (message, truncated) = cap_tool_text(message.to_string());
    json!({
        "content": [{ "type": "text", "text": message }],
        "structuredContent": {"truncated": truncated},
        "isError": true,
    })
}

fn paging_params(args: &Value, extra: &[&str]) -> Value {
    let mut params = serde_json::Map::new();
    for key in ["cursor", "limit", "snapshot"]
        .into_iter()
        .chain(extra.iter().copied())
    {
        if let Some(value) = args.get(key) {
            params.insert(key.into(), value.clone());
        }
    }
    Value::Object(params)
}

fn validate_tool_arguments(name: &str, args: &Value) -> Result<(), String> {
    let fields: &[(&str, ArgKind)] = match name {
        "kettle_run" => &[
            ("command", ArgKind::Strings),
            ("cols", ArgKind::Unsigned),
            ("rows", ArgKind::Unsigned),
            ("cwd", ArgKind::String),
            ("timeout_s", ArgKind::Number),
            ("strip_ansi", ArgKind::Bool),
        ],
        "kettle_list_panes" => &[
            ("cursor", ArgKind::String),
            ("limit", ArgKind::Unsigned),
            ("snapshot", ArgKind::String),
        ],
        "kettle_read_screen" => &[
            ("pane", ArgKind::Unsigned),
            ("scrollback_lines", ArgKind::Unsigned),
            ("include_selection", ArgKind::Bool),
            ("cursor", ArgKind::String),
            ("limit", ArgKind::Unsigned),
            ("snapshot", ArgKind::String),
        ],
        "kettle_read_cells" => &[
            ("pane", ArgKind::Unsigned),
            ("cursor", ArgKind::String),
            ("limit", ArgKind::Unsigned),
            ("snapshot", ArgKind::String),
        ],
        "kettle_ui_geometry" => &[("window", ArgKind::Unsigned)],
        "kettle_screenshot" => &[
            ("pane", ArgKind::Unsigned),
            ("full_window", ArgKind::Bool),
            ("path", ArgKind::String),
        ],
        "kettle_send_text" => &[("pane", ArgKind::Unsigned), ("text", ArgKind::String)],
        "kettle_run_command" => &[
            ("pane", ArgKind::Unsigned),
            ("command", ArgKind::String),
            ("timeout_s", ArgKind::Number),
        ],
        "kettle_send_keys" => &[("pane", ArgKind::Unsigned), ("keys", ArgKind::Strings)],
        "kettle_send_mouse" => &[
            ("window", ArgKind::Unsigned),
            ("event", ArgKind::String),
            ("x", ArgKind::Number),
            ("y", ArgKind::Number),
            ("button", ArgKind::String),
            ("wheel_lines", ArgKind::Integer),
        ],
        "kettle_resize_window" => &[
            ("window", ArgKind::Unsigned),
            ("width", ArgKind::Unsigned),
            ("height", ArgKind::Unsigned),
        ],
        "kettle_perform_action" => &[("action", ArgKind::String)],
        "kettle_wait_for" => &[
            ("pane", ArgKind::Unsigned),
            ("text", ArgKind::String),
            ("regex", ArgKind::String),
            ("quiet_ms", ArgKind::Unsigned),
            ("timeout_ms", ArgKind::Unsigned),
        ],
        _ => return Ok(()),
    };
    let Some(object) = args.as_object() else {
        return Ok(()); // null means an empty object and is handled by required fields.
    };
    for (key, value) in object {
        let Some((_, kind)) = fields.iter().find(|(name, _)| *name == key) else {
            return Err(format!("{name} does not accept argument '{key}'"));
        };
        let valid = match kind {
            ArgKind::String => value.is_string(),
            ArgKind::Bool => value.is_boolean(),
            ArgKind::Unsigned => value.as_u64().is_some(),
            ArgKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            ArgKind::Number => value.is_number(),
            ArgKind::Strings => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
        };
        if !valid {
            return Err(format!("{name} argument '{key}' has the wrong type"));
        }
    }
    Ok(())
}

fn cap_tool_text(text: String) -> (String, bool) {
    if text.len() <= MAX_TOOL_TEXT_BYTES {
        return (text, false);
    }
    const MARKER: &str = "\n\n[... Kettle MCP result truncated ...]\n\n";
    let budget = MAX_TOOL_TEXT_BYTES - MARKER.len();
    let mut head = budget / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (budget - head);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    (
        format!("{}{}{}", &text[..head], MARKER, &text[tail..]),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_have_required_shape() {
        let specs = tool_specs();
        assert!(specs.len() >= 8, "screenshot is part of the agent plane");
        for s in &specs {
            assert!(s["name"].is_string(), "tool missing name: {s}");
            assert!(s["description"].is_string(), "tool missing description");
            assert_eq!(s["inputSchema"]["type"], "object", "schema not an object");
            assert_eq!(s["inputSchema"]["additionalProperties"], false);
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

        let r = call_tool(&json!({
            "name": "kettle_run",
            "arguments": {"command": ["echo", 1]},
        }));
        assert_eq!(r["isError"], true);
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("wrong type")
        );
    }

    #[test]
    fn tools_call_requires_typed_envelope_and_object_arguments() {
        let result = call_tool(&json!({"name":"kettle_list_panes","arguments":[],"extra":1}));
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("object")
        );

        let result = call_tool(&json!({"name":"kettle_list_panes","arguments":[]}));
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("object")
        );

        assert!(
            validate_tool_call(&json!({
                "name":"kettle_list_panes",
                "arguments":{},
                "task": {"ttl": 1000},
            }))
            .is_ok()
        );
        assert!(
            validate_tool_call(&json!({
                "name":"kettle_list_panes",
                "arguments":null,
            }))
            .is_err()
        );
    }

    #[test]
    fn tool_text_cap_is_utf8_safe_and_keeps_head_and_tail() {
        let input = format!("head{}tail", "é".repeat(MAX_TOOL_TEXT_BYTES));
        let (output, truncated) = cap_tool_text(input);
        assert!(truncated);
        assert!(output.len() <= MAX_TOOL_TEXT_BYTES);
        assert!(output.starts_with("head"));
        assert!(output.ends_with("tail"));
    }

    /// v2.20.0: the new agent-plane tools validate their arguments BEFORE
    /// touching the control client, so a malformed call gets a crisp message
    /// even with no server running.
    #[test]
    fn send_keys_and_wait_for_validate_args_first() {
        let r = call_tool(&json!({"name": "kettle_send_keys", "arguments": {}}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("keys"));

        let r = call_tool(&json!({"name": "kettle_send_keys", "arguments": {"keys": []}}));
        assert_eq!(r["isError"], true);

        let r = call_tool(&json!({"name": "kettle_perform_action", "arguments": {}}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("action"));

        let r = call_tool(&json!({"name": "kettle_wait_for", "arguments": {"timeout_ms": 5}}));
        assert_eq!(r["isError"], true);
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("at least one of"),
            "wait_for must demand a condition"
        );
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
