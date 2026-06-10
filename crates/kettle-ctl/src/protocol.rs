//! Cycle 925 (agent-first A2): the control-plane wire protocol.
//!
//! Newline-delimited JSON (one message per line). Versioned with a leading
//! `"v"` field; the compatibility policy is **additive only** — new fields may
//! be added, unknown fields are ignored, and a message whose `v` exceeds the
//! reader's supported version is rejected with `unsupported_version` rather
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

/// The only wire-protocol version this build speaks. A peer announcing a higher
/// `v` is rejected rather than guessed at.
pub const PROTOCOL_VERSION: u32 = 1;

/// Hard cap on a single NDJSON line (request or response), mirroring the
/// remote.cmd 1 MiB guard. A longer line is a protocol error + connection close.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

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
    /// `v` is newer than this build supports.
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// Unknown method name.
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// A required parameter was missing or the wrong type.
    pub const BAD_PARAMS: &str = "bad_params";
    /// The named pane does not exist (closed, or never existed).
    pub const NO_SUCH_PANE: &str = "no_such_pane";
    /// A mutating method was called on a read-only server.
    pub const READ_ONLY: &str = "read_only";
    /// A `run_command` is already pending on this pane.
    pub const BUSY: &str = "busy";
    /// Internal server error.
    pub const INTERNAL: &str = "internal";
}

/// Parse one NDJSON request line. Enforces the size cap + version policy so the
/// server never has to. Returns the request, or a ready-to-send error response
/// when the line is malformed / a wrong version (with `id` recovered when
/// possible so the client can still correlate the failure).
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
    if req.v > PROTOCOL_VERSION {
        return Err(Response::err(
            req.id,
            error_codes::UNSUPPORTED_VERSION,
            format!("protocol v{} > supported v{PROTOCOL_VERSION}", req.v),
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
