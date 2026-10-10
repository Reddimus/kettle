//! The S6 captures of Claude Code and Codex replies at 60 to 80 columns:
//! every selection of a reply (the whole message, or its diagram rows) at
//! the pane's width, and every `/copy` clipboard, gives back the diagram's
//! source exactly, or is refused. Box art Codex drew is refused, as are the
//! selections whose wraps cannot be told from statements. The fixtures were
//! taken from the captures with the prototype that settled these rules,
//! and `cases.json` records what it gave each.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram-copy")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn every_s6_capture_gives_its_source_or_is_refused() {
    let cases: serde_json::Value =
        serde_json::from_str(&read(&root().join("cases.json"))).expect("cases.json");
    let (mut sources, mut refused) = (0, 0);
    for case in cases.as_array().expect("a list") {
        let file = case["file"].as_str().expect("a file");
        let path = root().join(file);
        let text = read(&path);
        let width = case["width"].as_u64().map(|width| width as usize);
        let got = kettle_core::diagram_sources(&text, width);
        match case["expect"].as_array() {
            Some(expect) => {
                let directory = path.parent().expect("a run");
                let want: Vec<String> = expect
                    .iter()
                    .map(|name| {
                        read(&directory.join(name.as_str().expect("a name")))
                            .trim_end_matches('\n')
                            .to_owned()
                    })
                    .collect();
                assert_eq!(got, Ok(want), "{file}");
                sources += 1;
            }
            None => {
                assert!(got.is_err(), "{file}: {got:?}");
                refused += 1;
            }
        }
    }
    assert_eq!((sources, refused), (93, 15));
}

/// Every diagram in the renderer's corpus, copied as `/copy` copies it,
/// comes back whole: each type it renders starts a diagram here, with its
/// front matter, comments and blank rows. A type the renderer gains that
/// this does not know fails here.
#[test]
fn every_renderer_corpus_diagram_copies_back_whole() {
    let corpus =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../kettle-media-render/tests/fixtures/mermaid");
    let mut copied = 0;
    for entry in std::fs::read_dir(&corpus).expect("the renderer corpus") {
        let path = entry.expect("an entry").path();
        if path.extension().is_none_or(|extension| extension != "mmd") {
            continue;
        }
        let text = read(&path);
        let rows: Vec<&str> = text.split('\n').map(str::trim_end).collect();
        let want = rows.join("\n").trim_end_matches('\n').to_owned();
        let name = path
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            kettle_core::diagram_sources(&text, None),
            Ok(vec![want]),
            "{name}"
        );
        copied += 1;
    }
    assert_eq!(copied, 37);
}
