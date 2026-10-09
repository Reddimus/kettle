//! `show`: media sent to the user's media shelf in the Kettle a caller runs
//! in. One parser serves every caller: `kettle show` and the MCP display tool
//! build a [`ShowRequest`] and send [`ShowRequest::into_params`], and the
//! server parses those params once, after admission, into owned parts, so an
//! inline source moves into its render job without a copy.
//!
//! The media itself is classified by the media worker, from its bytes: a
//! file's name, and the caller's word for it, decide nothing.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use kettle_media::{
    ExternalAttested, FailureCode, JobKind, MAX_RASTER_BYTES, MAX_SVG_BYTES, MediaKind, NativePath,
    ValidationError, Warning,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::{PROTOCOL_VERSION, Response, RpcError};

/// How long the server gives one `show` from its admission, queueing and
/// rendering included.
pub const SHOW_SERVER_DEADLINE: Duration = Duration::from_secs(15);
/// How long a client waits for a `show` reply: the server's deadline and a
/// margin for its reply to arrive.
pub const SHOW_CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest replacement key.
pub const MAX_SHOW_KEY_BYTES: usize = 256;
/// Longest title, before it is sanitized and fitted for display.
pub const MAX_SHOW_TITLE_BYTES: usize = 4 * 1024;
/// Longest inline card message, in UTF-16 units: Claude Code turns a longer
/// hook message into a stub instead of printing it.
pub const MAX_INLINE_MESSAGE_UTF16: usize = 9_900;

/// An inline card a harness's adapter asks for along with the shelf item.
/// Kettle decides whether the caller may have one; asking grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineTarget {
    /// Claude Code: a card printed by a synchronous hook under the call.
    ClaudeHook,
    /// Codex: a card printed by a `PostToolUse` hook under the call.
    CodexHook,
}

impl InlineTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeHook => "claude_hook",
            Self::CodexHook => "codex_hook",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "claude_hook" => Some(Self::ClaudeHook),
            "codex_hook" => Some(Self::CodexHook),
            _ => None,
        }
    }
}

/// An inline card's delivery, for the adapter that asked and never for the
/// model: the message its harness prints once, as Kettle wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineDelivery {
    pub message: String,
}

impl InlineDelivery {
    /// Whether the message fits what its harness prints whole.
    pub fn fits(&self) -> bool {
        self.message.encode_utf16().count() <= MAX_INLINE_MESSAGE_UTF16
    }
}

/// What to show.
#[derive(Debug, Clone, PartialEq)]
pub enum ShowSource {
    /// SVG text, rendered as SVG.
    Svg(String),
    /// Image bytes, classified by the worker.
    Image(Vec<u8>),
    /// An absolute path the caller attests by device and inode, classified by
    /// the worker from the file it opens.
    File {
        path: NativePath,
        attestation: ExternalAttested,
    },
}

impl ShowSource {
    /// The job kind this source renders as.
    pub fn job_kind(&self) -> JobKind {
        match self {
            Self::Svg(_) => JobKind::Svg,
            Self::Image(_) | Self::File { .. } => JobKind::Auto,
        }
    }
}

/// One parsed `show` request.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowRequest {
    pub source: ShowSource,
    /// Shown above the media, after sanitizing.
    pub title: Option<String>,
    /// A later `show` with the same key, in the same pane, replaces this
    /// item. A file's key defaults to its path.
    pub key: Option<String>,
    /// The pane to show it in. Honored only for full control; any other
    /// caller's pane comes from its process ancestry.
    pub pane: Option<u64>,
    /// An inline card asked for by a harness's adapter.
    pub inline: Option<InlineTarget>,
}

impl ShowRequest {
    /// Parse `show` params, consuming them. Unknown fields are ignored, so
    /// later additions stay compatible.
    pub fn parse(params: Value) -> Result<Self, FailureCode> {
        let mut fields = match params {
            Value::Object(fields) => fields,
            _ => return Err(FailureCode::BadParams),
        };
        let mut sources = ["svg", "image_b64", "path"]
            .into_iter()
            .filter(|name| fields.get(*name).is_some_and(|value| !value.is_null()));
        let name = sources.next().ok_or(FailureCode::BadParams)?;
        if sources.next().is_some() {
            return Err(FailureCode::BadParams);
        }
        let Some(Value::String(text)) = fields.remove(name) else {
            return Err(FailureCode::BadParams);
        };
        if text.is_empty() {
            return Err(FailureCode::BadParams);
        }
        let attestation = (
            optional_u64(&mut fields, "dev")?,
            optional_u64(&mut fields, "ino")?,
        );
        let source = match (name, attestation) {
            ("svg", (None, None)) if text.len() > MAX_SVG_BYTES => {
                return Err(FailureCode::TooLarge);
            }
            ("svg", (None, None)) => ShowSource::Svg(text),
            ("image_b64", (None, None)) => {
                // The size is checked before anything is decoded: first the
                // longest encoding of the cap, then the exact decoded size.
                if text.len() > MAX_RASTER_BYTES.div_ceil(3) * 4
                    || decoded_len(&text)? > MAX_RASTER_BYTES
                {
                    return Err(FailureCode::TooLarge);
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map_err(|_| FailureCode::BadParams)?;
                if bytes.len() > MAX_RASTER_BYTES {
                    return Err(FailureCode::TooLarge);
                }
                ShowSource::Image(bytes)
            }
            ("path", (Some(dev), Some(ino))) => {
                // `NativePath` refuses a relative path on every platform.
                let path =
                    NativePath::from_path(Path::new(&text)).map_err(|error| match error {
                        ValidationError::TooLarge => FailureCode::TooLarge,
                        _ => FailureCode::BadParams,
                    })?;
                ShowSource::File {
                    path,
                    attestation: ExternalAttested { dev, ino },
                }
            }
            _ => return Err(FailureCode::BadParams),
        };
        let title = optional_text(&mut fields, "title", MAX_SHOW_TITLE_BYTES)?;
        let key = optional_text(&mut fields, "key", MAX_SHOW_KEY_BYTES)?;
        let pane = match optional_u64(&mut fields, "pane")? {
            Some(0) => return Err(FailureCode::BadParams),
            pane => pane,
        };
        let inline = match fields.remove("inline") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => {
                Some(InlineTarget::parse(&text).ok_or(FailureCode::BadParams)?)
            }
            Some(_) => return Err(FailureCode::BadParams),
        };
        Ok(Self {
            source,
            title,
            key,
            pane,
            inline,
        })
    }

    /// The params [`ShowRequest::parse`] reads back as this request. A
    /// request it would refuse is refused here, with the same code; so is a
    /// file path that is not UTF-8, which has no JSON spelling.
    pub fn into_params(self) -> Result<Value, FailureCode> {
        self.check()?;
        let mut fields = Map::new();
        match self.source {
            ShowSource::Svg(text) => {
                fields.insert("svg".into(), Value::String(text));
            }
            ShowSource::Image(bytes) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                fields.insert("image_b64".into(), Value::String(encoded));
            }
            ShowSource::File { path, attestation } => {
                let text = native_path_text(&path).ok_or(FailureCode::BadParams)?;
                fields.insert("path".into(), Value::String(text));
                fields.insert("dev".into(), attestation.dev.into());
                fields.insert("ino".into(), attestation.ino.into());
            }
        }
        if let Some(title) = self.title {
            fields.insert("title".into(), Value::String(title));
        }
        if let Some(key) = self.key {
            fields.insert("key".into(), Value::String(key));
        }
        if let Some(pane) = self.pane {
            fields.insert("pane".into(), pane.into());
        }
        if let Some(inline) = self.inline {
            fields.insert("inline".into(), inline.as_str().into());
        }
        Ok(Value::Object(fields))
    }

    /// What [`ShowRequest::parse`] checks, for a request built in code.
    fn check(&self) -> Result<(), FailureCode> {
        let (empty, over) = match &self.source {
            ShowSource::Svg(text) => (text.is_empty(), text.len() > MAX_SVG_BYTES),
            ShowSource::Image(bytes) => (bytes.is_empty(), bytes.len() > MAX_RASTER_BYTES),
            ShowSource::File { .. } => (false, false),
        };
        let text = |text: &Option<String>, cap: usize| match text {
            Some(text) if text.len() > cap => Err(FailureCode::TooLarge),
            Some(text) if text.is_empty() => Err(FailureCode::BadParams),
            _ => Ok(()),
        };
        // In the order `parse` checks, so both name the same failure.
        if empty {
            return Err(FailureCode::BadParams);
        }
        if over {
            return Err(FailureCode::TooLarge);
        }
        text(&self.title, MAX_SHOW_TITLE_BYTES)?;
        text(&self.key, MAX_SHOW_KEY_BYTES)?;
        if self.pane == Some(0) {
            return Err(FailureCode::BadParams);
        }
        Ok(())
    }
}

/// How many bytes padded standard base64 `text` decodes to, from its length
/// and padding alone. A length that is not a whole number of four-character
/// groups is not padded standard base64.
fn decoded_len(text: &str) -> Result<usize, FailureCode> {
    if !text.len().is_multiple_of(4) {
        return Err(FailureCode::BadParams);
    }
    let padding = text
        .bytes()
        .rev()
        .take(2)
        .take_while(|byte| *byte == b'=')
        .count();
    Ok(text.len() / 4 * 3 - padding)
}

/// A native path's text, when it has one.
fn native_path_text(path: &NativePath) -> Option<String> {
    #[cfg(windows)]
    {
        let (units, rest) = path.as_bytes().as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        char::decode_utf16(units.iter().map(|unit| u16::from_le_bytes(*unit)))
            .collect::<Result<String, _>>()
            .ok()
    }
    #[cfg(not(windows))]
    {
        String::from_utf8(path.as_bytes().to_vec()).ok()
    }
}

fn optional_u64(fields: &mut Map<String, Value>, name: &str) -> Result<Option<u64>, FailureCode> {
    match fields.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(FailureCode::BadParams),
    }
}

/// An optional nonempty string of at most `cap` bytes.
fn optional_text(
    fields: &mut Map<String, Value>,
    name: &str,
    cap: usize,
) -> Result<Option<String>, FailureCode> {
    match fields.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() > cap => Err(FailureCode::TooLarge),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text)),
        Some(_) => Err(FailureCode::BadParams),
    }
}

/// A successful `show`: where the item went and what it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowResult {
    pub pane: u64,
    /// Whether the pane is the caller's own, by its process ancestry. An
    /// unverified item was routed by full control's choice or by the
    /// caller's environment, and its shelf entry names the sender.
    pub verified: bool,
    pub window: u64,
    /// The shelf item, stable while it stays on the shelf.
    pub item: u64,
    /// What the media turned out to be: `raster` or `svg`.
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub warnings: Vec<String>,
    /// The inline card's delivery, when the adapter asked for one and Kettle
    /// registered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<InlineDelivery>,
}

impl ShowResult {
    pub fn new(
        route: (u64, bool, u64),
        item: u64,
        kind: MediaKind,
        size: (u32, u32),
        warnings: &[Warning],
    ) -> Self {
        let (pane, verified, window) = route;
        Self {
            pane,
            verified,
            window,
            item,
            kind: kind.as_str().into(),
            width: size.0,
            height: size.1,
            warnings: warnings
                .iter()
                .map(|warning| warning.as_str().into())
                .collect(),
            inline: None,
        }
    }
}

/// The error reply for a refused or failed `show`: the fixed code, its
/// fixed reason when it has one, and wording that names no path, source or
/// identifier.
pub fn show_failure(id: u64, failure: FailureCode) -> Response {
    Response {
        v: PROTOCOL_VERSION,
        id,
        ok: false,
        result: Value::Null,
        error: Some(RpcError {
            code: failure.code().into(),
            message: failure.model_message().into(),
            reason: failure.reason().map(Into::into),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn absolute(name: &str) -> String {
        if cfg!(windows) {
            format!("C:\\media\\{name}")
        } else {
            format!("/media/{name}")
        }
    }

    #[test]
    fn exactly_one_nonempty_source() {
        for params in [
            json!(null),
            json!([]),
            json!({}),
            json!({"svg": null}),
            json!({"svg": ""}),
            json!({"svg": 7}),
            json!({"svg": "<svg/>", "image_b64": "AA=="}),
            json!({"image_b64": "AA==", "path": absolute("a.png"), "dev": 1, "ino": 2}),
            json!({"mermaid": "graph LR"}),
        ] {
            assert_eq!(
                ShowRequest::parse(params.clone()),
                Err(FailureCode::BadParams),
                "{params}"
            );
        }
    }

    #[test]
    fn options_are_bounded_and_typed() {
        let svg = |extra: Value| {
            let mut params = json!({"svg": "<svg/>"});
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            ShowRequest::parse(params)
        };
        let request =
            svg(json!({"title": "Plot", "key": "plot", "pane": 3, "later": true})).unwrap();
        assert_eq!(request.title.as_deref(), Some("Plot"));
        assert_eq!(request.key.as_deref(), Some("plot"));
        assert_eq!(request.pane, Some(3));
        for bad in [
            json!({"pane": 0}),
            json!({"pane": -1}),
            json!({"pane": "3"}),
            json!({"key": ""}),
            json!({"key": 5}),
            json!({"title": ""}),
            json!({"dev": 1, "ino": 2}),
        ] {
            assert_eq!(svg(bad.clone()), Err(FailureCode::BadParams), "{bad}");
        }
        assert!(svg(json!({"key": "k".repeat(MAX_SHOW_KEY_BYTES)})).is_ok());
        assert_eq!(
            svg(json!({"key": "k".repeat(MAX_SHOW_KEY_BYTES + 1)})),
            Err(FailureCode::TooLarge)
        );
        assert!(svg(json!({"title": "t".repeat(MAX_SHOW_TITLE_BYTES)})).is_ok());
        assert_eq!(
            svg(json!({"title": "t".repeat(MAX_SHOW_TITLE_BYTES + 1)})),
            Err(FailureCode::TooLarge)
        );
    }

    #[test]
    fn inline_svg_moves_and_is_refused_over_its_cap() {
        let text = format!("<svg>{}</svg>", " ".repeat(MAX_SVG_BYTES - 11));
        assert_eq!(text.len(), MAX_SVG_BYTES);
        let pointer = text.as_ptr();
        let mut fields = Map::new();
        fields.insert("svg".into(), Value::String(text));
        let ShowSource::Svg(moved) = ShowRequest::parse(Value::Object(fields)).unwrap().source
        else {
            panic!("an SVG source");
        };
        assert_eq!(moved.as_ptr(), pointer);
        assert_eq!(
            ShowRequest::parse(json!({"svg": "x".repeat(MAX_SVG_BYTES + 1)})),
            Err(FailureCode::TooLarge)
        );
    }

    #[test]
    fn image_bytes_are_standard_base64_and_worker_classified() {
        let bytes = vec![0, 255, 128, 1, 2];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let request = ShowRequest::parse(json!({"image_b64": encoded})).unwrap();
        assert_eq!(request.source.job_kind(), JobKind::Auto);
        assert_eq!(request.source, ShowSource::Image(bytes));
        // URL-safe alphabet and missing padding are not standard base64.
        for bad in ["_w==", "AAA"] {
            assert_eq!(
                ShowRequest::parse(json!({"image_b64": bad})),
                Err(FailureCode::BadParams)
            );
        }
        // Longer than any encoding of the cap, whatever its padding.
        assert_eq!(
            ShowRequest::parse(
                json!({"image_b64": "A".repeat(MAX_RASTER_BYTES.div_ceil(3) * 4 + 1)})
            ),
            Err(FailureCode::TooLarge)
        );
        // One byte over the cap is refused from its length, before decoding:
        // 44,739,244 characters with no padding are 33,554,433 bytes.
        let mut over = "A".repeat(MAX_RASTER_BYTES.div_ceil(3) * 4);
        assert_eq!(over.len() / 4 * 3, MAX_RASTER_BYTES + 1);
        assert_eq!(
            ShowRequest::parse(json!({"image_b64": over.clone()})),
            Err(FailureCode::TooLarge)
        );
        // One `=` makes it exactly the cap, which passes and decodes.
        over.replace_range(over.len() - 1.., "=");
        let at_cap = ShowRequest::parse(json!({"image_b64": over}));
        assert!(
            matches!(&at_cap, Ok(ShowRequest { source: ShowSource::Image(bytes), .. }) if bytes.len() == MAX_RASTER_BYTES),
            "exactly the cap is accepted"
        );
    }

    #[test]
    fn decoded_lengths_come_from_length_and_padding() {
        for (text, len) in [
            ("", 0),
            ("AAAA", 3),
            ("AAA=", 2),
            ("AA==", 1),
            ("AAAAAA==", 4),
        ] {
            assert_eq!(decoded_len(text), Ok(len), "{text}");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .unwrap()
                    .len(),
                len
            );
        }
        // `=` only counts at the end.
        assert_eq!(decoded_len("A=AA"), Ok(3));
        for bad in ["A", "AAAAA", "AA="] {
            assert_eq!(decoded_len(bad), Err(FailureCode::BadParams), "{bad}");
        }
    }

    #[test]
    fn a_path_is_absolute_and_attested() {
        let path = absolute("diagram.bin");
        let request = ShowRequest::parse(json!({"path": path, "dev": 0, "ino": 5})).unwrap();
        assert_eq!(request.source.job_kind(), JobKind::Auto);
        let ShowSource::File {
            path: native,
            attestation,
        } = &request.source
        else {
            panic!("a file source");
        };
        assert_eq!(*attestation, ExternalAttested { dev: 0, ino: 5 });
        assert_eq!(native, &NativePath::from_path(Path::new(&path)).unwrap());
        for bad in [
            json!({"path": path, "dev": 0}),
            json!({"path": path, "ino": 5}),
            json!({"path": path, "dev": -1, "ino": 5}),
            json!({"path": "relative/diagram.bin", "dev": 0, "ino": 5}),
        ] {
            assert_eq!(
                ShowRequest::parse(bad.clone()),
                Err(FailureCode::BadParams),
                "{bad}"
            );
        }
        let long = absolute(&"n".repeat(kettle_media::MAX_PATH_BYTES));
        assert_eq!(
            ShowRequest::parse(json!({"path": long, "dev": 0, "ino": 5})),
            Err(FailureCode::TooLarge)
        );
    }

    #[test]
    fn params_round_trip_through_the_one_parser() {
        let path = absolute("caf\u{e9}.png");
        for request in [
            ShowRequest {
                source: ShowSource::Svg("<svg/>".into()),
                title: Some("Plot".into()),
                key: Some("plot".into()),
                pane: Some(9),
                inline: Some(InlineTarget::ClaudeHook),
            },
            ShowRequest {
                source: ShowSource::Image(vec![0, 1, 254, 255]),
                title: None,
                key: None,
                pane: None,
                inline: None,
            },
            ShowRequest {
                source: ShowSource::File {
                    path: NativePath::from_path(Path::new(&path)).unwrap(),
                    attestation: ExternalAttested {
                        dev: u64::MAX,
                        ino: 1,
                    },
                },
                title: None,
                key: Some(path.clone()),
                pane: None,
                inline: None,
            },
        ] {
            let params = request.clone().into_params().unwrap();
            assert_eq!(ShowRequest::parse(params).unwrap(), request);
        }
    }

    /// An inline card is asked for by name; any other value is refused.
    #[test]
    fn an_inline_card_is_asked_for_by_name() {
        let parse = |inline: Value| {
            ShowRequest::parse(serde_json::json!({"svg": "<svg/>", "inline": inline}))
                .map(|request| request.inline)
        };
        assert_eq!(parse(Value::Null), Ok(None));
        assert_eq!(
            parse("claude_hook".into()),
            Ok(Some(InlineTarget::ClaudeHook))
        );
        assert_eq!(
            parse("codex_hook".into()),
            Ok(Some(InlineTarget::CodexHook))
        );
        assert_eq!(parse("codex".into()), Err(FailureCode::BadParams));
        assert_eq!(
            parse(serde_json::json!({"target": "claude_hook"})),
            Err(FailureCode::BadParams)
        );
    }

    /// A result carries an inline delivery only when there is one, so an
    /// ordinary reply is unchanged, and the delivery keeps to what its
    /// harness prints whole.
    #[test]
    fn an_inline_delivery_is_private_to_the_reply_that_has_one() {
        let result = ShowResult::new((1, true, 1), 2, MediaKind::Raster, (64, 48), &[]);
        let plain = serde_json::to_value(&result).unwrap();
        assert!(plain.get("inline").is_none());
        let parsed: ShowResult = serde_json::from_value(plain).unwrap();
        assert_eq!(parsed.inline, None);
        let delivery = InlineDelivery {
            message: "x".repeat(MAX_INLINE_MESSAGE_UTF16),
        };
        assert!(delivery.fits());
        // A cell's placeholder is two UTF-16 units.
        let over = InlineDelivery {
            message: "\u{10eeee}".repeat(MAX_INLINE_MESSAGE_UTF16 / 2 + 1),
        };
        assert!(!over.fits());
    }

    /// A request built in code is checked as a parsed one is.
    #[test]
    fn into_params_refuses_what_parse_would() {
        let base = || ShowRequest {
            source: ShowSource::Svg("<svg/>".into()),
            title: None,
            key: None,
            pane: None,
            inline: None,
        };
        for (request, failure) in [
            (
                ShowRequest {
                    source: ShowSource::Svg(String::new()),
                    ..base()
                },
                FailureCode::BadParams,
            ),
            (
                ShowRequest {
                    source: ShowSource::Image(Vec::new()),
                    ..base()
                },
                FailureCode::BadParams,
            ),
            (
                ShowRequest {
                    pane: Some(0),
                    ..base()
                },
                FailureCode::BadParams,
            ),
            (
                ShowRequest {
                    key: Some(String::new()),
                    ..base()
                },
                FailureCode::BadParams,
            ),
            (
                ShowRequest {
                    key: Some("k".repeat(MAX_SHOW_KEY_BYTES + 1)),
                    ..base()
                },
                FailureCode::TooLarge,
            ),
            (
                ShowRequest {
                    title: Some("t".repeat(MAX_SHOW_TITLE_BYTES + 1)),
                    ..base()
                },
                FailureCode::TooLarge,
            ),
            (
                ShowRequest {
                    source: ShowSource::Svg("x".repeat(MAX_SVG_BYTES + 1)),
                    ..base()
                },
                FailureCode::TooLarge,
            ),
            // Several faults: the one `parse` would name first.
            (
                ShowRequest {
                    title: Some("t".repeat(MAX_SHOW_TITLE_BYTES + 1)),
                    pane: Some(0),
                    ..base()
                },
                FailureCode::TooLarge,
            ),
        ] {
            assert_eq!(request.into_params(), Err(failure));
        }
    }

    #[test]
    fn failures_carry_fixed_codes_reasons_and_wording() {
        let response = show_failure(7, FailureCode::FileNotFound);
        assert_eq!((response.id, response.ok), (7, false));
        let error = response.error.unwrap();
        assert_eq!(error.code, "file_refused");
        assert_eq!(error.reason.as_deref(), Some("not_found"));
        assert_eq!(error.message, "The requested media file was not found.");
        let error = show_failure(8, FailureCode::NotInKettlePane).error.unwrap();
        assert_eq!(error.code, "not_in_kettle_pane");
        assert_eq!(error.reason, None);
        assert!(!error.message.contains("full"));
    }

    #[test]
    fn results_name_kinds_and_warnings_by_their_wire_words() {
        let result = ShowResult::new(
            (4, true, 2),
            11,
            MediaKind::Svg,
            (640, 480),
            &[Warning::FontFallback, Warning::MissingGlyphs],
        );
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"pane": 4, "verified": true, "window": 2, "item": 11, "kind": "svg",
                "width": 640, "height": 480, "warnings": ["font_fallback", "missing_glyphs"]})
        );
    }
}
