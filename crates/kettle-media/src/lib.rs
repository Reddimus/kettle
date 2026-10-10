#![forbid(unsafe_code)]
//! Bounded media data and deterministic frames. This crate opens no files and starts no process
//! itself: the [`client`] reaches the filesystem and the worker only through a platform its
//! caller supplies.
//! External requests cannot express GUI-only path authorization. See [`GuiActionWitness`].

pub mod client;
mod digest;
mod lifecycle;
pub mod video;
pub mod wire;
pub use digest::content_digest;

use std::time::Duration;

/// Native path bytes, including the Windows UTF-16LE encoding when applicable.
pub const MAX_PATH_BYTES: usize = 4 * 1024;
/// Mermaid input is refused above this limit, never truncated.
pub const MAX_MERMAID_BYTES: usize = 64 * 1024;
/// SVG input limit, before parsing or post-processing.
pub const MAX_SVG_BYTES: usize = 2 * 1024 * 1024;
/// Encoded raster input limit.
pub const MAX_RASTER_BYTES: usize = 32 * 1024 * 1024;
/// Decoded raster dimension limit per side.
pub const MAX_DECODED_EDGE: u32 = 8192;
/// Decoded raster RGBA storage limit.
pub const MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;
/// Rendered dimension limit per side. P2 also checks the device and preview account.
pub const MAX_RENDERED_EDGE: u32 = 4096;
/// Straight RGBA result limit.
pub const MAX_RENDERED_BYTES: usize = 64 * 1024 * 1024;
/// An SVG result's edge limit, tighter than the general one: the image is
/// fitted within the target and then within this and `MAX_SVG_RENDERED_PIXELS`.
pub const MAX_SVG_RENDERED_EDGE: u32 = 1024;
/// An SVG result's pixel limit.
pub const MAX_SVG_RENDERED_PIXELS: u64 = 1024 * 1024;
/// The pixels an SVG render may allocate for layers, filter results, masks,
/// clips and pattern tiles together, counted per use before any is allocated.
pub const MAX_SVG_LAYER_PIXELS: u64 = 4 * 1024 * 1024;
/// Elements an SVG document may hold.
pub const MAX_SVG_ELEMENTS: usize = 125_000;
/// Element nesting an SVG document may have, references followed.
pub const MAX_SVG_DEPTH: usize = 256;
/// An SVG document's work with references expanded: elements, path and
/// point numbers and text characters, each counted once per use.
pub const MAX_SVG_WORK: u64 = 1_000_000;
/// Whole Markdown input limit.
pub const MAX_MARKDOWN_BYTES: usize = 1024 * 1024;
/// Maximum Markdown diagram fences.
pub const MAX_FENCES: usize = 32;
/// Maximum bytes in each saved fence.
pub const MAX_FENCE_BYTES: usize = 64 * 1024;
/// Maximum fallback path/face pairs.
pub const MAX_FALLBACK_FONTS: usize = 8;
/// Maximum bytes held for one explicitly supplied fallback font file.
pub const MAX_FALLBACK_FONT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum bytes held for all fallback font files in one job.
pub const MAX_FALLBACK_FONT_TOTAL_BYTES: usize = 128 * 1024 * 1024;
/// Display text line limit in UTF-8 bytes.
pub const MAX_SOURCE_LINE_BYTES: usize = 4 * 1024;
/// Display text line count limit.
pub const MAX_SOURCE_LINES: usize = 2000;
/// Video still count limit from Appendix A.3.
pub const MAX_VIDEO_STILLS: u8 = 16;
/// Largest requested still edge from Appendix A.3.
pub const MAX_VIDEO_EDGE: u32 = 2560;
/// P1 bounded metadata choices. The plan does not specify script count or name length.
pub const MAX_UNCOVERED_SCRIPTS: usize = 32;
/// UTF-8 bytes per uncovered Unicode script name.
pub const MAX_SCRIPT_BYTES: usize = 64;
/// Maximum fixed warning codes in a reply.
pub const MAX_WARNINGS: usize = 16;
/// Bounded version text, normally `env!("CARGO_PKG_VERSION")`.
pub const MAX_VERSION_BYTES: usize = 64;
/// Hex digest of the source a binary was built from, up to 256 bits.
pub const MAX_SOURCE_HASH_BYTES: usize = 64;
/// No version negotiation. A header skew requires restart.
pub const PROTOCOL_VERSION: u16 = 3;
/// Longest textual source a reply returns as it was read: the SVG input
/// cap, the largest textual kind's.
pub const MAX_EXACT_SOURCE_BYTES: usize = MAX_SVG_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationError {
    TooLarge,
    BadParams,
    InvalidPath,
    IndexOutOfRange,
    InvalidUtf8,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "media limit exceeded",
            Self::BadParams => "invalid media parameters",
            Self::InvalidPath => "invalid native path",
            Self::IndexOutOfRange => "diagram index out of range",
            Self::InvalidUtf8 => "invalid UTF-8",
        })
    }
}
impl std::error::Error for ValidationError {}

pub(crate) fn cap(n: usize, max: usize) -> Result<(), ValidationError> {
    if n > max {
        Err(ValidationError::TooLarge)
    } else {
        Ok(())
    }
}

/// Checked RGBA size; use before decoder allocation or GPU upload.
pub fn rgba_len(w: u32, h: u32, edge: u32, bytes: usize) -> Result<usize, ValidationError> {
    if w == 0 || h == 0 || w > edge || h > edge {
        return Err(ValidationError::BadParams);
    }
    let n = usize::try_from(w)
        .ok()
        .and_then(|w| usize::try_from(h).ok().and_then(|h| w.checked_mul(h)))
        .and_then(|n| n.checked_mul(4))
        .ok_or(ValidationError::TooLarge)?;
    cap(n, bytes)?;
    Ok(n)
}

/// Absolute native path: raw bytes on Unix, little-endian UTF-16 code units on Windows.
/// No normalization, lossy conversion, filesystem access or attestation occurs here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePath(Vec<u8>);
impl NativePath {
    /// `path` in the platform's native encoding (its bytes on Unix, UTF-16LE
    /// on Windows), checked as [`NativePath::new`] checks it.
    pub fn from_path(path: &std::path::Path) -> Result<Self, ValidationError> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Self::new(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt as _;
            Self::new(
                path.as_os_str()
                    .encode_wide()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            )
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(ValidationError::InvalidPath)
        }
    }

    pub fn new(bytes: Vec<u8>) -> Result<Self, ValidationError> {
        validate_path(&bytes)?;
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// A display-only basename. Lossy conversion never changes the native
    /// path used by the worker and performs no filesystem access.
    pub fn file_name_lossy(&self) -> Option<String> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            std::path::Path::new(std::ffi::OsStr::from_bytes(self.as_bytes()))
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt as _;
            let units: Vec<u16> = self
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|unit| u16::from_le_bytes(*unit))
                .collect();
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&units))
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }
}
pub(crate) fn validate_path(bytes: &[u8]) -> Result<(), ValidationError> {
    cap(bytes.len(), MAX_PATH_BYTES)?;
    #[cfg(unix)]
    if bytes.first() != Some(&b'/') || bytes.contains(&0) {
        return Err(ValidationError::InvalidPath);
    }
    #[cfg(windows)]
    validate_windows_path(bytes)?;
    #[cfg(not(any(unix, windows)))]
    return Err(ValidationError::InvalidPath);
    Ok(())
}

// Also compiled in unit tests so native UTF-16 syntax has portable coverage.
#[cfg(any(windows, test))]
fn validate_windows_path(bytes: &[u8]) -> Result<(), ValidationError> {
    cap(bytes.len(), MAX_PATH_BYTES)?;
    let (units, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() || units.is_empty() || units.contains(&[0, 0]) {
        return Err(ValidationError::InvalidPath);
    }
    let unit = |i: usize| u16::from_le_bytes(units[i]);
    let separator = |u: u16| u == 92 || u == 47;
    let n = units.len();
    let drive = n >= 3
        && ((65..=90).contains(&unit(0)) || (97..=122).contains(&unit(0)))
        && unit(1) == 58
        && separator(unit(2));
    // A complete UNC prefix requires both a server and a share. A lone root or
    // drive-relative spelling must not depend on the worker's ambient cwd.
    let unc = if n > 2 && separator(unit(0)) && separator(unit(1)) && !separator(unit(2)) {
        if let Some(end) = (2..n).find(|&i| separator(unit(i))) {
            let share = end.checked_add(1).ok_or(ValidationError::InvalidPath)?;
            share < n && !separator(unit(share))
        } else {
            false
        }
    } else {
        false
    };
    if !drive && !unc {
        return Err(ValidationError::InvalidPath);
    }
    Ok(())
}

#[cfg(test)]
mod native_path_tests {
    use super::*;
    fn wide(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }
    #[test]
    fn display_basename_preserves_unicode_and_never_exposes_parent_directories() {
        let base = if cfg!(windows) { "C:\\media" } else { "/media" };
        let path =
            NativePath::from_path(&std::path::Path::new(base).join("caf\u{e9}.svg")).unwrap();
        assert_eq!(path.file_name_lossy().as_deref(), Some("caf\u{e9}.svg"));
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        assert_eq!(
            NativePath::from_path(std::path::Path::new(root))
                .unwrap()
                .file_name_lossy(),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn display_basename_does_not_modify_non_utf8_native_bytes() {
        let bytes = b"/media/diagram-\xff.svg".to_vec();
        let path = NativePath::new(bytes.clone()).unwrap();
        assert_eq!(
            path.file_name_lossy().as_deref(),
            Some("diagram-\u{fffd}.svg")
        );
        assert_eq!(path.as_bytes(), bytes);
    }

    #[cfg(windows)]
    #[test]
    fn display_basename_does_not_modify_unpaired_native_surrogates() {
        let mut bytes = wide("C:\\media\\diagram-");
        bytes.extend(0xd800u16.to_le_bytes());
        bytes.extend(wide(".svg"));
        let path = NativePath::new(bytes.clone()).unwrap();
        assert_eq!(
            path.file_name_lossy().as_deref(),
            Some("diagram-\u{fffd}.svg")
        );
        assert_eq!(path.as_bytes(), bytes);
    }

    #[test]
    fn from_path_is_the_native_encoding() {
        let path = if cfg!(windows) {
            "C:\\media\\caf\u{e9}.svg"
        } else {
            "/media/caf\u{e9}.svg"
        };
        let native = if cfg!(windows) {
            wide(path)
        } else {
            path.as_bytes().to_vec()
        };
        assert_eq!(
            NativePath::from_path(std::path::Path::new(path))
                .unwrap()
                .as_bytes(),
            native
        );
    }

    #[test]
    fn windows_absolute_syntax_without_lossy_conversion() {
        for s in [
            "C:\\media",
            "C:/media",
            "\\\\server\\share\\file",
            "//server/share",
            "\\\\?\\C:\\media",
        ] {
            assert!(validate_windows_path(&wide(s)).is_ok(), "{s}");
        }
        for s in [
            "media",
            "C:media",
            "\\media",
            "\\\\server",
            "\\\\server\\",
            "\\\\server\\\\share",
            "C:\\bad\0path",
        ] {
            assert!(validate_windows_path(&wide(s)).is_err(), "{s}");
        }
        assert!(validate_windows_path(&[1]).is_err());
        assert!(validate_windows_path(&vec![1; MAX_PATH_BYTES + 2]).is_err());
        // Native Windows paths can contain unpaired surrogates; preserve them.
        let mut path = wide("C:\\");
        path.extend(0xd800u16.to_le_bytes());
        assert!(validate_windows_path(&path).is_ok());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalAttested {
    pub dev: u64,
    pub ino: u64,
}

/// Explicit trust declaration for a GUI event handler. Only kettle-ui should call
/// `from_explicit_gui_action` after a file picker, drop, or preview click. This is
/// an API trust boundary, not runtime caller authentication. Never call it from ctl/MCP.
/// No wire decoder can produce this witness.
///
/// ```compile_fail
/// use kettle_media::UserPull;
/// let authorization = UserPull { _private: () };
/// ```
/// ```compile_fail
/// use kettle_media::ExternalSource;
/// let source = ExternalSource::UserPull;
/// ```
#[derive(Debug)]
pub struct GuiActionWitness {
    _private: (),
}
impl GuiActionWitness {
    pub fn from_explicit_gui_action() -> Self {
        Self { _private: () }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserPull {
    _private: (),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authorization {
    ExternalAttested(ExternalAttested),
    UserPull(UserPull),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Bytes(Vec<u8>),
    Path {
        path: NativePath,
        authorization: Authorization,
    },
}
impl Source {
    pub fn user_pull(path: NativePath, _action: GuiActionWitness) -> Self {
        Self::Path {
            path,
            authorization: Authorization::UserPull(UserPull { _private: () }),
        }
    }
}
/// ctl/MCP input has no UserPull variant, even when decoded from hostile frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalSource {
    Bytes(Vec<u8>),
    Path {
        path: NativePath,
        attestation: ExternalAttested,
    },
}
impl From<ExternalSource> for Source {
    fn from(s: ExternalSource) -> Self {
        match s {
            ExternalSource::Bytes(b) => Self::Bytes(b),
            ExternalSource::Path { path, attestation } => Self::Path {
                path,
                authorization: Authorization::ExternalAttested(attestation),
            },
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoStills {
    pub count: u8,
    pub max_edge: u32,
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub at_s: Option<f64>,
}
impl VideoStills {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.count == 0
            || self.count > MAX_VIDEO_STILLS
            || self.max_edge == 0
            || self.max_edge > MAX_VIDEO_EDGE
            || !self.start_s.is_finite()
            || self.start_s < 0.0
            || self
                .end_s
                .is_some_and(|t| !t.is_finite() || t < self.start_s)
            || self.at_s.is_some_and(|t| !t.is_finite() || t < 0.0)
            || (self.at_s.is_some()
                && (self.count != 1 || self.end_s.is_some() || self.start_s != 0.0))
        {
            return Err(ValidationError::BadParams);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JobKind {
    /// The worker classifies a file snapshot by its bytes.
    Auto,
    Mermaid,
    Svg,
    Raster,
    MarkdownDiagrams {
        index: u8,
    },
    VideoProbe,
    VideoStills(VideoStills),
}
impl JobKind {
    pub fn input_cap(self) -> usize {
        match self {
            Self::Mermaid => MAX_MERMAID_BYTES,
            Self::Svg => MAX_SVG_BYTES,
            Self::MarkdownDiagrams { .. } => MAX_MARKDOWN_BYTES,
            // The largest kind Auto can turn out to be; the worker applies
            // the actual kind's own cap once it knows it.
            Self::Auto => MAX_RASTER_BYTES.max(MAX_SVG_BYTES),
            _ => MAX_RASTER_BYTES,
        }
    }
    /// How long a worker may take from this job's arrival to its reply. An
    /// Auto job starts with the longest of the kinds it can turn out to be,
    /// then narrows to [`MediaKind::render_deadline`] of the actual kind,
    /// still counted from its arrival.
    pub fn render_deadline(self) -> Duration {
        MediaKind::for_job(self).map_or(AUTO_RENDER_DEADLINE, MediaKind::render_deadline)
    }
    pub fn validate(self) -> Result<(), ValidationError> {
        match self {
            Self::MarkdownDiagrams { index } if usize::from(index) >= MAX_FENCES => {
                Err(ValidationError::IndexOutOfRange)
            }
            Self::VideoStills(v) => v.validate(),
            _ => Ok(()),
        }
    }
}
/// An Auto job's deadline before it is classified: the longest of the kinds
/// the worker classifies into (raster and SVG).
const AUTO_RENDER_DEADLINE: Duration = Duration::from_secs(3);

/// What a job turned out to be. For an Auto job the worker decides it from
/// the bytes, never the file's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Raster,
    Svg,
    Mermaid,
    Markdown,
    Video,
}
impl MediaKind {
    pub(crate) fn for_job(kind: JobKind) -> Option<Self> {
        match kind {
            JobKind::Auto => None,
            JobKind::Raster => Some(Self::Raster),
            JobKind::Svg => Some(Self::Svg),
            JobKind::Mermaid => Some(Self::Mermaid),
            JobKind::MarkdownDiagrams { .. } => Some(Self::Markdown),
            JobKind::VideoProbe | JobKind::VideoStills(_) => Some(Self::Video),
        }
    }

    /// The longest source a render of this kind returns as it was read: a
    /// textual kind's input cap; `None` for a kind that is not text.
    pub fn exact_source_cap(self) -> Option<usize> {
        match self {
            Self::Svg => Some(MAX_SVG_BYTES),
            Self::Mermaid => Some(MAX_MERMAID_BYTES),
            Self::Markdown => Some(MAX_MARKDOWN_BYTES),
            Self::Raster | Self::Video => None,
        }
    }

    /// The kind's wire word in control replies.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raster => "raster",
            Self::Svg => "svg",
            Self::Mermaid => "mermaid",
            Self::Markdown => "markdown",
            Self::Video => "video",
        }
    }

    /// How long a worker may take from a job's arrival to its reply once the
    /// job is known to be this kind.
    pub fn render_deadline(self) -> Duration {
        match self {
            Self::Raster => Duration::from_secs(2),
            Self::Svg | Self::Mermaid | Self::Markdown | Self::Video => Duration::from_secs(3),
        }
    }
}

pub type Color = [u8; 4];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub palette: [Color; 16],
    pub accent: Color,
    pub is_dark: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Canvas {
    Theme,
    White,
    Checker,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub crop: Option<Crop>,
}
impl Target {
    pub fn validate(self) -> Result<(), ValidationError> {
        rgba_len(
            self.width,
            self.height,
            MAX_RENDERED_EDGE,
            MAX_RENDERED_BYTES,
        )?;
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(ValidationError::BadParams);
        }
        if let Some(c) = self.crop
            && (c.width == 0
                || c.height == 0
                || c.x.checked_add(c.width).is_none_or(|n| n > self.width)
                || c.y.checked_add(c.height).is_none_or(|n| n > self.height))
        {
            return Err(ValidationError::BadParams);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FallbackFont {
    pub path: NativePath,
    pub face_index: u32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Job<S = Source> {
    pub kind: JobKind,
    pub source: S,
    pub theme: Theme,
    pub canvas: Canvas,
    pub target: Target,
    pub fallback_fonts: Vec<FallbackFont>,
}
pub type ExternalRequest = Job<ExternalSource>;
impl From<ExternalRequest> for Job {
    fn from(j: ExternalRequest) -> Self {
        Self {
            kind: j.kind,
            source: j.source.into(),
            theme: j.theme,
            canvas: j.canvas,
            target: j.target,
            fallback_fonts: j.fallback_fonts,
        }
    }
}

/// What a parent and its worker must share to talk: the release, the source
/// both were built from, and the frame protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildId {
    pub crate_version: String,
    /// A hex digest of the source the binary was built from, not a commit: two
    /// builds of one dirty commit differ, a rebuild of the same source does not
    /// (the `kettle` build script's source hash).
    pub source_hash: String,
    pub protocol_version: u16,
}
impl BuildId {
    /// A binary's own identity: `version` is its `CARGO_PKG_VERSION` and
    /// `source_hash` the `KETTLE_SOURCE_HASH` its build script embedded, never
    /// a git commit.
    pub fn from_embedded(version: &str, source_hash: &str) -> Result<Self, ValidationError> {
        validate_build(version, source_hash)?;
        Ok(Self {
            crate_version: version.to_owned(),
            source_hash: source_hash.to_owned(),
            protocol_version: PROTOCOL_VERSION,
        })
    }
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_build(&self.crate_version, &self.source_hash)
    }
}
pub(crate) fn validate_build(version: &str, hash: &str) -> Result<(), ValidationError> {
    cap(version.len(), MAX_VERSION_BYTES)?;
    cap(hash.len(), MAX_SOURCE_HASH_BYTES)?;
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
        || hash.is_empty()
        || !hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ValidationError::BadParams);
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub build_id: BuildId,
}
/// Emit only after worker setup, including font setup, has completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub build_id: BuildId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeOutcome {
    Compatible,
    RestartRequired,
    ReverseSkew,
}
impl HandshakeOutcome {
    pub fn model_message(self) -> &'static str {
        match self {
            Self::Compatible => "Media worker is ready.",
            Self::RestartRequired => {
                "Kettle was updated. The user needs to restart Kettle before previews work. Do not retry."
            }
            Self::ReverseSkew => "Kettle is older than this command; restart Kettle. Do not retry.",
        }
    }
}
pub fn check_ready(hello: &Hello, ready: &Ready) -> HandshakeOutcome {
    if hello.build_id == ready.build_id && hello.build_id.protocol_version == PROTOCOL_VERSION {
        HandshakeOutcome::Compatible
    } else {
        HandshakeOutcome::RestartRequired
    }
}
pub fn check_peer_failure(code: FailureCode) -> Option<HandshakeOutcome> {
    match code {
        FailureCode::UnknownMethod => Some(HandshakeOutcome::ReverseSkew),
        FailureCode::RestartRequired => Some(HandshakeOutcome::RestartRequired),
        _ => None,
    }
}

/// Fixed codes only. No input bytes, paths, parser errors or arbitrary strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FailureCode {
    BadParams = 0,
    TooLarge = 1,
    FileNotFound = 2,
    FilePermission = 3,
    FileNotRegular = 4,
    FileTooLarge = 5,
    Changed = 6,
    IndexOutOfRange = 7,
    UnsupportedMedia = 8,
    UnsupportedPlatform = 9,
    RenderTimeout = 10,
    RenderResource = 11,
    RenderParse = 12,
    RestartRequired = 13,
    UnknownMethod = 14,
    Busy = 15,
    UnsupportedContainer = 16,
    CodecUnavailable = 17,
    BackendUnavailable = 18,
    ExternalOpenUnavailable = 19,
    DisplayDisabled = 20,
    NotInKettle = 21,
    NotInKettlePane = 22,
    DisplayOnly = 23,
    ReadOnly = 24,
    OverBudget = 25,
    WorkerUnavailable = 26,
}
impl FailureCode {
    /// Every failure, in code order.
    pub const ALL: [Self; 27] = [
        Self::BadParams,
        Self::TooLarge,
        Self::FileNotFound,
        Self::FilePermission,
        Self::FileNotRegular,
        Self::FileTooLarge,
        Self::Changed,
        Self::IndexOutOfRange,
        Self::UnsupportedMedia,
        Self::UnsupportedPlatform,
        Self::RenderTimeout,
        Self::RenderResource,
        Self::RenderParse,
        Self::RestartRequired,
        Self::UnknownMethod,
        Self::Busy,
        Self::UnsupportedContainer,
        Self::CodecUnavailable,
        Self::BackendUnavailable,
        Self::ExternalOpenUnavailable,
        Self::DisplayDisabled,
        Self::NotInKettle,
        Self::NotInKettlePane,
        Self::DisplayOnly,
        Self::ReadOnly,
        Self::OverBudget,
        Self::WorkerUnavailable,
    ];

    /// The failure a control reply names by its `code` and `reason`, so a
    /// client says it in its own fixed words; `None` for a pair Kettle never
    /// sends.
    pub fn from_wire(code: &str, reason: Option<&str>) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|failure| failure.code() == code && failure.reason() == reason)
    }

    /// Appendix A primary error code. Fixed subreasons are exposed by `reason`.
    pub fn code(self) -> &'static str {
        match self {
            Self::BadParams => "bad_params",
            Self::TooLarge => "too_large",
            Self::FileNotFound
            | Self::FilePermission
            | Self::FileNotRegular
            | Self::FileTooLarge => "file_refused",
            Self::Changed => "changed",
            Self::IndexOutOfRange => "index_out_of_range",
            Self::UnsupportedMedia => "unsupported_media",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::RenderTimeout | Self::RenderResource | Self::RenderParse => "render_failed",
            Self::RestartRequired => "restart_required",
            Self::UnknownMethod => "unknown_method",
            Self::Busy => "busy",
            Self::UnsupportedContainer => "unsupported_container",
            Self::CodecUnavailable => "codec_unavailable",
            Self::BackendUnavailable => "backend_unavailable",
            Self::ExternalOpenUnavailable => "external_open_unavailable",
            Self::DisplayDisabled => "display_disabled",
            Self::NotInKettle => "not_in_kettle",
            Self::NotInKettlePane => "not_in_kettle_pane",
            Self::DisplayOnly => "display_only",
            Self::ReadOnly => "read_only",
            Self::OverBudget => "over_budget",
            Self::WorkerUnavailable => "worker_unavailable",
        }
    }
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::FileNotFound => Some("not_found"),
            Self::FilePermission => Some("permission"),
            Self::FileNotRegular => Some("not_regular"),
            Self::FileTooLarge => Some("too_large"),
            Self::RenderTimeout => Some("timeout"),
            Self::RenderResource => Some("resource"),
            Self::RenderParse => Some("parse"),
            _ => None,
        }
    }
    /// Input-free wording from Appendix A. Provider/package-specific hints belong to P2.
    pub fn model_message(self) -> &'static str {
        match self {
            Self::RestartRequired => HandshakeOutcome::RestartRequired.model_message(),
            Self::UnknownMethod => HandshakeOutcome::ReverseSkew.model_message(),
            Self::BadParams => {
                "Invalid media request. Use exactly one supported source and valid bounded options."
            }
            Self::TooLarge => "Media exceeds the inline limit. Pass a file path instead.",
            Self::FileNotFound => "The requested media file was not found.",
            Self::FilePermission => {
                "The requested media file cannot be opened with this caller's permissions."
            }
            Self::FileNotRegular => "Media must be a regular file.",
            Self::FileTooLarge => "Media file exceeds the permitted size.",
            Self::Changed => "Media changed while being read. The user can Reload.",
            Self::IndexOutOfRange => "The requested diagram index is outside this file's gallery.",
            Self::UnsupportedMedia => {
                "This media type is not supported. Keep the text/path version."
            }
            Self::UnsupportedPlatform => "Media previews are unavailable on this platform.",
            Self::RenderTimeout => "Rendering exceeded its deadline. Do not retry this one.",
            Self::RenderResource => "Rendering exceeded its resource limit.",
            Self::RenderParse => "Media could not be rendered; keep the source version.",
            Self::Busy => "Kettle is busy rendering other previews. Do not retry this one.",
            Self::UnsupportedContainer => {
                "This container is not supported by available decoders. The user can install an appropriate decoder; do not install it yourself."
            }
            Self::CodecUnavailable => {
                "A decoder for this codec is unavailable. Show the platform package hint to the user."
            }
            Self::BackendUnavailable => {
                "This media backend is unavailable. Show the configured fallback/install hint."
            }
            Self::ExternalOpenUnavailable => {
                "No permitted external handler supports this media. Keep using the Kettle preview."
            }
            Self::DisplayDisabled => {
                "Kettle previews are off or Kettle is not running. The user can turn on Agent previews in Kettle Settings; display enables immediately. Claude integration needs a new pane/session. Do not change configuration or retry."
            }
            Self::NotInKettle => {
                "This session is not running inside Kettle, so it cannot display there. Keep the text version and do not retry."
            }
            Self::NotInKettlePane => {
                "Kettle could not verify or bind this local session to a pane. Keep the text version; do not retry or choose another pane."
            }
            Self::DisplayOnly => {
                "This display-only connection cannot read terminal contents or geometry."
            }
            Self::ReadOnly => "This connection cannot perform control mutations.",
            Self::OverBudget => "Preview is over budget. Close another preview or reduce its size.",
            Self::WorkerUnavailable => {
                "Media worker is unavailable. Keep the text version. Do not retry."
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: FailureCode,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Warning {
    SourceDisplayClipped = 0,
    MissingGlyphs = 1,
    FontFallback = 2,
    SilentVideo = 3,
}
impl Warning {
    /// The warning's wire word in control replies.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceDisplayClipped => "source_display_clipped",
            Self::MissingGlyphs => "missing_glyphs",
            Self::FontFallback => "font_fallback",
            Self::SilentVideo => "silent_video",
        }
    }
}
/// Open-file identity included in content digests. Timestamps are seconds + nanoseconds
/// relative to the Unix epoch; callers provide metadata from the held source descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_seconds: i64,
    pub mtime_nanos: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digest {
    pub sha256: [u8; 32],
    pub path_identity: Option<PathIdentity>,
}
/// Where a render's pixels sit, in target-box pixels, and the source's own
/// extent: what a zoomed view needs to ask for a crop and to place what
/// comes back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderLayout {
    /// The source's own width and height: a raster's pixels, an SVG's user
    /// units.
    pub source_width: f64,
    pub source_height: f64,
    /// The rectangle the fitted image fills in the target box, centered.
    pub image_in_target: Crop,
    /// The target-box rectangle the returned pixels cover: the fitted image,
    /// or the crop asked for. An SVG held to its pixel ceiling covers it with
    /// fewer pixels than it spans.
    pub result_in_target: Crop,
}
impl RenderLayout {
    /// Finite, positive source extents and nonempty rectangles in range, the
    /// returned `width` x `height` pixels no more than the rectangle they
    /// cover.
    pub fn validate(&self, width: u32, height: u32) -> Result<(), ValidationError> {
        let extent = |value: f64| value.is_finite() && value > 0.0;
        let rect = |crop: Crop| {
            crop.width > 0
                && crop.height > 0
                && crop.x.checked_add(crop.width).is_some()
                && crop.y.checked_add(crop.height).is_some()
        };
        let result = self.result_in_target;
        if extent(self.source_width)
            && extent(self.source_height)
            && rect(self.image_in_target)
            && rect(result)
            && width <= result.width
            && height <= result.height
        {
            Ok(())
        } else {
            Err(ValidationError::BadParams)
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub width: u32,
    pub height: u32,
    /// Straight, un-premultiplied RGBA in row-major order.
    pub rgba: Vec<u8>,
    pub digest: Digest,
    /// Where the pixels sit in the target box.
    pub layout: RenderLayout,
    /// A textual kind's input exactly as it was read, from the snapshot the
    /// digest covers; `None` for a raster.
    pub exact_source: Option<String>,
    /// Display lines, without embedded CR/LF. Refuse over-cap values; never truncate in the codec.
    pub source_text: Vec<String>,
    /// All fences from the same file version, for later gallery renders without reopening it.
    pub fence_sources: Vec<String>,
    pub fence_count: u8,
    pub fence_index: Option<u8>,
    pub uncovered_scripts: Vec<String>,
    pub warnings: Vec<Warning>,
}
/// A rendered job and what it turned out to be.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderOutput {
    pub kind: MediaKind,
    pub rendered: Rendered,
}

impl Rendered {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if rgba_len(
            self.width,
            self.height,
            MAX_RENDERED_EDGE,
            MAX_RENDERED_BYTES,
        )? != self.rgba.len()
        {
            return Err(ValidationError::BadParams);
        }
        validate_identity(self.digest.path_identity)?;
        self.layout.validate(self.width, self.height)?;
        if let Some(source) = &self.exact_source {
            cap(source.len(), MAX_EXACT_SOURCE_BYTES)?;
        }
        validate_fences(self.fence_count, self.fence_index, self.fence_sources.len())?;
        cap(self.source_text.len(), MAX_SOURCE_LINES)?;
        for line in &self.source_text {
            validate_line(line)?;
        }
        for s in &self.fence_sources {
            cap(s.len(), MAX_FENCE_BYTES)?;
        }
        cap(self.uncovered_scripts.len(), MAX_UNCOVERED_SCRIPTS)?;
        for s in &self.uncovered_scripts {
            cap(s.len(), MAX_SCRIPT_BYTES)?;
        }
        cap(self.warnings.len(), MAX_WARNINGS)?;
        Ok(())
    }
}
impl Rendered {
    /// [`Self::validate`], and as a render of `kind`: a textual kind returns
    /// its source as read, within the kind's input cap; any other kind none.
    pub fn validate_as(&self, kind: MediaKind) -> Result<(), ValidationError> {
        self.validate()?;
        match (kind.exact_source_cap(), &self.exact_source) {
            (Some(cap_bytes), Some(source)) => cap(source.len(), cap_bytes),
            (None, None) => Ok(()),
            _ => Err(ValidationError::BadParams),
        }
    }
}
pub(crate) fn validate_line(s: &str) -> Result<(), ValidationError> {
    cap(s.len(), MAX_SOURCE_LINE_BYTES)?;
    if s.contains(['\n', '\r']) {
        return Err(ValidationError::BadParams);
    }
    Ok(())
}
pub(crate) fn validate_identity(p: Option<PathIdentity>) -> Result<(), ValidationError> {
    if p.is_some_and(|p| p.mtime_nanos >= 1_000_000_000) {
        return Err(ValidationError::BadParams);
    }
    Ok(())
}
pub(crate) fn validate_fences(
    count: u8,
    index: Option<u8>,
    len: usize,
) -> Result<(), ValidationError> {
    cap(usize::from(count), MAX_FENCES)?;
    if usize::from(count) != len
        || (count == 0 && index.is_some())
        || (count > 0 && index.is_none())
        || index.is_some_and(|i| i >= count)
    {
        return Err(ValidationError::IndexOutOfRange);
    }
    Ok(())
}
