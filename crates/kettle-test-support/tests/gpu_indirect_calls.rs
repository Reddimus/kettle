//! Kettle issues no indirect draws or dispatches, so its wgpu instances leave
//! out wgpu's indirect-call validation (`kettle_instance_flags` in
//! `crates/kettle-render/src/gpu.rs`). That validation builds two compute
//! pipelines at every device's creation, which only cost startup while
//! nothing uses them. An indirect call added without it would run with
//! unchecked arguments, so this fails when one appears in Kettle's crates or
//! the vendored ones. Turn the validation back on with the first such call.

use std::path::{Path, PathBuf};

/// Whether `ident` names an indirect wgpu call: `draw_indirect`,
/// `draw_indexed_indirect`, `dispatch_workgroups_indirect`, or one of the
/// `multi_draw_*` calls, `_count` variants included.
fn is_indirect_call(ident: &str) -> bool {
    ident.ends_with("_indirect")
        || ident.ends_with("_indirect_count")
        || ident.starts_with("multi_draw")
}

/// The 1-based lines of `src` whose code, outside comments and literals,
/// names an indirect call, with the name.
fn indirect_calls(src: &str) -> Vec<(usize, String)> {
    let code = kettle_test_support::code_only(src);
    let mut found = Vec::new();
    for (index, line) in code.lines().enumerate() {
        for ident in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if is_indirect_call(ident) {
                found.push((index + 1, ident.to_owned()));
            }
        }
    }
    found
}

/// Every `.rs` file under `dir`, skipping build output and hidden folders.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_crate_issues_an_indirect_gpu_call() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    for tree in ["crates", "vendor"] {
        let mut files = Vec::new();
        rust_files(&root.join(tree), &mut files);
        assert!(
            files.len() >= if tree == "crates" { 200 } else { 50 },
            "{tree}/ has only {} Rust files; the scan is not looking at the workspace",
            files.len()
        );
        for file in files {
            let src = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            for (line, ident) in indirect_calls(&src) {
                found.push(format!(
                    "{}:{line}: {ident}",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "indirect GPU calls need wgpu's VALIDATION_INDIRECT_CALL back on in \
         kettle_instance_flags (crates/kettle-render/src/gpu.rs):\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scan_finds_each_indirect_call_and_nothing_else() {
    for call in [
        "pass.draw_indirect(&args, 0);",
        "pass.draw_indexed_indirect(&args, 0);",
        "pass.multi_draw_indirect(&args, 0, 4);",
        "pass.multi_draw_indexed_indirect_count(&args, 0, &count, 0, 4);",
        "compute.dispatch_workgroups_indirect(&args, 0);",
        "bundle.draw_indirect(&args, 0);",
    ] {
        let src = format!("fn encode() {{\n    {call}\n}}\n");
        assert_eq!(indirect_calls(&src).len(), 1, "{call}");
        assert_eq!(indirect_calls(&src)[0].0, 2, "{call}: on its line");
    }
    let quiet = "// draw_indirect is never called\n\
                 /* multi_draw_indirect */\n\
                 const NAME: &str = \"dispatch_workgroups_indirect\";\n\
                 use crate::object::indirect::IndirectObject;\n\
                 fn is_indirect_call(ident: &str) -> bool { false }\n\
                 fn instances_leave_out_indirect_call_validation() {}\n\
                 let usage = wgpu::BufferUsages::INDIRECT;\n\
                 pass.draw(0..4, 0..1);\n\
                 pass.draw_indexed(0..6, 0, 0..1);\n";
    assert!(
        indirect_calls(quiet).is_empty(),
        "{:?}",
        indirect_calls(quiet)
    );
}
