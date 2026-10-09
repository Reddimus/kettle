//! Real renderer-family and explicit-font integration.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "support/fonts.rs"]
mod fonts;

use std::os::unix::ffi::OsStrExt as _;

use kettle_media::{
    Canvas, FallbackFont, Job, JobKind, MediaKind, NativePath, Source, Target, Theme, Warning,
    content_digest,
};
use kettle_media_render::render_with_kind;

const CORPUS: &[(&str, &str)] = &[
    ("agentflow", include_str!("fixtures/mermaid/agentflow.mmd")),
    (
        "architecture",
        include_str!("fixtures/mermaid/architecture.mmd"),
    ),
    ("block", include_str!("fixtures/mermaid/block.mmd")),
    ("c4", include_str!("fixtures/mermaid/c4.mmd")),
    ("class", include_str!("fixtures/mermaid/class.mmd")),
    ("cynefin", include_str!("fixtures/mermaid/cynefin.mmd")),
    ("er", include_str!("fixtures/mermaid/er.mmd")),
    (
        "eventmodeling",
        include_str!("fixtures/mermaid/eventmodeling.mmd"),
    ),
    ("flowchart", include_str!("fixtures/mermaid/flowchart.mmd")),
    ("gantt", include_str!("fixtures/mermaid/gantt.mmd")),
    ("gitgraph", include_str!("fixtures/mermaid/gitgraph.mmd")),
    ("info", include_str!("fixtures/mermaid/info.mmd")),
    ("ishikawa", include_str!("fixtures/mermaid/ishikawa.mmd")),
    ("journey", include_str!("fixtures/mermaid/journey.mmd")),
    ("kanban", include_str!("fixtures/mermaid/kanban.mmd")),
    ("mindmap", include_str!("fixtures/mermaid/mindmap.mmd")),
    ("packet", include_str!("fixtures/mermaid/packet.mmd")),
    ("pie", include_str!("fixtures/mermaid/pie.mmd")),
    (
        "quadrantchart",
        include_str!("fixtures/mermaid/quadrantchart.mmd"),
    ),
    ("radar", include_str!("fixtures/mermaid/radar.mmd")),
    ("railroad", include_str!("fixtures/mermaid/railroad.mmd")),
    (
        "railroadAbnf",
        include_str!("fixtures/mermaid/railroadAbnf.mmd"),
    ),
    (
        "railroadEbnf",
        include_str!("fixtures/mermaid/railroadEbnf.mmd"),
    ),
    (
        "railroadPeg",
        include_str!("fixtures/mermaid/railroadPeg.mmd"),
    ),
    (
        "requirement",
        include_str!("fixtures/mermaid/requirement.mmd"),
    ),
    ("sankey", include_str!("fixtures/mermaid/sankey.mmd")),
    ("sequence", include_str!("fixtures/mermaid/sequence.mmd")),
    ("state", include_str!("fixtures/mermaid/state.mmd")),
    ("swimlane", include_str!("fixtures/mermaid/swimlane.mmd")),
    ("timeline", include_str!("fixtures/mermaid/timeline.mmd")),
    ("treeView", include_str!("fixtures/mermaid/treeView.mmd")),
    ("treemap", include_str!("fixtures/mermaid/treemap.mmd")),
    ("usecase", include_str!("fixtures/mermaid/usecase.mmd")),
    ("venn", include_str!("fixtures/mermaid/venn.mmd")),
    ("wardley", include_str!("fixtures/mermaid/wardley.mmd")),
    ("xychart", include_str!("fixtures/mermaid/xychart.mmd")),
    ("zenuml", include_str!("fixtures/mermaid/zenuml.mmd")),
];

fn job(source: &str, kind: JobKind) -> Job {
    Job {
        kind,
        source: Source::Bytes(source.as_bytes().to_vec()),
        target: Target {
            width: 640,
            height: 400,
            scale: 1.0,
            crop: None,
        },
        theme: Theme {
            background: [20, 24, 30, 255],
            foreground: [240, 242, 244, 255],
            palette: [[160, 100, 180, 255]; 16],
            accent: [125, 207, 255, 255],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        fallback_fonts: Vec::new(),
    }
}

#[test]
fn every_pinned_diagram_family_renders_real_pixels_and_auto_preserves_source() {
    assert_eq!(CORPUS.len(), 37);
    for &(family, source) in CORPUS {
        for kind in [JobKind::Mermaid, JobKind::Auto] {
            let job = job(source, kind);
            let (actual, output) = render_with_kind(&job, |_| {})
                .unwrap_or_else(|error| panic!("{family} {kind:?}: {error:?}"));
            output.validate().unwrap();
            assert_eq!(actual, MediaKind::Mermaid, "{family}");
            assert_eq!(
                output.digest,
                content_digest(source.as_bytes(), None).unwrap(),
                "{family}"
            );
            assert!(output.width > 0 && output.height > 0, "{family}");
            assert!(
                output.width <= job.target.width && output.height <= job.target.height,
                "{family}"
            );
            assert!(
                output
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0 && pixel[..3] != job.theme.background[..3]),
                "{family} produced only the canvas"
            );
            assert_eq!(output.fence_count, 0, "{family}");
            assert!(output.fence_sources.is_empty(), "{family}");
        }
    }
}

#[test]
fn mermaid_fallback_collection_face_matches_standalone_font_and_stays_per_job() {
    let directory = tempfile::tempdir().unwrap();
    let collection_path = directory.path().join("fallback.ttc");
    let second_path = directory.path().join("second.ttf");
    let (collection, _, second) = fonts::collection();
    std::fs::write(&collection_path, collection).unwrap();
    std::fs::write(&second_path, second).unwrap();
    let mut job = job(
        "flowchart LR\nA[\"\u{4e2d}\"] --> B[\"A\"]",
        JobKind::Mermaid,
    );
    let absent = render_with_kind(&job, |_| {}).unwrap().1;
    assert!(
        absent
            .uncovered_scripts
            .iter()
            .any(|script| script == "Han")
    );
    assert!(absent.warnings.contains(&Warning::MissingGlyphs));
    job.fallback_fonts.push(FallbackFont {
        path: NativePath::new(collection_path.as_os_str().as_bytes().to_vec()).unwrap(),
        face_index: 1,
    });
    let selected = render_with_kind(&job, |_| {}).unwrap().1;
    assert!(selected.uncovered_scripts.is_empty());
    assert!(!selected.warnings.contains(&Warning::MissingGlyphs));
    assert!(selected.warnings.contains(&Warning::FontFallback));
    job.fallback_fonts[0] = FallbackFont {
        path: NativePath::new(second_path.as_os_str().as_bytes().to_vec()).unwrap(),
        face_index: 0,
    };
    let standalone = render_with_kind(&job, |_| {}).unwrap().1;
    assert_eq!(selected.rgba, standalone.rgba);
    assert_ne!(selected.rgba, absent.rgba);
    job.fallback_fonts[0].path =
        NativePath::new(collection_path.as_os_str().as_bytes().to_vec()).unwrap();
    let other = render_with_kind(&job, |_| {}).unwrap().1;
    assert!(other.warnings.contains(&Warning::MissingGlyphs));
    assert_ne!(other.rgba, selected.rgba);
    job.fallback_fonts.clear();
    let later = render_with_kind(&job, |_| {}).unwrap().1;
    assert_eq!(later.rgba, absent.rgba);
    assert_eq!(later.warnings, absent.warnings);
}
