//! Fixed 11-byte header: `KMED`, u16 LE protocol, u8 kind, u32 LE payload length.
//! Integers/f64 bits are LE; blobs/text have u32 byte lengths; lists have u32 counts;
//! booleans/options use 0/1. Native paths carry a platform encoding tag.
//! Decode does a borrowed, allocation-free validation pass before materializing owned data.
use crate::*;
use std::io::{self, Read, Write};
use std::mem::size_of;

pub const MAGIC: [u8; 4] = *b"KMED";
pub const HEADER_BYTES: usize = 11;
// Fixed job fields, font lengths/indices and the largest optional fields fit in 256 bytes.
/// Parent/external frame cap including header, raster bytes and eight native font paths.
pub const MAX_PARENT_FRAME_BYTES: usize =
    HEADER_BYTES + MAX_RASTER_BYTES + MAX_FALLBACK_FONTS * (MAX_PATH_BYTES + 8) + 256;
/// Worker frame cap including pixels, exact source, display lines, saved fences, scripts and
/// fixed fields.
pub const MAX_WORKER_FRAME_BYTES: usize = HEADER_BYTES
    + MAX_RENDERED_BYTES
    + MAX_EXACT_SOURCE_BYTES
    + 4
    + MAX_SOURCE_LINES * (MAX_SOURCE_LINE_BYTES + 4)
    + MAX_FENCES * (MAX_FENCE_BYTES + 4)
    + MAX_UNCOVERED_SCRIPTS * (MAX_SCRIPT_BYTES + 4)
    + MAX_WARNINGS
    + MAX_VIDEO_RESULT_BYTES
    + 256;
/// A stills reply's video result at its largest: the flag, the video's
/// fixed fields, the sample count, every sample's two times and the
/// tolerance.
const MAX_VIDEO_RESULT_BYTES: usize =
    1 + (8 + 4 + 4 + 2 + 1 + 5 + 1 + 1) + 4 + MAX_VIDEO_STILLS as usize * 16 + 4;
/// The largest Ready (or Hello) frame: the header, two length-prefixed bounded strings and
/// the protocol version. A startup Failure is smaller still.
pub const MAX_READY_FRAME_BYTES: usize =
    HEADER_BYTES + 4 + MAX_VERSION_BYTES + 4 + MAX_SOURCE_HASH_BYTES + 2;
/// Conservative owned-decoder budget including list element storage as well as payloads.
pub const MAX_DECODE_ALLOCATION_BYTES: usize = MAX_WORKER_FRAME_BYTES
    + MAX_SOURCE_LINES * size_of::<String>()
    + MAX_FENCES * size_of::<String>()
    + MAX_UNCOVERED_SCRIPTS * size_of::<String>()
    + MAX_WARNINGS * size_of::<Warning>()
    + MAX_VIDEO_STILLS as usize * size_of::<StillSample>()
    + MAX_FALLBACK_FONTS * size_of::<FallbackFont>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ExternalToParent,
    ParentToWorker,
    WorkerToParent,
}
impl Direction {
    pub fn max_frame_bytes(self) -> usize {
        match self {
            Self::WorkerToParent => MAX_WORKER_FRAME_BYTES,
            _ => MAX_PARENT_FRAME_BYTES,
        }
    }
    /// Owned-decoder budget for this direction, including bounded list element storage.
    pub fn max_decode_allocation_bytes(self) -> usize {
        match self {
            Self::WorkerToParent => MAX_DECODE_ALLOCATION_BYTES,
            _ => MAX_PARENT_FRAME_BYTES + MAX_FALLBACK_FONTS * size_of::<FallbackFont>(),
        }
    }
    fn permits(self, kind: u8) -> bool {
        match self {
            Self::ExternalToParent => kind == 3,
            Self::ParentToWorker => matches!(kind, 1 | 4),
            Self::WorkerToParent => matches!(kind, 2 | 5 | 6 | 7),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Hello(Hello),
    Ready(Ready),
    ExternalRequest(ExternalRequest),
    Job(Job),
    Rendered(Rendered),
    DetectedRendered { kind: MediaKind, rendered: Rendered },
    Failure(Failure),
}
impl Frame {
    pub fn kind(&self) -> u8 {
        match self {
            Self::Hello(_) => 1,
            Self::Ready(_) => 2,
            Self::ExternalRequest(_) => 3,
            Self::Job(_) => 4,
            Self::Rendered(_) => 5,
            Self::Failure(_) => 6,
            Self::DetectedRendered { .. } => 7,
        }
    }
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Hello(h) => h.build_id.validate(),
            Self::Ready(r) => r.build_id.validate(),
            Self::Rendered(r) => r.validate(),
            Self::DetectedRendered { kind, rendered } => rendered.validate_as(*kind),
            Self::Failure(_) => Ok(()),
            Self::Job(j) => validate_worker_job(j),
            Self::ExternalRequest(j) => {
                validate_job(j.kind, &j.fallback_fonts, j.target)?;
                match &j.source {
                    ExternalSource::Bytes(b) => validate_bytes(j.kind, b),
                    ExternalSource::Path { path, .. } => validate_path(path.as_bytes()),
                }
            }
        }
    }
}
fn validate_worker_job(j: &Job) -> Result<(), ValidationError> {
    validate_job(j.kind, &j.fallback_fonts, j.target)?;
    match &j.source {
        Source::Bytes(b) => validate_bytes(j.kind, b),
        Source::Path { path, .. } => validate_path(path.as_bytes()),
    }
}
fn validate_bytes(kind: JobKind, b: &[u8]) -> Result<(), ValidationError> {
    cap(b.len(), kind.input_cap())?;
    if matches!(
        kind,
        JobKind::Mermaid | JobKind::Svg | JobKind::MarkdownDiagrams { .. }
    ) {
        std::str::from_utf8(b).map_err(|_| ValidationError::InvalidUtf8)?;
    }
    Ok(())
}
fn validate_job(
    kind: JobKind,
    fonts: &[FallbackFont],
    target: Target,
) -> Result<(), ValidationError> {
    kind.validate()?;
    target.validate()?;
    cap(fonts.len(), MAX_FALLBACK_FONTS)?;
    for font in fonts {
        validate_path(font.path.as_bytes())?;
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Validation(ValidationError),
    Truncated,
    TrailingBytes,
    BadMagic,
    RestartRequired,
    UnknownEnum,
    WrongDirection,
    LengthMismatch,
    Io,
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(e) => e.fmt(f),
            _ => f.write_str(match self {
                Self::Truncated => "truncated media frame",
                Self::TrailingBytes => "trailing media bytes",
                Self::BadMagic => "invalid media magic",
                Self::RestartRequired => "media protocol restart required",
                Self::UnknownEnum => "unknown media enum",
                Self::WrongDirection => "wrong media direction",
                Self::LengthMismatch => "media length mismatch",
                Self::Io => "media I/O failed",
                Self::Validation(_) => unreachable!("handled above"),
            }),
        }
    }
}
impl std::error::Error for WireError {}

impl From<ValidationError> for WireError {
    fn from(e: ValidationError) -> Self {
        Self::Validation(e)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationStats {
    pub requested_bytes: usize,
    pub largest_request: usize,
}
/// Includes requested Vec capacities and String copies. Invalid preflight frames request zero.
#[derive(Debug)]
pub struct DecodeReport {
    pub result: Result<Frame, WireError>,
    pub allocations: AllocationStats,
}

pub fn encode(frame: &Frame, direction: Direction) -> Result<Vec<u8>, WireError> {
    if !direction.permits(frame.kind()) {
        return Err(WireError::WrongDirection);
    }
    frame.validate()?;
    encode_payload(frame.kind(), direction, |w| put_frame(w, frame))
}
/// A worker job frame, encoded from a borrow. Byte-for-byte what `encode`
/// writes for `Frame::Job`, without copying the job (its source can be a
/// multi-megabyte image or document) into a frame first.
pub fn encode_job(job: &Job) -> Result<Vec<u8>, WireError> {
    const JOB: u8 = 4;
    let direction = Direction::ParentToWorker;
    if !direction.permits(JOB) {
        return Err(WireError::WrongDirection);
    }
    validate_worker_job(job)?;
    encode_payload(JOB, direction, |w| put_job(w, job))
}
/// Measure the payload, then write the header and payload into one exactly
/// sized buffer.
fn encode_payload(
    kind: u8,
    direction: Direction,
    put: impl Fn(&mut Writer) -> Result<(), WireError>,
) -> Result<Vec<u8>, WireError> {
    let mut measure = Writer {
        bytes: None,
        len: 0,
        max: direction.max_frame_bytes() - HEADER_BYTES,
    };
    put(&mut measure)?;
    let payload_len = u32::try_from(measure.len).map_err(|_| ValidationError::TooLarge)?;
    let capacity = measure
        .len
        .checked_add(HEADER_BYTES)
        .ok_or(ValidationError::TooLarge)?;
    let mut writer = Writer {
        bytes: Some(Vec::with_capacity(capacity)),
        len: 0,
        max: direction.max_frame_bytes(),
    };
    writer.raw(&MAGIC)?;
    writer.u16(PROTOCOL_VERSION)?;
    writer.u8(kind)?;
    writer.u32(payload_len)?;
    put(&mut writer)?;
    Ok(writer.bytes.unwrap_or_default())
}
/// Typed ctl/MCP entry point. No worker job or GUI-only source can be returned.
pub fn decode_external_request(bytes: &[u8]) -> Result<ExternalRequest, WireError> {
    match decode(bytes, Direction::ExternalToParent)? {
        Frame::ExternalRequest(request) => Ok(request),
        _ => Err(WireError::WrongDirection),
    }
}

pub fn decode(bytes: &[u8], direction: Direction) -> Result<Frame, WireError> {
    decode_with_stats(bytes, direction).result
}
pub fn decode_with_stats(bytes: &[u8], direction: Direction) -> DecodeReport {
    let mut allocations = AllocationStats::default();
    let result = (|| {
        let (kind, payload) = header(bytes, direction)?;
        let mut preflight = Reader {
            bytes: payload,
            pos: 0,
            owned: false,
            allocation_limit: direction.max_decode_allocation_bytes(),
            stats: AllocationStats::default(),
        };
        let _ = parse_frame(&mut preflight, kind)?;
        preflight.finish()?;
        let mut reader = Reader {
            bytes: payload,
            pos: 0,
            owned: true,
            allocation_limit: direction.max_decode_allocation_bytes(),
            stats: AllocationStats::default(),
        };
        let result = parse_frame(&mut reader, kind).and_then(|frame| {
            reader.finish()?;
            Ok(frame)
        });
        allocations = reader.stats;
        result
    })();
    DecodeReport {
        result,
        allocations,
    }
}
fn header(bytes: &[u8], direction: Direction) -> Result<(u8, &[u8]), WireError> {
    if bytes.len() < HEADER_BYTES {
        return Err(WireError::Truncated);
    }
    let (kind, len) = header_prefix(&bytes[..HEADER_BYTES], direction)?;
    let total = len
        .checked_add(HEADER_BYTES)
        .ok_or(ValidationError::TooLarge)?;
    if bytes.len() < total {
        return Err(WireError::Truncated);
    }
    if bytes.len() > total {
        return Err(WireError::TrailingBytes);
    }
    Ok((kind, &bytes[HEADER_BYTES..]))
}
fn header_prefix(bytes: &[u8], direction: Direction) -> Result<(u8, usize), WireError> {
    if bytes.len() != HEADER_BYTES {
        return Err(WireError::Truncated);
    }
    if bytes[..4] != MAGIC {
        return Err(WireError::BadMagic);
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) != PROTOCOL_VERSION {
        return Err(WireError::RestartRequired);
    }
    let kind = bytes[6];
    // Recognize tags from the same table that owns their permitted directions.
    if ![
        Direction::ExternalToParent,
        Direction::ParentToWorker,
        Direction::WorkerToParent,
    ]
    .into_iter()
    .any(|candidate| candidate.permits(kind))
    {
        return Err(WireError::UnknownEnum);
    }
    if !direction.permits(kind) {
        return Err(WireError::WrongDirection);
    }
    let len = usize::try_from(u32::from_le_bytes([
        bytes[7], bytes[8], bytes[9], bytes[10],
    ]))
    .map_err(|_| ValidationError::TooLarge)?;
    cap(len, direction.max_frame_bytes() - HEADER_BYTES)?;
    Ok((kind, len))
}
/// Reads one frame only; subsequent frames remain in the stream. EOF before a header is None.
/// The length/direction/version cap is checked before allocating a payload buffer. P2 owns deadlines.
pub fn read_frame(
    reader: &mut impl Read,
    direction: Direction,
) -> Result<Option<Frame>, WireError> {
    read_frame_within(reader, direction, direction.max_frame_bytes())
}
/// [`read_frame`] for a stage that expects a small frame: a header claiming more than
/// `max_frame_bytes` in all (header included) is refused before any payload buffer is
/// allocated. Ready is about 150 bytes, so it is not read under the full rendered-reply cap.
pub fn read_frame_within(
    reader: &mut impl Read,
    direction: Direction,
    max_frame_bytes: usize,
) -> Result<Option<Frame>, WireError> {
    let mut head = [0; HEADER_BYTES];
    loop {
        match reader.read(&mut head[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(WireError::Io),
        }
    }
    reader.read_exact(&mut head[1..]).map_err(read_error)?;
    let (_, len) = header_prefix(&head, direction)?;
    cap(len, max_frame_bytes.saturating_sub(HEADER_BYTES))?;
    let total = len
        .checked_add(HEADER_BYTES)
        .ok_or(ValidationError::TooLarge)?;
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&head);
    bytes.resize(total, 0);
    reader
        .read_exact(&mut bytes[HEADER_BYTES..])
        .map_err(read_error)?;
    decode(&bytes, direction).map(Some)
}
fn read_error(e: io::Error) -> WireError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        WireError::Truncated
    } else {
        WireError::Io
    }
}
pub fn write_frame(
    writer: &mut impl Write,
    frame: &Frame,
    direction: Direction,
) -> Result<(), WireError> {
    writer
        .write_all(&encode(frame, direction)?)
        .map_err(|_| WireError::Io)?;
    writer.flush().map_err(|_| WireError::Io)
}

struct Writer {
    bytes: Option<Vec<u8>>,
    len: usize,
    max: usize,
}
impl Writer {
    fn raw(&mut self, b: &[u8]) -> Result<(), WireError> {
        self.len = self
            .len
            .checked_add(b.len())
            .ok_or(ValidationError::TooLarge)?;
        cap(self.len, self.max)?;
        if let Some(v) = &mut self.bytes {
            v.extend_from_slice(b);
        }
        Ok(())
    }
    fn u8(&mut self, v: u8) -> Result<(), WireError> {
        self.raw(&[v])
    }
    fn u16(&mut self, v: u16) -> Result<(), WireError> {
        self.raw(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> Result<(), WireError> {
        self.raw(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> Result<(), WireError> {
        self.raw(&v.to_le_bytes())
    }
    fn f64(&mut self, v: f64) -> Result<(), WireError> {
        self.u64(v.to_bits())
    }
    fn count(&mut self, n: usize) -> Result<(), WireError> {
        self.u32(u32::try_from(n).map_err(|_| ValidationError::TooLarge)?)
    }
    fn blob(&mut self, b: &[u8]) -> Result<(), WireError> {
        self.count(b.len())?;
        self.raw(b)
    }
    fn path(&mut self, p: &NativePath) -> Result<(), WireError> {
        self.u8(if cfg!(windows) { 1 } else { 0 })?;
        self.blob(p.as_bytes())
    }
    fn opt_f64(&mut self, v: Option<f64>) -> Result<(), WireError> {
        self.u8(u8::from(v.is_some()))?;
        if let Some(v) = v {
            self.f64(v)?;
        }
        Ok(())
    }
    fn build(&mut self, b: &BuildId) -> Result<(), WireError> {
        self.blob(b.crate_version.as_bytes())?;
        self.blob(b.source_hash.as_bytes())?;
        self.u16(b.protocol_version)
    }
    fn kind(&mut self, k: JobKind) -> Result<(), WireError> {
        match k {
            JobKind::Auto => self.u8(6),
            JobKind::Mermaid => self.u8(0),
            JobKind::Svg => self.u8(1),
            JobKind::Raster => self.u8(2),
            JobKind::MarkdownDiagrams { index } => {
                self.u8(3)?;
                self.u8(index)
            }
            JobKind::VideoProbe => self.u8(4),
            JobKind::VideoStills(v) => {
                self.u8(5)?;
                self.u8(v.count)?;
                self.u32(v.max_edge)?;
                self.f64(v.start_s)?;
                self.opt_f64(v.end_s)?;
                self.opt_f64(v.at_s)?;
                match v.layout {
                    StillsLayout::Poster => self.u8(0),
                    StillsLayout::Sheet { cols, labels } => {
                        self.u8(1)?;
                        self.u8(cols)?;
                        self.u8(u8::from(labels))
                    }
                }
            }
            JobKind::CachedThumbnail(of) => {
                self.u8(7)?;
                self.raw(&of.uri_sha256)?;
                self.raw(&of.mtime_seconds.to_le_bytes())?;
                self.u32(of.mtime_nanos)
            }
        }
    }
    fn job_tail<S>(&mut self, j: &Job<S>) -> Result<(), WireError> {
        self.raw(&j.theme.background)?;
        self.raw(&j.theme.foreground)?;
        for c in j.theme.palette {
            self.raw(&c)?;
        }
        self.raw(&j.theme.accent)?;
        self.u8(u8::from(j.theme.is_dark))?;
        self.u8(match j.canvas {
            Canvas::Theme => 0,
            Canvas::White => 1,
            Canvas::Checker => 2,
        })?;
        self.u32(j.target.width)?;
        self.u32(j.target.height)?;
        self.f64(j.target.scale)?;
        self.u8(u8::from(j.target.crop.is_some()))?;
        if let Some(c) = j.target.crop {
            self.u32(c.x)?;
            self.u32(c.y)?;
            self.u32(c.width)?;
            self.u32(c.height)?;
        }
        self.count(j.fallback_fonts.len())?;
        for f in &j.fallback_fonts {
            self.path(&f.path)?;
            self.u32(f.face_index)?;
        }
        Ok(())
    }
    fn strings(&mut self, s: &[String]) -> Result<(), WireError> {
        self.count(s.len())?;
        for s in s {
            self.blob(s.as_bytes())?;
        }
        Ok(())
    }
}
fn put_job(w: &mut Writer, j: &Job) -> Result<(), WireError> {
    w.kind(j.kind)?;
    match &j.source {
        Source::Bytes(b) => {
            w.u8(0)?;
            w.blob(b)?;
        }
        Source::Path {
            path,
            authorization,
        } => {
            w.u8(1)?;
            w.path(path)?;
            match authorization {
                Authorization::ExternalAttested(a) => {
                    w.u8(0)?;
                    w.u64(a.dev)?;
                    w.u64(a.ino)?;
                }
                Authorization::UserPull(_) => w.u8(1)?,
            }
        }
    }
    w.job_tail(j)
}
fn put_frame(w: &mut Writer, frame: &Frame) -> Result<(), WireError> {
    match frame {
        Frame::Hello(h) => w.build(&h.build_id),
        Frame::Ready(r) => w.build(&r.build_id),
        Frame::Failure(f) => w.u8(f.code as u8),
        Frame::ExternalRequest(j) => {
            w.kind(j.kind)?;
            match &j.source {
                ExternalSource::Bytes(b) => {
                    w.u8(0)?;
                    w.blob(b)?;
                }
                ExternalSource::Path { path, attestation } => {
                    w.u8(1)?;
                    w.path(path)?;
                    w.u8(0)?;
                    w.u64(attestation.dev)?;
                    w.u64(attestation.ino)?;
                }
            }
            w.job_tail(j)
        }
        Frame::Job(j) => put_job(w, j),
        Frame::Rendered(rendered) => put_rendered(w, rendered),
        Frame::DetectedRendered { kind, rendered } => {
            w.u8(media_kind_tag(*kind))?;
            put_rendered(w, rendered)
        }
    }
}

fn media_kind_tag(kind: MediaKind) -> u8 {
    match kind {
        MediaKind::Raster => 0,
        MediaKind::Svg => 1,
        MediaKind::Mermaid => 2,
        MediaKind::Markdown => 3,
        MediaKind::Video => 4,
    }
}

fn put_rendered(w: &mut Writer, r: &Rendered) -> Result<(), WireError> {
    w.u32(r.width)?;
    w.u32(r.height)?;
    w.blob(&r.rgba)?;
    w.raw(&r.digest.sha256)?;
    w.u8(u8::from(r.digest.path_identity.is_some()))?;
    if let Some(p) = r.digest.path_identity {
        w.u64(p.dev)?;
        w.u64(p.ino)?;
        w.u64(p.size)?;
        w.u64(p.mtime_seconds as u64)?;
        w.u32(p.mtime_nanos)?;
    }
    w.strings(&r.source_text)?;
    w.u8(r.fence_count)?;
    w.u8(u8::from(r.fence_index.is_some()))?;
    if let Some(i) = r.fence_index {
        w.u8(i)?;
    }
    w.strings(&r.fence_sources)?;
    w.strings(&r.uncovered_scripts)?;
    w.count(r.warnings.len())?;
    for c in &r.warnings {
        w.u8(*c as u8)?;
    }
    let layout = &r.layout;
    w.f64(layout.source_width)?;
    w.f64(layout.source_height)?;
    for crop in [layout.image_in_target, layout.result_in_target] {
        w.u32(crop.x)?;
        w.u32(crop.y)?;
        w.u32(crop.width)?;
        w.u32(crop.height)?;
    }
    w.u8(u8::from(r.exact_source.is_some()))?;
    if let Some(source) = &r.exact_source {
        w.blob(source.as_bytes())?;
    }
    w.u8(u8::from(r.video.is_some()))?;
    if let Some(video) = &r.video {
        let info = &video.info;
        w.u64(info.duration_ms)?;
        w.u32(info.width)?;
        w.u32(info.height)?;
        w.u16(info.rotation)?;
        w.u8(info.codec as u8)?;
        w.u8(u8::from(info.fps_milli.is_some()))?;
        w.u32(info.fps_milli.unwrap_or(0))?;
        w.u8(u8::from(info.has_audio))?;
        w.u8(info.container.map_or(0, container_tag))?;
        w.count(video.samples.len())?;
        for sample in &video.samples {
            w.u64(sample.requested_ms)?;
            w.u64(sample.actual_ms)?;
        }
        w.u32(video.tolerance_ms)?;
    }
    Ok(())
}

/// A container's wire tag, from 1: 0 is an animated image, in no container.
fn container_tag(container: crate::video::VideoContainer) -> u8 {
    use crate::video::VideoContainer as C;
    match container {
        C::IsoBmff => 1,
        C::QuickTime => 2,
        C::Matroska => 3,
        C::WebM => 4,
        C::Avi => 5,
        C::FlashVideo => 6,
        C::MpegProgramStream => 7,
        C::MpegVideo => 8,
        C::MpegTransportStream => 9,
        C::Ogg => 10,
        C::Asf => 11,
    }
}

fn container_of(tag: u8) -> Result<Option<crate::video::VideoContainer>, WireError> {
    use crate::video::VideoContainer as C;
    Ok(Some(match tag {
        0 => return Ok(None),
        1 => C::IsoBmff,
        2 => C::QuickTime,
        3 => C::Matroska,
        4 => C::WebM,
        5 => C::Avi,
        6 => C::FlashVideo,
        7 => C::MpegProgramStream,
        8 => C::MpegVideo,
        9 => C::MpegTransportStream,
        10 => C::Ogg,
        11 => C::Asf,
        _ => return Err(WireError::UnknownEnum),
    }))
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    owned: bool,
    allocation_limit: usize,
    stats: AllocationStats,
}
impl<'a> Reader<'a> {
    fn finish(&self) -> Result<(), WireError> {
        if self.pos != self.bytes.len() {
            Err(WireError::TrailingBytes)
        } else {
            Ok(())
        }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(ValidationError::TooLarge)?;
        let b = self.bytes.get(self.pos..end).ok_or(WireError::Truncated)?;
        self.pos = end;
        Ok(b)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        self.take(N)?.try_into().map_err(|_| WireError::Truncated)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn f64(&mut self) -> Result<f64, WireError> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn flag(&mut self) -> Result<bool, WireError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::UnknownEnum),
        }
    }
    fn count(&mut self, max: usize) -> Result<usize, WireError> {
        let n = usize::try_from(self.u32()?).map_err(|_| ValidationError::TooLarge)?;
        cap(n, max)?;
        Ok(n)
    }
    fn blob(&mut self, max: usize) -> Result<&'a [u8], WireError> {
        let n = self.count(max)?;
        self.take(n)
    }
    fn text(&mut self, max: usize) -> Result<&'a str, WireError> {
        std::str::from_utf8(self.blob(max)?).map_err(|_| ValidationError::InvalidUtf8.into())
    }
    fn charge(&mut self, n: usize) -> Result<(), WireError> {
        self.stats.requested_bytes = self
            .stats
            .requested_bytes
            .checked_add(n)
            .ok_or(ValidationError::TooLarge)?;
        cap(self.stats.requested_bytes, self.allocation_limit)?;
        self.stats.largest_request = self.stats.largest_request.max(n);
        Ok(())
    }
    fn bytes_owned(&mut self, b: &[u8]) -> Result<Vec<u8>, WireError> {
        if self.owned {
            self.charge(b.len())?;
            Ok(b.to_vec())
        } else {
            Ok(Vec::new())
        }
    }
    fn string_owned(&mut self, s: &str) -> Result<String, WireError> {
        if self.owned {
            self.charge(s.len())?;
            Ok(s.to_owned())
        } else {
            Ok(String::new())
        }
    }
    fn list<T>(&mut self, n: usize) -> Result<Vec<T>, WireError> {
        if self.owned {
            self.charge(
                n.checked_mul(size_of::<T>())
                    .ok_or(ValidationError::TooLarge)?,
            )?;
            Ok(Vec::with_capacity(n))
        } else {
            Ok(Vec::new())
        }
    }
    fn path(&mut self) -> Result<NativePath, WireError> {
        if self.u8()? != if cfg!(windows) { 1 } else { 0 } {
            return Err(WireError::UnknownEnum);
        }
        let b = self.blob(MAX_PATH_BYTES)?;
        validate_path(b)?;
        Ok(NativePath(self.bytes_owned(b)?))
    }
    fn opt_f64(&mut self) -> Result<Option<f64>, WireError> {
        if self.flag()? {
            Ok(Some(self.f64()?))
        } else {
            Ok(None)
        }
    }
    fn build(&mut self) -> Result<BuildId, WireError> {
        let version = self.text(MAX_VERSION_BYTES)?;
        let hash = self.text(MAX_SOURCE_HASH_BYTES)?;
        validate_build(version, hash)?;
        Ok(BuildId {
            crate_version: self.string_owned(version)?,
            source_hash: self.string_owned(hash)?,
            protocol_version: self.u16()?,
        })
    }
    fn media_kind(&mut self) -> Result<MediaKind, WireError> {
        match self.u8()? {
            0 => Ok(MediaKind::Raster),
            1 => Ok(MediaKind::Svg),
            2 => Ok(MediaKind::Mermaid),
            3 => Ok(MediaKind::Markdown),
            4 => Ok(MediaKind::Video),
            _ => Err(WireError::UnknownEnum),
        }
    }
    fn kind(&mut self) -> Result<JobKind, WireError> {
        let k = match self.u8()? {
            0 => JobKind::Mermaid,
            1 => JobKind::Svg,
            2 => JobKind::Raster,
            3 => JobKind::MarkdownDiagrams { index: self.u8()? },
            4 => JobKind::VideoProbe,
            5 => JobKind::VideoStills(VideoStills {
                count: self.u8()?,
                max_edge: self.u32()?,
                start_s: self.f64()?,
                end_s: self.opt_f64()?,
                at_s: self.opt_f64()?,
                layout: match self.u8()? {
                    0 => StillsLayout::Poster,
                    1 => StillsLayout::Sheet {
                        cols: self.u8()?,
                        labels: self.flag()?,
                    },
                    _ => return Err(WireError::UnknownEnum),
                },
            }),
            6 => JobKind::Auto,
            7 => JobKind::CachedThumbnail(ThumbnailOf {
                uri_sha256: self.array()?,
                mtime_seconds: i64::from_le_bytes(self.array()?),
                mtime_nanos: self.u32()?,
            }),
            _ => return Err(WireError::UnknownEnum),
        };
        k.validate()?;
        Ok(k)
    }
    fn source(&mut self, kind: JobKind, external: bool) -> Result<Source, WireError> {
        match self.u8()? {
            0 => {
                let b = self.blob(kind.input_cap())?;
                if matches!(
                    kind,
                    JobKind::Mermaid | JobKind::Svg | JobKind::MarkdownDiagrams { .. }
                ) {
                    std::str::from_utf8(b).map_err(|_| ValidationError::InvalidUtf8)?;
                }
                Ok(Source::Bytes(self.bytes_owned(b)?))
            }
            1 => {
                let path = self.path()?;
                let authorization = match self.u8()? {
                    0 => Authorization::ExternalAttested(ExternalAttested {
                        dev: self.u64()?,
                        ino: self.u64()?,
                    }),
                    1 if !external => Authorization::UserPull(UserPull { _private: () }),
                    _ => return Err(WireError::UnknownEnum),
                };
                Ok(Source::Path {
                    path,
                    authorization,
                })
            }
            _ => Err(WireError::UnknownEnum),
        }
    }
    fn job_tail<S>(&mut self, kind: JobKind, source: S) -> Result<Job<S>, WireError> {
        let background = self.array()?;
        let foreground = self.array()?;
        let mut palette = [[0; 4]; 16];
        for c in &mut palette {
            *c = self.array()?;
        }
        let theme = Theme {
            background,
            foreground,
            palette,
            accent: self.array()?,
            is_dark: self.flag()?,
        };
        let canvas = match self.u8()? {
            0 => Canvas::Theme,
            1 => Canvas::White,
            2 => Canvas::Checker,
            _ => return Err(WireError::UnknownEnum),
        };
        let width = self.u32()?;
        let height = self.u32()?;
        let scale = self.f64()?;
        let crop = if self.flag()? {
            Some(Crop {
                x: self.u32()?,
                y: self.u32()?,
                width: self.u32()?,
                height: self.u32()?,
            })
        } else {
            None
        };
        let target = Target {
            width,
            height,
            scale,
            crop,
        };
        target.validate()?;
        let n = self.count(MAX_FALLBACK_FONTS)?;
        let mut fallback_fonts = self.list(n)?;
        for _ in 0..n {
            let font = FallbackFont {
                path: self.path()?,
                face_index: self.u32()?,
            };
            if self.owned {
                fallback_fonts.push(font);
            }
        }
        Ok(Job {
            kind,
            source,
            theme,
            canvas,
            target,
            fallback_fonts,
        })
    }
    fn strings(
        &mut self,
        count_cap: usize,
        byte_cap: usize,
        lines: bool,
    ) -> Result<Vec<String>, WireError> {
        let n = self.count(count_cap)?;
        let mut v = self.list(n)?;
        for _ in 0..n {
            let s = self.text(byte_cap)?;
            if lines {
                validate_line(s)?;
            }
            let s = self.string_owned(s)?;
            if self.owned {
                v.push(s);
            }
        }
        Ok(v)
    }
    /// A Rendered payload; for a reply that names its `kind`, its source is
    /// held to that kind: within its input cap when it is text, and absent
    /// when it is not.
    fn rendered(&mut self, kind: Option<MediaKind>) -> Result<Rendered, WireError> {
        let width = self.u32()?;
        let height = self.u32()?;
        let expected = rgba_len(width, height, MAX_RENDERED_EDGE, MAX_RENDERED_BYTES)?;
        let pixels = self.blob(MAX_RENDERED_BYTES)?;
        if pixels.len() != expected {
            return Err(WireError::LengthMismatch);
        }
        let rgba = self.bytes_owned(pixels)?;
        let sha256 = self.array()?;
        let path_identity = if self.flag()? {
            Some(PathIdentity {
                dev: self.u64()?,
                ino: self.u64()?,
                size: self.u64()?,
                mtime_seconds: self.u64()? as i64,
                mtime_nanos: self.u32()?,
            })
        } else {
            None
        };
        validate_identity(path_identity)?;
        let source_text = self.strings(MAX_SOURCE_LINES, MAX_SOURCE_LINE_BYTES, true)?;
        let fence_count = self.u8()?;
        let fence_index = if self.flag()? { Some(self.u8()?) } else { None };
        // Read count independently to validate index/count before reading any fence bytes.
        let n = self.count(MAX_FENCES)?;
        validate_fences(fence_count, fence_index, n)?;
        let mut fence_sources = self.list(n)?;
        for _ in 0..n {
            let s = self.text(MAX_FENCE_BYTES)?;
            let s = self.string_owned(s)?;
            if self.owned {
                fence_sources.push(s);
            }
        }
        let uncovered_scripts = self.strings(MAX_UNCOVERED_SCRIPTS, MAX_SCRIPT_BYTES, false)?;
        let n = self.count(MAX_WARNINGS)?;
        let mut warnings = self.list(n)?;
        for _ in 0..n {
            let w = match self.u8()? {
                0 => Warning::SourceDisplayClipped,
                1 => Warning::MissingGlyphs,
                2 => Warning::FontFallback,
                3 => Warning::SilentVideo,
                _ => return Err(WireError::UnknownEnum),
            };
            if self.owned {
                warnings.push(w);
            }
        }
        let source_width = self.f64()?;
        let source_height = self.f64()?;
        let mut crop = || -> Result<Crop, WireError> {
            Ok(Crop {
                x: self.u32()?,
                y: self.u32()?,
                width: self.u32()?,
                height: self.u32()?,
            })
        };
        let image_in_target = crop()?;
        let result_in_target = crop()?;
        let layout = RenderLayout {
            source_width,
            source_height,
            image_in_target,
            result_in_target,
        };
        layout.validate(width, height)?;
        let source_cap = kind.map(MediaKind::exact_source_cap);
        let exact_source = if self.flag()? {
            let cap_bytes = match source_cap {
                Some(Some(cap_bytes)) => cap_bytes,
                Some(None) => return Err(ValidationError::BadParams.into()),
                None => MAX_EXACT_SOURCE_BYTES,
            };
            let source = self.text(cap_bytes)?;
            Some(self.string_owned(source)?)
        } else if matches!(source_cap, Some(Some(_))) {
            return Err(ValidationError::BadParams.into());
        } else {
            None
        };
        // Video stills return what the video is and their frames' times, and
        // no other kind does.
        let video = if self.flag()? {
            if kind.is_some_and(|kind| kind != MediaKind::Video) {
                return Err(ValidationError::BadParams.into());
            }
            let duration_ms = self.u64()?;
            let width = self.u32()?;
            let height = self.u32()?;
            let rotation = self.u16()?;
            let codec = *VideoCodec::ALL
                .get(usize::from(self.u8()?))
                .ok_or(WireError::UnknownEnum)?;
            let has_fps = self.flag()?;
            let fps = self.u32()?;
            if !has_fps && fps != 0 {
                return Err(ValidationError::BadParams.into());
            }
            let has_audio = self.flag()?;
            let container = container_of(self.u8()?)?;
            // At most sixteen, so both passes keep them: the borrowed pass
            // validates the whole result before any pixel is copied.
            let n = self.count(usize::from(MAX_VIDEO_STILLS))?;
            let mut samples = Vec::with_capacity(n);
            for _ in 0..n {
                samples.push(StillSample {
                    requested_ms: self.u64()?,
                    actual_ms: self.u64()?,
                });
            }
            let video = VideoStillsResult {
                info: VideoInfo {
                    duration_ms,
                    width,
                    height,
                    rotation,
                    codec,
                    fps_milli: has_fps.then_some(fps),
                    has_audio,
                    container,
                },
                samples,
                tolerance_ms: self.u32()?,
            };
            video.validate()?;
            Some(video)
        } else if kind == Some(MediaKind::Video) {
            return Err(ValidationError::BadParams.into());
        } else {
            None
        };
        Ok(Rendered {
            width,
            height,
            rgba,
            digest: Digest {
                sha256,
                path_identity,
            },
            layout,
            exact_source,
            source_text,
            fence_sources,
            fence_count,
            fence_index,
            uncovered_scripts,
            warnings,
            video,
        })
    }
}
fn parse_frame(r: &mut Reader<'_>, kind: u8) -> Result<Frame, WireError> {
    Ok(match kind {
        1 => Frame::Hello(Hello {
            build_id: r.build()?,
        }),
        2 => Frame::Ready(Ready {
            build_id: r.build()?,
        }),
        3 => {
            let kind = r.kind()?;
            let source = match r.source(kind, true)? {
                Source::Bytes(b) => ExternalSource::Bytes(b),
                Source::Path {
                    path,
                    authorization: Authorization::ExternalAttested(attestation),
                } => ExternalSource::Path { path, attestation },
                Source::Path { .. } => return Err(WireError::UnknownEnum),
            };
            Frame::ExternalRequest(r.job_tail(kind, source)?)
        }
        4 => {
            let kind = r.kind()?;
            let source = r.source(kind, false)?;
            Frame::Job(r.job_tail(kind, source)?)
        }
        5 => Frame::Rendered(r.rendered(None)?),
        7 => {
            let kind = r.media_kind()?;
            Frame::DetectedRendered {
                kind,
                rendered: r.rendered(Some(kind))?,
            }
        }
        6 => Frame::Failure(Failure {
            code: failure_code(r.u8()?)?,
        }),
        _ => return Err(WireError::UnknownEnum),
    })
}
fn failure_code(v: u8) -> Result<FailureCode, WireError> {
    use FailureCode::*;
    Ok(match v {
        0 => BadParams,
        1 => TooLarge,
        2 => FileNotFound,
        3 => FilePermission,
        4 => FileNotRegular,
        5 => FileTooLarge,
        6 => Changed,
        7 => IndexOutOfRange,
        8 => UnsupportedMedia,
        9 => UnsupportedPlatform,
        10 => RenderTimeout,
        11 => RenderResource,
        12 => RenderParse,
        13 => RestartRequired,
        14 => UnknownMethod,
        15 => Busy,
        16 => UnsupportedContainer,
        17 => CodecUnavailable,
        18 => BackendUnavailable,
        19 => ExternalOpenUnavailable,
        20 => DisplayDisabled,
        21 => NotInKettle,
        22 => NotInKettlePane,
        23 => DisplayOnly,
        24 => ReadOnly,
        25 => OverBudget,
        26 => WorkerUnavailable,
        27 => SandboxUnavailable,
        _ => return Err(WireError::UnknownEnum),
    })
}
