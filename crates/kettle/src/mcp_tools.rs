//! The MCP tool registry.
//!
//! `kettle_run` runs a command headlessly via the exec engine in-process.
//! The other tools drive a running kettle via the control client; when no
//! server is discoverable they return an `isError` result with actionable text
//! (start `kettle --agent-server full`).
//!
//! `kettle mcp --display` offers `kettle_show` alone: it sends a file to the
//! media shelf of the pane the server runs in, needs only agent previews, and
//! never reads, types or runs anything.

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

/// Which tools a server offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSelection {
    /// Every control tool and `kettle_run`.
    Full,
    /// `kettle_show` alone.
    Display,
}

/// The tool specifications for `tools/list` (name + description + JSON Schema).
pub fn tool_specs(selection: ToolSelection) -> Vec<Value> {
    match selection {
        ToolSelection::Full => full_tool_specs(),
        ToolSelection::Display => vec![show_tool_spec()],
    }
}

/// An absolute path as this platform spells one, for the schema: rooted
/// here; a drive or a UNC share on Windows.
const ABSOLUTE_PATH_PATTERN: &str = if cfg!(windows) {
    r"^([A-Za-z]:[\\/]|[\\/]{2}[^\\/])"
} else {
    "^/"
};

/// `kettle_show`'s specification. The schema says what the validator
/// checks: exactly one source, nonempty strings, an absolute path, and no
/// other argument. JSON Schema counts characters where Kettle's caps count
/// bytes, so each `maxLength` is the byte cap, which no string within it
/// exceeds, and the descriptions name the caps in bytes.
fn show_tool_spec() -> Value {
    json!({
        "name": "kettle_show",
        "description": "Send an image, SVG or Mermaid file path to the user's Kettle display, \
            or render inline Mermaid source. Interactive supported harnesses get a card under \
            the call and a shelf entry; other modes use the shelf. Clicking opens it in the \
            viewer or preview lane. Tested diagram families: flowchart, sequence, state, class, \
            ER, gantt, pie, mindmap, gitGraph, timeline, journey and quadrant. Returns delivery \
            status and metadata, not the image contents.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "mermaid": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": kettle_media::MAX_MERMAID_BYTES,
                    "description": "Mermaid source to render, at most 64 KiB of UTF-8; keep the source in your reply too"
                },
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": kettle_media::MAX_PATH_BYTES,
                    "pattern": ABSOLUTE_PATH_PATTERN,
                    "description": "absolute path of an image, SVG or Mermaid file, at most 4 KiB"
                },
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": kettle_ctl::show::MAX_SHOW_TITLE_BYTES,
                    "description": "title for the shelf item (default: the file name), at most 4 KiB"
                },
                "key": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": kettle_ctl::show::MAX_SHOW_KEY_BYTES,
                    "description": "replace the shelf item with this key instead of adding another (default: the file's path), at most 256 bytes"
                }
            },
            "oneOf": [{"required": ["mermaid"]}, {"required": ["path"]}],
            "additionalProperties": false
        }
    })
}

fn full_tool_specs() -> Vec<Value> {
    vec![
        show_tool_spec(),
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
                    "cwd": {"type": "string", "description": "working directory (default: the MCP server's current directory)"},
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
                window, or all four crop fields for a physical-pixel window region. Use path to \
                choose the output file. Requires the agent server in `full` mode because saving \
                the PNG mutates the filesystem.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": {"type": "integer", "description": "pane id (default: focused pane)"},
                    "full_window": {"type": "boolean", "description": "capture the whole target window instead of cropping to a pane"},
                    "crop_x": {"type": "number", "minimum": 0, "description": "window-relative physical-pixel crop x; requires all four crop fields"},
                    "crop_y": {"type": "number", "minimum": 0, "description": "window-relative physical-pixel crop y; requires all four crop fields"},
                    "crop_width": {"type": "number", "exclusiveMinimum": 0, "description": "physical-pixel crop width; requires all four crop fields"},
                    "crop_height": {"type": "number", "exclusiveMinimum": 0, "description": "physical-pixel crop height; requires all four crop fields"},
                    "path": {"type": "string", "minLength": 1, "pattern": "\\S", "description": "new output PNG leaf beneath an already-existing parent; existing leaves are never overwritten (default: cache/kettle/shots/kettle-<time>-<pid>.png)"}
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
                exit code (if the shell has OSC 133 integration), duration, and output. Output is \
                capped at 10,000 retained lines and 512 KiB; output_truncated reports either cap. \
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
            "name": "kettle_dispatch_ui_key",
            "description": "Press bounded key tokens in the currently open Kettle UI modal. \
                Unlike kettle_send_keys, this never writes bytes to the terminal PTY. Use it \
                after kettle_perform_action start_search to test search editing/navigation safely; \
                with Search open, a chord the bar does not use runs its Kettle shortcut. \
                Requires the agent server in `full` mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "keys": {"type": "array", "minItems": 1, "maxItems": 64, "items": {"type": "string", "minLength": 1, "maxLength": 64}, "description": "UI key tokens, pressed in order"}
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
                    "wheel_lines": {"type": "integer", "description": "signed terminal-scroll lines for wheel events (pre-quantized; skips the sub-detent accumulator)"},
                    "wheel_delta": {"type": "number", "description": "signed RAW wheel detents for wheel events, fractions allowed (e.g. 0.08 per event to emulate a precision touchpad). Mutually exclusive with wheel_lines; runs the real sub-detent accumulator"}
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
                    "timeout_ms": {"type": "integer", "description": "overall deadline (default 30000, max 300000)"},
                    "poll_ms": {"type": "integer", "minimum": 50, "maximum": 5000, "description": "screen poll interval (default 100)"}
                },
                "additionalProperties": false
            }
        }),
    ]
}

/// Dispatch a `tools/call`. `params` is `{name, arguments}`. Returns an
/// MCP tool result (`{content: [...], isError?}`).
pub fn call_tool(selection: ToolSelection, params: &Value) -> Value {
    call_tool_inner(selection, params, None)
}

/// Dispatch a tool while observing the owning JSON-RPC request's cancellation
/// flag. Local runs terminate their child, while control-backed calls stop
/// waiting and drop the connection so the server can release deferred work.
pub fn call_tool_cancellable(
    selection: ToolSelection,
    params: &Value,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Value {
    call_tool_inner(selection, params, Some(cancelled))
}

pub(crate) fn validate_tool_call(selection: ToolSelection, params: &Value) -> Result<(), String> {
    parse_tool_call(selection, params).map(|_| ())
}

fn parse_tool_call(selection: ToolSelection, params: &Value) -> Result<ToolCallParams, String> {
    if params
        .get("arguments")
        .is_some_and(|arguments| !arguments.is_object())
    {
        return Err("tools/call 'arguments' must be an object".into());
    }
    let call: ToolCallParams = serde_json::from_value(params.clone())
        .map_err(|error| format!("invalid tools/call params: {error}"))?;
    if !is_known_tool(selection, &call.name) {
        return Err(format!("unknown tool '{}'", call.name));
    }
    Ok(call)
}

fn is_known_tool(selection: ToolSelection, name: &str) -> bool {
    if selection == ToolSelection::Display {
        // `kettle_card` is the hook's retrieval, never listed for the model.
        return matches!(name, "kettle_show" | "kettle_card");
    }
    matches!(
        name,
        "kettle_show"
            | "kettle_run"
            | "kettle_list_panes"
            | "kettle_read_screen"
            | "kettle_read_cells"
            | "kettle_ui_geometry"
            | "kettle_screenshot"
            | "kettle_send_text"
            | "kettle_run_command"
            | "kettle_send_keys"
            | "kettle_dispatch_ui_key"
            | "kettle_send_mouse"
            | "kettle_resize_window"
            | "kettle_perform_action"
            | "kettle_wait_for"
    )
}

fn call_tool_inner(
    selection: ToolSelection,
    params: &Value,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Value {
    let call = match parse_tool_call(selection, params) {
        Ok(call) => call,
        Err(error) => return error_result(&error),
    };
    let args = call.arguments.unwrap_or_else(|| json!({}));
    if let Err(error) = validate_tool_arguments(&call.name, &args) {
        // Every kettle_show failure keeps its fixed wording and says the
        // model has not seen the media, an unknown argument included.
        if call.name == "kettle_show" {
            return show_failed(kettle_media::FailureCode::BadParams.model_message());
        }
        return error_result(&error);
    }
    match call.name.as_str() {
        "kettle_show" => tool_kettle_show(&args, params, cancelled, crate::mcp_display::session()),
        "kettle_card" => tool_kettle_card(&args, params, crate::mcp_display::session()),
        "kettle_run" => tool_kettle_run(&args, cancelled),
        "kettle_list_panes" => ctl_call(
            "list_panes",
            forwarded_ctl_arguments(&call.name, &args),
            cancelled,
        ),
        "kettle_read_screen" => ctl_call(
            "read_screen",
            forwarded_ctl_arguments(&call.name, &args),
            cancelled,
        ),
        "kettle_read_cells" => ctl_call(
            "read_cells",
            forwarded_ctl_arguments(&call.name, &args),
            cancelled,
        ),
        "kettle_ui_geometry" => ctl_call(
            "ui_geometry",
            forwarded_ctl_arguments(&call.name, &args),
            cancelled,
        ),
        "kettle_screenshot" => ctl_call(
            "screenshot",
            forwarded_ctl_arguments(&call.name, &args),
            cancelled,
        ),
        "kettle_send_text" => {
            let Some(text) = args.get("text").and_then(|t| t.as_str()) else {
                return error_result("kettle_send_text requires a 'text' string");
            };
            let _ = text;
            ctl_call(
                "send_text",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_run_command" => {
            let Some(cmd) = args.get("command").and_then(|c| c.as_str()) else {
                return error_result("kettle_run_command requires a 'command' string");
            };
            let _ = cmd;
            ctl_call(
                "run_command",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_send_keys" => {
            let Some(keys) = args.get("keys").and_then(|k| k.as_array()) else {
                return error_result("kettle_send_keys requires a 'keys' string array");
            };
            if keys.is_empty() {
                return error_result("kettle_send_keys 'keys' must be non-empty");
            }
            ctl_call(
                "send_keys",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_dispatch_ui_key" => {
            let Some(keys) = args.get("keys").and_then(|k| k.as_array()) else {
                return error_result("kettle_dispatch_ui_key requires a 'keys' string array");
            };
            if keys.is_empty() {
                return error_result("kettle_dispatch_ui_key 'keys' must be non-empty");
            }
            ctl_call(
                "dispatch_ui_key",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_send_mouse" => {
            let Some(event) = args.get("event").and_then(|e| e.as_str()) else {
                return error_result("kettle_send_mouse requires an 'event' string");
            };
            let _ = event;
            ctl_call(
                "send_mouse",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_resize_window" => {
            let Some(width) = args.get("width") else {
                return error_result("kettle_resize_window requires a 'width' integer");
            };
            let Some(height) = args.get("height") else {
                return error_result("kettle_resize_window requires a 'height' integer");
            };
            let _ = (width, height);
            ctl_call(
                "resize_window",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_perform_action" => {
            let Some(action) = args.get("action").and_then(|a| a.as_str()) else {
                return error_result("kettle_perform_action requires an 'action' string");
            };
            let _ = action;
            ctl_call(
                "perform_action",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        "kettle_wait_for" => {
            if args.get("text").is_none()
                && args.get("regex").is_none()
                && args.get("quiet_ms").is_none()
            {
                return error_result(
                    "kettle_wait_for needs at least one of 'text', 'regex', 'quiet_ms'",
                );
            }
            ctl_call(
                "wait_for",
                forwarded_ctl_arguments(&call.name, &args),
                cancelled,
            )
        }
        other => error_result(&format!("unknown tool '{other}'")),
    }
}

/// `kettle_show`: send an absolute file path, or Mermaid source, to the
/// media shelf of the pane this server runs in, through the same strict
/// discovery and wording as `kettle show`. The result says where it went
/// and what it was, never what it shows.
fn tool_kettle_show(
    args: &Value,
    params: &Value,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    session: &crate::mcp_display::DisplaySession,
) -> Value {
    use kettle_media::FailureCode;
    // A card only where this call's hook can collect it; Kettle decides
    // whether the caller may have one.
    let card = session.card_for(params, std::time::Instant::now());
    // Exactly one source: Mermaid text, which the request checks as Kettle
    // does, or an absolute path.
    let source = match (args.get("mermaid"), args.get("path")) {
        (Some(Value::String(text)), None) => kettle_ctl::show::ShowSource::Mermaid(text.clone()),
        (None, Some(Value::String(path))) if std::path::Path::new(path).is_absolute() => {
            match crate::show_cli::file_source(std::path::Path::new(path), false) {
                Ok(source) => source,
                Err(message) => return show_failed(&message),
            }
        }
        _ => return show_failed(FailureCode::BadParams.model_message()),
    };
    let name = args
        .get("path")
        .and_then(Value::as_str)
        .and_then(|path| std::path::Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let text = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_owned);
    let params = match crate::show_cli::request_params(kettle_ctl::show::ShowRequest {
        source,
        title: text("title"),
        key: text("key"),
        pane: None,
        inline: card
            .as_ref()
            .and(session.hook())
            .map(crate::mcp_display::CardHook::target),
    }) {
        Ok(params) => params,
        Err(message) => return show_failed(&message),
    };
    let mut client = match Client::discover_display(None) {
        Ok(client) => client,
        Err(error) => return show_failed(&crate::show_cli::failure_text(&error)),
    };
    let reply = match cancelled {
        Some(cancelled) => client.call_cancellable("show", params, cancelled),
        None => client.call_with_timeout("show", params, kettle_ctl::show::SHOW_CALL_TIMEOUT),
    };
    match reply.map(serde_json::from_value::<kettle_ctl::show::ShowResult>) {
        Ok(Ok(result)) => {
            let stored = match (card, &result.inline) {
                (Some(id), Some(delivery)) if delivery.fits() => {
                    session.store(id, delivery.message.clone(), std::time::Instant::now())
                }
                _ => false,
            };
            if stored {
                show_sent_card(&result, name.as_deref().unwrap_or("diagram"))
            } else {
                show_sent(&result)
            }
        }
        Ok(Err(_)) => show_failed("Kettle answered in a form this tool does not know."),
        Err(error) => show_failed(&crate::show_cli::failure_text(&error)),
    }
}

/// A `kettle_show` that reached the shelf: one plain line and status-only
/// structured content, which never carry the media itself.
fn show_sent(result: &kettle_ctl::show::ShowResult) -> Value {
    let sender = if result.verified {
        ""
    } else {
        ", from an unverified sender"
    };
    let text = format!(
        "Sent to the Kettle media shelf of pane {} ({} {}x{}{sender}); the user can open it \
         there. You have not seen its contents.",
        result.pane, result.kind, result.width, result.height
    );
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": {
            "status": "sent",
            "delivery": "shelf",
            "pane": result.pane,
            "window": result.window,
            "item": result.item,
            "verified": result.verified,
            "kind": result.kind,
            "width": result.width,
            "height": result.height,
            "warnings": result.warnings,
            "model_has_seen": false,
        },
    })
}

/// A `kettle_show` whose card waits for this call's hook: the line says it
/// shows below the call, and neither the line nor the structured content
/// carries the card's text.
fn show_sent_card(result: &kettle_ctl::show::ShowResult, name: &str) -> Value {
    let mut sent = show_sent(result);
    let text = format!(
        "Sent to Kettle for display below this call: {name} ({} {}x{}). You have not seen its \
         contents.",
        result.kind, result.width, result.height
    );
    sent["content"][0]["text"] = Value::String(text);
    sent["structuredContent"]["delivery"] = Value::String("card".into());
    sent
}

/// `kettle_card`: the hook's one-time retrieval of a card's message, as hook
/// output. Hidden from `tools/list`. A model's own call carries its tool-use
/// id in its metadata, so it is refused before anything is looked up.
fn tool_kettle_card(
    args: &Value,
    params: &Value,
    session: &crate::mcp_display::DisplaySession,
) -> Value {
    if crate::mcp_display::from_model(params) {
        return error_result("kettle_card is for Kettle's hook; the model does not call it.");
    }
    let Some(id) = args.get("tool_use_id").and_then(Value::as_str) else {
        return error_result("kettle_card requires a 'tool_use_id' string");
    };
    let message = session.take(id, std::time::Instant::now());
    let output = match message {
        Some(message) => json!({ "systemMessage": message }),
        None => json!({}),
    };
    json!({ "content": [{ "type": "text", "text": output.to_string() }] })
}

/// A `kettle_show` that did not reach the shelf, in its fixed wording.
fn show_failed(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "structuredContent": {"status": "failed", "model_has_seen": false},
        "isError": true,
    })
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
        // Always bound the run. A child that never exits (interactive prompt,
        // daemon) would otherwise tie up one of the MCP server's tool workers
        // forever. Default 30s, capped 0.1-600s (mirrors run_command). On
        // expiry the child is killed and exec reports 124.
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

/// Call a control-server method and render the result or error as an MCP
/// tool result.
fn ctl_call(
    method: &str,
    params: Value,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Value {
    let mut client = match Client::discover(None) {
        Ok(c) => c,
        Err(e) => return ctl_discovery_error(&e),
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

fn ctl_discovery_error(error: &impl std::fmt::Display) -> Value {
    error_result(&format!(
        "{error}\n(start kettle with `kettle --agent-server full` for this tool)"
    ))
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

fn forwarded_ctl_arguments(name: &str, args: &Value) -> Value {
    let mut params = serde_json::Map::new();
    for (key, _) in tool_argument_fields(name).unwrap_or_default() {
        if let Some(value) = args.get(key) {
            params.insert((*key).into(), value.clone());
        }
    }
    Value::Object(params)
}

fn tool_argument_fields(name: &str) -> Option<&'static [(&'static str, ArgKind)]> {
    Some(match name {
        "kettle_show" => &[
            ("mermaid", ArgKind::String),
            ("path", ArgKind::String),
            ("title", ArgKind::String),
            ("key", ArgKind::String),
        ],
        "kettle_card" => &[("tool_use_id", ArgKind::String)],
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
            ("crop_x", ArgKind::Number),
            ("crop_y", ArgKind::Number),
            ("crop_width", ArgKind::Number),
            ("crop_height", ArgKind::Number),
            ("path", ArgKind::String),
        ],
        "kettle_send_text" => &[("pane", ArgKind::Unsigned), ("text", ArgKind::String)],
        "kettle_run_command" => &[
            ("pane", ArgKind::Unsigned),
            ("command", ArgKind::String),
            ("timeout_s", ArgKind::Number),
        ],
        "kettle_send_keys" => &[("pane", ArgKind::Unsigned), ("keys", ArgKind::Strings)],
        "kettle_dispatch_ui_key" => &[("keys", ArgKind::Strings)],
        "kettle_send_mouse" => &[
            ("window", ArgKind::Unsigned),
            ("event", ArgKind::String),
            ("x", ArgKind::Number),
            ("y", ArgKind::Number),
            ("button", ArgKind::String),
            ("wheel_lines", ArgKind::Integer),
            ("wheel_delta", ArgKind::Number),
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
            ("poll_ms", ArgKind::Unsigned),
        ],
        _ => return None,
    })
}

fn validate_tool_arguments(name: &str, args: &Value) -> Result<(), String> {
    let Some(fields) = tool_argument_fields(name) else {
        return Ok(());
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

    fn names(selection: ToolSelection) -> Vec<String> {
        tool_specs(selection)
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }

    /// Display mode offers `kettle_show` alone and takes no other call; full
    /// mode does not offer it.
    #[test]
    fn display_mode_offers_exactly_kettle_show() {
        assert_eq!(names(ToolSelection::Display), ["kettle_show"]);
        assert_eq!(names(ToolSelection::Full)[0], "kettle_show");
        assert_eq!(
            tool_specs(ToolSelection::Full)[0],
            tool_specs(ToolSelection::Display)[0],
            "one kettle_show for both"
        );
        let schema = &tool_specs(ToolSelection::Display)[0]["inputSchema"];
        assert_eq!(
            schema["oneOf"],
            json!([{"required": ["mermaid"]}, {"required": ["path"]}])
        );
        assert_eq!(schema["additionalProperties"], json!(false));
        let properties: Vec<_> = schema["properties"].as_object().unwrap().keys().collect();
        // In the order the schema lists them, as JSON objects keep it.
        assert_eq!(properties, ["mermaid", "path", "title", "key"]);
        for name in names(ToolSelection::Full)
            .into_iter()
            .filter(|name| name != "kettle_show")
        {
            assert!(
                validate_tool_call(
                    ToolSelection::Display,
                    &json!({"name": name, "arguments": {}})
                )
                .is_err(),
                "display mode must refuse {name}"
            );
        }
        for selection in [ToolSelection::Display, ToolSelection::Full] {
            assert!(
                validate_tool_call(
                    selection,
                    &json!({"name": "kettle_show", "arguments": {"path": "/x.png"}})
                )
                .is_ok()
            );
        }
    }

    /// Whether `args` meets `schema`, for the part of JSON Schema
    /// `kettle_show`'s uses: an object of string properties with lengths in
    /// characters and its absolute-path pattern, `oneOf` of required names,
    /// and no other property.
    fn schema_accepts(schema: &Value, args: &Value) -> bool {
        let Some(object) = args.as_object() else {
            return false;
        };
        let properties = schema["properties"].as_object().unwrap();
        let fits = object.iter().all(|(name, value)| {
            let Some(property) = properties.get(name) else {
                return false;
            };
            let Some(text) = value.as_str() else {
                return false;
            };
            let length = text.chars().count() as u64;
            let pattern = match property["pattern"].as_str() {
                None => true,
                Some(pattern) => {
                    assert_eq!(pattern, ABSOLUTE_PATH_PATTERN);
                    let path = std::path::Path::new(text);
                    path.is_absolute() && (cfg!(windows) || text.starts_with('/'))
                }
            };
            property["minLength"]
                .as_u64()
                .is_none_or(|least| length >= least)
                && property["maxLength"]
                    .as_u64()
                    .is_none_or(|most| length <= most)
                && pattern
        });
        let matching = schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|branch| {
                branch["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|name| object.contains_key(name.as_str().unwrap()))
            })
            .count();
        fits && matching == 1
    }

    /// The schema says what the validator checks: a request the validator
    /// sends on is one the schema accepts, and for text of one byte per
    /// character the two agree exactly. Lengths are tried at each cap and
    /// one past it, in ASCII and in four-byte characters, which JSON Schema
    /// counts as one.
    #[test]
    fn the_schema_and_the_validator_agree() {
        use kettle_media::FailureCode;
        let schema = show_tool_spec()["inputSchema"].clone();
        let directory = kettle_test_support::private_tempdir("kettle-mcp-parity-");
        let file = directory.path().join("plot.png");
        std::fs::write(&file, b"png").unwrap();
        let file = file.to_str().unwrap().to_owned();
        // Refused before anything is sent, or sent on (and, with no Kettle
        // in a test, refused there for another reason).
        let sent_on = |args: &Value| {
            let result = call_tool(
                ToolSelection::Display,
                &json!({"name": "kettle_show", "arguments": args}),
            );
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            text != FailureCode::BadParams.model_message()
                && text != FailureCode::TooLarge.model_message()
        };
        let text = |unit: &str, count: usize| unit.repeat(count);
        let mut cases = vec![
            json!({}),
            json!({"mermaid": "graph LR", "path": file}),
            json!({"mermaid": ""}),
            json!({"path": ""}),
            json!({"path": "relative/plot.png"}),
            json!({"path": file, "title": ""}),
            json!({"path": file, "key": ""}),
            json!({"path": file, "pane": 1}),
            json!({"mermaid": 7}),
            json!({"mermaid": null}),
        ];
        let mermaid = kettle_media::MAX_MERMAID_BYTES;
        let title = kettle_ctl::show::MAX_SHOW_TITLE_BYTES;
        let key = kettle_ctl::show::MAX_SHOW_KEY_BYTES;
        for (unit, width) in [("m", 1), ("\u{1f600}", 4)] {
            for count in [mermaid / width, mermaid / width + 1] {
                cases.push(json!({"mermaid": text(unit, count)}));
            }
            for count in [title / width, title / width + 1] {
                cases.push(json!({"path": file, "title": text(unit, count)}));
            }
            for count in [key / width, key / width + 1] {
                cases.push(json!({"path": file, "key": text(unit, count)}));
            }
        }
        for args in &cases {
            let ascii = args.as_object().is_some_and(|object| {
                object
                    .values()
                    .all(|value| value.as_str().is_none_or(str::is_ascii))
            });
            let (schema_says, validator_says) = (schema_accepts(&schema, args), sent_on(args));
            assert!(
                schema_says || !validator_says,
                "the schema refuses what is sent: {args}"
            );
            if ascii {
                assert_eq!(schema_says, validator_says, "{args}");
            }
        }
    }

    /// Requests Kettle would refuse are refused here in its fixed wording,
    /// before anything is sent.
    #[test]
    fn kettle_show_refuses_bad_requests_in_fixed_wording() {
        use kettle_media::FailureCode;
        let show = |arguments: Value| {
            call_tool(
                ToolSelection::Display,
                &json!({"name": "kettle_show", "arguments": arguments}),
            )
        };
        let text = |result: &Value| result["content"][0]["text"].as_str().unwrap().to_owned();
        let directory = kettle_test_support::private_tempdir("kettle-mcp-show-");
        let file = directory.path().join("plot.png");
        std::fs::write(&file, b"png").unwrap();
        let file = file.to_str().unwrap();
        for (arguments, failure) in [
            (json!({}), FailureCode::BadParams),
            (json!({"path": "relative/plot.png"}), FailureCode::BadParams),
            (
                json!({"path": directory.path().join("gone.png").to_str().unwrap()}),
                FailureCode::FileNotFound,
            ),
            (
                json!({"path": directory.path().to_str().unwrap()}),
                FailureCode::FileNotRegular,
            ),
            (
                json!({"path": file, "key": "k".repeat(kettle_ctl::show::MAX_SHOW_KEY_BYTES + 1)}),
                FailureCode::TooLarge,
            ),
            (json!({"path": file, "title": ""}), FailureCode::BadParams),
            (json!({"path": file, "pane": 3}), FailureCode::BadParams),
            (json!({"path": file, "title": 7}), FailureCode::BadParams),
            (
                json!({"mermaid": "graph LR", "path": file}),
                FailureCode::BadParams,
            ),
            (json!({"mermaid": ""}), FailureCode::BadParams),
            (json!({"mermaid": null}), FailureCode::BadParams),
            (
                json!({"mermaid": "%".repeat(kettle_media::MAX_MERMAID_BYTES + 1)}),
                FailureCode::TooLarge,
            ),
        ] {
            let result = show(arguments.clone());
            assert_eq!(result["isError"], json!(true), "{arguments}");
            assert_eq!(text(&result), failure.model_message(), "{arguments}");
            assert_eq!(result["structuredContent"]["status"], "failed");
            assert_eq!(result["structuredContent"]["model_has_seen"], false);
        }
    }

    /// `kettle_card` is the hook's: known only to a display server, never
    /// listed, refused to a model's own call, and answered once.
    #[test]
    fn kettle_card_is_the_hooks_alone_and_answers_once() {
        use crate::mcp_display::{DisplaySession, TOOL_USE_ID_META};
        let listed = tool_specs(ToolSelection::Display);
        assert!(listed.iter().all(|spec| spec["name"] != "kettle_card"));
        let call = |meta: Value| json!({"name": "kettle_card", "arguments": {"tool_use_id": "toolu_1"}, "_meta": meta});
        assert!(validate_tool_call(ToolSelection::Display, &call(json!({}))).is_ok());
        assert!(validate_tool_call(ToolSelection::Full, &call(json!({}))).is_err());
        assert!(
            validate_tool_call(
                ToolSelection::Display,
                &json!({"name": "kettle_card", "arguments": {"tool_use_id": "toolu_1", "extra": 1}})
            )
            .is_ok(),
            "the envelope passes; the arguments are checked when it runs"
        );
        let session = DisplaySession::new(Some(crate::mcp_display::CardHook::Claude));
        let now = std::time::Instant::now();
        assert!(session.store("toolu_1".into(), "\nrows\ncaption".into(), now));
        let args = json!({"tool_use_id": "toolu_1"});
        let refused =
            tool_kettle_card(&args, &call(json!({TOOL_USE_ID_META: "toolu_9"})), &session);
        assert_eq!(refused["isError"], json!(true));
        // Codex's model calls carry its call id.
        let refused = tool_kettle_card(&args, &call(json!({"callId": "exec-9"})), &session);
        assert_eq!(refused["isError"], json!(true));
        let hook = tool_kettle_card(&args, &call(json!({})), &session);
        let output: Value =
            serde_json::from_str(hook["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(output, json!({"systemMessage": "\nrows\ncaption"}));
        let again = tool_kettle_card(&args, &call(json!({})), &session);
        assert_eq!(again["content"][0]["text"], "{}", "taken once");
        let missing = tool_kettle_card(&json!({}), &call(json!({})), &session);
        assert_eq!(missing["isError"], json!(true), "the id is required");
    }

    /// A card's text is the hook's, never the model's: the line says where
    /// the media shows, and nothing of the card is in the result.
    #[test]
    fn a_card_result_carries_none_of_the_cards_text() {
        let mut result = kettle_ctl::show::ShowResult::new(
            (4, true, 2),
            9,
            kettle_media::MediaKind::Raster,
            (640, 480),
            &[],
        );
        result.inline = Some(kettle_ctl::show::InlineDelivery {
            message: "\n\u{10eeee}SECRET-ROWS\nplot.png - raster 640x480".into(),
        });
        let sent = show_sent_card(&result, "plot.png");
        let encoded = sent.to_string();
        assert!(!encoded.contains("SECRET-ROWS") && !encoded.contains('\u{10eeee}'));
        let text = sent["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("below this call") && text.contains("plot.png"));
        assert!(text.contains("not seen"));
        assert_eq!(sent["structuredContent"]["delivery"], "card");
        assert_eq!(sent["structuredContent"]["model_has_seen"], false);
        assert!(!show_sent(&result).to_string().contains("SECRET-ROWS"));
    }

    /// The model is told where the media went and that it has not seen it;
    /// nothing of the media itself is in the result.
    #[test]
    fn kettle_show_results_carry_status_and_never_media() {
        let result = kettle_ctl::show::ShowResult {
            pane: 4,
            verified: false,
            window: 2,
            item: 9,
            kind: "svg".into(),
            width: 640,
            height: 480,
            warnings: vec!["font_fallback".into()],
            inline: None,
        };
        let sent = show_sent(&result);
        let text = sent["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("pane 4") && text.contains("svg 640x480"));
        assert!(text.contains("unverified") && text.contains("not seen"));
        assert_eq!(sent["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            sent["structuredContent"],
            json!({"status": "sent", "delivery": "shelf", "pane": 4, "window": 2, "item": 9,
                "verified": false, "kind": "svg", "width": 640, "height": 480,
                "warnings": ["font_fallback"], "model_has_seen": false})
        );
        assert!(sent.get("isError").is_none());
    }

    #[test]
    fn tool_specs_have_required_shape() {
        use std::collections::BTreeSet;

        let specs = tool_specs(ToolSelection::Full);
        assert!(specs.len() >= 8, "screenshot is part of the agent plane");
        for s in &specs {
            assert!(s["name"].is_string(), "tool missing name: {s}");
            assert!(s["description"].is_string(), "tool missing description");
            assert_eq!(s["inputSchema"]["type"], "object", "schema not an object");
            assert_eq!(s["inputSchema"]["additionalProperties"], false);

            let name = s["name"].as_str().expect("tool name");
            let schema_keys: BTreeSet<&str> = s["inputSchema"]["properties"]
                .as_object()
                .expect("schema properties")
                .keys()
                .map(String::as_str)
                .collect();
            let validator_keys: BTreeSet<&str> = tool_argument_fields(name)
                .expect("every listed tool has one argument declaration")
                .iter()
                .map(|(key, _)| *key)
                .collect();
            assert_eq!(
                schema_keys, validator_keys,
                "{name} schema and validator/ctl-forwarding keys drifted"
            );
        }
        let run = specs
            .iter()
            .find(|s| s["name"] == "kettle_run")
            .expect("kettle_run present");
        assert_eq!(run["inputSchema"]["required"][0], "command");

        let screenshot = specs
            .iter()
            .find(|spec| spec["name"] == "kettle_screenshot")
            .expect("kettle_screenshot present");
        let path_description = screenshot["inputSchema"]["properties"]["path"]["description"]
            .as_str()
            .expect("screenshot path description");
        assert!(
            path_description.contains("already-existing parent")
                && path_description.contains("never overwritten"),
            "the MCP schema must disclose the screenshot path's creation semantics"
        );
        assert_eq!(
            screenshot["inputSchema"]["properties"]["path"]["pattern"], "\\S",
            "schema-valid screenshot paths must contain a non-whitespace character"
        );
        for field in ["crop_x", "crop_y", "crop_width", "crop_height"] {
            assert_eq!(
                screenshot["inputSchema"]["properties"][field]["type"], "number",
                "{field} must remain available to privacy-preserving region captures"
            );
        }
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let r = call_tool(
            ToolSelection::Full,
            &json!({"name": "nope", "arguments": {}}),
        );
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
        let r = call_tool(
            ToolSelection::Full,
            &json!({"name": "kettle_run", "arguments": {"command": []}}),
        );
        assert_eq!(r["isError"], true);

        let r = call_tool(
            ToolSelection::Full,
            &json!({
                "name": "kettle_run",
                "arguments": {"command": ["echo", 1]},
            }),
        );
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
        let result = call_tool(
            ToolSelection::Full,
            &json!({"name":"kettle_list_panes","arguments":[],"extra":1}),
        );
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("object")
        );

        let result = call_tool(
            ToolSelection::Full,
            &json!({"name":"kettle_list_panes","arguments":[]}),
        );
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("object")
        );

        assert!(
            validate_tool_call(
                ToolSelection::Full,
                &json!({
                    "name":"kettle_list_panes",
                    "arguments":{},
                    "task": {"ttl": 1000},
                })
            )
            .is_ok()
        );
        assert!(
            validate_tool_call(
                ToolSelection::Full,
                &json!({
                    "name":"kettle_list_panes",
                    "arguments":null,
                })
            )
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

    /// Agent-plane tools validate their arguments BEFORE touching the control
    /// client, so a malformed call gets a crisp message even with no server
    /// running.
    #[test]
    fn send_keys_and_wait_for_validate_args_first() {
        let r = call_tool(
            ToolSelection::Full,
            &json!({"name": "kettle_send_keys", "arguments": {}}),
        );
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("keys"));

        let r = call_tool(
            ToolSelection::Full,
            &json!({"name": "kettle_send_keys", "arguments": {"keys": []}}),
        );
        assert_eq!(r["isError"], true);

        let r = call_tool(
            ToolSelection::Full,
            &json!({"name": "kettle_perform_action", "arguments": {}}),
        );
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("action"));

        let r = call_tool(
            ToolSelection::Full,
            &json!({"name": "kettle_wait_for", "arguments": {"timeout_ms": 5}}),
        );
        assert_eq!(r["isError"], true);
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("at least one of"),
            "wait_for must demand a condition"
        );

        assert!(
            validate_tool_arguments("kettle_wait_for", &json!({"quiet_ms": 300, "poll_ms": 50}))
                .is_ok(),
            "the MCP wait_for surface must accept the ctl server's poll_ms parameter"
        );
        let wait = tool_specs(ToolSelection::Full)
            .into_iter()
            .find(|spec| spec["name"] == "kettle_wait_for")
            .expect("kettle_wait_for spec");
        assert_eq!(wait["inputSchema"]["properties"]["poll_ms"]["minimum"], 50);
        assert_eq!(
            wait["inputSchema"]["properties"]["poll_ms"]["maximum"],
            5000
        );
    }

    #[test]
    fn ctl_tool_without_server_is_actionable_error() {
        // Exercise the deterministic discovery-failure formatter directly:
        // a developer may have a real Kettle server running beside this test.
        let r = ctl_discovery_error(&"no server");
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
