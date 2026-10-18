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
    ExternalAttested, FailureCode, JobKind, MAX_FENCES, MAX_MERMAID_BYTES, MAX_RASTER_BYTES,
    MAX_SVG_BYTES, MediaKind, NativePath, Warning,
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

/// An inline card asked for along with the shelf item: by a harness's
/// adapter, or by `kettle show --inline`. Kettle decides whether the caller
/// may have one; asking grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlineTarget {
    /// Claude Code: a card printed by a synchronous hook under the call.
    ClaudeHook,
    /// Codex: a card printed by a `PostToolUse` hook under the call.
    CodexHook,
    /// `kettle show --inline`: a card the command prints itself, straight
    /// to the pane's own terminal.
    Cli,
}

impl InlineTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeHook => "claude_hook",
            Self::CodexHook => "codex_hook",
            Self::Cli => "cli",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "claude_hook" => Some(Self::ClaudeHook),
            "codex_hook" => Some(Self::CodexHook),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }
}

/// Why `kettle show --inline` got no card, its item on the shelf all the
/// same. Fixed words, said in [`InlineRefusal::text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlineRefusal {
    /// The command does not run in the pane it is shown in, as its process
    /// ancestry says.
    Unverified,
    /// Its standard output is not the pane's own terminal: a pipe, a file,
    /// or another terminal (`tmux`, `script`).
    NotTerminal,
    /// It does not run in the pane's foreground.
    Background,
    /// A full-screen program owns the pane's screen.
    AlternateScreen,
    /// The pane is too small for a card with its label and caption.
    TooSmall,
    /// The pane holds, or just registered, as many cards as it may.
    Limit,
    /// This platform has no inline cards from the command line.
    Unsupported,
}

impl InlineRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::NotTerminal => "not_terminal",
            Self::Background => "background",
            Self::AlternateScreen => "alternate_screen",
            Self::TooSmall => "too_small",
            Self::Limit => "limit",
            Self::Unsupported => "unsupported",
        }
    }

    /// What the command says on standard error, its item on the shelf.
    pub const fn text(self) -> &'static str {
        match self {
            Self::Unverified => "no inline card: this command does not run in that pane",
            Self::NotTerminal => {
                "no inline card: its output is not the pane's own terminal (a pipe, a file, tmux)"
            }
            Self::Background => "no inline card: it does not run in the pane's foreground",
            Self::AlternateScreen => "no inline card: a full-screen program owns the pane",
            Self::TooSmall => "no inline card: the pane is too small for one",
            Self::Limit => "no inline card: the pane has as many cards as it may for now",
            Self::Unsupported => "no inline card: this platform has none from the command line",
        }
    }
}

/// An inline card's delivery, for the adapter that asked and never for the
/// model: the message its harness prints once, as Kettle wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineDelivery {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permit: Option<crate::agent_launch::PickupPermit>,
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
    /// Mermaid text, rendered as Mermaid.
    Mermaid(String),
    /// An absolute path the caller attests by device and inode, rendered as
    /// `kind` says.
    File {
        path: NativePath,
        attestation: ExternalAttested,
        kind: FileKind,
    },
}

/// What a shown file is rendered as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// What the worker finds its bytes are.
    Classified,
    /// Mermaid, as the caller says it is (`"kind": "mermaid"`).
    Mermaid,
    /// Page `index`, from 0, of the Markdown diagram gallery it holds
    /// (`"markdown_index"`), below [`MAX_FENCES`].
    MarkdownPage(u8),
    /// Page `index`, from 0, of the PDF it is (`"page_index"`).
    PdfPage(u32),
}

impl ShowSource {
    /// The job kind this source renders as.
    pub fn job_kind(&self) -> JobKind {
        match self {
            Self::Svg(_) => JobKind::Svg,
            Self::Mermaid(_)
            | Self::File {
                kind: FileKind::Mermaid,
                ..
            } => JobKind::Mermaid,
            Self::File {
                kind: FileKind::MarkdownPage(index),
                ..
            } => JobKind::MarkdownDiagrams { index: *index },
            Self::File {
                kind: FileKind::PdfPage(index),
                ..
            } => JobKind::PdfPage { index: *index },
            Self::Image(_)
            | Self::File {
                kind: FileKind::Classified,
                ..
            } => JobKind::Auto,
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
        let mut sources = ["svg", "image_b64", "mermaid", "path"]
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
        // Only a file says what it is; inline text has its field's name.
        let mermaid = match fields.remove("kind") {
            None | Some(Value::Null) => false,
            Some(Value::String(kind)) if kind == "mermaid" && name == "path" => true,
            Some(_) => return Err(FailureCode::BadParams),
        };
        // A page of a gallery or a PDF is a file's too, and never one said
        // to be Mermaid, nor both; past the most pages a gallery holds, it is
        // in none.
        let kind = match (
            optional_u64(&mut fields, "markdown_index")?,
            optional_u64(&mut fields, "page_index")?,
        ) {
            (None, None) if mermaid => FileKind::Mermaid,
            (None, None) => FileKind::Classified,
            _ if name != "path" || mermaid => return Err(FailureCode::BadParams),
            (Some(_), Some(_)) => return Err(FailureCode::BadParams),
            (Some(index), None) => FileKind::MarkdownPage(
                u8::try_from(index)
                    .ok()
                    .filter(|&index| usize::from(index) < MAX_FENCES)
                    .ok_or(FailureCode::IndexOutOfRange)?,
            ),
            (None, Some(index)) => {
                FileKind::PdfPage(u32::try_from(index).map_err(|_| FailureCode::IndexOutOfRange)?)
            }
        };
        let source = match (name, attestation) {
            ("svg", (None, None)) if text.len() > MAX_SVG_BYTES => {
                return Err(FailureCode::TooLarge);
            }
            ("svg", (None, None)) => ShowSource::Svg(text),
            ("mermaid", (None, None)) if text.len() > MAX_MERMAID_BYTES => {
                return Err(FailureCode::TooLarge);
            }
            ("mermaid", (None, None)) => ShowSource::Mermaid(text),
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
                // `NativePath` refuses a relative path on every platform, and
                // one too long: an option out of bounds, not media too large.
                let path =
                    NativePath::from_path(Path::new(&text)).map_err(|_| FailureCode::BadParams)?;
                ShowSource::File {
                    path,
                    attestation: ExternalAttested { dev, ino },
                    kind,
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
            ShowSource::Mermaid(text) => {
                fields.insert("mermaid".into(), Value::String(text));
            }
            ShowSource::File {
                path,
                attestation,
                kind,
            } => {
                let text = native_path_text(&path).ok_or(FailureCode::BadParams)?;
                fields.insert("path".into(), Value::String(text));
                fields.insert("dev".into(), attestation.dev.into());
                fields.insert("ino".into(), attestation.ino.into());
                match kind {
                    FileKind::Classified => {}
                    FileKind::Mermaid => {
                        fields.insert("kind".into(), "mermaid".into());
                    }
                    FileKind::MarkdownPage(index) => {
                        fields.insert("markdown_index".into(), index.into());
                    }
                    FileKind::PdfPage(index) => {
                        fields.insert("page_index".into(), index.into());
                    }
                }
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
            ShowSource::Mermaid(text) => (text.is_empty(), text.len() > MAX_MERMAID_BYTES),
            ShowSource::File { .. } => (false, false),
        };
        let text = |text: &Option<String>, cap: usize| match text {
            Some(text) if text.len() > cap => Err(FailureCode::TooLarge),
            Some(text) if text.is_empty() => Err(FailureCode::BadParams),
            _ => Ok(()),
        };
        // In the order `parse` checks, so both name the same failure.
        if let ShowSource::File {
            kind: FileKind::MarkdownPage(index),
            ..
        } = self.source
            && usize::from(index) >= MAX_FENCES
        {
            return Err(FailureCode::IndexOutOfRange);
        }
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

/// The kind a reply, `list_panes` and a card's caption say for a file Kettle
/// does not draw, shown as itself.
pub const FILE_KIND: &str = "file";

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
    /// What the media turned out to be: `raster`, `svg`, `mermaid`,
    /// `markdown`, `video` or `pdf`; or `file` for a file Kettle does not
    /// draw, shown as itself, whose `width` and `height` are 0.
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub warnings: Vec<String>,
    /// The inline card's delivery, when the adapter asked for one and Kettle
    /// registered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<InlineDelivery>,
    /// Why `kettle show --inline` got no card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_refused: Option<InlineRefusal>,
    /// For a PDF: the page shown, from 0, and how many the document has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<ShowPage>,
    /// What a video is, beside its poster's `width` and `height`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<ShowVideo>,
    /// For a file shown as itself: its size, and why nothing was drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<ShowFile>,
}

/// A file Kettle does not draw, shown as itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowFile {
    /// Its length in bytes.
    pub bytes: u64,
    /// Why nothing was drawn: `unsupported_media` (Kettle draws no file of
    /// its kind), `codec_unavailable` (no decoder on this system for its
    /// codec or its container) or `too_large` (larger than Kettle draws).
    pub reason: String,
}

/// A file's size as people read it, in decimal units: `812 B`, `2.4 MB`,
/// `38 MB`. The same in every language, as a card's caption is.
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    // A value that would round to 1000 reads in the next unit.
    while value >= 999.5 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// A PDF page shown: which, from 0, of how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowPage {
    pub index: u32,
    pub count: u32,
}

impl From<kettle_media::PageOf> for ShowPage {
    fn from(page: kettle_media::PageOf) -> Self {
        Self {
            index: page.index,
            count: page.count,
        }
    }
}

/// A video shown: what it is, from the render of its poster. Numbers and a
/// codec name from Kettle's own table, nothing from the media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowVideo {
    pub duration_ms: u64,
    /// The picture as shown, its rotation applied.
    pub width: u32,
    pub height: u32,
    /// Kettle's name for the codec (`h264`, `vp8`, ... or `unknown`).
    pub codec: String,
    /// Frames per thousand seconds, when the stream says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps_milli: Option<u32>,
    pub has_audio: bool,
}

impl From<kettle_media::VideoInfo> for ShowVideo {
    fn from(info: kettle_media::VideoInfo) -> Self {
        Self {
            duration_ms: info.duration_ms,
            width: info.width,
            height: info.height,
            codec: info.codec.as_str().into(),
            fps_milli: info.fps_milli,
            has_audio: info.has_audio,
        }
    }
}

impl ShowResult {
    /// What was shown, in the words every adapter's line uses: `svg 640x480`,
    /// or for a video its own size, codec, length and sound, `video 1280x720
    /// VP8 0:12, no audio`. Numbers and Kettle's own names only.
    pub fn summary(&self) -> String {
        if let Some(file) = &self.file {
            let why = match file.reason.as_str() {
                "codec_unavailable" => {
                    "no decoder for it on this system; the user can install ffmpeg, which you \
                     must not install yourself"
                }
                "too_large" => "larger than Kettle draws",
                _ => "a kind Kettle does not draw",
            };
            return format!("{FILE_KIND} {}, {why}", size_label(file.bytes));
        }
        let Some(video) = &self.video else {
            let summary = format!("{} {}x{}", self.kind, self.width, self.height);
            return match self.page {
                Some(page) => format!("{summary}, page {} of {}", page.index + 1, page.count),
                None => summary,
            };
        };
        let codec = kettle_media::VideoCodec::ALL
            .into_iter()
            .find(|codec| codec.as_str() == video.codec)
            .and_then(kettle_media::VideoCodec::name);
        let seconds = video.duration_ms / 1000;
        let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
        let length = if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        };
        let mut summary = format!("video {}x{}", video.width, video.height);
        if let Some(codec) = codec {
            summary.push(' ');
            summary.push_str(codec);
        }
        summary.push(' ');
        summary.push_str(&length);
        summary.push_str(if video.has_audio {
            ", with audio"
        } else {
            ", no audio"
        });
        summary
    }

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
            inline_refused: None,
            page: None,
            video: None,
            file: None,
        }
    }

    /// A file shown as itself, `bytes` long, not drawn for `reason`.
    pub fn file(route: (u64, bool, u64), item: u64, bytes: u64, reason: &str) -> Self {
        Self {
            kind: FILE_KIND.into(),
            file: Some(ShowFile {
                bytes,
                reason: reason.into(),
            }),
            ..Self::new(route, item, MediaKind::Raster, (0, 0), &[])
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
            json!({"mermaid": "graph LR", "svg": "<svg/>"}),
            json!({"mermaid": ""}),
            json!({"mermaid": "graph LR", "dev": 1, "ino": 2}),
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
            kind: FileKind::Classified,
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
        // At most 4 KiB as the platform spells it (UTF-16 on Windows): one
        // unit more is an option out of bounds.
        let unit = if cfg!(windows) { 2 } else { 1 };
        let name = |units: usize| absolute(&"n".repeat(units - absolute("").len()));
        let at = name(kettle_media::MAX_PATH_BYTES / unit);
        assert!(ShowRequest::parse(json!({"path": at, "dev": 0, "ino": 5})).is_ok());
        let over = name(kettle_media::MAX_PATH_BYTES / unit + 1);
        assert_eq!(
            ShowRequest::parse(json!({"path": over, "dev": 0, "ino": 5})),
            Err(FailureCode::BadParams)
        );
    }

    /// Mermaid comes inline, as text of at most 64 KiB, or as a file the
    /// caller says is Mermaid; nothing else takes a kind.
    #[test]
    fn mermaid_is_inline_text_or_a_file_said_to_be_mermaid() {
        let at = "%".repeat(MAX_MERMAID_BYTES);
        let request = ShowRequest::parse(json!({"mermaid": at})).unwrap();
        assert_eq!(request.source.job_kind(), JobKind::Mermaid);
        assert_eq!(request.source, ShowSource::Mermaid(at.clone()));
        assert_eq!(
            ShowRequest::parse(json!({"mermaid": format!("{at}%")})),
            Err(FailureCode::TooLarge)
        );
        let path = absolute("flow.txt");
        let file = ShowRequest::parse(json!({"path": path, "dev": 1, "ino": 2, "kind": "mermaid"}))
            .unwrap();
        assert_eq!(file.source.job_kind(), JobKind::Mermaid);
        for bad in [
            json!({"path": path, "dev": 1, "ino": 2, "kind": "svg"}),
            json!({"path": path, "dev": 1, "ino": 2, "kind": 1}),
            json!({"svg": "<svg/>", "kind": "mermaid"}),
            json!({"mermaid": "graph LR", "kind": "mermaid"}),
        ] {
            assert_eq!(
                ShowRequest::parse(bad.clone()),
                Err(FailureCode::BadParams),
                "{bad}"
            );
        }
    }

    /// A file may name a page of the PDF it is, from 0; never with a gallery
    /// page, as Mermaid, or of inline media. The reply's summary says which
    /// page it shows of how many, counting from 1.
    #[test]
    fn a_file_may_name_a_pdf_page() {
        let path = absolute("report.pdf");
        let page = |index: Value| {
            ShowRequest::parse(json!({"path": path, "dev": 1, "ino": 2, "page_index": index}))
        };
        for (index, expected) in [(0, 0), (299, 299), (u64::from(u32::MAX), u32::MAX)] {
            let request = page(json!(index)).unwrap();
            assert_eq!(
                request.source.job_kind(),
                JobKind::PdfPage { index: expected }
            );
            // It reads back as itself.
            let params = request.clone().into_params().unwrap();
            assert_eq!(params["page_index"], json!(index));
            assert_eq!(ShowRequest::parse(params).unwrap(), request);
        }
        assert_eq!(
            page(json!(u64::from(u32::MAX) + 1)),
            Err(FailureCode::IndexOutOfRange)
        );
        for bad in [
            page(json!(-1)),
            page(json!("1")),
            ShowRequest::parse(json!({"svg": "<svg/>", "page_index": 0})),
            ShowRequest::parse(
                json!({"path": path, "dev": 1, "ino": 2, "page_index": 0, "markdown_index": 0}),
            ),
            ShowRequest::parse(
                json!({"path": path, "dev": 1, "ino": 2, "kind": "mermaid", "page_index": 0}),
            ),
        ] {
            assert_eq!(bad, Err(FailureCode::BadParams));
        }
        let mut result = ShowResult::new((1, true, 1), 2, MediaKind::Pdf, (816, 1056), &[]);
        assert_eq!(result.summary(), "pdf 816x1056");
        result.page = Some(ShowPage {
            index: 2,
            count: 12,
        });
        assert_eq!(result.summary(), "pdf 816x1056, page 3 of 12");
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["page"], json!({"index": 2, "count": 12}));
    }

    /// A file may name a page of the Markdown gallery it holds, from 0 to
    /// the most pages a gallery has; past that it is in none. Only a file
    /// names one, and never a file said to be Mermaid.
    #[test]
    fn a_file_may_name_a_gallery_page() {
        let path = absolute("plan.md");
        let page = |index: Value| {
            ShowRequest::parse(json!({"path": path, "dev": 1, "ino": 2, "markdown_index": index}))
        };
        let request = page(json!(0)).unwrap();
        assert_eq!(
            request.source.job_kind(),
            JobKind::MarkdownDiagrams { index: 0 }
        );
        let last = MAX_FENCES as u64 - 1;
        assert_eq!(
            page(json!(last)).unwrap().source.job_kind(),
            JobKind::MarkdownDiagrams { index: last as u8 }
        );
        for past in [json!(last + 1), json!(256), json!(u64::MAX)] {
            assert_eq!(
                page(past.clone()),
                Err(FailureCode::IndexOutOfRange),
                "{past}"
            );
        }
        assert_eq!(page(Value::Null).unwrap().source.job_kind(), JobKind::Auto);
        for bad in [
            page(json!(-1)),
            page(json!(1.5)),
            page(json!("1")),
            ShowRequest::parse(json!({"svg": "<svg/>", "markdown_index": 0})),
            ShowRequest::parse(json!({"mermaid": "graph LR", "markdown_index": 0})),
            ShowRequest::parse(json!({"image_b64": "AAAA", "markdown_index": 0})),
            ShowRequest::parse(
                json!({"path": path, "dev": 1, "ino": 2, "kind": "mermaid", "markdown_index": 0}),
            ),
        ] {
            assert_eq!(bad, Err(FailureCode::BadParams));
        }
        // A request built in code is held to the same bound.
        let built = ShowRequest {
            source: ShowSource::File {
                path: NativePath::from_path(Path::new(&path)).unwrap(),
                attestation: ExternalAttested { dev: 1, ino: 2 },
                kind: FileKind::MarkdownPage(MAX_FENCES as u8),
            },
            title: Some(String::new()),
            key: None,
            pane: None,
            inline: None,
        };
        assert_eq!(built.into_params(), Err(FailureCode::IndexOutOfRange));
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
                    kind: FileKind::Classified,
                },
                title: None,
                key: Some(path.clone()),
                pane: None,
                inline: None,
            },
            ShowRequest {
                source: ShowSource::Mermaid("graph LR\n  A[\"caf\u{e9}\"] --> B".into()),
                title: Some("Flow".into()),
                key: None,
                pane: None,
                inline: Some(InlineTarget::CodexHook),
            },
            ShowRequest {
                source: ShowSource::File {
                    path: NativePath::from_path(Path::new(&path)).unwrap(),
                    attestation: ExternalAttested { dev: 3, ino: 4 },
                    kind: FileKind::Mermaid,
                },
                title: None,
                key: None,
                pane: None,
                inline: None,
            },
            ShowRequest {
                source: ShowSource::File {
                    path: NativePath::from_path(Path::new(&path)).unwrap(),
                    attestation: ExternalAttested { dev: 5, ino: 6 },
                    kind: FileKind::MarkdownPage(31),
                },
                title: Some("Plan".into()),
                key: None,
                pane: None,
                inline: None,
            },
        ] {
            let params = request.clone().into_params().unwrap();
            assert_eq!(ShowRequest::parse(params).unwrap(), request);
        }
    }

    /// A video's result carries what it is beside its poster's size, in
    /// Kettle's own codec names; any other result says nothing of video.
    #[test]
    fn a_video_result_says_what_the_video_is() {
        let info = kettle_media::VideoInfo {
            duration_ms: 12_400,
            width: 1280,
            height: 720,
            rotation: 90,
            codec: kettle_media::VideoCodec::Vp8,
            fps_milli: None,
            has_audio: true,
            container: Some(kettle_media::video::VideoContainer::WebM),
        };
        let mut result = ShowResult::new((1, true, 1), 2, MediaKind::Video, (400, 225), &[]);
        let plain = serde_json::to_value(&result).unwrap();
        assert!(plain.get("video").is_none());
        result.video = Some(ShowVideo::from(info));
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(
            value["video"],
            serde_json::json!({"duration_ms": 12_400, "width": 1280, "height": 720,
                "codec": "vp8", "has_audio": true})
        );
        assert_eq!(
            (value["width"].clone(), value["kind"].clone()),
            (400.into(), "video".into())
        );
        let back: ShowResult = serde_json::from_value(value).unwrap();
        assert_eq!(back, result);
        assert_eq!(result.summary(), "video 1280x720 VP8 0:12, with audio");
        result.video.as_mut().unwrap().codec = "made-up".into();
        result.video.as_mut().unwrap().has_audio = false;
        assert_eq!(
            result.summary(),
            "video 1280x720 0:12, no audio",
            "no name a sender made up"
        );
        result.video = None;
        assert_eq!(result.summary(), "video 400x225");
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
        assert_eq!(parse("cli".into()), Ok(Some(InlineTarget::Cli)));
        assert_eq!(parse("codex".into()), Err(FailureCode::BadParams));
        assert_eq!(
            parse(serde_json::json!({"target": "claude_hook"})),
            Err(FailureCode::BadParams)
        );
        // Each target reads back as itself.
        for target in [
            InlineTarget::ClaudeHook,
            InlineTarget::CodexHook,
            InlineTarget::Cli,
        ] {
            assert_eq!(InlineTarget::parse(target.as_str()), Some(target));
        }
    }

    /// Why a command-line card was refused travels as its wire word, only
    /// when there is a refusal, and each word has its own fixed note.
    #[test]
    fn an_inline_refusal_is_its_wire_word_and_only_when_there_is_one() {
        let mut result = ShowResult::new((1, true, 1), 2, MediaKind::Svg, (64, 48), &[]);
        assert!(
            serde_json::to_value(&result)
                .unwrap()
                .get("inline_refused")
                .is_none()
        );
        let all = [
            InlineRefusal::Unverified,
            InlineRefusal::NotTerminal,
            InlineRefusal::Background,
            InlineRefusal::AlternateScreen,
            InlineRefusal::TooSmall,
            InlineRefusal::Limit,
            InlineRefusal::Unsupported,
        ];
        let mut notes = std::collections::HashSet::new();
        for refusal in all {
            result.inline_refused = Some(refusal);
            let value = serde_json::to_value(&result).unwrap();
            assert_eq!(value["inline_refused"], refusal.as_str());
            let back: ShowResult = serde_json::from_value(value).unwrap();
            assert_eq!(back.inline_refused, Some(refusal));
            assert!(refusal.text().starts_with("no inline card: "));
            assert!(notes.insert(refusal.text()));
        }
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
            permit: None,
            message: "x".repeat(MAX_INLINE_MESSAGE_UTF16),
        };
        assert!(delivery.fits());
        // A cell's placeholder is two UTF-16 units.
        let over = InlineDelivery {
            permit: None,
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

    /// A file shown as itself says so, with its size and why nothing was
    /// drawn, and no size of its own; its line reads as people read sizes.
    #[test]
    fn a_file_result_says_it_drew_nothing() {
        let result = ShowResult::file((4, true, 2), 11, 2_400_000, "unsupported_media");
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"pane": 4, "verified": true, "window": 2, "item": 11, "kind": "file",
                "width": 0, "height": 0, "warnings": [],
                "file": {"bytes": 2_400_000, "reason": "unsupported_media"}})
        );
        assert_eq!(result.summary(), "file 2.4 MB, a kind Kettle does not draw");
        for (reason, line) in [
            (
                "codec_unavailable",
                "file 2.4 MB, no decoder for it on this system; the user can install ffmpeg, \
                 which you must not install yourself",
            ),
            ("too_large", "file 2.4 MB, larger than Kettle draws"),
        ] {
            assert_eq!(
                ShowResult::file((4, true, 2), 11, 2_400_000, reason).summary(),
                line
            );
        }
        let back: ShowResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(back, result);
    }

    /// Sizes read in decimal units, one decimal below ten.
    #[test]
    fn sizes_read_as_people_read_them() {
        for (bytes, label) in [
            (0, "0 B"),
            (812, "812 B"),
            (999, "999 B"),
            (1000, "1.0 kB"),
            (2_400_000, "2.4 MB"),
            (9_949_999, "9.9 MB"),
            (9_950_000, "10 MB"),
            (38_200_000, "38 MB"),
            (999_499_999, "999 MB"),
            (999_500_000, "1.0 GB"),
            (u64::MAX, "18446744 TB"),
        ] {
            assert_eq!(size_label(bytes), label, "{bytes}");
        }
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
