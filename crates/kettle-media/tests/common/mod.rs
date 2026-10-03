#![allow(dead_code)]
use kettle_media::*;
pub fn build() -> BuildId {
    BuildId {
        crate_version: "5.0.0".into(),
        source_hash: "ab12".into(),
        protocol_version: PROTOCOL_VERSION,
    }
}
pub fn hello() -> Hello {
    Hello { build_id: build() }
}
pub fn ready() -> Ready {
    Ready { build_id: build() }
}
pub fn theme() -> Theme {
    Theme {
        background: [0; 4],
        foreground: [0; 4],
        palette: [[0; 4]; 16],
        accent: [0; 4],
        is_dark: false,
    }
}
pub fn job() -> Job {
    Job {
        kind: JobKind::Raster,
        source: Source::Bytes(vec![7]),
        theme: theme(),
        canvas: Canvas::Theme,
        target: Target {
            width: 1,
            height: 1,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: vec![],
    }
}
pub fn external() -> ExternalRequest {
    let j = job();
    Job {
        kind: j.kind,
        source: ExternalSource::Bytes(vec![7]),
        theme: j.theme,
        canvas: j.canvas,
        target: j.target,
        fallback_fonts: j.fallback_fonts,
    }
}
pub fn rendered() -> Rendered {
    Rendered {
        width: 1,
        height: 1,
        rgba: vec![255, 0, 0, 128],
        digest: content_digest(&[7], None).unwrap(),
        source_text: vec![],
        fence_sources: vec![],
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: vec![],
        warnings: vec![],
    }
}
pub fn path() -> NativePath {
    #[cfg(unix)]
    let b = b"/tmp/media".to_vec();
    #[cfg(windows)]
    let b = "C:\\media"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    NativePath::new(b).unwrap()
}
