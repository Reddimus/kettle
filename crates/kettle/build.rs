//! Embed the current git SHA in `KETTLE_GIT_SHA` so `kettle --version` can
//! include it. Other Rust CLIs that ship with this affordance (cargo,
//! rustc, ripgrep, fd) use the SHA to disambiguate bug-report builds —
//! without it, "kettle 0.1.0" on every nightly cargo install looks
//! identical to the last release.
//!
//! Outputs `KETTLE_GIT_SHA` as one of three forms:
//! - `" (<sha12>)"` — clean tip of a git checkout. Ready to concat onto
//!   the version string with one `concat!()` at the call site.
//! - `" (<sha12>+dirty)"` — same commit but with uncommitted working-tree
//!   changes. Mirrors `git describe --dirty` convention so dev-iter
//!   builds are distinguishable from the matching clean commit in bug
//!   reports.
//! - `""` — source-tarball / vendored / no-git case. `concat!` of an
//!   empty string is a no-op, so the call site doesn't need a cfg.
//!
//! The script runs on every cargo build so source edits refresh the
//! `+dirty` marker. The git subprocesses take ~20ms, well under
//! build-time noise.

#[path = "build_support/source_id.rs"]
mod source_id;

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let repo_root = PathBuf::from(&manifest).join("../..");

    // On Windows targets, embed packaging/windows/kettle.ico into the
    // .exe as a resource so the taskbar, Explorer, and Alt-Tab all
    // display our icon. No-op on Linux/macOS builds — the `target_os`
    // cfg-gates the entire winresource path so cross-platform builds
    // never even consider it. `set_icon` accepts a path relative to
    // the crate root, hence `../../packaging/...`.
    #[cfg(target_os = "windows")]
    {
        let ico = repo_root.join("packaging/windows/kettle.ico");
        if ico.exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon(ico.to_str().unwrap_or_default());
            if let Err(e) = res.compile() {
                // Don't fail the build on a resource-compile glitch —
                // the .exe will just lack an icon and a contributor
                // working without the MSVC resource compiler can
                // still iterate. Print to stderr so it shows up in
                // `cargo build --verbose` but stays out of the way.
                eprintln!("warning: winresource compile failed: {e}");
            }
            println!("cargo:rerun-if-changed=../../packaging/windows/kettle.ico");
        }
    }

    // Re-run on every cargo build so the `+dirty` marker refreshes on
    // ANY source edit. With no rerun-if directives, Cargo scans only
    // THIS package's directory, so an edit to kettle-ui or kettle-vt
    // would leave a clean-tree SHA in the binary.
    //
    // Emit a `rerun-if-changed=NONEXISTENT_FORCE_RERUN` directive
    // that points at a file Cargo can never stat. Per the
    // build-script protocol, Cargo treats a missing path as
    // "always changed" and re-runs the script every time. The
    // two `git` invocations below are ~10ms total; running on
    // every cargo build is well under build-time noise.
    println!("cargo:rerun-if-changed=NONEXISTENT_FORCE_RERUN_FOR_KETTLE_GIT_SHA");

    let repo_arg = repo_root.to_str().unwrap_or(".");
    let sha = Command::new("git")
        .args(["-C", repo_arg, "rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Tag the SHA with `+dirty` if the working tree has
    // uncommitted changes. Without this, a dev build with edits to
    // `src/main.rs` reports the same SHA as the clean tip — bug
    // reports against custom builds are indistinguishable from
    // bug reports against the matching upstream commit. `git status
    // --porcelain` produces empty output on a clean tree, non-empty
    // on any modification (tracked or untracked). Mirrors the
    // `git describe --dirty` convention.
    let dirty = Command::new("git")
        .args(["-C", repo_arg, "status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    match &sha {
        Some(sha) if dirty => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha}+dirty)"),
        Some(sha) => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha})"),
        None => println!("cargo:rustc-env=KETTLE_GIT_SHA="),
    }

    // `KETTLE_SOURCE_ID` names the source this binary was built from, and
    // `KETTLE_SOURCE_HASH` is its hex hash alone. A worker built beside this
    // binary must share it with the GUI that starts it (an update may have
    // replaced an executable since), because the frames between them are
    // defined by the source. See `build_support/source_id.rs`.
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let hash = format!("{:016x}", source_id::source_hash(&repo_root));
    println!("cargo:rustc-env=KETTLE_SOURCE_ID={version} ({hash})");
    println!("cargo:rustc-env=KETTLE_SOURCE_HASH={hash}");
}
