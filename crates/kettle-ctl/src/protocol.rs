//! The control-plane wire protocol.
//!
//! Newline-delimited JSON (one message per line). Versioned with a leading
//! `"v"` field; the compatibility policy is **additive only** — new fields may
//! be added, unknown fields are ignored, and a message whose `v` differs from
//! the reader's supported version is rejected with `unsupported_version` rather
//! than mis-parsed. Keeping `params`/`result` as free-form JSON values means a
//! new method needs no protocol-struct change — only a server handler + a
//! client call site.
//!
//! Shapes:
//! ```text
//! → {"v":1,"id":7,"method":"send_text","params":{"pane":3,"text":"ls\n"}}
//! ← {"v":1,"id":7,"ok":true,"result":{}}
//! ← {"v":1,"id":7,"ok":false,"error":{"code":"no_such_pane","message":"…"}}
//! ← {"v":1,"event":"output","pane":3,"data":"…","seq":42}   (subscribed only)
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only wire-protocol version this build speaks. Protocol v1 is not
/// backward-negotiated: every request, response, and event must carry exactly
/// this value.
pub const PROTOCOL_VERSION: u32 = 1;

/// Hard cap on a client request line. A longer line is a protocol error and the
/// connection is closed.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Hard cap on one server response/event line. This is intentionally smaller
/// than the request cap so a screen or cell read cannot amplify a small request
/// into an unbounded allocation in clients and MCP bridges.
pub const MAX_RESPONSE_LINE_BYTES: usize = 768 * 1024;

/// Maximum page size accepted by list/grid methods.
pub const MAX_PAGE_ITEMS: usize = 4096;

/// Default page size. Existing small results remain single-page while large
/// grids and multi-window sessions are bounded.
pub const DEFAULT_PAGE_ITEMS: usize = 1024;

/// Authorization class for a control method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Available in `agent-server=read-only` and `full`.
    Read,
    /// Requires `agent-server=full`.
    Mutate,
}

/// Thread on which a method is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    /// Forward to the UI thread, which owns application state.
    Ui,
    /// Run on the connection worker without blocking the UI thread.
    Connection,
}

/// Every control method supported by protocol v1. This enum is the single
/// source of truth for dispatch, privilege checks, and execution placement;
/// unknown wire strings are still reported as `unknown_method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    GetState,
    ListTabs,
    ListPanes,
    ReadScreen,
    ReadCells,
    UiGeometry,
    Screenshot,
    Subscribe,
    SendText,
    SendKeys,
    DispatchUiKey,
    DispatchKeybind,
    SendMouse,
    ResizeWindow,
    PerformAction,
    RunCommand,
    WaitFor,
}

impl Method {
    /// Complete method table, used by tests and diagnostics.
    pub const ALL: [Self; 17] = [
        Self::GetState,
        Self::ListTabs,
        Self::ListPanes,
        Self::ReadScreen,
        Self::ReadCells,
        Self::UiGeometry,
        Self::Screenshot,
        Self::Subscribe,
        Self::SendText,
        Self::SendKeys,
        Self::DispatchUiKey,
        Self::DispatchKeybind,
        Self::SendMouse,
        Self::ResizeWindow,
        Self::PerformAction,
        Self::RunCommand,
        Self::WaitFor,
    ];

    /// Parse the stable protocol-v1 method spelling.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "get_state" => Self::GetState,
            "list_tabs" => Self::ListTabs,
            "list_panes" => Self::ListPanes,
            "read_screen" => Self::ReadScreen,
            "read_cells" => Self::ReadCells,
            "ui_geometry" => Self::UiGeometry,
            "screenshot" => Self::Screenshot,
            "subscribe" => Self::Subscribe,
            "send_text" => Self::SendText,
            "send_keys" => Self::SendKeys,
            "dispatch_ui_key" => Self::DispatchUiKey,
            "dispatch_keybind" => Self::DispatchKeybind,
            "send_mouse" => Self::SendMouse,
            "resize_window" => Self::ResizeWindow,
            "perform_action" => Self::PerformAction,
            "run_command" => Self::RunCommand,
            "wait_for" => Self::WaitFor,
            _ => return None,
        })
    }

    /// Stable wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GetState => "get_state",
            Self::ListTabs => "list_tabs",
            Self::ListPanes => "list_panes",
            Self::ReadScreen => "read_screen",
            Self::ReadCells => "read_cells",
            Self::UiGeometry => "ui_geometry",
            Self::Screenshot => "screenshot",
            Self::Subscribe => "subscribe",
            Self::SendText => "send_text",
            Self::SendKeys => "send_keys",
            Self::DispatchUiKey => "dispatch_ui_key",
            Self::DispatchKeybind => "dispatch_keybind",
            Self::SendMouse => "send_mouse",
            Self::ResizeWindow => "resize_window",
            Self::PerformAction => "perform_action",
            Self::RunCommand => "run_command",
            Self::WaitFor => "wait_for",
        }
    }

    /// Privilege required by this method.
    pub const fn capability(self) -> Capability {
        match self {
            Self::Screenshot
            | Self::SendText
            | Self::SendKeys
            | Self::DispatchUiKey
            | Self::DispatchKeybind
            | Self::SendMouse
            | Self::ResizeWindow
            | Self::PerformAction
            | Self::RunCommand => Capability::Mutate,
            _ => Capability::Read,
        }
    }

    /// Execution placement for this method.
    pub const fn execution(self) -> Execution {
        match self {
            Self::WaitFor => Execution::Connection,
            _ => Execution::Ui,
        }
    }
}

/// Parsed additive paging controls. Cursors are decimal item offsets so v1
/// clients can persist and inspect them without a binary codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub offset: usize,
    pub limit: usize,
    pub snapshot: Option<String>,
}

impl PageRequest {
    /// Parse optional `cursor`, `limit`, and `snapshot` fields.
    pub fn from_params(params: &Value) -> Result<Self, &'static str> {
        let offset = match params.get("cursor") {
            None | Some(Value::Null) => 0,
            Some(Value::String(cursor)) => cursor
                .parse::<usize>()
                .map_err(|_| "cursor must be a decimal item offset")?,
            _ => return Err("cursor must be a string"),
        };
        let limit = match params.get("limit") {
            None | Some(Value::Null) => DEFAULT_PAGE_ITEMS,
            Some(value) => value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n > 0)
                .ok_or("limit must be a positive integer")?
                .min(MAX_PAGE_ITEMS),
        };
        let snapshot = match params.get("snapshot") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() && value.len() <= 128 => {
                Some(value.clone())
            }
            Some(Value::String(_)) => return Err("snapshot must be 1..=128 bytes"),
            _ => return Err("snapshot must be a string"),
        };
        if offset > 0 && snapshot.is_none() {
            return Err("snapshot is required when cursor is non-zero");
        }
        Ok(Self {
            offset,
            limit,
            snapshot,
        })
    }

    /// End offset and additive continuation metadata for a collection.
    pub fn bounds(&self, total: usize) -> (usize, usize, Option<String>, bool) {
        let start = self.offset.min(total);
        let end = start.saturating_add(self.limit).min(total);
        let truncated = end < total;
        let next = truncated.then(|| end.to_string());
        (start, end, next, truncated)
    }
}

/// A client→server request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version (must be `PROTOCOL_VERSION`).
    pub v: u32,
    /// Monotonic per-connection correlation id; the matching response echoes it.
    pub id: u64,
    /// Method name (e.g. `list_panes`, `run_command`).
    pub method: String,
    /// Method parameters; shape is method-specific. Absent → `Null`.
    #[serde(default)]
    pub params: Value,
}

/// A server→client response (correlated to a `Request` by `id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    /// Present when `ok` — the method result (shape is method-specific).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    /// Present when `!ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// A structured error payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Stable machine-readable code (see [`error_codes`]).
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

/// A server→client event (only after `subscribe`). Distinguished from a
/// `Response` on the wire by the presence of an `event` field instead of `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    /// Event kind: `output`, `command_finished`, `pane_open`, `pane_close`,
    /// `pane_focus`, `title`, `agent_attached`, `lag`.
    pub event: String,
    /// The pane this event concerns, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<u64>,
    /// Event payload (shape is kind-specific).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl Response {
    /// A success response carrying `result`.
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            ok: true,
            result,
            error: None,
        }
    }

    /// An error response.
    pub fn err(id: u64, code: &str, message: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            ok: false,
            result: Value::Null,
            error: Some(RpcError {
                code: code.to_string(),
                message: message.into(),
            }),
        }
    }
}

impl Event {
    /// Build an event of `kind` for `pane` (or `None`) carrying `data`.
    pub fn new(kind: &str, pane: Option<u64>, data: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            event: kind.to_string(),
            pane,
            data,
        }
    }
}

/// Stable error codes (the `code` field of [`RpcError`]).
pub mod error_codes {
    /// The request line was not valid JSON / not a `Request`.
    pub const BAD_REQUEST: &str = "bad_request";
    /// `v` differs from the only version this build supports.
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// Unknown method name.
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// A required parameter was missing or the wrong type.
    pub const BAD_PARAMS: &str = "bad_params";
    /// The named pane does not exist (closed, or never existed).
    pub const NO_SUCH_PANE: &str = "no_such_pane";
    /// A mutating method was called on a read-only server, or the
    /// target pane was toggled read-only by the user (right-click "Read only"
    /// / `toggle_read_only`). The error message distinguishes the two; pane
    /// state is also visible as the `read_only` field in `list_panes`.
    pub const READ_ONLY: &str = "read_only";
    /// A `run_command` is already pending on this pane.
    pub const BUSY: &str = "busy";
    /// Internal server error.
    pub const INTERNAL: &str = "internal";
    /// A handler result could not be represented within the response budget.
    pub const RESPONSE_TOO_LARGE: &str = "response_too_large";
    /// A paged live-state read continued after the underlying snapshot changed.
    pub const STALE_SNAPSHOT: &str = "stale_snapshot";
}

/// Parse one ALREADY-BUFFERED NDJSON request line, validating the size cap +
/// version policy. Returns the request, or a ready-to-send error response when
/// the line is malformed / over-cap / a wrong version (with `id` recovered when
/// possible so the client can still correlate the failure).
///
/// NOTE: this can only validate a line the caller has already assembled — it
/// cannot bound the *assembly* itself. Every reader (the server's
/// `connection_loop`, the client's `read_capped_line`) MUST therefore also
/// enforce `MAX_LINE_BYTES` incrementally while reading bytes, so a peer that
/// never sends a newline can't grow the read buffer without bound. This
/// post-hoc check is the second line of defense, not the only one.
pub fn parse_request_line(line: &str) -> Result<Request, Response> {
    if line.len() > MAX_LINE_BYTES {
        return Err(Response::err(
            0,
            error_codes::BAD_REQUEST,
            "request line exceeds 1 MiB",
        ));
    }
    let req: Request = serde_json::from_str(line).map_err(|e| {
        // Best-effort id recovery so a parse failure can still correlate.
        let id = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
            .unwrap_or(0);
        Response::err(
            id,
            error_codes::BAD_REQUEST,
            format!("invalid request: {e}"),
        )
    })?;
    if req.v != PROTOCOL_VERSION {
        return Err(Response::err(
            req.id,
            error_codes::UNSUPPORTED_VERSION,
            format!(
                "protocol v{} is unsupported; expected v{PROTOCOL_VERSION}",
                req.v
            ),
        ));
    }
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let line = r#"{"v":1,"id":7,"method":"send_text","params":{"pane":3,"text":"ls\n"}}"#;
        let req = parse_request_line(line).expect("valid");
        assert_eq!(req.id, 7);
        assert_eq!(req.method, "send_text");
        assert_eq!(req.params["pane"], 3);
        assert_eq!(req.params["text"], "ls\n");
    }

    #[test]
    fn missing_params_defaults_to_null() {
        let req = parse_request_line(r#"{"v":1,"id":1,"method":"get_state"}"#).expect("valid");
        assert!(req.params.is_null());
    }

    #[test]
    fn newer_version_is_rejected_with_correlatable_id() {
        let err = parse_request_line(r#"{"v":99,"id":42,"method":"x"}"#).unwrap_err();
        assert_eq!(err.id, 42, "id recovered so the client can correlate");
        assert_eq!(err.error.unwrap().code, error_codes::UNSUPPORTED_VERSION);
    }

    #[test]
    fn older_version_is_also_rejected() {
        let err = parse_request_line(r#"{"v":0,"id":9,"method":"x"}"#).unwrap_err();
        assert_eq!(err.id, 9);
        assert_eq!(err.error.unwrap().code, error_codes::UNSUPPORTED_VERSION);
    }

    #[test]
    fn method_table_is_unique_and_classified() {
        let mut names = std::collections::HashSet::new();
        for method in Method::ALL {
            assert!(names.insert(method.as_str()));
            assert_eq!(Method::from_name(method.as_str()), Some(method));
            let _ = method.capability();
            let _ = method.execution();
        }
        assert_eq!(Method::Screenshot.capability(), Capability::Mutate);
    }

    #[test]
    fn paging_is_bounded_and_validated() {
        let page = PageRequest::from_params(&serde_json::json!({
            "cursor": "10",
            "limit": MAX_PAGE_ITEMS + 10,
            "snapshot": "abc",
        }))
        .unwrap();
        assert_eq!(page.offset, 10);
        assert_eq!(page.limit, MAX_PAGE_ITEMS);
        assert_eq!(page.bounds(20), (10, 20, None, false));
        assert!(PageRequest::from_params(&serde_json::json!({"cursor": -1})).is_err());
        assert!(PageRequest::from_params(&serde_json::json!({"limit": 0})).is_err());
        assert!(PageRequest::from_params(&serde_json::json!({"cursor": "1"})).is_err());
    }

    #[test]
    fn garbage_line_is_bad_request_not_panic() {
        let err = parse_request_line("not json at all").unwrap_err();
        assert!(!err.ok);
        assert_eq!(err.error.unwrap().code, error_codes::BAD_REQUEST);
    }

    #[test]
    fn oversize_line_is_rejected() {
        let big = format!(
            r#"{{"v":1,"id":1,"method":"x","params":"{}"}}"#,
            "a".repeat(MAX_LINE_BYTES)
        );
        let err = parse_request_line(&big).unwrap_err();
        assert_eq!(err.error.unwrap().code, error_codes::BAD_REQUEST);
    }

    #[test]
    fn response_serializes_without_null_noise() {
        let ok = Response::ok(1, serde_json::json!({"a":1}));
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains(r#""ok":true"#));
        assert!(!s.contains("error"), "ok response omits error: {s}");

        let err = Response::err(2, error_codes::NO_SUCH_PANE, "gone");
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains(r#""ok":false"#));
        assert!(!s.contains("result"), "err response omits result: {s}");
        assert!(s.contains("no_such_pane"));
    }

    #[test]
    fn event_serializes_with_kind() {
        let ev = Event::new("output", Some(3), serde_json::json!("hi"));
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains(r#""event":"output""#));
        assert!(s.contains(r#""pane":3"#));
    }
}
